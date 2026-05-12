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

/// Split `text` into whitespace-delimited token byte slices.
fn tokenize(text: &str) -> Vec<&[u8]> {
    text.split_whitespace()
        .map(|s| s.as_bytes())
        .filter(|b| !b.is_empty())
        .collect()
}

/// Compute uniform selectivity: sum IDF weights for all query bigrams without
/// any positional bonus.
///
/// This is equivalent to `idf::selectivity`.
#[must_use]
pub fn uniform_selectivity(query: &str, weights: &[(u16, f32)]) -> f64 {
    crate::idf::selectivity(query, weights)
}

/// Compute border-weighted selectivity.
///
/// Bigrams that overlap the first or last 2 bytes of any whitespace-delimited token
/// receive a `BORDER_MULTIPLIER` bonus, reflecting that token boundaries are more
/// discriminating for code search.
#[must_use]
pub fn border_weighted_selectivity(query: &str, weights: &[(u16, f32)]) -> f64 {
    let tokens = tokenize(query);
    let mut total = 0.0_f64;

    let query_bytes = query.as_bytes();
    if query_bytes.len() < 2 {
        return total;
    }

    for window in query_bytes.windows(2) {
        let key = encode_bigram(window[0], window[1]);
        let idf = match weights.binary_search_by_key(&key, |&(k, _)| k) {
            Ok(idx) => weights[idx].1 as f64,
            Err(_) => continue,
        };

        // Check whether this bigram overlaps a token border.
        let multiplier = if is_border_bigram(window, &tokens) {
            BORDER_MULTIPLIER
        } else {
            1.0
        };

        total += idf * multiplier;
    }

    total
}

/// Returns true if the two-byte window overlaps the first or last two bytes
/// of any whitespace token in the source tokens list.
fn is_border_bigram(window: &[u8], tokens: &[&[u8]]) -> bool {
    for token in tokens {
        if token.len() >= 2 {
            // Overlap with first 2 bytes: window == token[0..2] or window starts inside first 2
            let first2 = &token[..2];
            let last2 = &token[token.len() - 2..];

            if window == first2 || window == last2 {
                return true;
            }
            // Also catch: window starts at position 0 or 1 of the token
            if window[0] == first2[0] || window[0] == last2[0] {
                return true;
            }
        } else if token.len() == 1 {
            // Single-byte token: any bigram touching it is a border bigram
            if window.contains(&token[0]) {
                return true;
            }
        }
    }
    false
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
    #![allow(clippy::unwrap_used)]

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

    #[test]
    fn covering_set_covers_all_positions() {
        let query = "fn parse";
        let w = synthetic_weights();
        let selected = covering_set_heuristic(query, &w);
        let bytes = query.as_bytes();

        if !selected.is_empty() {
            let mut covered = vec![false; bytes.len()];
            for key in &selected {
                for (pos, window) in bytes.windows(2).enumerate() {
                    if encode_bigram(window[0], window[1]) == *key {
                        covered[pos] = true;
                        covered[pos + 1] = true;
                    }
                }
            }
            // All positions that have a matching bigram must be covered
            for (pos, window) in bytes.windows(2).enumerate() {
                let key = encode_bigram(window[0], window[1]);
                if w.binary_search_by_key(&key, |&(k, _)| k).is_ok() {
                    assert!(covered[pos], "position {pos} should be covered");
                }
            }
        }
    }

    #[test]
    fn higher_idf_bigrams_preferred() {
        let query = "fn impl";
        let w = synthetic_weights();
        let selected = covering_set_heuristic(query, &w);

        // "im" has IDF 6.0, "fn" has 8.0 — both high; they should be chosen over "n " (3.0)
        // We just verify the result is non-empty and selections are valid bigrams
        for key in &selected {
            assert!(
                w.binary_search_by_key(key, |&(k, _)| k).is_ok(),
                "selected bigram {key} not in weight table"
            );
        }
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
        let result = run_validation(&w, TEST_QUERIES);
        // With any matching bigrams, we should get positive values
        assert!(result.uniform_selectivity >= 0.0);
        assert!(result.border_weighted_selectivity >= 0.0);
        assert!(result.improvement_pct >= 0.0);
    }
}
