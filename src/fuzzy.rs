//! Char-level, Unicode-safe fuzzy prefix matching.
//!
//! Salvaged from the crate's former fst-backed engine: [`prefix_edit_distance_le`]
//! answers "is some prefix of `key` within `max` character edits (Levenshtein) of
//! the whole `query`?". It operates on Unicode scalar values, so a Cyrillic typo
//! counts as one edit (a byte-oriented Levenshtein would miscount). The trie's
//! [`crate::Trie::get_by_prefix_fuzzy`] walks candidate words and filters them with
//! this function.

/// Largest edit distance accepted by [`crate::Trie::get_by_prefix_fuzzy`]. Small on
/// purpose: 1–2 covers realistic typos, and the candidate scan cost grows with
/// tolerance.
pub const MAX_EDITS: u32 = 2;

/// Whether some prefix of `key` is within `max` character edits (Levenshtein) of the
/// full `query`.
///
/// DP over the standard edit-distance matrix `M[k][j] = dist(key[..k], query[..j])`,
/// extending `key` one char at a time and tracking `min_k M[k][n]` (n = query len).
/// Extra `key` chars beyond a match are free (that is the "prefix" part). Rows whose
/// every entry exceeds `max` allow an early break, since they can only grow.
pub(crate) fn prefix_edit_distance_le(query: &[char], key: &str, max: u32) -> bool {
    let n = query.len();
    // row[j] = edit distance between the current key prefix and query[..j].
    let mut row: Vec<u32> = (0..=n as u32).collect();
    if row[n] <= max {
        return true; // empty key prefix already within budget (query shorter than max)
    }
    for c in key.chars() {
        let mut prev_diag = row[0]; // M[k][0]
        row[0] += 1; // M[k+1][0]
        let mut row_min = row[0];
        for j in 1..=n {
            let m_k_j = row[j]; // M[k][j] before overwrite
            let cost = if query[j - 1] == c { 0 } else { 1 };
            row[j] = (prev_diag + cost) // substitute / match
                .min(row[j] + 1) // delete key char
                .min(row[j - 1] + 1); // insert query char
            prev_diag = m_k_j;
            row_min = row_min.min(row[j]);
        }
        if row[n] <= max {
            return true;
        }
        if row_min > max {
            break; // no longer key prefix can bring any entry back under budget
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_edit_distance_le_unit() {
        let q: Vec<char> = "солнечо".chars().collect();
        // Prefix "солнечн" of the key is 1 substitution away; extra chars are free.
        assert!(prefix_edit_distance_le(&q, "солнечногорск", 1));
        assert!(!prefix_edit_distance_le(&q, "солнечногорск", 0));

        let q2: Vec<char> = "kat".chars().collect();
        assert!(prefix_edit_distance_le(&q2, "katze", 0)); // exact prefix
        assert!(prefix_edit_distance_le(&q2, "cat", 1)); // 1 substitution
        assert!(!prefix_edit_distance_le(&q2, "dog", 2)); // too far

        // Deletion: query longer than a short key still matches within budget.
        let q3: Vec<char> = "berlim".chars().collect();
        assert!(prefix_edit_distance_le(&q3, "berlin", 1));
    }
}
