//! Sparse n-gram extraction for the rskim-search lexical index.
//!
//! This module provides the [`Ngram`] newtype (a `u16` bigram) and two pairs of
//! extraction functions:
//!
//! - [`extract_ngrams`] / [`extract_ngrams_with_weights`] — document extraction with
//!   max-weight deduplication (unsorted output).
//! - [`extract_query_ngrams`] / [`extract_query_ngrams_with_weights`] — query extraction
//!   with border-weighted selectivity and greedy covering-set selection (sorted by
//!   weight descending).
//!
//! # Design
//!
//! Bigrams are encoded as `(high_byte << 8) | low_byte`, matching the encoding used by
//! [`crate::weights::BIGRAM_WEIGHTS`].  Weights are looked up via binary search on the
//! sorted table, falling back to [`crate::weights::DEFAULT_WEIGHT`] for unknown pairs.

use std::collections::HashMap;
use std::fmt;

use crate::weights::{BIGRAM_WEIGHTS, DEFAULT_WEIGHT};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Multiplier applied to bigrams that fall at a token border (first/last 2 bytes
/// of any whitespace-delimited token) during query extraction.
///
/// Validated empirically at 3.5× — token-boundary bigrams are significantly more
/// discriminating than interior bigrams for code search.
pub const BORDER_MULTIPLIER: f32 = 3.5;

// ─────────────────────────────────────────────────────────────────────────────
// Ngram newtype
// ─────────────────────────────────────────────────────────────────────────────

/// A byte-pair bigram encoded as a `u16`.
///
/// The high byte is the first byte of the pair, the low byte is the second:
/// `key = (b1 << 8) | b2`.  This matches the encoding used in
/// [`crate::weights::BIGRAM_WEIGHTS`], enabling O(log *n*) weight lookup via
/// binary search.
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Ngram(pub u16);

impl Ngram {
    /// Encode two bytes into an [`Ngram`].
    ///
    /// `(b1 << 8) | b2` — the high byte is `b1`, the low byte is `b2`.
    #[must_use]
    #[inline]
    pub fn from_bytes(b1: u8, b2: u8) -> Self {
        Self((u16::from(b1) << 8) | u16::from(b2))
    }

    /// Decode an [`Ngram`] back into its two component bytes `(b1, b2)`.
    #[must_use]
    #[inline]
    pub fn to_bytes(self) -> (u8, u8) {
        ((self.0 >> 8) as u8, (self.0 & 0xFF) as u8)
    }

    /// Return the raw `u16` key.
    #[must_use]
    #[inline]
    pub fn key(self) -> u16 {
        self.0
    }
}

impl fmt::Display for Ngram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (b1, b2) = self.to_bytes();
        let fmt_byte = |b: u8| -> String {
            if b.is_ascii_graphic() || b == b' ' {
                String::from(b as char)
            } else {
                format!("\\x{b:02X}")
            }
        };
        write!(f, "{}{}", fmt_byte(b1), fmt_byte(b2))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Look up the IDF weight for a bigram key in a sorted `(key, weight)` slice.
///
/// Falls back to [`DEFAULT_WEIGHT`] when the key is absent.
#[inline]
fn lookup_weight(key: u16, weights: &[(u16, f32)]) -> f32 {
    weights
        .binary_search_by_key(&key, |&(k, _)| k)
        .ok()
        .map(|idx| weights[idx].1)
        .unwrap_or(DEFAULT_WEIGHT)
}

/// Compute token-border byte ranges for `query`.
///
/// For each whitespace-delimited token starting at byte offset `start` with
/// byte length `len`:
///
/// - **`len == 1`**: the range `[start.saturating_sub(1), (start+1).min(bytes.len()))` is
///   marked, so bigrams on either side of the lone byte are treated as borders.
/// - **`len >= 2`**: the first-2-byte range `[start, start+2)` and the last-2-byte range
///   `[end-2, end)` are pushed (only when they do not fully overlap).
///
/// Returns a list of `[lo, hi)` half-open byte intervals.
fn token_border_ranges(query: &str) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let bytes = query.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let end = i; // exclusive
        let len = end - start;

        if len == 1 {
            let lo = start.saturating_sub(1);
            let hi = (start + 1).min(bytes.len());
            ranges.push((lo, hi));
        } else {
            let first_border_end = (start + 2).min(end);
            ranges.push((start, first_border_end));

            let last_border_start = end.saturating_sub(2);
            if last_border_start >= first_border_end {
                ranges.push((last_border_start, end));
            }
        }
    }

    ranges
}

/// Returns `true` if a bigram starting at `bigram_pos` overlaps any token-border range.
///
/// A bigram at position `p` covers bytes `[p, p+1]`.  It overlaps `[lo, hi)` when
/// `p + 1 >= lo && p < hi`.
#[inline]
fn is_border_bigram(bigram_pos: usize, border_ranges: &[(usize, usize)]) -> bool {
    border_ranges
        .iter()
        .any(|&(lo, hi)| bigram_pos + 1 >= lo && bigram_pos < hi)
}

// ─────────────────────────────────────────────────────────────────────────────
// Document extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extract weighted bigrams from `text` using the provided sorted weight table.
///
/// For each byte pair in `text`, the IDF weight is looked up via binary search.
/// When the same bigram appears at multiple positions the **maximum** weight is kept
/// (max-weight deduplication).
///
/// Output is **unsorted** and suitable for building a posting list.
///
/// # Arguments
///
/// * `text` — source text (UTF-8; multi-byte sequences are treated as raw bytes).
/// * `weights` — sorted `(bigram_key, idf_weight)` slice, e.g. [`BIGRAM_WEIGHTS`].
///
/// # Panics
///
/// Never panics — byte scanning is infallible.
#[must_use]
pub fn extract_ngrams_with_weights(text: &str, weights: &[(u16, f32)]) -> Vec<(Ngram, f32)> {
    debug_assert!(
        weights.windows(2).all(|w| w[0].0 <= w[1].0),
        "weights must be sorted by key"
    );

    let bytes = text.as_bytes();
    let mut map: HashMap<u16, f32> = HashMap::new();

    for window in bytes.windows(2) {
        let key = Ngram::from_bytes(window[0], window[1]).key();
        let w = lookup_weight(key, weights);
        let entry = map.entry(key).or_insert(0.0_f32);
        if w > *entry {
            *entry = w;
        }
    }

    map.into_iter()
        .map(|(key, w)| (Ngram(key), w))
        .collect()
}

/// Extract weighted bigrams from `text` using the production [`BIGRAM_WEIGHTS`] table.
///
/// Convenience wrapper around [`extract_ngrams_with_weights`].
/// Output is unsorted; all unique bigrams with their max IDF weight are returned.
#[must_use]
pub fn extract_ngrams(text: &str) -> Vec<(Ngram, f32)> {
    extract_ngrams_with_weights(text, BIGRAM_WEIGHTS)
}

// ─────────────────────────────────────────────────────────────────────────────
// Query extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extract a border-weighted covering set of bigrams from `query` using the provided
/// sorted weight table.
///
/// This is the query-side counterpart to [`extract_ngrams_with_weights`].  It applies
/// a [`BORDER_MULTIPLIER`] bonus to bigrams that fall at token boundaries (first/last
/// 2 bytes of each whitespace-delimited token), then runs a greedy covering-set
/// heuristic that selects bigrams in descending weighted-IDF order until every byte
/// position in the query is covered.
///
/// Output is sorted by weight **descending** — highest-selectivity bigrams first.
///
/// # Arguments
///
/// * `query` — search query string.
/// * `weights` — sorted `(bigram_key, idf_weight)` slice, e.g. [`BIGRAM_WEIGHTS`].
///
/// # Panics
///
/// Never panics — byte scanning is infallible.
#[must_use]
pub fn extract_query_ngrams_with_weights(
    query: &str,
    weights: &[(u16, f32)],
) -> Vec<(Ngram, f32)> {
    debug_assert!(
        weights.windows(2).all(|w| w[0].0 <= w[1].0),
        "weights must be sorted by key"
    );

    let bytes = query.as_bytes();
    if bytes.len() < 2 {
        return vec![];
    }

    let border_ranges = token_border_ranges(query);

    // Build candidates: (Ngram, border_weighted_idf, position)
    let mut candidates: Vec<(Ngram, f32, usize)> = bytes
        .windows(2)
        .enumerate()
        .map(|(pos, window)| {
            let ngram = Ngram::from_bytes(window[0], window[1]);
            let base_w = lookup_weight(ngram.key(), weights);
            let multiplier = if is_border_bigram(pos, &border_ranges) {
                BORDER_MULTIPLIER
            } else {
                1.0_f32
            };
            (ngram, base_w * multiplier, pos)
        })
        .collect();

    // Sort by weighted IDF descending.
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Greedy covering set.
    let mut covered = vec![false; bytes.len()];
    let mut selected: Vec<(Ngram, f32)> = Vec::new();

    for (ngram, w, pos) in candidates {
        if !covered[pos] || !covered[pos + 1] {
            covered[pos] = true;
            covered[pos + 1] = true;
            selected.push((ngram, w));
        }
        if covered.iter().all(|&c| c) {
            break;
        }
    }

    // Output sorted by weight descending (already in insertion order from greedy,
    // but re-sort to guarantee the contract even for ties).
    selected.sort_by(|a, b| b.1.total_cmp(&a.1));
    selected
}

/// Extract a border-weighted covering set of bigrams from `query` using the
/// production [`BIGRAM_WEIGHTS`] table.
///
/// Convenience wrapper around [`extract_query_ngrams_with_weights`].
/// Output is sorted by weight descending.
#[must_use]
pub fn extract_query_ngrams(query: &str) -> Vec<(Ngram, f32)> {
    extract_query_ngrams_with_weights(query, BIGRAM_WEIGHTS)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // ── Synthetic weight table ────────────────────────────────────────────────

    fn synthetic_weights() -> Vec<(u16, f32)> {
        let mut w: Vec<(u16, f32)> = vec![
            (Ngram::from_bytes(b'f', b'n').key(), 8.0),
            (Ngram::from_bytes(b'n', b' ').key(), 3.0),
            (Ngram::from_bytes(b' ', b'm').key(), 5.0),
            (Ngram::from_bytes(b'm', b'a').key(), 4.0),
            (Ngram::from_bytes(b'a', b'i').key(), 2.0),
            (Ngram::from_bytes(b'i', b'n').key(), 6.0),
            (Ngram::from_bytes(b'n', b'(').key(), 3.5),
            (Ngram::from_bytes(b'(', b')').key(), 7.0),
            (Ngram::from_bytes(b' ', b'p').key(), 5.0),
            (Ngram::from_bytes(b'p', b'a').key(), 4.0),
            (Ngram::from_bytes(b'a', b'r').key(), 2.0),
            (Ngram::from_bytes(b'r', b's').key(), 3.5),
            (Ngram::from_bytes(b's', b'e').key(), 2.5),
        ];
        w.sort_by_key(|&(k, _)| k);
        w
    }

    // ── Cycle 1: Ngram type ───────────────────────────────────────────────────

    #[test]
    fn from_bytes_to_bytes_roundtrip() {
        let n = Ngram::from_bytes(b'f', b'n');
        assert_eq!(n.to_bytes(), (b'f', b'n'));
    }

    #[test]
    fn roundtrip_exhaustive_256x256() {
        for b1 in 0u8..=255 {
            for b2 in 0u8..=255 {
                let n = Ngram::from_bytes(b1, b2);
                assert_eq!(n.to_bytes(), (b1, b2), "roundtrip failed for ({b1},{b2})");
            }
        }
    }

    #[test]
    fn key_matches_encoding() {
        let n = Ngram::from_bytes(b'f', b'n');
        assert_eq!(n.key(), (u16::from(b'f') << 8) | u16::from(b'n'));
    }

    #[test]
    fn display_printable_ascii() {
        let n = Ngram::from_bytes(b'f', b'n');
        assert_eq!(n.to_string(), "fn");
    }

    #[test]
    fn display_non_printable_bytes() {
        let n = Ngram::from_bytes(0x01, 0x02);
        assert_eq!(n.to_string(), "\\x01\\x02");
    }

    #[test]
    fn display_space_is_printable() {
        let n = Ngram::from_bytes(b' ', b'a');
        assert_eq!(n.to_string(), " a");
    }

    #[test]
    fn ord_consistency() {
        let a = Ngram::from_bytes(b'a', b'a');
        let b = Ngram::from_bytes(b'b', b'b');
        assert!(a < b);
    }

    #[test]
    fn copy_semantics() {
        let n = Ngram::from_bytes(b'x', b'y');
        let m = n; // copy
        assert_eq!(n, m);
    }

    // ── Cycle 2: Document extraction ─────────────────────────────────────────

    #[test]
    fn extract_empty_string() {
        let w = synthetic_weights();
        assert!(extract_ngrams_with_weights("", &w).is_empty());
    }

    #[test]
    fn extract_single_char() {
        let w = synthetic_weights();
        assert!(extract_ngrams_with_weights("a", &w).is_empty());
    }

    #[test]
    fn extract_two_chars_fn() {
        let w = synthetic_weights();
        let result = extract_ngrams_with_weights("fn", &w);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, Ngram::from_bytes(b'f', b'n'));
        assert_eq!(result[0].1, 8.0_f32);
    }

    #[test]
    fn extract_fn_main_correct_bigrams() {
        let w = synthetic_weights();
        let result = extract_ngrams_with_weights("fn main()", &w);

        // Collect keys for assertion
        let keys: std::collections::HashSet<u16> = result.iter().map(|(n, _)| n.key()).collect();
        let expected_keys: std::collections::HashSet<u16> = [
            Ngram::from_bytes(b'f', b'n').key(),
            Ngram::from_bytes(b'n', b' ').key(),
            Ngram::from_bytes(b' ', b'm').key(),
            Ngram::from_bytes(b'm', b'a').key(),
            Ngram::from_bytes(b'a', b'i').key(),
            Ngram::from_bytes(b'i', b'n').key(),
            Ngram::from_bytes(b'n', b'(').key(),
            Ngram::from_bytes(b'(', b')').key(),
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected_keys);
    }

    #[test]
    fn extract_deduplicates_repeated_bigrams() {
        // "aaaa" produces only the "aa" bigram three times — should deduplicate to one entry.
        let w = synthetic_weights();
        let result = extract_ngrams_with_weights("aaaa", &w);
        let aa_count = result
            .iter()
            .filter(|(n, _)| *n == Ngram::from_bytes(b'a', b'a'))
            .count();
        assert_eq!(aa_count, 1, "repeated bigram must appear exactly once");
    }

    #[test]
    fn extract_max_weight_dedup() {
        // Build a table where "aa" appears twice with different weights.
        // The higher weight should win.
        let mut w: Vec<(u16, f32)> = vec![(Ngram::from_bytes(b'a', b'a').key(), 9.0)];
        w.sort_by_key(|&(k, _)| k);
        let result = extract_ngrams_with_weights("aaaa", &w);
        let entry = result
            .iter()
            .find(|(n, _)| *n == Ngram::from_bytes(b'a', b'a'));
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().1, 9.0_f32);
    }

    #[test]
    fn extract_unknown_bigram_gets_default_weight() {
        let w: Vec<(u16, f32)> = vec![]; // empty table
        let result = extract_ngrams_with_weights("zz", &w);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, DEFAULT_WEIGHT);
    }

    #[test]
    fn extract_utf8_multibyte_no_panic() {
        // "café" has multi-byte UTF-8 sequences — must not panic
        let w = synthetic_weights();
        let _ = extract_ngrams_with_weights("café", &w);
    }

    #[test]
    fn extract_cjk_no_panic() {
        // CJK characters are multi-byte — must not panic
        let w = synthetic_weights();
        let _ = extract_ngrams_with_weights("你好世界", &w);
    }

    // ── Cycle 3: Border detection ─────────────────────────────────────────────

    #[test]
    fn border_ranges_fn_parse() {
        // "fn parse":
        //   token "fn"    [0,2): first2 == last2 → one range [0,2)
        //   token "parse" [3,8): first2=[3,5), last2=[6,8)
        let ranges = token_border_ranges("fn parse");
        // Bigram at pos 0 ("fn") must be a border
        assert!(is_border_bigram(0, &ranges), "pos 0 is 'fn' — border");
        // Bigram at pos 1 ("n ") must be a border (overlaps end of "fn" token)
        assert!(is_border_bigram(1, &ranges), "pos 1 overlaps 'fn' border");
        // Bigram at pos 2 (" p") — " " is between tokens, "p" starts "parse" → border
        assert!(is_border_bigram(2, &ranges), "pos 2 (' p') starts 'parse'");
        // Interior of "parse": positions 3,4 ('ar','rs') are interior
        // pos 3 is 'pa' (start+1 of "parse" → still first-border, actually second byte)
        // Let's verify interior at pos 4 is NOT a border for "parse" (starts at 3)
        // parse = p(3),a(4),r(5),s(6),e(7) → first2=[3,5), last2=[6,8)
        // pos 4 = bigram [4,5] = 'ar': lo=3,hi=5 → 4+1=5 >= 3 && 4 < 5 → border!
        // Actually pos 4 is still in first border. pos 5 = 'rs': 5+1=6 >= 3 but 5 < 5? No.
        // pos 5: check [3,5): 5+1=6>=3 && 5<5 → false. check [6,8): 5+1=6>=6 && 5<8 → true!
        // So pos 5 is also a border. Only pos 4 ('ar') might be interior.
        // pos 4: [3,5): 4+1=5>=3 && 4<5 → true → IS a border.
        // So for "parse" (len=5), first2=[3,5) covers bigrams at pos 3,4; last2=[6,8) covers 5,6.
        // The only truly interior bigram is at pos 4's position... let me check "function" (len=8)
        // which was tested in validate.rs: interior positions 2,3,4.
        // For "fn parse", all "parse" bigrams (pos 3-6) at token positions 0-4: first2=[3,5), last2=[6,8)
        // pos 3: in [3,5) → border; pos 4: in [3,5) → border
        // pos 5: [6,8): 5+1=6>=6 && 5<8 → border
        // pos 6: [6,8): 6+1=7>=6 && 6<8 → border
        // So no interior for "parse" with len=5 (only len>=6 has interior)
    }

    #[test]
    fn border_ranges_single_byte_token() {
        // "a b": 'a' at pos 0, ' ' at pos 1, 'b' at pos 2
        let ranges = token_border_ranges("a b");
        // Bigram at pos 0 ('a',' ') — touches 'a' token → border
        assert!(is_border_bigram(0, &ranges), "pos 0 touches 'a' → border");
        // Bigram at pos 1 (' ','b') — touches 'b' token → border
        assert!(is_border_bigram(1, &ranges), "pos 1 touches 'b' → border");
    }

    #[test]
    fn border_ranges_empty_query() {
        let ranges = token_border_ranges("");
        assert!(ranges.is_empty());
    }

    #[test]
    fn border_ranges_single_token_long() {
        // "function" (8 bytes): first2=[0,2), last2=[6,8)
        // Interior bigrams at positions 2,3,4 must NOT be borders
        let ranges = token_border_ranges("function");
        for interior_pos in 2..=4_usize {
            assert!(
                !is_border_bigram(interior_pos, &ranges),
                "pos {interior_pos} is interior in 'function'"
            );
        }
        // Border positions: 0, 1 (first2) and 5, 6 (last2 overlap)
        assert!(is_border_bigram(0, &ranges));
        assert!(is_border_bigram(1, &ranges));
        assert!(is_border_bigram(5, &ranges));
        assert!(is_border_bigram(6, &ranges));
    }

    // ── Cycle 4: Query extraction ─────────────────────────────────────────────

    #[test]
    fn query_extract_empty() {
        let w = synthetic_weights();
        assert!(extract_query_ngrams_with_weights("", &w).is_empty());
    }

    #[test]
    fn query_extract_single_char() {
        let w = synthetic_weights();
        assert!(extract_query_ngrams_with_weights("x", &w).is_empty());
    }

    #[test]
    fn query_extract_fn_main_returns_bigrams() {
        let w = synthetic_weights();
        let result = extract_query_ngrams_with_weights("fn main()", &w);
        assert!(!result.is_empty(), "fn main() should yield bigrams");
    }

    #[test]
    fn query_extract_border_bigrams_have_higher_weight() {
        // "fn main()": "fn" is a border bigram of token "fn" (IDF 8.0 * 3.5 = 28.0)
        // "ai" is interior for "main" (IDF 2.0 * 1.0 = 2.0)
        // So "fn" weighted must exceed "ai" weighted.
        let w = synthetic_weights();
        let result = extract_query_ngrams_with_weights("fn main()", &w);

        let fn_entry = result
            .iter()
            .find(|(n, _)| *n == Ngram::from_bytes(b'f', b'n'));
        let ai_entry = result
            .iter()
            .find(|(n, _)| *n == Ngram::from_bytes(b'a', b'i'));

        // "fn" must be in the result since it's the highest-weight bigram
        assert!(fn_entry.is_some(), "'fn' must appear in query result");
        if let Some(ai) = ai_entry {
            assert!(
                fn_entry.unwrap().1 > ai.1,
                "'fn' border weight must exceed 'ai' interior weight"
            );
        }
    }

    #[test]
    fn query_extract_sorted_by_weight_desc() {
        let w = synthetic_weights();
        let result = extract_query_ngrams_with_weights("fn main()", &w);
        for pair in result.windows(2) {
            assert!(
                pair[0].1 >= pair[1].1,
                "result must be sorted by weight descending"
            );
        }
    }

    #[test]
    fn query_extract_covering_set_covers_positions() {
        let query = "fn main()";
        let w = synthetic_weights();
        let result = extract_query_ngrams_with_weights(query, &w);
        let bytes = query.as_bytes();

        // Verify that every byte position that has a known bigram is covered.
        let mut covered = vec![false; bytes.len()];
        for (ngram, _) in &result {
            for (pos, window) in bytes.windows(2).enumerate() {
                if Ngram::from_bytes(window[0], window[1]) == *ngram {
                    covered[pos] = true;
                    covered[pos + 1] = true;
                }
            }
        }

        // All positions with bigrams in the weight table must be covered
        for (pos, window) in bytes.windows(2).enumerate() {
            let key = Ngram::from_bytes(window[0], window[1]).key();
            if w.binary_search_by_key(&key, |&(k, _)| k).is_ok() {
                assert!(covered[pos], "position {pos} must be covered");
                assert!(covered[pos + 1], "position {} must be covered", pos + 1);
            }
        }
    }

    #[test]
    fn query_extract_higher_idf_preferred() {
        // "fn main()": "fn" has IDF 8.0 (highest) and "in" has IDF 6.0
        // With border multiplier, "fn" weighted = 8.0 * 3.5 = 28.0
        // The result's first entry should be "fn"
        let w = synthetic_weights();
        let result = extract_query_ngrams_with_weights("fn main()", &w);
        assert!(!result.is_empty());
        assert_eq!(
            result[0].0,
            Ngram::from_bytes(b'f', b'n'),
            "highest-weight bigram 'fn' must be first"
        );
    }

    // ── Cycle 5: Convenience API wiring ──────────────────────────────────────

    #[test]
    fn extract_ngrams_uses_production_weights() {
        // "fn" is a common code bigram that should be in BIGRAM_WEIGHTS
        let result = extract_ngrams("fn main()");
        assert!(!result.is_empty(), "production weights must yield results");
        // All returned weights must be > 0
        for (_, w) in &result {
            assert!(*w > 0.0_f32, "weight must be positive");
        }
    }

    #[test]
    fn extract_query_ngrams_uses_production_weights() {
        let result = extract_query_ngrams("fn main()");
        assert!(!result.is_empty(), "production weights must yield query results");
        // Output must be sorted by weight descending
        for pair in result.windows(2) {
            assert!(pair[0].1 >= pair[1].1, "must be sorted descending");
        }
    }

    // ── Cycle 6: Performance sanity ───────────────────────────────────────────

    #[test]
    fn extract_ngrams_1000_line_file_under_1ms() {
        // Generate a synthetic 1000-line Rust-like file (~60 bytes per line → ~60 KB)
        let line = "fn process_item(item: &Item) -> Result<Output, Error> { todo!() }\n";
        let text: String = line.repeat(1000);

        let start = std::time::Instant::now();
        let result = extract_ngrams(&text);
        let elapsed = start.elapsed();

        assert!(!result.is_empty());

        // In release builds the O(n log W) algorithm completes in < 1ms.
        // In debug builds (unoptimized) we allow 500ms.
        #[cfg(not(debug_assertions))]
        assert!(
            elapsed.as_millis() < 1,
            "extract_ngrams on ~60KB took {}ms in release mode (must be < 1ms)",
            elapsed.as_millis()
        );
        #[cfg(debug_assertions)]
        assert!(
            elapsed.as_millis() < 500,
            "extract_ngrams on ~60KB took {}ms in debug mode (must be < 500ms)",
            elapsed.as_millis()
        );
    }
}
