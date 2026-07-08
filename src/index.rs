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

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::Path;

use fst::automaton::{Automaton, Str};
use fst::{IntoStreamer, Map, Streamer};
use memmap2::Mmap;

use crate::normalize::normalize;
use crate::records::{Record, RecordStore};

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

    /// Autocomplete: records whose normalized key starts with `prefix`, deduped by
    /// `group`, ranked by `rank` descending, truncated to `limit`.
    ///
    /// Returns neutral [`Record`]s; decode `payload` with your domain adapter (e.g.
    /// [`crate::geo::GeoRecord::from_record`]).
    pub fn suggest(&self, prefix: &str, limit: usize) -> io::Result<Vec<Record>> {
        let norm = normalize(prefix);
        if norm.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        // Prefix range scan over the FST.
        let automaton = Str::new(&norm).starts_with();
        let mut stream = self.map.search(automaton).into_stream();

        let mut record_ids: Vec<u32> = Vec::new();
        let mut seen_rid: HashSet<u32> = HashSet::new();
        while let Some((_key, value)) = stream.next() {
            for rid in self.store.posting(value as u32)? {
                if seen_rid.insert(rid) {
                    record_ids.push(rid);
                }
            }
        }

        // Hydrate + dedup by group.
        let mut seen_group: HashSet<u64> = HashSet::new();
        let mut out: Vec<Record> = Vec::with_capacity(record_ids.len());
        for rid in record_ids {
            let r = self.store.record(rid)?;
            if seen_group.insert(r.group) {
                out.push(r);
            }
        }

        // Rank: rank desc, payload asc as a deterministic tiebreak.
        out.sort_by(|a, b| b.rank.cmp(&a.rank).then_with(|| a.payload.cmp(&b.payload)));
        out.truncate(limit);
        Ok(out)
    }
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
}
