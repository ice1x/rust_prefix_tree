//! `records.bin` — the parallel, memory-mappable payload blob (README §4).
//!
//! The store is **domain-agnostic**: it knows nothing about geo. It holds two
//! sections, both indexed by a flat offset table so any entry can be read in O(1)
//! without parsing what comes before it:
//!
//! * **postings** — for each distinct FST value `j`, the list of record ids that
//!   share that normalized key. The FST maps `key -> j`; a prefix scan yields a set
//!   of `j`s, which expand to record ids here.
//! * **records** — per-item data indexed by record id, each a neutral triple:
//!   `group` (dedup key), `rank` (sort weight, higher first) and an opaque
//!   `payload` the caller's domain adapter encodes/decodes (see [`crate::geo`]).
//!
//! Layout (all little-endian), where every `*_off` is absolute from the start of
//! the buffer:
//!
//! ```text
//!   0  magic        [8]  = b"GEOIDX01"
//!   8  n_postings   u32
//!  12  n_records    u32
//!  16  post_idx_off u64   -> (n_postings+1) u64 offsets into postings payload
//!  24  rec_idx_off  u64   -> (n_records+1)  u64 offsets into records payload
//!  32  post_pay_off u64
//!  40  rec_pay_off  u64
//!  48  total_len    u64
//!  ... postings index, records index, postings payload, records payload
//! ```
//!
//! A postings entry is `u32 count` followed by `count` × `u32 record_id`.
//! A record is `u64 group | i64 rank | u32 payload_len | payload_bytes`.

use std::io;

pub const MAGIC: &[u8; 8] = b"GEOIDX01";
pub const HEADER_LEN: usize = 56;

/// A single stored record: a dedup `group`, a sort `rank`, and an opaque `payload`.
///
/// The core never interprets `payload`; a domain adapter (e.g. [`crate::geo`])
/// decides its bytes. `group` dedups results (same group → kept once); `rank` orders
/// them (descending). For geo these are `gid` and `population`; for a people index
/// they might be a person id and a relevance score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub group: u64,
    pub rank: i64,
    pub payload: Vec<u8>,
}

pub(crate) fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub(crate) fn read_u16(buf: &[u8], at: usize) -> io::Result<u16> {
    let b = buf
        .get(at..at + 2)
        .ok_or_else(|| err("u16 out of bounds"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn read_u32(buf: &[u8], at: usize) -> io::Result<u32> {
    let b = buf
        .get(at..at + 4)
        .ok_or_else(|| err("u32 out of bounds"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub(crate) fn read_u64(buf: &[u8], at: usize) -> io::Result<u64> {
    let b = buf
        .get(at..at + 8)
        .ok_or_else(|| err("u64 out of bounds"))?;
    Ok(u64::from_le_bytes(b.try_into().unwrap()))
}

pub(crate) fn read_i64(buf: &[u8], at: usize) -> io::Result<i64> {
    Ok(read_u64(buf, at)? as i64)
}

/// Read a `u16`-length-prefixed UTF-8 string at `at`; returns `(string, next_off)`.
/// Shared by domain adapters that pack strings into a payload.
pub(crate) fn read_str(buf: &[u8], at: usize) -> io::Result<(String, usize)> {
    let len = read_u16(buf, at)? as usize;
    let start = at + 2;
    let bytes = buf
        .get(start..start + len)
        .ok_or_else(|| err("string out of bounds"))?;
    let s = std::str::from_utf8(bytes)
        .map_err(|_| err("string not utf-8"))?
        .to_owned();
    Ok((s, start + len))
}

/// Append a `u16`-length-prefixed UTF-8 string. Shared by the builder and adapters.
pub(crate) fn push_str(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = u16::try_from(bytes.len()).expect("string field exceeds 65535 bytes");
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Read-only view over a `records.bin` buffer. Generic over the backing store so it
/// works over an owned `Vec<u8>` (tests) or a `memmap2::Mmap` (production).
pub struct RecordStore<B: AsRef<[u8]>> {
    buf: B,
    n_postings: u32,
    n_records: u32,
    post_idx_off: u64,
    rec_idx_off: u64,
    post_pay_off: u64,
    rec_pay_off: u64,
}

impl<B: AsRef<[u8]>> RecordStore<B> {
    /// Parse and validate the header of a `records.bin` buffer.
    pub fn new(buf: B) -> io::Result<Self> {
        let bytes = buf.as_ref();
        if bytes.len() < HEADER_LEN {
            return Err(err("records.bin shorter than header"));
        }
        if &bytes[0..8] != MAGIC {
            return Err(err("records.bin bad magic"));
        }
        let n_postings = read_u32(bytes, 8)?;
        let n_records = read_u32(bytes, 12)?;
        let post_idx_off = read_u64(bytes, 16)?;
        let rec_idx_off = read_u64(bytes, 24)?;
        let post_pay_off = read_u64(bytes, 32)?;
        let rec_pay_off = read_u64(bytes, 40)?;
        let total_len = read_u64(bytes, 48)?;
        if total_len as usize != bytes.len() {
            return Err(err("records.bin length mismatch"));
        }
        Ok(Self {
            buf,
            n_postings,
            n_records,
            post_idx_off,
            rec_idx_off,
            post_pay_off,
            rec_pay_off,
        })
    }

    #[inline]
    pub fn n_records(&self) -> u32 {
        self.n_records
    }

    #[inline]
    pub fn n_postings(&self) -> u32 {
        self.n_postings
    }

    /// Record ids for postings entry `index` (an FST value).
    pub fn posting(&self, index: u32) -> io::Result<Vec<u32>> {
        if index >= self.n_postings {
            return Err(err("posting index out of range"));
        }
        let buf = self.buf.as_ref();
        let slot = self.post_idx_off as usize + index as usize * 8;
        let start = self.post_pay_off + read_u64(buf, slot)?;
        let count = read_u32(buf, start as usize)?;
        let mut ids = Vec::with_capacity(count as usize);
        let mut at = start as usize + 4;
        for _ in 0..count {
            ids.push(read_u32(buf, at)?);
            at += 4;
        }
        Ok(ids)
    }

    /// The record with the given id.
    pub fn record(&self, id: u32) -> io::Result<Record> {
        if id >= self.n_records {
            return Err(err("record id out of range"));
        }
        let buf = self.buf.as_ref();
        let slot = self.rec_idx_off as usize + id as usize * 8;
        let mut at = (self.rec_pay_off + read_u64(buf, slot)?) as usize;

        let group = read_u64(buf, at)?;
        at += 8;
        let rank = read_i64(buf, at)?;
        at += 8;
        let len = read_u32(buf, at)? as usize;
        at += 4;
        let payload = buf
            .get(at..at + len)
            .ok_or_else(|| err("payload out of bounds"))?
            .to_vec();

        Ok(Record {
            group,
            rank,
            payload,
        })
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

    // Build a tiny records.bin through the real builder and read it back.
    fn roundtrip() -> RecordStore<Vec<u8>> {
        let mut b = IndexBuilder::new();
        let a = b.add_record(rec(101, 3_600_000, "Berlin"));
        let c = b.add_record(rec(202, 130_000, "Bern"));
        b.add_key("berlin", a);
        b.add_key("bern", c);
        // shared alias key -> both records
        b.add_key("ber", a);
        b.add_key("ber", c);
        let (_fst, records) = b.build().unwrap();
        RecordStore::new(records).unwrap()
    }

    #[test]
    fn reads_records_back() {
        let store = roundtrip();
        assert_eq!(store.n_records(), 2);
        assert_eq!(store.record(0).unwrap(), rec(101, 3_600_000, "Berlin"));
        assert_eq!(store.record(1).unwrap(), rec(202, 130_000, "Bern"));
    }

    #[test]
    fn out_of_range_ids_error() {
        let store = roundtrip();
        assert!(store.record(99).is_err());
        assert!(store.posting(99).is_err());
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = vec![0u8; HEADER_LEN];
        bytes[0..8].copy_from_slice(b"NOTAGEO!");
        assert!(RecordStore::new(bytes).is_err());
    }

    #[test]
    fn truncated_buffer_rejected() {
        // Shorter than the header.
        assert!(RecordStore::new(vec![0u8; HEADER_LEN - 1]).is_err());
        // Correct magic but a total_len that disagrees with the actual length.
        let (_fst, mut records) = {
            let mut b = IndexBuilder::new();
            let a = b.add_record(rec(1, 1, "x"));
            b.add_key("x", a);
            b.build().unwrap()
        };
        records.pop(); // corrupt the length
        assert!(RecordStore::new(records).is_err());
    }

    #[test]
    fn posting_returns_sorted_unique_ids() {
        let mut b = IndexBuilder::new();
        let r0 = b.add_record(rec(10, 1, "a"));
        let r1 = b.add_record(rec(11, 1, "b"));
        let r2 = b.add_record(rec(12, 1, "c"));
        // Single key linked to records out of order and with a duplicate.
        b.add_key("k", r2);
        b.add_key("k", r0);
        b.add_key("k", r0); // dup
        b.add_key("k", r1);
        let (_fst, records) = b.build().unwrap();
        let store = RecordStore::new(records).unwrap();
        assert_eq!(store.n_postings(), 1);
        assert_eq!(store.posting(0).unwrap(), vec![r0, r1, r2]);
    }

    #[test]
    fn binary_and_negative_fields_roundtrip() {
        let mut b = IndexBuilder::new();
        let payload = vec![0u8, 255, 10, 13, 0, 42];
        b.add_record(Record {
            group: u64::MAX,
            rank: i64::MIN,
            payload: payload.clone(),
        });
        let (_fst, records) = b.build().unwrap();
        let store = RecordStore::new(records).unwrap();
        let got = store.record(0).unwrap();
        assert_eq!(got.group, u64::MAX);
        assert_eq!(got.rank, i64::MIN);
        assert_eq!(got.payload, payload);
    }

    #[test]
    fn empty_payload_roundtrips() {
        let mut b = IndexBuilder::new();
        b.add_record(rec(1, 0, ""));
        let (_fst, records) = b.build().unwrap();
        let store = RecordStore::new(records).unwrap();
        assert_eq!(store.record(0).unwrap().payload, Vec::<u8>::new());
    }
}
