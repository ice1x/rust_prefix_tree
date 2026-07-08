//! `geo_trie_rs` — a memory-mapped, `fst`-backed prefix-autocomplete index.
//!
//! Despite the name, the engine is **domain-agnostic**: it maps normalized string
//! keys to neutral [`records::Record`]s (`group` dedup key, `rank` sort weight,
//! opaque `payload` bytes). Geo is one adapter over it ([`geo`]); a people/ФИО or
//! product index is just a different adapter — the core is unchanged.
//!
//! It is the Rust backend proposed in the repo README: it replaces the pure-Python
//! dict trie behind the `reviews` service's `/geo/suggest` endpoint with a compact,
//! memory-mapped [`fst::Map`] index plus a parallel records blob.
//!
//! Two artifacts are produced offline and mmapped at run time:
//! * `index.fst`   — normalized key → postings-index (see [`builder`], [`index`]),
//! * `records.bin` — postings + neutral per-item records (see [`records`]).
//!
//! The core (build + query + normalization) is pure Rust and fully unit-tested.
//! The optional `python` feature adds a PyO3 extension module of the same name.

pub mod builder;
pub mod geo;
pub mod index;
pub mod normalize;
pub mod records;

pub use builder::IndexBuilder;
pub use geo::GeoRecord;
pub use index::Index;
pub use normalize::normalize;
pub use records::{Record, RecordStore};

#[cfg(feature = "python")]
mod python;
