//! `metatrie` — a prefix-tree (trie) autocomplete index whose nodes carry
//! **metadata records**.
//!
//! It is the Rust member of a small family of hand-written prefix trees
//! ([Python `ice1x/prefix_tree`](https://github.com/ice1x/prefix_tree),
//! [Go `ice1x/go-prefix-trie`](https://github.com/ice1x/go-prefix-trie)): the same
//! insert / prefix-collect / field-query / sort-by-field semantics, ported to Rust
//! and extended with deletion, O(1) count, and optional typo-tolerant fuzzy search.
//!
//! Two layers:
//! * [`Trie<V>`] — the generic, dependency-free prefix tree: `insert` (appends),
//!   `get`, `get_by_prefix`, `has_prefix`, `delete` (prunes), `update`, `count`,
//!   `clear`, and `get_by_prefix_fuzzy`.
//! * The [`Record`] layer — for `Trie<Record>` (a node holds a list of
//!   `Record = map of String -> Value`), adds the dynamic, runtime-keyed queries:
//!   [`Trie::get_by_prefix_and_query`], [`Trie::get_by_word_and_query`],
//!   [`Trie::get_by_prefix_sort_by`] and [`Trie::get_by_prefix_sort_desc_by`].
//!
//! Keys are stored verbatim by default; call [`Trie::with_normalizer`] (e.g. with the
//! feature-gated [`normalize`]) for case/diacritic folding. The trie does no internal
//! locking — it is `Send + Sync` when `V` is; wrap it in a `RwLock`/`Mutex` to share.
//!
//! ```
//! use metatrie::{Trie, Record, Value, Order};
//!
//! let mut trie: Trie<Record> = Trie::new();
//! let mut r = Record::new();
//! r.insert("city".into(), Value::from("Berlin"));
//! r.insert("population".into(), Value::from(3_600_000_i64));
//! trie.insert("berlin", r).unwrap();
//!
//! assert!(trie.has_prefix("ber"));
//! let hits = trie.get_by_prefix_sort_by("ber", "population", Order::Desc);
//! assert_eq!(hits.len(), 1);
//! ```

mod fuzzy;
mod record;
mod trie;
mod value;

#[cfg(feature = "normalize")]
mod normalize;

#[cfg(feature = "python")]
mod python;

pub use fuzzy::MAX_EDITS;
pub use trie::{Order, Trie, TrieError};
pub use value::{Record, Value};

#[cfg(feature = "normalize")]
pub use normalize::normalize;
