//! Selectivity validation — compare uniform vs. border-weighted scoring strategies.

use crate::extract::encode_bigram;
use crate::types::ValidationResult;

/// Standard test queries used for validation reporting.
pub const TEST_QUERIES: &[&str] = &[
    "fn parse",
    "impl Iterator",
    "async function",
    "class Builder",
    "def __init__",
    "SELECT * FROM",
    "import React",
    "func main",
    "public static void",
];

/// Multiplier applied to bigrams at token borders (first/last 2 bytes of each token).
const BORDER_MULTIPLIER: f64 = 3.5;

/// Compute uniform selectivity: sum IDF weights for all query bigrams without
/// any positional bonus.
///
/// This is equivalent to `idf::selectivity`.
#[must_use]
pub fn uniform_selectivity(query: &str, weights: &[(u16, f32)]) -> f64 {
    crate::idf::selectivity(query, weights)
}

/// Compute the byte ranges in `query` that constitute token borders.
///
/// For each whitespace-delimited token starting at byte offset `start` with length `len`:
/// - The border is the first two bytes (`start..start+2`) and the last two bytes
///   (`start+len-2..start+len`). For single-byte tokens the entire token is a border.
///
/// Returns a list of `[range_start, range_end)` half-open byte intervals, deduplicated
/// and clamped to the token bounds.
fn token_border_ranges(query: &str) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    // Walk the raw bytes to identify token start offsets so we can compute byte positions
    // without relying on char indices (all inputs are ASCII-ish code queries).
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace.
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Consume the token.
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let end = i; // exclusive
        let len = end - start;

        if len == 1 {
            // Single-byte token: every bigram that touches this position is a border bigram.
            // A bigram at position p covers bytes [p, p+1].  The single token byte is at `start`.
            // The bigram at position `start - 1` covers [start-1, start] and the bigram at
            // position `start` covers [start, start+1].  We model this by marking the range
            // [start, start+1) so that any bigram starting at `start-1` or `start` is flagged.
            // We use a half-open range [start.saturating_sub(1), start+1) to capture both.
            let lo = start.saturating_sub(1);
            let hi = (start + 1).min(bytes.len());
            ranges.push((lo, hi));
        } else {
            // First-2-bytes border: bigrams starting at `start` or `start+1` overlap.
            let first_border_end = (start + 2).min(end);
            ranges.push((start, first_border_end));

            // Last-2-bytes border: bigrams starting at `end-2` or `end-1` overlap.
            let last_border_start = end.saturating_sub(2);
            // Only push a separate range if it does not fully overlap the first-border range.
            if last_border_start >= first_border_end {
                ranges.push((last_border_start, end));
            }
        }
    }

    ranges
}

/// Compute border-weighted selectivity.
///
/// Bigrams that overlap the first or last 2 bytes of any whitespace-delimited token
/// receive a `BORDER_MULTIPLIER` bonus, reflecting that token boundaries are more
/// discriminating for code search.
///
/// Border detection is positional: the bigram's byte offset in `query` is checked
/// against the precomputed token border ranges, so only bigrams that actually sit
/// at a token boundary receive the multiplier — not all bigrams whose byte values
/// happen to match a border byte.
#[must_use]
pub fn border_weighted_selectivity(query: &str, weights: &[(u16, f32)]) -> f64 {
    let border_ranges = token_border_ranges(query);
    let mut total = 0.0_f64;

    let query_bytes = query.as_bytes();
    if query_bytes.len() < 2 {
        return total;
    }

    for (pos, window) in query_bytes.windows(2).enumerate() {
        let key = encode_bigram(window[0], window[1]);
        let idf = match weights.binary_search_by_key(&key, |&(k, _)| k) {
            Ok(idx) => weights[idx].1 as f64,
            Err(_) => continue,
        };

        // A bigram at byte offset `pos` covers positions [pos, pos+1].
        // It is a border bigram if `pos` falls within any precomputed border range.
        let multiplier = if is_border_bigram(pos, &border_ranges) {
            BORDER_MULTIPLIER
        } else {
            1.0
        };

        total += idf * multiplier;
    }

    total
}

/// Returns true if a bigram starting at `bigram_pos` in the query overlaps any
/// token border range.
///
/// `border_ranges` is the output of [`token_border_ranges`] — a list of
/// `[lo, hi)` byte intervals covering the first/last two bytes of every token.
/// A bigram at position `p` covers bytes `p` and `p+1`, so it overlaps a range
/// `[lo, hi)` whenever `p < hi && p + 1 >= lo`, i.e. `p >= lo.saturating_sub(1)
/// && p < hi`.  Because bigrams are exactly 2 bytes wide, the simpler check
/// `lo <= p + 1 && p < hi` (i.e. the bigram's last byte is inside or touching the
/// range) is equivalent and used here for clarity.
fn is_border_bigram(bigram_pos: usize, border_ranges: &[(usize, usize)]) -> bool {
    border_ranges
        .iter()
        .any(|&(lo, hi)| bigram_pos + 1 >= lo && bigram_pos < hi)
}

/// Run both scoring strategies over all test queries and return the aggregated result.
#[must_use]
pub fn run_validation(weights: &[(u16, f32)], test_queries: &[&str]) -> ValidationResult {
    if test_queries.is_empty() || weights.is_empty() {
        return ValidationResult {
            uniform_selectivity: 0.0,
            border_weighted_selectivity: 0.0,
            improvement_pct: 0.0,
        };
    }

    let uniform: f64 = test_queries
        .iter()
        .map(|q| uniform_selectivity(q, weights))
        .sum::<f64>()
        / test_queries.len() as f64;

    let border: f64 = test_queries
        .iter()
        .map(|q| border_weighted_selectivity(q, weights))
        .sum::<f64>()
        / test_queries.len() as f64;

    let improvement_pct = if uniform > 0.0 {
        (border - uniform) / uniform * 100.0
    } else {
        0.0
    };

    ValidationResult {
        uniform_selectivity: uniform,
        border_weighted_selectivity: border,
        improvement_pct,
    }
}

/// Greedy covering-set heuristic.
///
/// Selects bigrams from `query` in descending IDF order until every byte position
/// in the query is covered by at least one bigram. Returns the selected bigram keys.
#[must_use]
pub fn covering_set_heuristic(query: &str, weights: &[(u16, f32)]) -> Vec<u16> {
    let bytes = query.as_bytes();
    if bytes.len() < 2 {
        return vec![];
    }

    // Build candidate list: (bigram_key, idf, start_position_in_query)
    let mut candidates: Vec<(u16, f32, usize)> = bytes
        .windows(2)
        .enumerate()
        .filter_map(|(pos, window)| {
            let key = encode_bigram(window[0], window[1]);
            weights
                .binary_search_by_key(&key, |&(k, _)| k)
                .ok()
                .map(|idx| (key, weights[idx].1, pos))
        })
        .collect();

    // Sort by IDF descending.
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut covered = vec![false; bytes.len()];
    let mut selected = Vec::new();

    for (key, _, pos) in candidates {
        if !covered[pos] || !covered[pos + 1] {
            covered[pos] = true;
            covered[pos + 1] = true;
            selected.push(key);
        }

        // Stop once all positions are covered.
        if covered.iter().all(|&c| c) {
            break;
        }
    }

    selected
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Build a small synthetic weight table for testing.
    fn synthetic_weights() -> Vec<(u16, f32)> {
        let mut w: Vec<(u16, f32)> = vec![
            (encode_bigram(b'f', b'n'), 8.0), // "fn" — very selective
            (encode_bigram(b'n', b' '), 3.0), // "n " — moderate
            (encode_bigram(b' ', b'p'), 5.0), // " p" — selective
            (encode_bigram(b'p', b'a'), 4.0), // "pa" — moderate
            (encode_bigram(b'a', b'r'), 2.0), // "ar" — common
            (encode_bigram(b'r', b's'), 3.5), // "rs" — moderate
            (encode_bigram(b'i', b'm'), 6.0), // "im" — selective
            (encode_bigram(b'm', b'p'), 5.5), // "mp" — selective
            (encode_bigram(b'p', b'l'), 4.5), // "pl"
            (encode_bigram(b'l', b' '), 2.5), // "l "
        ];
        w.sort_by_key(|&(k, _)| k);
        w
    }

    #[test]
    fn empty_query_empty_covering_set() {
        let w = synthetic_weights();
        assert!(covering_set_heuristic("", &w).is_empty());
    }

    #[test]
    fn single_char_empty_covering_set() {
        let w = synthetic_weights();
        assert!(covering_set_heuristic("x", &w).is_empty());
    }

    // -------------------------------------------------------------------------
    // is_border_bigram — positional correctness
    // -------------------------------------------------------------------------

    /// "fn parse": the bigram "fn" starts at position 0 — that IS a token border
    /// (first 2 bytes of the "fn" token).
    #[test]
    fn is_border_bigram_first_token_start_is_border() {
        // Query: "fn parse"  (positions: f=0, n=1, ' '=2, p=3, a=4, r=5, s=6, e=7)
        let ranges = token_border_ranges("fn parse");
        // "fn" token spans [0,2): border = [0,2) only (len==2, first2 == last2).
        // Bigram "fn" starts at pos 0 — should be a border.
        assert!(
            is_border_bigram(0, &ranges),
            "bigram at pos 0 ('fn') must be a border bigram"
        );
    }

    /// Interior bigrams of "function" (8 bytes) are those at positions 2, 3, 4.
    ///
    /// "function": f=0,u=1,n=2,c=3,t=4,i=5,o=6,n=7
    ///
    /// First-2 border covers bigram positions 0 and 1 (bytes 0..2 of the token).
    /// Last-2  border covers bigram positions 5 and 6:
    ///   - pos 6 covers bytes [6,7] — the last-2 bigram itself.
    ///   - pos 5 covers bytes [5,6] — its right byte is byte 6 (inside [6,8)), so it
    ///     overlaps the last-2-byte region and is therefore a border bigram.
    ///
    /// Truly interior positions with no overlap with either border: 2, 3, 4.
    #[test]
    fn is_border_bigram_interior_is_not_border() {
        // Query: "function"  (single 8-byte token)
        let ranges = token_border_ranges("function");
        for interior_pos in 2..=4_usize {
            assert!(
                !is_border_bigram(interior_pos, &ranges),
                "bigram at interior pos {interior_pos} must NOT be a border bigram"
            );
        }
    }

    /// Last-two-bytes border: in "function" the bigrams at positions 6 and 7
    /// overlap the last 2 bytes and must be flagged as border.
    #[test]
    fn is_border_bigram_last_two_bytes_is_border() {
        // "function" has 8 bytes; last-2 border = [6,8).
        let ranges = token_border_ranges("function");
        // Bigram at pos 6 covers bytes 6-7 — last 2 bytes.
        assert!(
            is_border_bigram(6, &ranges),
            "bigram at pos 6 (last 2 bytes) must be a border bigram"
        );
    }

    /// Single-byte token: bigrams on either side of the lone character are borders.
    #[test]
    fn is_border_bigram_single_byte_token_neighbours_are_borders() {
        // Query: "a b" — "a" is a 1-byte token at pos 0, "b" is a 1-byte token at pos 2.
        // The bigram "a " starts at pos 0 and must be flagged for the "a" token.
        // The bigram " b" starts at pos 1 and must be flagged for the "b" token
        // (the single-byte range for "b" covers pos 1).
        let ranges = token_border_ranges("a b");
        assert!(
            is_border_bigram(0, &ranges),
            "bigram at pos 0 next to single-byte token 'a' must be a border"
        );
        assert!(
            is_border_bigram(1, &ranges),
            "bigram at pos 1 touching single-byte token 'b' must be a border"
        );
    }

    // -------------------------------------------------------------------------
    // covering_set_heuristic
    // -------------------------------------------------------------------------

    #[test]
    fn covering_set_covers_all_positions() {
        let query = "fn parse";
        let w = synthetic_weights();
        let selected = covering_set_heuristic(query, &w);
        let bytes = query.as_bytes();

        // The weight table contains bigrams from "fn parse", so the result must be non-empty.
        assert!(
            !selected.is_empty(),
            "covering set for 'fn parse' must be non-empty given the synthetic weights"
        );

        let mut covered = vec![false; bytes.len()];
        for key in &selected {
            for (pos, window) in bytes.windows(2).enumerate() {
                if encode_bigram(window[0], window[1]) == *key {
                    covered[pos] = true;
                    covered[pos + 1] = true;
                }
            }
        }
        // All positions that have a matching bigram must be covered.
        for (pos, window) in bytes.windows(2).enumerate() {
            let key = encode_bigram(window[0], window[1]);
            if w.binary_search_by_key(&key, |&(k, _)| k).is_ok() {
                assert!(covered[pos], "position {pos} should be covered");
            }
        }
    }

    #[test]
    fn higher_idf_bigrams_preferred() {
        let query = "fn impl";
        let w = synthetic_weights();
        let selected = covering_set_heuristic(query, &w);

        // The result must be non-empty: the query has bigrams in the weight table.
        assert!(
            !selected.is_empty(),
            "covering set for 'fn impl' must be non-empty"
        );

        // "fn" has IDF 8.0 and "im" has IDF 6.0 — both are the highest-scoring bigrams
        // in "fn impl".  The greedy heuristic must include both.
        let fn_key = encode_bigram(b'f', b'n');
        let im_key = encode_bigram(b'i', b'm');
        assert!(
            selected.contains(&fn_key),
            "high-IDF bigram 'fn' (8.0) must be in the selected set"
        );
        assert!(
            selected.contains(&im_key),
            "high-IDF bigram 'im' (6.0) must be in the selected set"
        );
    }

    #[test]
    fn border_selectivity_exceeds_uniform_for_code_queries() {
        let w = synthetic_weights();

        // Use "fn parse" — has tokens "fn" and "parse"
        let query = "fn parse";
        let uniform = uniform_selectivity(query, &w);
        let border = border_weighted_selectivity(query, &w);

        // Border should be >= uniform (multiplier is 1.0 or BORDER_MULTIPLIER)
        assert!(
            border >= uniform,
            "border ({border}) should be >= uniform ({uniform})"
        );
    }

    #[test]
    fn run_validation_returns_nonzero_for_nonempty_table() {
        let w = synthetic_weights();
        let result = run_validation(&w, &["fn parse"]);

        // The query "fn parse" has bigrams from the weight table, so uniform must be > 0.
        assert!(
            result.uniform_selectivity > 0.0,
            "uniform_selectivity must be positive for 'fn parse'"
        );
        // Border weighting can only increase the score, so border must also be > 0.
        assert!(
            result.border_weighted_selectivity > 0.0,
            "border_weighted_selectivity must be positive for 'fn parse'"
        );
        // With BORDER_MULTIPLIER = 3.5 applied to at least some bigrams, border > uniform.
        assert!(
            result.border_weighted_selectivity > result.uniform_selectivity,
            "border ({}) must exceed uniform ({}) for a query with token-boundary bigrams",
            result.border_weighted_selectivity,
            result.uniform_selectivity
        );
    }
}
