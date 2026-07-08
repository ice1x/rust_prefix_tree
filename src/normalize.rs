//! Key normalization for the geo index.
//!
//! This mirrors the Python `normalize()` contract used by the pure-Python trie so
//! that index-build folding and query folding agree exactly (see README §5, §10).
//! Each character (after NFKD) falls into exactly one of three classes, matching
//! the Python normalizer's `_SEPARATORS = " -.,()/"`:
//!
//! 1. NFKD decomposition,
//! 2. drop combining marks (accents/diacritics),
//! 3. lowercase,
//! 4. **alphanumeric** → keep; **separator** (whitespace, `-`, `.`, `,`, `(`, `)`,
//!    `/`) → collapse runs to a single ASCII space; **anything else** (apostrophes,
//!    quotes, `_`, other punctuation) → drop with no space. Trim the edges.
//!
//! Dropping (not spacing) apostrophes/quotes is what keeps byte-for-byte parity
//! with Python: `O'Brien -> obrien`, `St. John's -> st johns` (see issue #4).
//!
//! The function is **idempotent**: `normalize(normalize(s)) == normalize(s)`.

use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

/// Characters that are treated as separators and collapsed to a single space.
/// Matches the Python normalizer's `_SEPARATORS = " -.,()/"` (whitespace generalizes
/// the space). Note: `_`, `'`, `"` are deliberately NOT here — they are dropped.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '.' | ',' | '(' | ')' | '/')
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

    let mut out = String::with_capacity(decomposed.len());
    let mut pending_space = false;
    for c in decomposed.chars() {
        if c.is_alphanumeric() {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        } else if is_separator(c) {
            // Only emit a space if we already have real content buffered.
            if !out.is_empty() {
                pending_space = true;
            }
        }
        // else: non-alnum, non-separator (apostrophe, quote, `_`, …) -> dropped.
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
        // `_` is NOT a separator (matches Python) -> dropped, not spaced.
        assert_eq!(normalize("a_b/c,d.e"), "ab c d e");
        assert_eq!(normalize("  spaced   out  "), "spaced out");
        assert_eq!(normalize("Saint-Étienne"), "saint etienne");
    }

    #[test]
    fn apostrophes_and_quotes_are_dropped_not_spaced() {
        // Issue #4: match the Python contract — `'`/`"` are removed with no space.
        assert_eq!(normalize("O'Brien"), "obrien");
        assert_eq!(normalize("St. John's"), "st johns");
        assert_eq!(normalize("N'Djamena"), "ndjamena");
        assert_eq!(normalize("Val-d'Or"), "val dor"); // hyphen spaces, apostrophe drops
        assert_eq!(normalize("\"Quoted\""), "quoted");
        assert_eq!(normalize("St. John's (Town)"), "st johns town");
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

    #[test]
    fn nfkd_compatibility_decomposition() {
        // Ligatures, superscripts and full-width forms fold to their ASCII base.
        assert_eq!(normalize("ﬁnd"), "find"); // U+FB01 ligature fi
        assert_eq!(normalize("m²"), "m2"); // superscript two
        assert_eq!(normalize("ＦＵＬＬ"), "full"); // full-width latin
    }

    #[test]
    fn collapses_mixed_and_repeated_separators() {
        assert_eq!(normalize("a  -_/,. b"), "a b");
        assert_eq!(normalize("(New) York"), "new york");
    }

    #[test]
    fn only_separators_and_diacritics_yield_empty() {
        assert_eq!(normalize(" - _ / "), "");
        // A lone combining acute accent (U+0301) decomposes/drops to nothing.
        assert_eq!(normalize("\u{0301}"), "");
    }

    #[test]
    fn idempotent_on_compatibility_and_mixed() {
        for s in ["ﬁnd", "m²", "ＦＵＬＬ", "(New)--York", "  O'Brien  "] {
            assert_eq!(
                normalize(&normalize(s)),
                normalize(s),
                "not idempotent: {s:?}"
            );
        }
    }
}
