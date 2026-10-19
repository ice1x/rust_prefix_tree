//! Optional PyO3 bindings (`python` feature): a `Trie` class over `Trie<Record>`,
//! exposing the metadata/autocomplete API to Python with dict payloads.
//!
//! Python values map to [`Value`] as: `None`→Null, `bool`→Bool, `int`→Int,
//! `float`→Float, `str`→String. Other types raise `ValueError`.

// The `#[pymethods]` macro expansion for fallible methods trips clippy's
// `useless_conversion` on the generated `PyResult` plumbing (a known pyo3 0.22
// codegen artifact, not our code). Silence it for this binding module only.
#![allow(clippy::useless_conversion)]

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyList};

use crate::trie::{Order, Trie, TrieError};
use crate::value::{Record, Value};

fn obj_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if obj.is_none() {
        return Ok(Value::Null);
    }
    // `bool` is a subclass of `int`, so test it before integer extraction.
    if let Ok(b) = obj.downcast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(Value::Int(i));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(Value::Float(f));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(Value::String(s));
    }
    Err(PyValueError::new_err(
        "unsupported value type for a metatrie Record (expected None/bool/int/float/str)",
    ))
}

fn value_to_obj(py: Python<'_>, v: &Value) -> PyObject {
    match v {
        Value::Null => py.None(),
        Value::Bool(b) => (*b).into_py(py),
        Value::Int(i) => (*i).into_py(py),
        Value::Float(f) => (*f).into_py(py),
        Value::String(s) => s.as_str().into_py(py),
    }
}

fn dict_to_record(dict: &Bound<'_, PyDict>) -> PyResult<Record> {
    let mut rec = Record::new();
    for (k, val) in dict.iter() {
        let key: String = k.extract()?;
        rec.insert(key, obj_to_value(&val)?);
    }
    Ok(rec)
}

fn record_to_dict(py: Python<'_>, rec: &Record) -> PyResult<PyObject> {
    let d = PyDict::new_bound(py);
    for (k, v) in rec {
        d.set_item(k, value_to_obj(py, v))?;
    }
    Ok(d.into_py(py))
}

fn records_to_list(py: Python<'_>, recs: &[&Record]) -> PyResult<Py<PyList>> {
    let list = PyList::empty_bound(py);
    for r in recs {
        list.append(record_to_dict(py, r)?)?;
    }
    Ok(list.unbind())
}

fn map_err(e: TrieError) -> PyErr {
    match e {
        TrieError::EmptyKey => PyValueError::new_err("key cannot be empty"),
        TrieError::NotFound => PyKeyError::new_err("key not found"),
        TrieError::TooManyEdits { max } => {
            PyValueError::new_err(format!("max_edits exceeds the maximum of {max}"))
        }
    }
}

/// A prefix-tree autocomplete index with dict (metadata) payloads.
#[pyclass(name = "Trie")]
struct PyTrie {
    inner: Trie<Record>,
}

#[pymethods]
impl PyTrie {
    #[new]
    fn new() -> Self {
        PyTrie { inner: Trie::new() }
    }

    /// Insert `word` with a dict payload (appends if the word already exists).
    fn insert(&mut self, word: &str, data: &Bound<'_, PyDict>) -> PyResult<()> {
        let rec = dict_to_record(data)?;
        self.inner.insert(word, rec).map_err(map_err)
    }

    /// The list of dict payloads stored under the exact `word`, or `None`.
    fn get(&self, py: Python<'_>, word: &str) -> PyResult<Option<Py<PyList>>> {
        match self.inner.get(word) {
            Some(recs) => {
                let refs: Vec<&Record> = recs.iter().collect();
                Ok(Some(records_to_list(py, &refs)?))
            }
            None => Ok(None),
        }
    }

    /// Whether any word starts with `prefix`.
    fn has_prefix(&self, prefix: &str) -> bool {
        self.inner.has_prefix(prefix)
    }

    /// All payloads whose word starts with `prefix` (empty prefix = whole tree).
    fn get_by_prefix(&self, py: Python<'_>, prefix: &str) -> PyResult<Py<PyList>> {
        records_to_list(py, &self.inner.get_by_prefix(prefix))
    }

    /// Typo-tolerant prefix search within `max_edits` character edits.
    #[pyo3(signature = (prefix, max_edits=1))]
    fn get_by_prefix_fuzzy(
        &self,
        py: Python<'_>,
        prefix: &str,
        max_edits: u32,
    ) -> PyResult<Py<PyList>> {
        let out = self
            .inner
            .get_by_prefix_fuzzy(prefix, max_edits)
            .map_err(map_err)?;
        records_to_list(py, &out)
    }

    /// Payloads under `prefix` matching every `(key, value)` in `query`.
    fn get_by_prefix_and_query(
        &self,
        py: Python<'_>,
        prefix: &str,
        query: &Bound<'_, PyDict>,
    ) -> PyResult<Py<PyList>> {
        let q = dict_to_record(query)?;
        records_to_list(py, &self.inner.get_by_prefix_and_query(prefix, &q))
    }

    /// The first payload under the exact `word` matching `query`, or `None`.
    fn get_by_word_and_query(
        &self,
        py: Python<'_>,
        word: &str,
        query: &Bound<'_, PyDict>,
    ) -> PyResult<Option<PyObject>> {
        let q = dict_to_record(query)?;
        match self.inner.get_by_word_and_query(word, &q) {
            Some(r) => Ok(Some(record_to_dict(py, r)?)),
            None => Ok(None),
        }
    }

    /// Payloads under `prefix` sorted by field `key` (descending by default).
    #[pyo3(signature = (prefix, key, descending=true))]
    fn get_by_prefix_sort_by(
        &self,
        py: Python<'_>,
        prefix: &str,
        key: &str,
        descending: bool,
    ) -> PyResult<Py<PyList>> {
        let order = if descending { Order::Desc } else { Order::Asc };
        records_to_list(py, &self.inner.get_by_prefix_sort_by(prefix, key, order))
    }

    /// Remove `word`, returning its payloads (raises `KeyError` if absent).
    fn delete(&mut self, py: Python<'_>, word: &str) -> PyResult<Py<PyList>> {
        let removed = self.inner.delete(word).map_err(map_err)?;
        let refs: Vec<&Record> = removed.iter().collect();
        records_to_list(py, &refs)
    }

    /// Replace all payloads of `word` with one dict, returning the old payloads.
    fn update(
        &mut self,
        py: Python<'_>,
        word: &str,
        data: &Bound<'_, PyDict>,
    ) -> PyResult<Py<PyList>> {
        let rec = dict_to_record(data)?;
        let old = self.inner.update(word, rec).map_err(map_err)?;
        let refs: Vec<&Record> = old.iter().collect();
        records_to_list(py, &refs)
    }

    /// Total number of payloads stored.
    fn count(&self) -> usize {
        self.inner.count()
    }

    /// Whether the trie holds no payloads.
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Remove all words and payloads.
    fn clear(&mut self) {
        self.inner.clear();
    }
}

/// The `metatrie` Python module.
#[pymodule]
fn metatrie(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTrie>()?;
    Ok(())
}
