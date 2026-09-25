//! Word-token boundary mapping for positional (phrase / --near) search.
//!
//! Maps every byte of a source string to the ordinal of the word-token it
//! belongs to, so the indexer can attach a token ordinal to each posting
//! (v5, #392 / #380 Phase 2).

/// Single source of truth for the word-byte definition used throughout the search stack.
///
/// Returns `true` for ASCII alphanumeric characters and `_`; `false` for everything
/// else (punctuation, whitespace, and any non-ASCII byte).  Non-ASCII bytes are
/// treated as separators — a v1 tradeoff that under-tokenizes multi-byte text but
/// is byte-consistent and cannot panic.
///
/// **Invariant (D10 / AD-393-3):** every tokenizer in the search stack —
/// `word_token_indices` (indexer/reader), `collect_word_spans` (CLI predicate),
/// and `extract_query_positional_tokens` (n-gram query builder) — MUST use this
/// single function as its word-boundary test.  Sharing one definition makes a
/// future rule change (e.g. adding `$` or Unicode word chars) automatically
/// propagate to all sites, preventing the silent-empty / false-positive divergence
/// class that #393 was introduced to fix.
#[inline]
pub(crate) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Map every byte of `source` to the ordinal of the word-token it belongs to.
///
/// A "word byte" is `is_word_byte(b)` (ASCII `[A-Za-z0-9_]`); each maximal run
/// of word bytes is one token.  Separator (non-word) bytes inherit the ordinal
/// of the preceding word (0 before the first word).
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
        let is_word = is_word_byte(b);
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

/// Map every byte of `source` to **both** the word-token ordinal and the byte
/// length of the maximal word run it belongs to, in a single O(n) pass.
///
/// Returns `(positions, lengths)` where:
/// - `positions[i]` — word-token ordinal at byte `i`.  Semantics are identical
///   to [`word_token_indices`]: monotonically non-decreasing, non-word bytes
///   inherit the ordinal of the preceding word (0 before the first word).
/// - `lengths[i]` — byte span of the maximal word run at byte `i`, or `0` for
///   non-word bytes.  This is the value stored in
///   [`crate::index::format::PostingEntry::token_length`] for the AD-411-7
///   exact-token gate (`p.token_length == word.byte_len`, reader.rs).
///
/// Both output vecs have length `source.len()`.
///
/// ## Single-source-of-truth note
///
/// Computing both arrays here in one traversal ensures that word-run boundaries
/// are defined exactly once — via the shared [`is_word_byte`] primitive — and
/// that `positions` and `lengths` are always derived from the same scan.  The
/// caller (the indexer) therefore cannot accidentally desync them by editing
/// one traversal without the other.
///
/// # Invariants
/// - `positions.len() == source.len()`
/// - `lengths.len()  == source.len()`
/// - `positions` is monotonically non-decreasing
/// - `lengths[i] == 0` whenever `!is_word_byte(source.as_bytes()[i])`
#[must_use]
pub(crate) fn word_token_indices_and_lengths(source: &str) -> (Vec<u32>, Vec<u32>) {
    let bytes = source.as_bytes();
    let mut positions = Vec::with_capacity(bytes.len());
    let mut lengths = Vec::with_capacity(bytes.len());
    let mut token: u32 = 0;
    let mut seen_word = false;
    let mut i = 0usize;

    while i < bytes.len() {
        if is_word_byte(bytes[i]) {
            // Find the end of this maximal word run.
            let start = i;
            while i < bytes.len() && is_word_byte(bytes[i]) {
                i += 1;
            }
            // The first word is token 0; every subsequent word increments —
            // matching word_token_indices semantics exactly.
            if seen_word {
                token = token.saturating_add(1);
            }
            seen_word = true;
            let len = (i - start) as u32;
            // Fill both arrays for the entire run at once.
            for _ in start..i {
                positions.push(token);
                lengths.push(len);
            }
        } else {
            // Non-word byte: inherit the current ordinal; length is 0.
            positions.push(token);
            lengths.push(0);
            i += 1;
        }
    }

    (positions, lengths)
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

    // ── word_token_indices_and_lengths ───────────────────────────────────────

    #[test]
    fn and_lengths_positions_match_word_token_indices() {
        // positions must be byte-for-byte identical to word_token_indices.
        let cases = [
            "foo::bar",
            "let x = foo(bar_baz, 42);",
            "  ab",
            "",
            "aébc",
            "hello",
            "::::",
        ];
        for s in cases {
            let expected = word_token_indices(s);
            let (positions, _) = word_token_indices_and_lengths(s);
            assert_eq!(positions, expected, "positions mismatch for {:?}", s);
        }
    }

    #[test]
    fn and_lengths_foo_colons_bar() {
        // "foo" is a 3-byte run → each byte gets length 3.
        // "::" are non-word → length 0.
        // "bar" is a 3-byte run → each byte gets length 3.
        let (positions, lengths) = word_token_indices_and_lengths("foo::bar");
        assert_eq!(positions, vec![0, 0, 0, 0, 0, 1, 1, 1]);
        assert_eq!(lengths, vec![3, 3, 3, 0, 0, 3, 3, 3]);
    }

    #[test]
    fn and_lengths_leading_separators() {
        // "  ab": non-word prefix → 0-lengths; "ab" is a 2-byte run.
        let (positions, lengths) = word_token_indices_and_lengths("  ab");
        assert_eq!(positions, vec![0, 0, 0, 0]);
        assert_eq!(lengths, vec![0, 0, 2, 2]);
    }

    #[test]
    fn and_lengths_empty_is_empty() {
        let (positions, lengths) = word_token_indices_and_lengths("");
        assert!(positions.is_empty());
        assert!(lengths.is_empty());
    }

    #[test]
    fn and_lengths_all_separators() {
        // No word runs → lengths are all 0, positions stay at 0.
        let (positions, lengths) = word_token_indices_and_lengths("::::");
        assert_eq!(positions, vec![0, 0, 0, 0]);
        assert_eq!(lengths, vec![0, 0, 0, 0]);
    }

    #[test]
    fn and_lengths_invariants_hold() {
        // Both vecs same length as source; positions non-decreasing;
        // non-word bytes have length 0.
        let s = "let x = foo(bar_baz, 42);";
        let (positions, lengths) = word_token_indices_and_lengths(s);
        assert_eq!(positions.len(), s.len());
        assert_eq!(lengths.len(), s.len());
        for w in positions.windows(2) {
            assert!(w[1] >= w[0], "positions must be non-decreasing");
        }
        for (i, (&b, &len)) in s.as_bytes().iter().zip(lengths.iter()).enumerate() {
            if !is_word_byte(b) {
                assert_eq!(len, 0, "non-word byte at {i} must have length 0");
            }
        }
    }
}
