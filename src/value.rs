//! The dynamic payload model used by the [`Record`] layer.
//!
//! The generic [`crate::Trie`] stores any `V`; this module provides the ready-made
//! value type that mirrors the Python reference's "list of dicts" payload: a
//! [`Record`] is a map of `String` keys to [`Value`]s, and a trie node holds a list
//! of them. The [`Record`]-specific trie methods (field queries, sort-by-field) live
//! in [`crate::record`].

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// A dynamically-typed field value, mirroring the scalar JSON/Python value kinds a
/// record field can hold.
///
/// Equality is **exact per variant** (so `Int(1) != Bool(true)` and
/// `Int(3) != Float(3.0)`), matching a strict interpretation of the Python original.
/// For *ordering* (used by sort-by-field) integers and floats are compared
/// numerically — see [`Value::total_cmp`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Absence of a value (the Python `None`). Sorts lowest and is the fallback for
    /// a record missing the sort/query key.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed 64-bit integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// An owned UTF-8 string.
    String(String),
}

impl Value {
    /// Coarse type rank for cross-type ordering: `Null < Bool < numeric < String`.
    /// Int and Float share the numeric rank so they compare by value, not by kind.
    fn kind_rank(&self) -> u8 {
        match self {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Int(_) | Value::Float(_) => 2,
            Value::String(_) => 3,
        }
    }

    /// A **total** order over values, used by [`crate::Trie::get_by_prefix_sort_by`].
    ///
    /// Within a kind, values compare naturally; `Int`/`Float` compare as `f64` via
    /// [`f64::total_cmp`] (so `NaN` is ordered, never `Equal`-by-accident); across
    /// kinds they compare by [`Value::kind_rank`]. This never panics, unlike the
    /// Python version which raises on mixed/un-comparable sort keys.
    pub fn total_cmp(&self, other: &Value) -> Ordering {
        use Value::*;
        match (self, other) {
            (Null, Null) => Ordering::Equal,
            (Bool(a), Bool(b)) => a.cmp(b),
            (Int(a), Int(b)) => a.cmp(b),
            (Float(a), Float(b)) => a.total_cmp(b),
            (Int(a), Float(b)) => (*a as f64).total_cmp(b),
            (Float(a), Int(b)) => a.total_cmp(&(*b as f64)),
            (String(a), String(b)) => a.cmp(b),
            _ => self.kind_rank().cmp(&other.kind_rank()),
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int(v as i64)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_owned())
    }
}

/// A record: an ordered map of field name to [`Value`]. This is the payload type of
/// the [`Record`] trie layer ([`crate::record`]).
///
/// A [`BTreeMap`] (not a `HashMap`) gives deterministic iteration and `Debug`, so
/// tests are reproducible and field order is stable.
pub type Record = BTreeMap<String, Value>;

/// Whether `record` satisfies `query`: every `(key, value)` pair in `query` is
/// present in `record` and compares equal (exact [`Value`] equality).
///
/// This is the AND-filter used by [`crate::Trie::get_by_prefix_and_query`] and
/// [`crate::Trie::get_by_word_and_query`], ported from the Python original.
pub(crate) fn record_matches(record: &Record, query: &Record) -> bool {
    query.iter().all(|(k, v)| record.get(k) == Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_equality_is_per_variant() {
        assert_ne!(Value::Int(1), Value::Bool(true));
        assert_ne!(Value::Int(3), Value::Float(3.0));
        assert_eq!(Value::Int(3), Value::Int(3));
        assert_eq!(Value::from("x"), Value::String("x".to_owned()));
    }

    #[test]
    fn total_order_across_kinds() {
        // Null < Bool < numeric < String.
        assert_eq!(Value::Null.total_cmp(&Value::Bool(false)), Ordering::Less);
        assert_eq!(Value::Bool(true).total_cmp(&Value::Int(0)), Ordering::Less);
        assert_eq!(
            Value::Int(9).total_cmp(&Value::String("a".into())),
            Ordering::Less
        );
    }

    #[test]
    fn numeric_int_float_compare_by_value() {
        assert_eq!(Value::Int(2).total_cmp(&Value::Float(2.5)), Ordering::Less);
        assert_eq!(Value::Float(3.0).total_cmp(&Value::Int(3)), Ordering::Equal);
        assert_eq!(Value::Int(10).total_cmp(&Value::Int(9)), Ordering::Greater);
    }

    #[test]
    fn record_match_is_subset_and() {
        let mut rec = Record::new();
        rec.insert("type".into(), Value::Bool(true));
        rec.insert("gender".into(), Value::Bool(false));
        rec.insert("age".into(), Value::Int(23));

        let mut q = Record::new();
        q.insert("type".into(), Value::Bool(true));
        q.insert("gender".into(), Value::Bool(false));
        assert!(record_matches(&rec, &q));

        // Missing key fails.
        let mut q2 = Record::new();
        q2.insert("missing".into(), Value::Int(1));
        assert!(!record_matches(&rec, &q2));

        // Present key, wrong value fails.
        let mut q3 = Record::new();
        q3.insert("age".into(), Value::Int(99));
        assert!(!record_matches(&rec, &q3));

        // Empty query matches everything.
        assert!(record_matches(&rec, &Record::new()));
    }
}
