//! Build side: turn a set of records plus `(normalized_key, record_id)` pairs into
//! the two artifacts the query path memory-maps — `index.fst` and `records.bin`
//! (README §6).
//!
//! `fst::MapBuilder` requires keys inserted in sorted, unique order, so the builder
//! groups the pairs by key: each distinct key becomes one **postings** entry (the
//! sorted-unique record ids under it) and the FST maps `key -> postings_index`.

use std::collections::BTreeMap;
use std::io;

use fst::MapBuilder;

use crate::records::{Record, HEADER_LEN, MAGIC};

// Record payload serialization lives here; string packing helpers are shared from
// `records` so domain adapters and the builder agree on the encoding.

/// Accumulates records and key→record links, then emits `(fst, records)` bytes.
#[derive(Default)]
pub struct IndexBuilder {
    records: Vec<Record>,
    /// key -> set of record ids (BTreeMap keeps keys sorted for the FST).
    keys: BTreeMap<String, Vec<u32>>,
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a record; returns its record id (a stable index).
    pub fn add_record(&mut self, record: Record) -> u32 {
        let id = self.records.len() as u32;
        self.records.push(record);
        id
    }

    /// Link a (already-normalized) key to a record id. May be called many times for
    /// the same key (aliases → same place) or the same record (place → many keys).
    pub fn add_key(&mut self, key: &str, record_id: u32) {
        self.keys.entry(key.to_owned()).or_default().push(record_id);
    }

    /// Serialize the FST and the records blob. Returns `(fst_bytes, records_bytes)`.
    pub fn build(self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        // 1. Postings, in sorted key order (== FST insertion order).
        let mut postings: Vec<Vec<u32>> = Vec::with_capacity(self.keys.len());
        let mut fst_buf =
            MapBuilder::new(Vec::new()).map_err(|e| io::Error::other(e.to_string()))?;
        for (idx, (key, mut ids)) in self.keys.into_iter().enumerate() {
            ids.sort_unstable();
            ids.dedup();
            fst_buf
                .insert(key.as_bytes(), idx as u64)
                .map_err(|e| io::Error::other(e.to_string()))?;
            postings.push(ids);
        }
        let fst_bytes = fst_buf
            .into_inner()
            .map_err(|e| io::Error::other(e.to_string()))?;

        // 2. Records blob.
        let records_bytes = serialize_records(&postings, &self.records);
        Ok((fst_bytes, records_bytes))
    }
}

/// Serialize the postings + records into the `records.bin` layout (see `records.rs`).
fn serialize_records(postings: &[Vec<u32>], records: &[Record]) -> Vec<u8> {
    // Postings payload + relative offset table (n+1 entries, last = payload len).
    let mut post_pay = Vec::new();
    let mut post_idx = Vec::with_capacity(postings.len() + 1);
    for p in postings {
        post_idx.push(post_pay.len() as u64);
        post_pay.extend_from_slice(&(p.len() as u32).to_le_bytes());
        for id in p {
            post_pay.extend_from_slice(&id.to_le_bytes());
        }
    }
    post_idx.push(post_pay.len() as u64);

    // Records payload + relative offset table.
    let mut rec_pay = Vec::new();
    let mut rec_idx = Vec::with_capacity(records.len() + 1);
    for r in records {
        rec_idx.push(rec_pay.len() as u64);
        rec_pay.extend_from_slice(&r.group.to_le_bytes());
        rec_pay.extend_from_slice(&(r.rank as u64).to_le_bytes());
        rec_pay.extend_from_slice(&(r.payload.len() as u32).to_le_bytes());
        rec_pay.extend_from_slice(&r.payload);
    }
    rec_idx.push(rec_pay.len() as u64);

    // Absolute offsets.
    let post_idx_off = HEADER_LEN as u64;
    let rec_idx_off = post_idx_off + (post_idx.len() * 8) as u64;
    let post_pay_off = rec_idx_off + (rec_idx.len() * 8) as u64;
    let rec_pay_off = post_pay_off + post_pay.len() as u64;
    let total_len = rec_pay_off + rec_pay.len() as u64;

    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(postings.len() as u32).to_le_bytes());
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    out.extend_from_slice(&post_idx_off.to_le_bytes());
    out.extend_from_slice(&rec_idx_off.to_le_bytes());
    out.extend_from_slice(&post_pay_off.to_le_bytes());
    out.extend_from_slice(&rec_pay_off.to_le_bytes());
    out.extend_from_slice(&total_len.to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_LEN);
    for o in &post_idx {
        out.extend_from_slice(&o.to_le_bytes());
    }
    for o in &rec_idx {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&post_pay);
    out.extend_from_slice(&rec_pay);
    debug_assert_eq!(out.len() as u64, total_len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(group: u64, rank: i64, payload: &str) -> Record {
        Record {
            group,
            rank,
            payload: payload.as_bytes().to_vec(),
        }
    }

    #[test]
    fn rejects_nothing_and_builds_empty() {
        let (fst, records) = IndexBuilder::new().build().unwrap();
        assert!(!fst.is_empty(), "even an empty fst has a header");
        // Empty store must still parse.
        let store = crate::records::RecordStore::new(records).unwrap();
        assert_eq!(store.n_records(), 0);
        assert_eq!(store.n_postings(), 0);
    }

    #[test]
    fn one_posting_per_distinct_key_in_sorted_order() {
        let mut b = IndexBuilder::new();
        let r0 = b.add_record(rec(1, 1, "zero"));
        let r1 = b.add_record(rec(2, 1, "one"));
        // Insert keys out of lexicographic order; builder must sort them.
        b.add_key("b", r1);
        b.add_key("b", r0);
        b.add_key("a", r0);
        let (fst, records) = b.build().unwrap();

        let store = crate::records::RecordStore::new(records).unwrap();
        assert_eq!(store.n_postings(), 2, "two distinct keys -> two postings");

        // The FST maps each key to its postings index in sorted key order:
        // "a" -> 0, "b" -> 1.
        let map = fst::Map::new(fst).unwrap();
        assert_eq!(map.get("a"), Some(0));
        assert_eq!(map.get("b"), Some(1));
        assert_eq!(map.get("missing"), None);

        assert_eq!(store.posting(0).unwrap(), vec![r0]); // key "a"
        assert_eq!(store.posting(1).unwrap(), vec![r0, r1]); // key "b", sorted-unique
    }

    #[test]
    fn unicode_keys_supported() {
        let mut b = IndexBuilder::new();
        let r = b.add_record(rec(1, 1, "place"));
        b.add_key("солнечногорск", r);
        b.add_key("zürich", r);
        let (fst, _records) = b.build().unwrap();
        let map = fst::Map::new(fst).unwrap();
        assert!(map.get("солнечногорск").is_some());
        assert!(map.get("zürich").is_some());
    }
}
