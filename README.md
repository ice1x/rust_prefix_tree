# metatrie

A prefix-tree (trie) autocomplete index whose nodes carry **metadata records** —
not just keys. Give it words with attached data; get back prefix suggestions you can
filter by field, sort by field, and match with typo tolerance.

It is the Rust member of a small family of hand-written prefix trees:
[Python `ice1x/prefix_tree`](https://github.com/ice1x/prefix_tree) and
[Go `ice1x/go-prefix-trie`](https://github.com/ice1x/go-prefix-trie). Same core
semantics (insert / prefix-collect / field-query / sort-by-field), ported to Rust and
extended with deletion (with node pruning), O(1) count, and optional fuzzy search.

> **Scope, honestly.** This is a plain character trie (one node per `char`), not a
> compressed/radix/FST structure — it does not aim to beat crates like `fst`,
> `radix_trie` or `patricia_tree` on raw speed or memory. Its reason to exist is the
> *bundled autocomplete ergonomics* (metadata records + field queries + sort + fuzzy)
> and API parity across the Python/Go/Rust family.

## Two layers

- **Generic `Trie<V>`** — store any payload: `insert` (appends), `get`,
  `get_by_prefix`, `has_prefix`, `delete` (prunes empty nodes), `update`, `count`,
  `clear`, `get_by_prefix_fuzzy`.
- **`Record` layer** (`Trie<Record>`, where `Record = BTreeMap<String, Value>`) — adds
  the dynamic, runtime-keyed queries: `get_by_prefix_and_query`,
  `get_by_word_and_query`, `get_by_prefix_sort_by`, `get_by_prefix_sort_desc_by`.

Keys are stored verbatim by default; opt into case/diacritic folding with
`Trie::with_normalizer` (the bundled `normalize` lives behind the `normalize` feature).
No internal locking: a `Trie<V>` is `Send + Sync` when `V` is — wrap it in a `RwLock`
or `Mutex` to share across threads.

## Usage

```rust
use metatrie::{Trie, Record, Value, Order};

let mut trie: Trie<Record> = Trie::new();

let mut berlin = Record::new();
berlin.insert("name".into(), Value::from("Berlin"));
berlin.insert("population".into(), Value::from(3_600_000_i64));
trie.insert("berlin", berlin).unwrap();

let mut bern = Record::new();
bern.insert("name".into(), Value::from("Bern"));
bern.insert("population".into(), Value::from(130_000_i64));
trie.insert("bern", bern).unwrap();

// Prefix suggestions ranked by a field.
let ranked = trie.get_by_prefix_sort_by("ber", "population", Order::Desc);
assert_eq!(ranked.len(), 2);

// Filter a prefix result by exact field values.
let hits = trie.get_by_prefix_and_query("ber", &{
    let mut q = Record::new();
    q.insert("name".into(), Value::from("Bern"));
    q
});
assert_eq!(hits.len(), 1);

// Typo-tolerant prefix search (up to MAX_EDITS character edits).
let fuzzy = trie.get_by_prefix_fuzzy("berlim", 1).unwrap();
assert_eq!(fuzzy.len(), 1);
```

## Features

- `normalize` (off by default) — exposes `metatrie::normalize`, a Unicode key
  normalizer (NFKD + case/diacritic folding) to pass to `Trie::with_normalizer`. Pulls
  in `unicode-normalization`.
- `python` (off by default) — a [PyO3](https://pyo3.rs) extension module exposing the
  `Record` trie to Python as a `Trie` class with `dict` payloads. Build a wheel with
  [maturin](https://www.maturin.rs): `maturin build --release`.

The **default build has zero dependencies**.

## Ordering note

Child edges are a `BTreeMap<char, _>`, so `get_by_prefix` returns payloads in a
deterministic, sorted-by-`char` order. This differs from the Python family's
dict-insertion order and the Go family's randomized map order; use
`get_by_prefix_sort_by` whenever you need a specific order.

## License

Apache-2.0.
