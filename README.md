# Rewriting the geo prefix tree in Rust (PyO3)

Design note for moving the `reviews` geo autocomplete from a pure-Python prefix
tree to a Rust-backed index, exposed in-process to the existing FastAPI service
via **PyO3 / maturin**.

Status: **core implemented.** The Rust crate (FST index + records store +
`normalize` + `suggest`), the `build-index` CLI, and the PyO3 extension module are
built and tested. See [§12 Implementation](#12-implementation-what-exists-now) for
the quickstart and layout. The sections below are the original design rationale.

---

## 1. Why

The autocomplete shipped in `services/reviews_api/services/geo/` is a pure-Python
trie (dict-per-node), tailored from [`ice1x/prefix_tree`](https://github.com/ice1x/prefix_tree).
It works and is fast enough to *query* (prefix lookups are microseconds), but it
has three structural costs that only grow with the dataset:

1. **Memory.** A dict-of-dicts trie is heavy — every node is a Python `dict` plus
   `PyObject` overhead per character. For `cities1000` (~170k places, and each
   place indexed under several alias keys → ~0.5–1M keys) this is on the order of
   several hundred MB **per process**.
2. **Per-worker duplication.** `api` runs `uvicorn --workers 2`, and each worker
   builds and holds its **own** copy of the trie. So the RAM cost is ×2 today,
   and ×N if we scale workers. Nothing is shared.
3. **Ceiling.** Holding the *whole planet* — `allCountries` (~12M features incl.
   villages and hydronyms), each with multilingual alternate names — is simply
   not feasible in a pure-Python dict trie on a normal box. We are capped at the
   "cities" tiers.

There is also a **load-time** cost: the index is rebuilt from the TSV on every
worker start (in the app lifespan), which grows linearly with dataset size.

The goal of a Rust rewrite is to remove all three: a compact, memory-mapped index
that is (a) 10–50× smaller, (b) **shared** read-only across workers via the OS
page cache, and (c) able to hold the full planet. As a bonus we get near-instant
startup (mmap, no rebuild) and optional typo tolerance.

## 2. Non-goals

- Not changing the HTTP contract. `GET /api/v1/geo/suggest?query=&limit=` and the
  `GeoSuggestion` schema stay identical.
- Not changing the frontend. It already calls `/geo/suggest`.
- Not rewriting the whole service in Rust — only the index data structure.

## 3. Why Rust + PyO3 (and not Go)

The autocomplete is deliberately **in-process** inside `reviews_api` (a decision
made when it shipped — no separate microservice). That constraint drives the
language choice:

- **Rust + PyO3** compiles to a native Python extension module (built with
  `maturin`). We swap the internals of `GeoIndex` for a Rust-backed one behind the
  *same* Python API — the FastAPI route, DI, and tests are untouched. This keeps
  the in-process architecture.
- **Go** ([`ice1x/go-prefix-trie`](https://github.com/ice1x/go-prefix-trie)) does
  not embed cleanly into a Python process: you'd go through cgo + a C shared
  library (painful build, GC interaction) or run it as a **separate sidecar**
  service over HTTP/gRPC — which is exactly the microservice we chose *not* to
  build. Go's trie is excellent, but its natural home is a standalone process.

Conclusion: keep the Go implementation as a possible standalone service if we ever
want one; use **Rust + PyO3** for the in-process win.

## 4. Data structure: FST, not a hand-rolled trie

The compactness win comes from the structure, not just the language. The
recommended core is a **Finite State Transducer** via the mature
[`fst`](https://crates.io/crates/fst) crate (BurntSushi):

- An `fst::Map` stores an ordered set of keys → `u64` values in a **single
  contiguous byte buffer** that can be **memory-mapped** from disk.
- Size is tiny: shared prefixes *and* shared suffixes are collapsed (a DAWG/MA-FST,
  not just a prefix trie). Hundreds of thousands of place names compress to a few
  MB.
- Prefix queries are a range scan over the FST (`map.range().ge(prefix)…`), and it
  natively supports **Levenshtein automata** for fuzzy matching — i.e. optional
  typo tolerance ("солнечо" → Солнечногорск), which the current exact-prefix trie
  can't do.

Because the FST is a flat mmap'd file, **all uvicorn workers share the same
physical pages** (read-only mmap → OS page cache). The ×2 (×N) duplication
disappears: the index exists once in RAM regardless of worker count.

> A hand-written Rust radix trie is a fine exercise and gives full control, but
> `fst` already delivers compactness + mmap sharing + fuzzy for free. Start with
> `fst`; only hand-roll if a concrete need appears.

### Keys, values, and metadata

`fst::Map` maps `bytes -> u64`. We put the *normalized* key (same normalization as
today: NFKD, drop combining, lowercase, collapse separators — UTF-8 bytes) as the
FST key, and encode a **record id** in the `u64` value. Multiple alias keys for one
place all map to the same record id (dedup by GeoNames id happens at query time,
as today).

Per-record metadata (`gid, lat, lon, country, population, feature_code, display
name`) lives in a **parallel memory-mapped columnar/records file**, indexed by
record id. So the query path is: FST range scan → collect record ids →
dedup by gid → sort by population → slice `limit` → read those records from the
mmap'd metadata blob. All zero-copy, all shared across workers.

## 5. Architecture

```
                         build time (offline, on the server)
  GeoNames dumps ──► cli.build_geo_dataset ──► cities1000.tsv         (already exists)
                                     │
                                     └──► geo_index build ──► index.fst + records.bin
                                                              (new: compact, mmap-ready)

                         run time (inside the api container)
  uvicorn worker 1 ┐
  uvicorn worker 2 ┼── mmap(index.fst) + mmap(records.bin)  ← ONE shared copy in page cache
  uvicorn worker N ┘        via the Rust PyO3 extension `geo_trie_rs`
```

The Python side keeps `GeoIndex` as the public seam:

```python
# services/geo/index.py  (unchanged public API; internals swapped)
class GeoIndex:
    def suggest(self, query: str, limit: int = 8) -> list[GeoRecord]: ...
```

Two backends behind a flag `GEO_BACKEND=rust|python` (default `python` until Rust
is proven, then flip):

- `python`  → today's `GeoTrie`.
- `rust`    → thin wrapper over `geo_trie_rs.Index` (the PyO3 module), which mmaps
  `index.fst` + `records.bin` and returns records.

### PyO3 module sketch (Rust)

```rust
use pyo3::prelude::*;
use fst::{Map, IntoStreamer, Streamer, automaton::Str};

#[pyclass]
struct Index { map: Map<memmap2::Mmap>, records: RecordStore }

#[pymethods]
impl Index {
    #[staticmethod]
    fn open(fst_path: &str, records_path: &str) -> PyResult<Index> { /* mmap both */ }

    /// Returns (gid, name, lat, lon, country, population, feature_code) tuples,
    /// deduped by gid, ranked by population desc, truncated to `limit`.
    fn suggest(&self, prefix: &str, limit: usize) -> PyResult<Vec<PyRecord>> {
        let aut = Str::new(prefix).starts_with();
        let mut stream = self.map.search(aut).into_stream();
        // collect ids, dedup by gid, sort by population, take `limit`, hydrate
    }
}

#[pymodule]
fn geo_trie_rs(_py: Python<'_>, m: &PyModule) -> PyResult<()> {
    m.add_class::<Index>()?; Ok(())
}
```

### Python wrapper sketch

```python
# services/geo/rust_backend.py
import geo_trie_rs
from .index import GeoRecord

class RustGeoIndex:
    def __init__(self, fst_path: str, records_path: str):
        self._idx = geo_trie_rs.Index.open(fst_path, records_path)

    def suggest(self, query: str, limit: int = 8) -> list[GeoRecord]:
        norm = normalize(query)                 # reuse the exact same normalizer
        if len(norm) < MIN_PREFIX_LEN:
            return []
        return [GeoRecord(*row) for row in self._idx.suggest(norm, limit)]
```

Note: normalization stays in Python (or is mirrored 1:1 in Rust) so index and
query fold identically — the idempotent `normalize()` we already have is the
contract.

## 6. Build pipeline

Extend the existing offline tool rather than replace it:

- `cli.build_geo_dataset` keeps producing `cities1000.tsv` (human-readable,
  diffable, already wired into the mount).
- Add `cli.build_geo_index` (or a `--emit-fst` flag) that reads the TSV and writes
  `index.fst` + `records.bin`. Building an FST requires keys in **sorted order** —
  `fst::MapBuilder` enforces this, so the step is: normalize every (key, record-id)
  pair → sort → stream into the builder. Fast in Rust even for `allCountries`.

Deploy: mount the built artifacts read-only (same pattern as the current
`geo-data/ → /data` mount) and point the service at them via env
(`GEO_FST_PATH`, `GEO_RECORDS_PATH`).

## 7. Memory & performance (rough)

| | Pure-Python dict trie (today) | Rust `fst` (mmap) |
|---|---|---|
| `cities1000` (~170k) index size | ~hundreds of MB, **×2 workers** | a few MB, **shared once** |
| `allCountries` (~12M) | not feasible | tens–low hundreds of MB, shared |
| Startup | rebuild from TSV per worker | mmap open (near-instant) |
| Prefix query | microseconds | microseconds (comparable) |
| Fuzzy / typo | not supported | Levenshtein automata |

The headline win is **memory + sharing + ceiling**, not raw query latency (both are
already fast). This is what unlocks "the whole world, including villages and
hydronyms" in a 2-worker container.

## 8. Toolchain / packaging

- Build the extension with **maturin**; ship a wheel so the Docker image doesn't
  need a Rust toolchain at runtime — use a multi-stage `Dockerfile.prod`:
  stage 1 (rust:slim) builds the wheel, stage 2 (python:slim) `pip install`s it.
- CI runs on self-hosted **macOS ARM64**; dev builds a macOS arm64 wheel, prod
  needs a linux x86_64/arm64 wheel — build per-target in the image, or use
  `maturin build` in the Docker build stage (prod-target only) and keep a local
  dev build for the Mac.
- Add `geo_trie_rs` to the build; keep pure-Python `GeoTrie` as the fallback so a
  broken wheel never takes the endpoint down.

## 9. Rollout plan

1. Land `cli.build_geo_index` and the Rust crate behind `GEO_BACKEND=python`
   (default) — no behavior change.
2. Build artifacts on the server, mount them, flip a single container to
   `GEO_BACKEND=rust`, diff its `/geo/suggest` output against the Python backend
   for a set of queries (parity check).
3. Flip default to `rust`; keep `python` as an emergency fallback for one release.
4. Once trusted, raise the dataset to `cities500` or `allCountries` (now that RAM
   allows) and drop the pure-Python trie.

## 10. Risks / open questions

- **Cross-platform wheels** (macOS arm64 dev vs linux prod) — handled by building
  the prod wheel inside the Docker build stage; the Mac only needs it for local
  runs/tests.
- **Normalization drift** — Python and Rust must fold identically. Safest: keep
  normalization in Python and pass already-normalized bytes into Rust (index build
  and query both call the same `normalize()`).
- **Ranking beyond population** — if we later want importance/feature-class
  weighting, that logic moves into the Rust `suggest` (or stays a post-step in
  Python on the small `limit`-sized result).
- **Fuzzy** is optional; enabling Levenshtein changes result semantics, so gate it
  behind a query param before turning it on by default.

## 11. Effort estimate (rough)

- Rust crate (`fst` map + records store + PyO3 bindings + `suggest`): ~1–2 days.
- `cli.build_geo_index` + parity test harness: ~0.5 day.
- Docker multi-stage build + maturin wiring + `GEO_BACKEND` flag + fallback: ~0.5–1 day.
- Rollout / parity validation: ~0.5 day.

Total: roughly **3–4 focused days** for a drop-in Rust backend with mmap sharing,
after which scaling to the full planet is a data change, not a code change.

## 12. Implementation (what exists now)

The core described above is built as the `geo_trie_rs` crate. It is pure Rust by
default (so `cargo test` / `cargo clippy` need no Python); the PyO3 extension module
is gated behind the `python` cargo feature and produced as an abi3 wheel by maturin.

### Crate layout

| File | Role |
|---|---|
| `src/normalize.rs` | `normalize()` — NFKD, drop combining marks, lowercase, collapse separators; idempotent. Mirrors the Python folding (§5, §10). |
| `src/records.rs`   | `records.bin` reader (`RecordStore`, `Record`) — postings + per-place metadata, offset-indexed for O(1) access, works over `Vec<u8>` or `Mmap`. |
| `src/builder.rs`   | `IndexBuilder` — groups `(key, record_id)` pairs into postings and emits `(index.fst, records.bin)` bytes. Keys deduped-sorted for `fst::MapBuilder`. |
| `src/index.rs`     | `Index` — mmaps both artifacts (`Index::open`) and serves `suggest(prefix, limit)`: prefix scan → dedup by gid → rank by population → truncate. |
| `src/python.rs`    | PyO3 module `geo_trie_rs` (`Index.open`, `Index.suggest`, `normalize`), feature = `python`. |
| `src/bin/build_index.rs` | `build-index <input.tsv> <out_dir>` CLI. |

The `records.bin` binary layout is documented at the top of `src/records.rs`.

### Build the index (offline)

TSV columns (tab-separated, optional `#` header): `gid  name  lat  lon  country
population  feature_code  keys`, where `keys` is a `|`-separated list of raw alias
strings (defaults to `name` if empty). A sample is in `tests/fixtures/cities.tsv`.

```sh
cargo run --release --bin build-index -- cities1000.tsv ./geo-index
# -> ./geo-index/index.fst  ./geo-index/records.bin
```

### Query from Rust

```rust
use geo_trie_rs::Index;
use memmap2::Mmap;

let idx = Index::<Mmap>::open("geo-index/index.fst", "geo-index/records.bin")?;
for r in idx.suggest("berl", 8)? {
    println!("{} ({}) pop={}", r.name, r.country, r.population);
}
```

### Query from Python (the reviews-service seam)

```sh
maturin build --release --features python --out dist   # abi3 wheel
pip install dist/geo_trie_rs-*.whl
```

```python
import geo_trie_rs
idx = geo_trie_rs.Index.open("geo-index/index.fst", "geo-index/records.bin")
# rows: list[(gid, name, lat, lon, country, population, feature_code)]
rows = idx.suggest("berl", limit=8)
# Fold queries identically to the index (idempotent, matches the Python normalizer):
key = geo_trie_rs.normalize("Zürich-HB")   # -> "zurich hb"
```

This drops in behind `RustGeoIndex` (§5): the wrapper maps each returned tuple to a
`GeoRecord`, keeping the `/geo/suggest` contract unchanged.

### Not yet built

Fuzzy / Levenshtein matching (§4), the Docker multi-stage prod build (§8), and the
`GEO_BACKEND` flag wiring on the Python side (§5) — all still to do.

---

### TL;DR

Keep the Python `GeoIndex` API and the `/geo/suggest` contract; swap the dict trie
for a memory-mapped `fst`-based index built in Rust and exposed via PyO3. Result:
~10–50× less memory, a **single shared** copy across uvicorn workers (not ×2),
near-instant startup, headroom for `allCountries`, and optional typo tolerance —
all in-process, no sidecar. Go stays a good option only if we ever want a separate
service.
