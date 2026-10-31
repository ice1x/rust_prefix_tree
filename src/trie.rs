//! The generic prefix tree.
//!
//! [`Trie<V>`] is an uncompressed character trie (one level per Unicode `char`)
//! whose nodes store a *list* of payloads `V`. A node is a "word" iff its list is
//! non-empty — there is no end-of-word flag, exactly the Python reference's contract.
//! [`insert`](Trie::insert) **appends** (words may carry several payloads; aliases
//! and duplicates accumulate).
//!
//! Child edges live in a [`BTreeMap<char, _>`], so subtree traversal is deterministic
//! (sorted by `char`). This differs from the Python original's dict-insertion order
//! and the Go version's randomized map order, but is stable and dependency-free; all
//! ordered queries go through [`get_by_prefix_sort_by`](Trie::get_by_prefix_sort_by).

use std::collections::BTreeMap;

use crate::fuzzy::{prefix_edit_distance_le, MAX_EDITS};

/// Sort direction for [`Trie::get_by_prefix_sort_by`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// Errors returned by the mutating and bounded query methods.
///
/// Note there is no "nil data" error (Go's `ErrNilData`): a `V` is always present by
/// the type system, so there is nothing to reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrieError {
    /// A word/key was empty (or normalized to empty).
    EmptyKey,
    /// The word was not present as a complete word.
    NotFound,
    /// `max_edits` exceeded [`MAX_EDITS`].
    TooManyEdits {
        /// The maximum accepted edit distance.
        max: u32,
    },
}

impl std::fmt::Display for TrieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrieError::EmptyKey => write!(f, "key cannot be empty"),
            TrieError::NotFound => write!(f, "key not found"),
            TrieError::TooManyEdits { max } => {
                write!(f, "max_edits exceeds the maximum of {max}")
            }
        }
    }
}

impl std::error::Error for TrieError {}

/// A single trie node: a list of payloads plus child edges keyed by `char`.
struct Node<V> {
    data: Vec<V>,
    children: BTreeMap<char, Node<V>>,
}

impl<V> Node<V> {
    fn new() -> Self {
        Node {
            data: Vec::new(),
            children: BTreeMap::new(),
        }
    }

    fn is_word(&self) -> bool {
        !self.data.is_empty()
    }

    /// A node can be pruned when it holds no data and has no children.
    fn is_prunable(&self) -> bool {
        self.data.is_empty() && self.children.is_empty()
    }
}

/// A generic prefix tree mapping string keys to lists of payloads `V`.
///
/// See the [module docs](crate::trie) for the data-structure contract. For the
/// dynamic metadata API (field queries and sort-by-field) use the [`Record`] payload
/// and the methods in [`crate::record`].
pub struct Trie<V> {
    root: Node<V>,
    len: usize,
    normalizer: Option<fn(&str) -> String>,
}

impl<V> Default for Trie<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> Trie<V> {
    /// An empty trie that stores keys verbatim (no normalization).
    pub fn new() -> Self {
        Trie {
            root: Node::new(),
            len: 0,
            normalizer: None,
        }
    }

    /// An empty trie that applies `normalizer` to every word and prefix before use,
    /// so that insert-time and query-time folding always agree. Pass
    /// [`crate::normalize`] (requires the `normalize` feature) or your own function.
    pub fn with_normalizer(normalizer: fn(&str) -> String) -> Self {
        Trie {
            root: Node::new(),
            len: 0,
            normalizer: Some(normalizer),
        }
    }

    /// Apply the normalizer if configured, else borrow the input unchanged.
    fn norm<'a>(&self, s: &'a str) -> std::borrow::Cow<'a, str> {
        match self.normalizer {
            Some(f) => std::borrow::Cow::Owned(f(s)),
            None => std::borrow::Cow::Borrowed(s),
        }
    }

    /// Descend to the node reached by consuming every `char` of `key`. An empty key
    /// resolves to the root (the "whole tree"). Returns `None` if the path is absent.
    fn find_node(&self, key: &str) -> Option<&Node<V>> {
        let mut node = &self.root;
        for c in key.chars() {
            node = node.children.get(&c)?;
        }
        Some(node)
    }

    /// Insert `value` under `word`, appending to any payloads already stored there.
    ///
    /// # Errors
    /// [`TrieError::EmptyKey`] if `word` is empty or normalizes to empty.
    pub fn insert(&mut self, word: &str, value: V) -> Result<(), TrieError> {
        let key = self.norm(word);
        if key.is_empty() {
            return Err(TrieError::EmptyKey);
        }
        let mut node = &mut self.root;
        for c in key.chars() {
            node = node.children.entry(c).or_insert_with(Node::new);
        }
        node.data.push(value);
        self.len += 1;
        Ok(())
    }

    /// The payloads stored under the exact `word`, or `None` if `word` is absent or
    /// is only an intermediate prefix (not a complete word).
    pub fn get(&self, word: &str) -> Option<&[V]> {
        let key = self.norm(word);
        let node = self.find_node(&key)?;
        if node.is_word() {
            Some(&node.data)
        } else {
            None
        }
    }

    /// Whether any word in the trie starts with `prefix`. An empty prefix is `true`
    /// (it matches everything) as long as the trie is non-empty's root path exists —
    /// the root always exists, so an empty prefix is always `true`.
    pub fn has_prefix(&self, prefix: &str) -> bool {
        let key = self.norm(prefix);
        self.find_node(&key).is_some()
    }

    /// All payloads whose word starts with `prefix`: the prefix node's own payloads
    /// plus every payload in the subtree below it, in deterministic (sorted-`char`,
    /// pre-order) order. An empty prefix returns the whole tree. Returns an empty
    /// vector if the prefix is absent.
    pub fn get_by_prefix(&self, prefix: &str) -> Vec<&V> {
        let key = self.norm(prefix);
        self.collect_prefix(&key)
    }

    /// Like [`get_by_prefix`](Trie::get_by_prefix) but assumes `key` is already
    /// normalized (used internally to avoid double-normalizing).
    fn collect_prefix(&self, key: &str) -> Vec<&V> {
        match self.find_node(key) {
            Some(node) => {
                let mut out = Vec::new();
                collect(node, &mut out);
                out
            }
            None => Vec::new(),
        }
    }

    /// Typo-tolerant prefix search: payloads whose word has a prefix within
    /// `max_edits` character edits (Levenshtein) of `prefix`.
    ///
    /// `max_edits == 0` is exact and returns the same payloads as
    /// [`get_by_prefix`](Trie::get_by_prefix). To bound the scan, candidates are
    /// anchored on the query's first character (a first-character typo is not
    /// recovered — a standard autocomplete assumption).
    ///
    /// # Errors
    /// [`TrieError::TooManyEdits`] if `max_edits > MAX_EDITS`.
    pub fn get_by_prefix_fuzzy(&self, prefix: &str, max_edits: u32) -> Result<Vec<&V>, TrieError> {
        if max_edits > MAX_EDITS {
            return Err(TrieError::TooManyEdits { max: MAX_EDITS });
        }
        let key = self.norm(prefix);
        if max_edits == 0 {
            return Ok(self.collect_prefix(&key));
        }
        let qchars: Vec<char> = key.chars().collect();
        if qchars.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        if let Some(child) = self.root.children.get(&qchars[0]) {
            let mut path = String::new();
            path.push(qchars[0]);
            fuzzy_dfs(child, &mut path, &qchars, max_edits, &mut out);
        }
        Ok(out)
    }

    /// Remove the exact `word`, pruning any nodes left empty, and return the payloads
    /// that were stored there.
    ///
    /// # Errors
    /// [`TrieError::EmptyKey`] if `word` is empty; [`TrieError::NotFound`] if `word`
    /// is absent or is only an intermediate prefix.
    pub fn delete(&mut self, word: &str) -> Result<Vec<V>, TrieError> {
        let key = self.norm(word);
        if key.is_empty() {
            return Err(TrieError::EmptyKey);
        }
        let chars: Vec<char> = key.chars().collect();
        match delete_rec(&mut self.root, &chars) {
            Some(removed) => {
                self.len -= removed.len();
                Ok(removed)
            }
            None => Err(TrieError::NotFound),
        }
    }

    /// Replace all payloads of the exact `word` with the single `value`, returning
    /// the old payloads.
    ///
    /// # Errors
    /// [`TrieError::EmptyKey`] if `word` is empty; [`TrieError::NotFound`] if `word`
    /// is absent or is only an intermediate prefix.
    pub fn update(&mut self, word: &str, value: V) -> Result<Vec<V>, TrieError> {
        let key = self.norm(word);
        if key.is_empty() {
            return Err(TrieError::EmptyKey);
        }
        // Resolve mutably without borrowing self twice.
        let mut node = &mut self.root;
        for c in key.chars() {
            match node.children.get_mut(&c) {
                Some(n) => node = n,
                None => return Err(TrieError::NotFound),
            }
        }
        if node.data.is_empty() {
            return Err(TrieError::NotFound);
        }
        let old = std::mem::replace(&mut node.data, vec![value]);
        self.len = self.len - old.len() + 1;
        Ok(old)
    }

    /// Total number of payloads stored across all words (O(1)).
    pub fn count(&self) -> usize {
        self.len
    }

    /// Whether the trie holds no payloads.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Remove all words and payloads.
    pub fn clear(&mut self) {
        self.root = Node::new();
        self.len = 0;
    }
}

/// Pre-order collection: this node's data, then each child's subtree (sorted `char`).
fn collect<'a, V>(node: &'a Node<V>, out: &mut Vec<&'a V>) {
    out.extend(node.data.iter());
    for child in node.children.values() {
        collect(child, out);
    }
}

/// DFS collecting payloads of every word node whose path is a fuzzy match.
fn fuzzy_dfs<'a, V>(
    node: &'a Node<V>,
    path: &mut String,
    query: &[char],
    max_edits: u32,
    out: &mut Vec<&'a V>,
) {
    if node.is_word() && prefix_edit_distance_le(query, path, max_edits) {
        out.extend(node.data.iter());
    }
    for (&c, child) in &node.children {
        path.push(c);
        fuzzy_dfs(child, path, query, max_edits, out);
        path.pop();
    }
}

/// Recursive delete returning the removed payloads, or `None` if the target word is
/// absent / not a complete word. Prunes empty child nodes on the way back up.
fn delete_rec<V>(node: &mut Node<V>, chars: &[char]) -> Option<Vec<V>> {
    match chars.split_first() {
        Some((&first, rest)) => {
            let child = node.children.get_mut(&first)?;
            let removed = delete_rec(child, rest)?;
            if child.is_prunable() {
                node.children.remove(&first);
            }
            Some(removed)
        }
        None => {
            if node.data.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut node.data))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small `Trie<&str>` with overlapping prefixes and a shared-prefix word.
    fn sample() -> Trie<&'static str> {
        let mut t = Trie::new();
        t.insert("hello", "hello").unwrap();
        t.insert("help", "help").unwrap();
        t.insert("hero", "hero").unwrap();
        t
    }

    // --- insert / structure (Go TestTrie_Insert parity) ---

    #[test]
    fn insert_appends_multiple_payloads_per_word() {
        let mut t = Trie::new();
        t.insert("hello", 1).unwrap();
        t.insert("hello", 2).unwrap();
        assert_eq!(t.get("hello"), Some(&[1, 2][..]));
        assert_eq!(t.count(), 2);
    }

    #[test]
    fn insert_empty_word_errors() {
        let mut t: Trie<i32> = Trie::new();
        assert_eq!(t.insert("", 1), Err(TrieError::EmptyKey));
        assert!(t.is_empty());
    }

    // --- get / search (Go TestTrie_Search parity) ---

    #[test]
    fn get_exact_returns_all_entries() {
        let t = sample();
        assert_eq!(t.get("hello"), Some(&["hello"][..]));
    }

    #[test]
    fn get_prefix_that_is_not_a_word_is_none() {
        let t = sample();
        // "hel" is only an intermediate prefix of hello/help.
        assert_eq!(t.get("hel"), None);
    }

    #[test]
    fn get_missing_is_none() {
        let t = sample();
        assert_eq!(t.get("goodbye"), None);
        assert_eq!(t.get(""), None);
    }

    // --- has_prefix (Go TestTrie_HasPrefix parity) ---

    #[test]
    fn has_prefix_matrix() {
        let t = sample();
        assert!(t.has_prefix("hel"));
        assert!(t.has_prefix("hero"));
        assert!(t.has_prefix("")); // empty prefix matches everything
        assert!(!t.has_prefix("hi"));
        assert!(!t.has_prefix("goodbye"));
    }

    // --- get_by_prefix (Python _get_by_prefix parity) ---

    #[test]
    fn get_by_prefix_returns_node_and_subtree() {
        let t = sample();
        let mut got = t.get_by_prefix("hel");
        got.sort();
        assert_eq!(got, vec![&"hello", &"help"]);
    }

    #[test]
    fn empty_prefix_returns_whole_tree() {
        // Replaces the Python "%%" sentinel: "" == whole tree.
        let t = sample();
        assert_eq!(t.get_by_prefix("").len(), 3);
    }

    #[test]
    fn get_by_prefix_absent_is_empty() {
        let t = sample();
        assert!(t.get_by_prefix("zzz").is_empty());
    }

    #[test]
    fn get_by_prefix_is_repeatable_and_nonmutating() {
        // Python test_no_output_increment: two calls yield equal results.
        let t = sample();
        let a = t.get_by_prefix("hel");
        let b = t.get_by_prefix("hel");
        assert_eq!(a, b);
        assert_eq!(t.count(), 3);
    }

    // --- delete (Go TestTrie_Delete parity + pruning) ---

    #[test]
    fn delete_removes_word_and_returns_data() {
        let mut t = sample();
        assert_eq!(t.delete("hello"), Ok(vec!["hello"]));
        assert_eq!(t.get("hello"), None);
        assert_eq!(t.count(), 2);
    }

    #[test]
    fn delete_keeps_siblings() {
        let mut t = sample();
        t.delete("hello").unwrap();
        assert_eq!(t.get("help"), Some(&["help"][..]));
        assert_eq!(t.get("hero"), Some(&["hero"][..]));
    }

    #[test]
    fn delete_empty_and_absent_and_prefix_error() {
        let mut t = sample();
        assert_eq!(t.delete(""), Err(TrieError::EmptyKey));
        assert_eq!(t.delete("nope"), Err(TrieError::NotFound));
        assert_eq!(t.delete("hel"), Err(TrieError::NotFound)); // prefix, not a word
    }

    #[test]
    fn delete_prunes_empty_nodes() {
        let mut t = Trie::new();
        t.insert("abc", 1).unwrap();
        t.delete("abc").unwrap();
        // The whole a->b->c chain is gone, so no prefix survives.
        assert!(!t.has_prefix("a"));
        assert!(t.is_empty());
    }

    #[test]
    fn delete_does_not_prune_nodes_still_in_use() {
        let mut t = Trie::new();
        t.insert("ab", 1).unwrap();
        t.insert("abc", 2).unwrap();
        t.delete("abc").unwrap();
        // "ab" is still a word, so its node (and the path to it) must survive.
        assert_eq!(t.get("ab"), Some(&[1][..]));
        assert!(!t.has_prefix("abc"));
    }

    // --- update (Go TestTrie_Update parity) ---

    #[test]
    fn update_replaces_all_and_returns_old() {
        let mut t = Trie::new();
        t.insert("k", 1).unwrap();
        t.insert("k", 2).unwrap();
        assert_eq!(t.update("k", 9), Ok(vec![1, 2]));
        assert_eq!(t.get("k"), Some(&[9][..]));
        assert_eq!(t.count(), 1);
    }

    #[test]
    fn update_empty_and_absent_error() {
        let mut t = sample();
        assert_eq!(t.update("", "x"), Err(TrieError::EmptyKey));
        assert_eq!(t.update("nope", "x"), Err(TrieError::NotFound));
        assert_eq!(t.update("hel", "x"), Err(TrieError::NotFound));
    }

    // --- count / clear (Go parity) ---

    #[test]
    fn count_tracks_inserts_and_deletes() {
        let mut t = Trie::new();
        assert_eq!(t.count(), 0);
        t.insert("a", 1).unwrap();
        t.insert("a", 2).unwrap();
        t.insert("b", 3).unwrap();
        assert_eq!(t.count(), 3);
        t.delete("a").unwrap();
        assert_eq!(t.count(), 1);
    }

    #[test]
    fn clear_empties_and_zeroes_count() {
        let mut t = sample();
        t.clear();
        assert_eq!(t.count(), 0);
        assert!(t.is_empty());
        assert!(t.get_by_prefix("").is_empty());
    }

    // --- unicode / edge cases (Go TestTrie_EdgeCases parity) ---

    #[test]
    fn unicode_and_special_and_long_keys() {
        let mut t = Trie::new();
        t.insert("你好", 1).unwrap();
        t.insert("test@#$%", 2).unwrap();
        let long = "x".repeat(1000);
        t.insert(&long, 3).unwrap();
        assert_eq!(t.get("你好"), Some(&[1][..]));
        assert_eq!(t.get("test@#$%"), Some(&[2][..]));
        assert_eq!(t.get(&long), Some(&[3][..]));
    }

    // --- normalizer ---

    #[test]
    fn normalizer_folds_on_insert_and_query() {
        fn lower(s: &str) -> String {
            s.to_lowercase()
        }
        let mut t = Trie::with_normalizer(lower);
        t.insert("Hello", 1).unwrap();
        assert_eq!(t.get("HELLO"), Some(&[1][..]));
        assert_eq!(t.get_by_prefix("he").len(), 1);
    }

    #[test]
    fn default_trie_is_case_sensitive() {
        let t = sample();
        assert_eq!(t.get("HELLO"), None);
    }

    // --- fuzzy ---

    fn fuzzy_sample() -> Trie<&'static str> {
        let mut t = Trie::new();
        t.insert("solnechnogorsk", "Solnechnogorsk").unwrap();
        t.insert("berlin", "Berlin").unwrap();
        t
    }

    #[test]
    fn fuzzy_recovers_near_miss() {
        let t = fuzzy_sample();
        // Exact prefix misses...
        assert!(t.get_by_prefix("solnecho").is_empty());
        // ...fuzzy with 1 edit recovers it.
        let out = t.get_by_prefix_fuzzy("solnecho", 1).unwrap();
        assert_eq!(out, vec![&"Solnechnogorsk"]);
    }

    #[test]
    fn fuzzy_zero_equals_exact() {
        let t = fuzzy_sample();
        for q in ["sol", "solnechnogorsk", "ber", "berlin", "xyz"] {
            assert_eq!(t.get_by_prefix(q), t.get_by_prefix_fuzzy(q, 0).unwrap());
        }
    }

    #[test]
    fn fuzzy_rejects_too_many_edits() {
        let t = fuzzy_sample();
        assert_eq!(
            t.get_by_prefix_fuzzy("sol", MAX_EDITS + 1),
            Err(TrieError::TooManyEdits { max: MAX_EDITS })
        );
    }

    #[test]
    fn fuzzy_does_not_pull_unrelated_records() {
        let t = fuzzy_sample();
        let out = t.get_by_prefix_fuzzy("solnecho", 1).unwrap();
        assert!(out.iter().all(|&&r| r != "Berlin"));
    }

    #[test]
    fn trie_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Trie<crate::Record>>();
    }
}
