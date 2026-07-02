//! Word-token boundary mapping for positional (phrase / --near) search.
//!
//! Maps every byte of a source string to the ordinal of the word-token it
//! belongs to, so the indexer can attach a token ordinal to each posting
//! (v5, #392 / #380 Phase 2).

/// Map every byte of `source` to the ordinal of the word-token it belongs to.
///
/// A "word byte" is ASCII `[A-Za-z0-9_]`; each maximal run of word bytes is one
/// token. Separator (non-word) bytes inherit the ordinal of the preceding word
/// (0 before the first word). Non-ASCII bytes are treated as separators — a v1
/// tradeoff that under-tokenizes multi-byte text but is byte-consistent and
/// cannot panic.
///
/// # Invariants
/// - `out.len() == source.len()`
/// - `out` is monotonically non-decreasing
///
/// Runs in O(`source.len()`) with a single allocation.
#[must_use]
pub(crate) fn word_token_indices(source: &str) -> Vec<u32> {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut token: u32 = 0;
    let mut in_word = false;
    let mut seen_word = false;
    for &b in bytes {
        let is_word = b.is_ascii_alphanumeric() || b == b'_';
        if is_word {
            if !in_word {
                // Entering a new word run. The first word is token 0; every
                // subsequent new word increments the ordinal.
                if seen_word {
                    token = token.saturating_add(1);
                }
                seen_word = true;
                in_word = true;
            }
        } else {
            in_word = false;
        }
        out.push(token);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_foo_colons_bar() {
        // "foo" -> token 0, "::" inherits 0, "bar" -> token 1
        assert_eq!(word_token_indices("foo::bar"), vec![0, 0, 0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn len_matches_source_and_is_monotonic() {
        let s = "let x = foo(bar_baz, 42);";
        let m = word_token_indices(s);
        assert_eq!(m.len(), s.len());
        for w in m.windows(2) {
            assert!(w[1] >= w[0], "token map must be non-decreasing");
        }
    }

    #[test]
    fn leading_separators_are_token_zero() {
        assert_eq!(word_token_indices("  ab"), vec![0, 0, 0, 0]);
    }

    #[test]
    fn empty_source_is_empty() {
        assert!(word_token_indices("").is_empty());
    }

    #[test]
    fn non_ascii_bytes_are_separators() {
        // "é" is 2 bytes (0xC3 0xA9), both non-word separators between words.
        let m = word_token_indices("aébc");
        assert_eq!(m.len(), "aébc".len());
        // 'a' -> 0; the two é bytes inherit 0; "bc" -> 1
        assert_eq!(m.first(), Some(&0));
        assert_eq!(m.last(), Some(&1));
    }
}
