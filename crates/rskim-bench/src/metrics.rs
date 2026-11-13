//! Information retrieval metric: Mean Reciprocal Rank.
//!
//! The function is pure: no I/O or search infrastructure dependencies.

/// Compute Mean Reciprocal Rank over a set of queries.
///
/// Each query contributes its reciprocal rank, then the mean is taken.
/// Returns 0.0 for an empty query set.
///
/// # Arguments
/// * `rrs` — one reciprocal rank per query
#[must_use]
pub fn mrr(rrs: &[f64]) -> f64 {
    if rrs.is_empty() {
        return 0.0;
    }
    let result = rrs.iter().sum::<f64>() / rrs.len() as f64;
    debug_assert!(
        result.is_finite(),
        "MRR must be finite; check input rrs for NaN/Inf"
    );
    result
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mrr_single_query_rank_1() {
        assert!((mrr(&[1.0]) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn mrr_multiple_queries() {
        // ranks 1, 2, 3 → rrs 1.0, 0.5, 0.333...
        let rrs = [1.0, 0.5, 1.0 / 3.0];
        let expected = (1.0 + 0.5 + 1.0 / 3.0) / 3.0;
        assert!((mrr(&rrs) - expected).abs() < 1e-9);
    }

    #[test]
    fn mrr_all_zeros() {
        assert!((mrr(&[0.0, 0.0, 0.0]) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn mrr_empty() {
        assert!((mrr(&[]) - 0.0).abs() < f64::EPSILON);
    }
}
