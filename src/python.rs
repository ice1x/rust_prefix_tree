//! PyO3 extension module `geo_trie_rs` (README §5).
//!
//! Enabled by the `python` feature and built into a wheel with maturin. It wraps
//! the pure-Rust [`crate::Index`] over a `memmap2::Mmap` so all uvicorn workers
//! share one read-only copy of the artifacts via the OS page cache.
//!
//! Python surface:
//! ```python
//! import geo_trie_rs
//! idx = geo_trie_rs.Index.open("index.fst", "records.bin")
//! # -> list[(gid, name, lat, lon, country, population, feature_code)]
//! rows = idx.suggest("berl", 8)
//! ```

// `useless_conversion` fires on `#[pymethods]`-macro-generated result conversions,
// not on our code; the expansion is not something we can rewrite.
#![allow(clippy::useless_conversion)]

use memmap2::Mmap;
use pyo3::exceptions::PyIOError;
use pyo3::prelude::*;

use crate::index::Index as CoreIndex;

/// A record row as returned to Python: `(gid, name, lat, lon, country, population,
/// feature_code)`. Ordering matches the Python `GeoRecord` positional fields.
type PyRow = (u64, String, f64, f64, String, i64, String);

/// Memory-mapped geo index exposed to Python.
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

    /// Autocomplete: rows whose normalized key starts with `prefix`, deduped by
    /// GeoNames id, ranked by population desc, truncated to `limit`.
    #[pyo3(signature = (prefix, limit = 8))]
    fn suggest(&self, prefix: &str, limit: usize) -> PyResult<Vec<PyRow>> {
        self.inner
            .suggest(prefix, limit)
            .map(|records| {
                records
                    .into_iter()
                    .map(|r| {
                        (
                            r.gid,
                            r.name,
                            r.lat,
                            r.lon,
                            r.country,
                            r.population,
                            r.feature_code,
                        )
                    })
                    .collect()
            })
            .map_err(|e| PyIOError::new_err(e.to_string()))
    }
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
    m.add_function(wrap_pyfunction!(normalize, m)?)?;
    Ok(())
}
