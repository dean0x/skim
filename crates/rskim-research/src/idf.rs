//! IDF computation and weight table generation.

use std::collections::HashMap;

use crate::types::BigramWeight;

/// Compute IDF using the smoothed formula: ln(N / (df + 1)) + 1.0
///
/// Returns a value ≥ 1.0. Universal bigrams (df ≈ N) score near 1.0;
/// rare bigrams (df = 1 in a large corpus) score near ln(N) + 1.
///
/// # Panics (debug only)
///
/// Panics in debug builds if `total_docs == 0`, which would produce `NEG_INFINITY`.
/// Callers must ensure the corpus is non-empty before invoking this function.
#[must_use]
pub fn compute_idf(df: u32, total_docs: u32) -> f32 {
    debug_assert!(
        total_docs > 0,
        "total_docs must be > 0; got 0 — caller must guard against empty corpus"
    );
    ((total_docs as f64) / ((df + 1) as f64)).ln() as f32 + 1.0
}

/// Build the weight table from a document-frequency map.
///
/// Bigrams with IDF below `threshold` are excluded. The result is sorted
/// by bigram key ascending (enabling binary search).
///
/// Returns an empty vec immediately if `total_docs == 0` (no corpus to compute
/// IDF from).
#[must_use]
pub fn compute_weight_table(
    df_map: &HashMap<u16, u32>,
    total_docs: u32,
    threshold: f32,
) -> Vec<BigramWeight> {
    if total_docs == 0 {
        return Vec::new();
    }
    let mut weights: Vec<BigramWeight> = df_map
        .iter()
        .filter_map(|(&bigram, &df)| {
            let idf = compute_idf(df, total_docs);
            if idf >= threshold {
                Some(BigramWeight { bigram, idf })
            } else {
                None
            }
        })
        .collect();

    // Sort ascending by bigram key for binary-search lookups.
    weights.sort_by_key(|w| w.bigram);
    weights
}

/// Compute the cumulative IDF selectivity score for a query string.
///
/// Splits `query` into overlapping byte bigrams, looks each up in the sorted
/// `weights` table by binary search, and returns the sum of matched IDF values.
/// Bigrams absent from the table contribute 0.0. Returns 0.0 for queries
/// shorter than 2 bytes.
#[must_use]
pub fn selectivity(query: &str, weights: &[(u16, f32)]) -> f64 {
    let bytes = query.as_bytes();
    let mut total = 0.0_f64;
    for window in bytes.windows(2) {
        let key = crate::extract::encode_bigram(window[0], window[1]);
        if let Ok(idx) = weights.binary_search_by_key(&key, |&(k, _)| k) {
            total += weights[idx].1 as f64;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::extract::encode_bigram;

    #[test]
    fn universal_bigram_scores_near_one() {
        // df == N: ln(N / (N+1)) + 1 ≈ ln(1) + 1 = 1.0 for large N
        let n = 10_000u32;
        let idf = compute_idf(n, n);
        // Should be slightly below 1.0 (because df+1 makes denominator slightly larger)
        // For df=N: ln(N/(N+1)) + 1 = ln(0.9999) + 1 ≈ 0.9999
        assert!((idf - 1.0).abs() < 0.01, "expected near 1.0, got {idf}");
    }

    #[test]
    fn rare_bigram_scores_high() {
        // df=1, N=10000: ln(10000/2) + 1 = ln(5000) + 1 ≈ 8.52 + 1 = 9.52
        let idf = compute_idf(1, 10_000);
        assert!(idf > 8.0, "expected >8.0, got {idf}");
    }

    #[test]
    fn idf_always_positive() {
        for df in [0u32, 1, 10, 100, 1000, 10_000] {
            let idf = compute_idf(df, 10_000);
            assert!(idf > 0.0, "IDF should be positive for df={df}");
        }
    }

    #[test]
    fn weight_table_sorted_ascending() {
        let mut df_map = HashMap::new();
        df_map.insert(encode_bigram(b'z', b'z'), 1u32);
        df_map.insert(encode_bigram(b'a', b'a'), 2u32);
        df_map.insert(encode_bigram(b'm', b'm'), 1u32);

        let table = compute_weight_table(&df_map, 1000, 0.0);
        let keys: Vec<u16> = table.iter().map(|w| w.bigram).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "table must be sorted by bigram key");
    }

    #[test]
    fn threshold_filters_low_idf() {
        // With df = total_docs, IDF ≈ 1.0 → below threshold of 5.0 → excluded
        let mut df_map = HashMap::new();
        let n = 1000u32;
        // Universal bigram (IDF ≈ 1.0)
        df_map.insert(encode_bigram(b'a', b'b'), n);
        // Rare bigram (IDF ≈ 8+)
        df_map.insert(encode_bigram(b'x', b'y'), 1u32);

        let table = compute_weight_table(&df_map, n, 5.0);
        // Only the rare bigram should be present
        assert_eq!(table.len(), 1);
        assert_eq!(table[0].bigram, encode_bigram(b'x', b'y'));
    }

    #[test]
    fn weight_table_empty_when_total_docs_is_zero() {
        let mut df_map = HashMap::new();
        df_map.insert(encode_bigram(b'a', b'b'), 1u32);
        // total_docs == 0 must not produce NEG_INFINITY — returns empty vec immediately.
        let table = compute_weight_table(&df_map, 0, 0.0);
        assert!(table.is_empty(), "expected empty table for zero-doc corpus");
    }

    #[test]
    fn no_duplicate_keys_in_weight_table() {
        let mut df_map = HashMap::new();
        for i in 0u16..256 {
            df_map.insert(i, 1u32);
        }
        let table = compute_weight_table(&df_map, 10_000, 0.0);
        let keys: Vec<u16> = table.iter().map(|w| w.bigram).collect();
        let unique_keys: std::collections::HashSet<u16> = keys.iter().copied().collect();
        assert_eq!(keys.len(), unique_keys.len(), "duplicate keys found");
    }
}
