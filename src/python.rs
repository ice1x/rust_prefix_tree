//! PyO3 extension module `geo_trie_rs` (README §5).
//!
//! Enabled by the `python` feature and built into a wheel with maturin. It wraps
//! the pure-Rust [`crate::Index`] over a `memmap2::Mmap` so all uvicorn workers
//! share one read-only copy of the artifacts via the OS page cache.
//!
//! The exposed [`Index`] is **domain-agnostic**: `suggest` returns
//! `(rank, group, payload_bytes)` triples and the caller decodes the payload. For
//! the geo build, [`geo_unpack`] turns a triple into the familiar 7-tuple.
//!
//! ```python
//! import geo_trie_rs
//! idx = geo_trie_rs.Index.open("index.fst", "records.bin")
//! for rank, group, payload in idx.suggest("berl", 8):
//!     gid, name, lat, lon, country, population, feature_code = \
//!         geo_trie_rs.geo_unpack(rank, group, payload)
//! ```

// `useless_conversion` fires on `#[pymethods]`-macro-generated result conversions,
// not on our code; the expansion is not something we can rewrite.
#![allow(clippy::useless_conversion)]

use memmap2::Mmap;
use pyo3::exceptions::PyIOError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::geo::GeoRecord;
use crate::index::Index as CoreIndex;
use crate::records::Record;

/// A neutral result row exposed to Python: `(rank, group, payload_bytes)`.
type PyHit<'py> = (i64, u64, Bound<'py, PyBytes>);

/// The geo 7-tuple: `(gid, name, lat, lon, country, population, feature_code)`,
/// matching the Python `GeoRecord` positional fields.
type PyGeoRow = (u64, String, f64, f64, String, i64, String);

/// Memory-mapped, domain-agnostic autocomplete index exposed to Python.
#[pyclass]
struct Index {
    inner: CoreIndex<Mmap>,
}

#[pymethods]
impl Index {
    /// Open by memory-mapping `index.fst` + `records.bin` read-only.
    #[staticmethod]
    fn open(fst_path: &str, records_path: &str) -> PyResult<Self> {
        CoreIndex::<Mmap>::open(fst_path, records_path)
            .map(|inner| Self { inner })
            .map_err(|e| PyIOError::new_err(e.to_string()))
    }

    /// Number of records held.
    fn __len__(&self) -> usize {
        self.inner.len() as usize
    }

    /// Autocomplete: `(rank, group, payload_bytes)` rows whose normalized key starts
    /// with `prefix`, deduped by `group`, ranked by `rank` desc, truncated to
    /// `limit`. Decode `payload_bytes` with your domain adapter (e.g. `geo_unpack`).
    ///
    /// `max_edits` (default 0) enables typo tolerance: 0 is exact prefix (identical
    /// to before), 1–2 recover near-miss queries via a Levenshtein automaton. Keep
    /// it opt-in / behind a query param — it changes result semantics (README §10).
    #[pyo3(signature = (prefix, limit = 8, max_edits = 0))]
    fn suggest<'py>(
        &self,
        py: Python<'py>,
        prefix: &str,
        limit: usize,
        max_edits: u32,
    ) -> PyResult<Vec<PyHit<'py>>> {
        self.inner
            .suggest_fuzzy(prefix, limit, max_edits)
            .map(|records| {
                records
                    .into_iter()
                    .map(|r| (r.rank, r.group, PyBytes::new_bound(py, &r.payload)))
                    .collect()
            })
            .map_err(|e| PyIOError::new_err(e.to_string()))
    }
}

/// Decode a geo payload triple `(rank, group, payload)` (as returned by
/// [`Index::suggest`] over a geo-built index) into the 7-tuple
/// `(gid, name, lat, lon, country, population, feature_code)`.
#[pyfunction]
fn geo_unpack(rank: i64, group: u64, payload: &[u8]) -> PyResult<PyGeoRow> {
    let record = Record {
        group,
        rank,
        payload: payload.to_vec(),
    };
    let g = GeoRecord::from_record(&record).map_err(|e| PyIOError::new_err(e.to_string()))?;
    Ok((
        g.gid,
        g.name,
        g.lat,
        g.lon,
        g.country,
        g.population,
        g.feature_code,
    ))
}

/// Normalize a string with the exact folding used at index-build time. Exposed so
/// the Python side can fold queries identically (README §10).
#[pyfunction]
fn normalize(input: &str) -> String {
    crate::normalize::normalize(input)
}

#[pymodule]
fn geo_trie_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Index>()?;
    m.add_function(wrap_pyfunction!(geo_unpack, m)?)?;
    m.add_function(wrap_pyfunction!(normalize, m)?)?;
    Ok(())
}
