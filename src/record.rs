//! The metadata [`Record`] layer over [`Trie`].
//!
//! These inherent methods exist only for `Trie<Record>` and provide the dynamic,
//! runtime-keyed queries the Python reference exposes: filter a prefix result by
//! exact field equality, resolve one record by word + filter, and sort a prefix
//! result by an arbitrary field.

use crate::trie::{Order, Trie};
use crate::value::{record_matches, Record};

impl Trie<Record> {
    /// Records whose word starts with `prefix` and that match every `(key, value)`
    /// pair in `query` (exact field equality). Port of Python
    /// `get_by_prefix_and_query`.
    pub fn get_by_prefix_and_query(&self, prefix: &str, query: &Record) -> Vec<&Record> {
        self.get_by_prefix(prefix)
            .into_iter()
            .filter(|r| record_matches(r, query))
            .collect()
    }

    /// The first record stored under the exact `word` that matches `query`, or
    /// `None`. Port of Python `get_by_word_and_query` (exact word path, not subtree).
    pub fn get_by_word_and_query(&self, word: &str, query: &Record) -> Option<&Record> {
        self.get(word)?.iter().find(|r| record_matches(r, query))
    }

    /// Records whose word starts with `prefix`, sorted by field `key` in `order`.
    /// A record missing `key` sorts as [`crate::Value::Null`] (lowest). The sort is
    /// stable.
    pub fn get_by_prefix_sort_by(&self, prefix: &str, key: &str, order: Order) -> Vec<&Record> {
        use crate::value::Value;
        let mut out = self.get_by_prefix(prefix);
        out.sort_by(|a, b| {
            let av = a.get(key).unwrap_or(&Value::Null);
            let bv = b.get(key).unwrap_or(&Value::Null);
            let ord = av.total_cmp(bv);
            match order {
                Order::Asc => ord,
                Order::Desc => ord.reverse(),
            }
        });
        out
    }

    /// Records whose word starts with `prefix`, sorted by field `key` descending.
    /// Port of Python `get_by_prefix_sort_desc_by`.
    pub fn get_by_prefix_sort_desc_by(&self, prefix: &str, key: &str) -> Vec<&Record> {
        self.get_by_prefix_sort_by(prefix, key, Order::Desc)
    }
}

#[cfg(test)]
mod tests {
    use crate::trie::{Order, Trie};
    use crate::value::{Record, Value};

    /// Build a record from `(key, value)` pairs.
    fn rec(pairs: &[(&str, Value)]) -> Record {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    /// The Python test.py fixture: four Cyrillic people under their names.
    fn people() -> Trie<Record> {
        let mut t = Trie::new();
        t.insert(
            "иван",
            rec(&[
                ("name", "иван".into()),
                ("age", 31.into()),
                ("gender", true.into()),
                ("type", true.into()),
            ]),
        )
        .unwrap();
        t.insert(
            "ирина",
            rec(&[
                ("name", "ирина".into()),
                ("age", 23.into()),
                ("gender", false.into()),
                ("type", true.into()),
            ]),
        )
        .unwrap();
        t.insert(
            "ио",
            rec(&[
                ("name", "ио".into()),
                ("age", 3.into()),
                ("gender", true.into()),
                ("type", false.into()),
            ]),
        )
        .unwrap();
        t.insert(
            "иванович",
            rec(&[
                ("name", "иванович".into()),
                ("age", 51.into()),
                ("gender", true.into()),
                ("type", false.into()),
            ]),
        )
        .unwrap();
        t
    }

    fn names(records: &[&Record]) -> Vec<String> {
        records
            .iter()
            .map(|r| match r.get("name") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn sort_desc_by_age() {
        // Python test_get_by_prefix_sort_desc_by: prefix "ив" desc by age.
        let t = people();
        let got = t.get_by_prefix_sort_desc_by("ив", "age");
        assert_eq!(names(&got), vec!["иванович", "иван"]); // 51 before 31; "ио" excluded
    }

    #[test]
    fn sort_asc_by_age() {
        let t = people();
        let got = t.get_by_prefix_sort_by("и", "age", Order::Asc);
        // 3, 23, 31, 51
        assert_eq!(names(&got), vec!["ио", "ирина", "иван", "иванович"]);
    }

    #[test]
    fn whole_tree_via_empty_prefix() {
        // Python test_len used the "%%" sentinel; "" is the idiomatic equivalent.
        let t = people();
        assert_eq!(t.get_by_prefix("").len(), 4);
    }

    #[test]
    fn prefix_and_query_filters() {
        // Python test_get_by_prefix_and_query: prefix "и", {type:true, gender:false}.
        let t = people();
        let q = rec(&[("type", true.into()), ("gender", false.into())]);
        let got = t.get_by_prefix_and_query("и", &q);
        assert_eq!(names(&got), vec!["ирина"]);
    }

    #[test]
    fn prefix_absent_returns_empty() {
        let t = people();
        assert!(t.get_by_prefix("zzz").is_empty());
        assert!(t.get_by_prefix_and_query("zzz", &Record::new()).is_empty());
    }

    #[test]
    fn word_and_query_first_match_or_none() {
        let t = people();
        // Exact word path, matching query -> the record.
        let q = rec(&[("gender", true.into())]);
        let got = t.get_by_word_and_query("иван", &q);
        assert_eq!(got.and_then(|r| r.get("name")), Some(&Value::from("иван")));

        // Unknown word -> None (Python test_get_by_word_and_query_not_found).
        assert!(t.get_by_word_and_query("нет", &q).is_none());

        // Known word, non-matching query -> None.
        let q2 = rec(&[("gender", false.into())]);
        assert!(t.get_by_word_and_query("иван", &q2).is_none());
    }

    #[test]
    fn sort_by_string_field() {
        let mut t: Trie<Record> = Trie::new();
        t.insert("apple", rec(&[("v", "apple".into())])).unwrap();
        t.insert("apricot", rec(&[("v", "apricot".into())]))
            .unwrap();
        t.insert("avocado", rec(&[("v", "avocado".into())]))
            .unwrap();
        let asc = t.get_by_prefix_sort_by("a", "v", Order::Asc);
        assert_eq!(names_of(&asc, "v"), vec!["apple", "apricot", "avocado"]);
        let desc = t.get_by_prefix_sort_by("a", "v", Order::Desc);
        assert_eq!(names_of(&desc, "v"), vec!["avocado", "apricot", "apple"]);
    }

    fn names_of(records: &[&Record], key: &str) -> Vec<String> {
        records
            .iter()
            .map(|r| match r.get(key) {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            })
            .collect()
    }
}
