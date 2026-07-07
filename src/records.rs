//! `records.bin` — the parallel, memory-mappable metadata blob (README §4).
//!
//! It holds two sections, both indexed by a flat offset table so any entry can be
//! read in O(1) without parsing what comes before it:
//!
//! * **postings** — for each distinct FST value `j`, the list of record ids that
//!   share that normalized key. The FST maps `key -> j`; a prefix scan yields a set
//!   of `j`s, which expand to record ids here.
//! * **records** — the per-place metadata (`gid, lat, lon, population, country,
//!   feature_code, name`), indexed by record id.
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
//! A record is `u64 gid | f64 lat | f64 lon | i64 population` then three
//! length-prefixed (`u16 len` + bytes) UTF-8 strings: `country`, `feature_code`,
//! `name`.

use std::io;

pub const MAGIC: &[u8; 8] = b"GEOIDX01";
pub const HEADER_LEN: usize = 56;

/// A single place record, as returned by a query.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub gid: u64,
    pub lat: f64,
    pub lon: f64,
    pub population: i64,
    pub country: String,
    pub feature_code: String,
    pub name: String,
}

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn read_u16(buf: &[u8], at: usize) -> io::Result<u16> {
    let b = buf
        .get(at..at + 2)
        .ok_or_else(|| err("u16 out of bounds"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(buf: &[u8], at: usize) -> io::Result<u32> {
    let b = buf
        .get(at..at + 4)
        .ok_or_else(|| err("u32 out of bounds"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64(buf: &[u8], at: usize) -> io::Result<u64> {
    let b = buf
        .get(at..at + 8)
        .ok_or_else(|| err("u64 out of bounds"))?;
    Ok(u64::from_le_bytes(b.try_into().unwrap()))
}

fn read_i64(buf: &[u8], at: usize) -> io::Result<i64> {
    Ok(read_u64(buf, at)? as i64)
}

fn read_f64(buf: &[u8], at: usize) -> io::Result<f64> {
    Ok(f64::from_bits(read_u64(buf, at)?))
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

        let gid = read_u64(buf, at)?;
        at += 8;
        let lat = read_f64(buf, at)?;
        at += 8;
        let lon = read_f64(buf, at)?;
        at += 8;
        let population = read_i64(buf, at)?;
        at += 8;

        let (country, next) = read_str(buf, at)?;
        at = next;
        let (feature_code, next) = read_str(buf, at)?;
        at = next;
        let (name, _next) = read_str(buf, at)?;

        Ok(Record {
            gid,
            lat,
            lon,
            population,
            country,
            feature_code,
            name,
        })
    }
}

fn read_str(buf: &[u8], at: usize) -> io::Result<(String, usize)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::IndexBuilder;

    fn rec(gid: u64, name: &str, pop: i64) -> Record {
        Record {
            gid,
            lat: 1.5,
            lon: -2.5,
            population: pop,
            country: "DE".into(),
            feature_code: "PPL".into(),
            name: name.into(),
        }
    }

    // Build a tiny records.bin through the real builder and read it back.
    fn roundtrip() -> RecordStore<Vec<u8>> {
        let mut b = IndexBuilder::new();
        let a = b.add_record(rec(101, "Berlin", 3_600_000));
        let c = b.add_record(rec(202, "Bern", 130_000));
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
        assert_eq!(store.record(0).unwrap(), rec(101, "Berlin", 3_600_000));
        assert_eq!(store.record(1).unwrap(), rec(202, "Bern", 130_000));
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
}
