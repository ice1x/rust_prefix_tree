//! `geo_trie_rs` — a memory-mapped, `fst`-backed geo autocomplete index.
//!
//! This is the Rust backend proposed in the repo README: it replaces the
//! pure-Python dict trie behind the `reviews` service's `/geo/suggest` endpoint
//! with a compact, memory-mapped [`fst::Map`] index plus a parallel records blob.
//!
//! Two artifacts are produced offline and mmapped at run time:
//! * `index.fst`   — normalized key → postings-index (see [`builder`], [`index`]),
//! * `records.bin` — postings + per-place metadata (see [`records`]).
//!
//! The core (build + query + normalization) is pure Rust and fully unit-tested.
//! The optional `python` feature adds a PyO3 extension module of the same name.

pub mod builder;
pub mod index;
pub mod normalize;
pub mod records;

pub use builder::IndexBuilder;
pub use index::Index;
pub use normalize::normalize;
pub use records::{Record, RecordStore};

#[cfg(feature = "python")]
mod python;
