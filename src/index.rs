//! Query side: memory-map `index.fst` + `records.bin` and serve prefix
//! autocomplete (README §4, §5).
//!
//! The engine is **domain-agnostic** — it returns neutral [`Record`]s
//! (`group`/`rank`/`payload`). A domain adapter such as [`crate::geo`] decodes the
//! payload; see [`Index::suggest`].
//!
//! `suggest(prefix, limit)`:
//! 1. normalize the prefix (idempotent — safe even if the caller pre-normalized),
//! 2. run a `StartsWith` automaton over the FST (a range scan),
//! 3. expand matched FST values → postings → record ids,
//! 4. dedup by record id then by `group`,
//! 5. rank by `rank` desc (payload asc as a deterministic tiebreak) and take `limit`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::Path;

use fst::automaton::{Automaton, Str};
use fst::{IntoStreamer, Map, Streamer};
use memmap2::Mmap;

use crate::normalize::normalize;
use crate::records::{Record, RecordStore};

/// Largest edit distance accepted by [`Index::suggest_fuzzy`]. Small on purpose:
/// 1–2 covers realistic typos, and the candidate scan cost grows with tolerance.
pub const MAX_EDITS: u32 = 2;

/// A read-only autocomplete index over some byte backing store `B` (an owned
/// `Vec<u8>` in tests, a `memmap2::Mmap` in production).
pub struct Index<B: AsRef<[u8]>> {
    map: Map<B>,
    store: RecordStore<B>,
}

impl Index<Vec<u8>> {
    /// Build an in-memory index directly from serialized bytes (used by tests and
    /// callers that already hold the buffers).
    pub fn from_bytes(fst_bytes: Vec<u8>, records_bytes: Vec<u8>) -> io::Result<Self> {
        let map = Map::new(fst_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let store = RecordStore::new(records_bytes)?;
        Ok(Self { map, store })
    }
}

impl Index<Mmap> {
    /// Open an index by memory-mapping both artifacts read-only. All processes that
    /// mmap the same files share one physical copy via the OS page cache (README §4).
    ///
    /// # Safety
    /// mmap assumes the files are not mutated underneath the process. Deploy them
    /// read-only (the intended pattern), and this holds.
    pub fn open(fst_path: impl AsRef<Path>, records_path: impl AsRef<Path>) -> io::Result<Self> {
        let fst_file = File::open(fst_path)?;
        let records_file = File::open(records_path)?;
        // SAFETY: artifacts are deployed read-only; see method doc.
        let fst_mmap = unsafe { Mmap::map(&fst_file)? };
        let records_mmap = unsafe { Mmap::map(&records_file)? };
        let map = Map::new(fst_mmap)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let store = RecordStore::new(records_mmap)?;
        Ok(Self { map, store })
    }
}

impl<B: AsRef<[u8]>> Index<B> {
    /// Number of records held.
    pub fn len(&self) -> u32 {
        self.store.n_records()
    }

    /// Whether the index holds no records.
    pub fn is_empty(&self) -> bool {
        self.store.n_records() == 0
    }

    /// Exact-prefix autocomplete: records whose normalized key starts with `prefix`,
    /// deduped by `group`, ranked by `rank` descending, truncated to `limit`.
    ///
    /// Returns neutral [`Record`]s; decode `payload` with your domain adapter (e.g.
    /// [`crate::geo::GeoRecord::from_record`]).
    pub fn suggest(&self, prefix: &str, limit: usize) -> io::Result<Vec<Record>> {
        self.suggest_fuzzy(prefix, limit, 0)
    }

    /// Fuzzy autocomplete tolerating up to `max_edits` typos (issue #8, README §4).
    ///
    /// `max_edits == 0` is exact prefix and byte-identical to [`Index::suggest`].
    /// Otherwise a key matches if some prefix of it is within `max_edits`
    /// **character** edits (Levenshtein) of the normalized query — so `солнечо`
    /// (1 edit) recovers `Солнечногорск`. To stay bounded, candidates are the keys
    /// that share the query's first character (typos in the first character are not
    /// recovered — a standard autocomplete assumption). `max_edits` must be
    /// `<= MAX_EDITS`.
    ///
    /// Fuzzy changes result semantics, so keep it opt-in (gate it behind a query
    /// param before enabling by default — README §10).
    pub fn suggest_fuzzy(
        &self,
        prefix: &str,
        limit: usize,
        max_edits: u32,
    ) -> io::Result<Vec<Record>> {
        let norm = normalize(prefix);
        if norm.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if max_edits > MAX_EDITS {
            return Err(err_other(format!(
                "max_edits {max_edits} exceeds MAX_EDITS {MAX_EDITS}"
            )));
        }

        let record_ids = if max_edits == 0 {
            // Exact prefix — same path (and results) as before fuzzy existed.
            self.gather_exact(&norm)?
        } else {
            self.gather_fuzzy(&norm, max_edits)?
        };
        self.finalize(record_ids, limit)
    }

    /// Collect record ids for keys with the exact `prefix` (a range scan).
    fn gather_exact(&self, prefix: &str) -> io::Result<Vec<u32>> {
        let mut stream = self
            .map
            .search(Str::new(prefix).starts_with())
            .into_stream();
        let mut record_ids: Vec<u32> = Vec::new();
        let mut seen_rid: HashSet<u32> = HashSet::new();
        while let Some((_key, value)) = stream.next() {
            for rid in self.store.posting(value as u32)? {
                if seen_rid.insert(rid) {
                    record_ids.push(rid);
                }
            }
        }
        Ok(record_ids)
    }

    /// Collect record ids for keys some prefix of which is within `max_edits`
    /// character edits of `query`. Candidates are keys sharing the query's first
    /// character; each is filtered by [`prefix_edit_distance_le`].
    fn gather_fuzzy(&self, query: &str, max_edits: u32) -> io::Result<Vec<u32>> {
        let qchars: Vec<char> = query.chars().collect();
        // Anchor the scan on the first character to bound the candidate set.
        let mut first = String::new();
        first.push(qchars[0]);
        let mut stream = self
            .map
            .search(Str::new(&first).starts_with())
            .into_stream();

        let mut record_ids: Vec<u32> = Vec::new();
        let mut seen_rid: HashSet<u32> = HashSet::new();
        while let Some((key_bytes, value)) = stream.next() {
            let key = match std::str::from_utf8(key_bytes) {
                Ok(k) => k,
                Err(_) => continue,
            };
            if prefix_edit_distance_le(&qchars, key, max_edits) {
                for rid in self.store.posting(value as u32)? {
                    if seen_rid.insert(rid) {
                        record_ids.push(rid);
                    }
                }
            }
        }
        Ok(record_ids)
    }

    /// Hydrate record ids into ranked, deduped records (shared by both paths).
    fn finalize(&self, record_ids: Vec<u32>, limit: usize) -> io::Result<Vec<Record>> {
        // Hydrate + dedup by group, keeping the BEST representative of each group
        // (issue #5): highest rank, then payload asc — i.e. the member that would
        // sort first. For geo a group (gid) has exactly one record, so this is a
        // no-op; it matters only for domains that put several records under one
        // group.
        let mut best: HashMap<u64, Record> = HashMap::with_capacity(record_ids.len());
        for rid in record_ids {
            let r = self.store.record(rid)?;
            match best.entry(r.group) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if outranks(&r, e.get()) {
                        e.insert(r);
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(r);
                }
            }
        }

        // Rank: rank desc, then payload asc, then group asc — a total, deterministic
        // order independent of the HashMap's iteration order.
        let mut out: Vec<Record> = best.into_values().collect();
        out.sort_by(|a, b| {
            b.rank
                .cmp(&a.rank)
                .then_with(|| a.payload.cmp(&b.payload))
                .then_with(|| a.group.cmp(&b.group))
        });
        out.truncate(limit);
        Ok(out)
    }
}

fn err_other(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Whether some prefix of `key` is within `max` character edits (Levenshtein) of
/// the full `query`. Operates on Unicode scalar values (so a Cyrillic typo counts
/// as one edit, unlike fst's byte-oriented Levenshtein).
///
/// DP over the standard edit-distance matrix `M[k][j] = dist(key[..k], query[..j])`,
/// extending `key` one char at a time and tracking `min_k M[k][n]` (n = query len).
/// Extra `key` chars beyond a match are free (that is the "prefix" part). Rows whose
/// every entry exceeds `max` allow an early break, since they can only grow.
fn prefix_edit_distance_le(query: &[char], key: &str, max: u32) -> bool {
    let n = query.len();
    // row[j] = edit distance between the current key prefix and query[..j].
    let mut row: Vec<u32> = (0..=n as u32).collect();
    if row[n] <= max {
        return true; // empty key prefix already within budget (query shorter than max)
    }
    for c in key.chars() {
        let mut prev_diag = row[0]; // M[k][0]
        row[0] += 1; // M[k+1][0]
        let mut row_min = row[0];
        for j in 1..=n {
            let m_k_j = row[j]; // M[k][j] before overwrite
            let cost = if query[j - 1] == c { 0 } else { 1 };
            row[j] = (prev_diag + cost) // substitute / match
                .min(row[j] + 1) // delete key char
                .min(row[j - 1] + 1); // insert query char
            prev_diag = m_k_j;
            row_min = row_min.min(row[j]);
        }
        if row[n] <= max {
            return true;
        }
        if row_min > max {
            break; // no longer key prefix can bring any entry back under budget
        }
    }
    false
}

/// Whether `a` is a better group representative than `b`: higher rank wins, ties
/// broken by payload ascending (the same order the final sort uses).
fn outranks(a: &Record, b: &Record) -> bool {
    (a.rank, std::cmp::Reverse(&a.payload)) > (b.rank, std::cmp::Reverse(&b.payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IndexBuilder;

    fn rec(group: u64, rank: i64, payload: &str) -> Record {
        Record {
            group,
            rank,
            payload: payload.as_bytes().to_vec(),
        }
    }

    fn payload(r: &Record) -> &str {
        std::str::from_utf8(&r.payload).unwrap()
    }

    /// Build an index with a few overlapping-prefix items and shared aliases.
    /// Payload is just the display string; group is a stable item id; rank a weight.
    fn sample() -> Index<Vec<u8>> {
        let mut b = IndexBuilder::new();
        let berlin = b.add_record(rec(1, 3_600_000, "Berlin"));
        let bern = b.add_record(rec(2, 130_000, "Bern"));
        let bergen = b.add_record(rec(3, 280_000, "Bergen"));
        let paris = b.add_record(rec(4, 2_100_000, "Paris"));

        // Each item indexed under its own normalized name...
        b.add_key("berlin", berlin);
        b.add_key("bern", bern);
        b.add_key("bergen", bergen);
        b.add_key("paris", paris);
        // ...and an alias that several items share.
        b.add_key("ber", berlin);
        b.add_key("ber", bern);
        b.add_key("ber", bergen);

        let (fst, records) = b.build().unwrap();
        Index::from_bytes(fst, records).unwrap()
    }

    #[test]
    fn prefix_matches_and_ranks_by_rank() {
        let idx = sample();
        let out = idx.suggest("ber", 10).unwrap();
        let names: Vec<_> = out.iter().map(payload).collect();
        // Berlin (3.6M) > Bergen (280k) > Bern (130k).
        assert_eq!(names, vec!["Berlin", "Bergen", "Bern"]);
    }

    #[test]
    fn respects_limit() {
        let idx = sample();
        let out = idx.suggest("ber", 2).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(payload(&out[0]), "Berlin");
        assert_eq!(payload(&out[1]), "Bergen");
    }

    #[test]
    fn exact_deeper_prefix() {
        let idx = sample();
        let out = idx.suggest("berl", 10).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(payload(&out[0]), "Berlin");
    }

    #[test]
    fn dedups_by_group_across_alias_and_name() {
        let idx = sample();
        // "Berlin" is reachable via both "ber" alias and "berlin" key; must appear once.
        let out = idx.suggest("ber", 10).unwrap();
        let berlins = out.iter().filter(|r| r.group == 1).count();
        assert_eq!(berlins, 1);
    }

    #[test]
    fn normalizes_query() {
        let idx = sample();
        // Uppercase + trailing separators should fold to "ber".
        assert_eq!(idx.suggest("  BER-- ", 10).unwrap().len(), 3);
    }

    #[test]
    fn empty_and_miss() {
        let idx = sample();
        assert!(idx.suggest("", 10).unwrap().is_empty());
        assert!(idx.suggest("zzz", 10).unwrap().is_empty());
        assert!(idx.suggest("ber", 0).unwrap().is_empty());
    }

    #[test]
    fn len_and_is_empty() {
        assert_eq!(sample().len(), 4);
        assert!(!sample().is_empty());

        let (fst, records) = IndexBuilder::new().build().unwrap();
        let empty = Index::from_bytes(fst, records).unwrap();
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
        assert!(empty.suggest("anything", 10).unwrap().is_empty());
    }

    #[test]
    fn equal_rank_tiebreaks_on_payload_asc() {
        let mut b = IndexBuilder::new();
        // Same rank; deterministic order must be by payload bytes ascending.
        let beta = b.add_record(rec(1, 100, "Beta"));
        let alpha = b.add_record(rec(2, 100, "Alpha"));
        b.add_key("x", beta);
        b.add_key("x", alpha);
        let (fst, records) = b.build().unwrap();
        let idx = Index::from_bytes(fst, records).unwrap();

        let out = idx.suggest("x", 10).unwrap();
        assert_eq!(payload(&out[0]), "Alpha");
        assert_eq!(payload(&out[1]), "Beta");
    }

    #[test]
    fn group_dedup_keeps_highest_rank_member() {
        // Issue #5: two records share group 42 under the same key but have
        // different ranks; the higher-rank one must be the survivor regardless of
        // insertion/scan order.
        let mut b = IndexBuilder::new();
        let low = b.add_record(rec(42, 10, "low-rank"));
        let high = b.add_record(rec(42, 999, "high-rank"));
        // Insert low first so scan order would otherwise keep it.
        b.add_key("k", low);
        b.add_key("k", high);
        let (fst, records) = b.build().unwrap();
        let idx = Index::from_bytes(fst, records).unwrap();

        let out = idx.suggest("k", 10).unwrap();
        assert_eq!(out.len(), 1, "group 42 must collapse to one row");
        assert_eq!(out[0].rank, 999);
        assert_eq!(payload(&out[0]), "high-rank");
    }

    #[test]
    fn negative_ranks_order_descending() {
        let mut b = IndexBuilder::new();
        let low = b.add_record(rec(1, -100, "low"));
        let high = b.add_record(rec(2, -1, "high"));
        b.add_key("k", low);
        b.add_key("k", high);
        let (fst, records) = b.build().unwrap();
        let idx = Index::from_bytes(fst, records).unwrap();

        let out = idx.suggest("k", 10).unwrap();
        // -1 > -100, so "high" comes first.
        assert_eq!(payload(&out[0]), "high");
        assert_eq!(payload(&out[1]), "low");
    }

    #[test]
    fn limit_larger_than_results_returns_all() {
        let idx = sample();
        let out = idx.suggest("ber", 999).unwrap();
        assert_eq!(out.len(), 3);
    }

    /// Index a long key to exercise fuzzy-prefix recovery.
    fn fuzzy_sample() -> Index<Vec<u8>> {
        let mut b = IndexBuilder::new();
        let sol = b.add_record(rec(1, 52_798, "Solnechnogorsk"));
        let ber = b.add_record(rec(2, 3_600_000, "Berlin"));
        b.add_key("solnechnogorsk", sol);
        b.add_key("berlin", ber);
        let (fst, records) = b.build().unwrap();
        Index::from_bytes(fst, records).unwrap()
    }

    #[test]
    fn fuzzy_recovers_near_miss() {
        let idx = fuzzy_sample();
        // "solnecho" is NOT an exact prefix of "solnechnogorsk"...
        assert!(idx.suggest("solnecho", 8).unwrap().is_empty());
        // ...but within 1 edit of the prefix "solnechn", so fuzzy recovers it.
        let out = idx.suggest_fuzzy("solnecho", 8, 1).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(payload(&out[0]), "Solnechnogorsk");

        // Two typos need distance 2.
        assert!(idx.suggest_fuzzy("salnecha", 8, 1).unwrap().is_empty());
        let out2 = idx.suggest_fuzzy("salnecha", 8, 2).unwrap();
        assert_eq!(out2.len(), 1);
        assert_eq!(payload(&out2[0]), "Solnechnogorsk");
    }

    #[test]
    fn fuzzy_off_is_byte_identical_to_exact() {
        let idx = fuzzy_sample();
        for q in ["sol", "solnechnogorsk", "ber", "berlin", "xyz", ""] {
            assert_eq!(
                idx.suggest(q, 8).unwrap(),
                idx.suggest_fuzzy(q, 8, 0).unwrap(),
                "fuzzy(0) diverges from exact for {q:?}"
            );
        }
    }

    #[test]
    fn fuzzy_still_prefix_not_whole_word() {
        // Distance-1 fuzzy of a short prefix must not accidentally pull unrelated
        // records: "berlin" is far from "solnecho".
        let idx = fuzzy_sample();
        let out = idx.suggest_fuzzy("solnecho", 8, 1).unwrap();
        assert!(out.iter().all(|r| payload(r) != "Berlin"));
    }

    #[test]
    fn fuzzy_rejects_too_many_edits() {
        let idx = fuzzy_sample();
        let e = idx.suggest_fuzzy("sol", 8, MAX_EDITS + 1).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn prefix_edit_distance_le_unit() {
        let q: Vec<char> = "солнечо".chars().collect();
        // Prefix "солнечн" of the key is 1 substitution away; extra chars are free.
        assert!(prefix_edit_distance_le(&q, "солнечногорск", 1));
        assert!(!prefix_edit_distance_le(&q, "солнечногорск", 0));

        let q2: Vec<char> = "kat".chars().collect();
        assert!(prefix_edit_distance_le(&q2, "katze", 0)); // exact prefix
        assert!(prefix_edit_distance_le(&q2, "cat", 1)); // 1 substitution
        assert!(!prefix_edit_distance_le(&q2, "dog", 2)); // too far
                                                          // Deletion: query longer than a short key still matches within budget.
        let q3: Vec<char> = "berlim".chars().collect();
        assert!(prefix_edit_distance_le(&q3, "berlin", 1));
    }

    #[test]
    fn unicode_prefix_query() {
        let mut b = IndexBuilder::new();
        let r = b.add_record(rec(1, 1, "Zürich"));
        b.add_key(&crate::normalize::normalize("Zürich"), r);
        let (fst, records) = b.build().unwrap();
        let idx = Index::from_bytes(fst, records).unwrap();

        // Query with accents + case; folds to the same key.
        assert_eq!(idx.suggest("zür", 10).unwrap().len(), 1);
        assert_eq!(idx.suggest("ZURI", 10).unwrap().len(), 1);
    }

    #[test]
    fn open_from_mmap_matches_from_bytes() {
        use std::io::Write;
        let mut b = IndexBuilder::new();
        let r = b.add_record(rec(7, 5, "Berlin"));
        b.add_key("berlin", r);
        let (fst, records) = b.build().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let fst_path = dir.path().join("index.fst");
        let rec_path = dir.path().join("records.bin");
        std::fs::File::create(&fst_path)
            .unwrap()
            .write_all(&fst)
            .unwrap();
        std::fs::File::create(&rec_path)
            .unwrap()
            .write_all(&records)
            .unwrap();

        let idx = Index::<Mmap>::open(&fst_path, &rec_path).unwrap();
        let out = idx.suggest("berl", 10).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].group, 7);
        assert_eq!(payload(&out[0]), "Berlin");
    }

    #[test]
    fn open_missing_file_errors() {
        assert!(Index::<Mmap>::open("/nonexistent/index.fst", "/nonexistent/records.bin").is_err());
    }
}
