//! Key normalization for the geo index.
//!
//! This mirrors the Python `normalize()` contract used by the pure-Python trie so
//! that index-build folding and query folding agree exactly (see README §5, §10):
//!
//! 1. NFKD decomposition,
//! 2. drop combining marks (accents/diacritics),
//! 3. lowercase,
//! 4. collapse any run of separators (whitespace, `-`, `_`, `/`, `,`, `.`) into a
//!    single ASCII space, and trim leading/trailing spaces.
//!
//! The function is **idempotent**: `normalize(normalize(s)) == normalize(s)`.

use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

/// Characters that are treated as separators and collapsed to a single space.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '_' | '/' | ',' | '.' | '\'' | '"' | '(' | ')')
}

/// Normalize a raw place/query string into its canonical index key form.
///
/// Both the index build and the query path MUST call this so that the folded
/// bytes are identical on either side.
pub fn normalize(input: &str) -> String {
    // NFKD + drop combining marks, then lowercase.
    let decomposed: String = input
        .nfkd()
        .filter(|&c| !is_combining_mark(c))
        .collect::<String>()
        .to_lowercase();

    // Collapse separator runs to a single space and trim.
    let mut out = String::with_capacity(decomposed.len());
    let mut pending_space = false;
    for c in decomposed.chars() {
        if is_separator(c) {
            // Only emit a space if we already have real content buffered.
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_ascii() {
        assert_eq!(normalize("Berlin"), "berlin");
        assert_eq!(normalize("NEW YORK"), "new york");
    }

    #[test]
    fn strips_diacritics() {
        // Café -> cafe, Solnechnogorsk-ish Cyrillic stays but lowercased.
        assert_eq!(normalize("Café"), "cafe");
        assert_eq!(normalize("Zürich"), "zurich");
        assert_eq!(normalize("São Paulo"), "sao paulo");
        assert_eq!(normalize("Đà Nẵng"), "đa nang");
    }

    #[test]
    fn collapses_separators() {
        assert_eq!(normalize("New---York"), "new york");
        assert_eq!(normalize("a_b/c,d.e"), "a b c d e");
        assert_eq!(normalize("  spaced   out  "), "spaced out");
        assert_eq!(normalize("Saint-Étienne"), "saint etienne");
    }

    #[test]
    fn trims_edges_and_handles_empty() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   "), "");
        assert_eq!(normalize("---"), "");
        assert_eq!(normalize("  Rio  "), "rio");
    }

    #[test]
    fn is_idempotent() {
        for s in ["Café  de   Flore", "São-Paulo", "  Zürich_HB ", "N.Y.C."] {
            let once = normalize(s);
            let twice = normalize(&once);
            assert_eq!(once, twice, "normalize not idempotent for {s:?}");
        }
    }

    #[test]
    fn keeps_cyrillic_lowercased() {
        assert_eq!(normalize("Солнечногорск"), "солнечногорск");
    }
}
