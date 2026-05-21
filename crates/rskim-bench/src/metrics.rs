//! Information retrieval metrics: MRR and Precision@K.
//!
//! All functions are pure — no I/O or search infrastructure dependencies.
//! They operate on ranked lists of `FileId` values and a set of relevant IDs.

use rskim_search::FileId;

/// Compute the Reciprocal Rank of the first relevant result.
///
/// Returns 1/rank where rank is the 1-indexed position of the first relevant
/// result in the ranked list. Returns 0.0 if no relevant document appears.
///
/// # Arguments
/// * `ranked` — ordered list of results (best first)
/// * `relevant_id` — the single relevant document for this query
#[must_use]
pub fn reciprocal_rank(ranked: &[FileId], relevant_id: FileId) -> f64 {
    for (i, &fid) in ranked.iter().enumerate() {
        if fid == relevant_id {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Compute the rank (1-indexed) of the first relevant result.
///
/// Returns 0 if no relevant document appears in the ranked list.
#[must_use]
pub fn rank_of(ranked: &[FileId], relevant_id: FileId) -> usize {
    for (i, &fid) in ranked.iter().enumerate() {
        if fid == relevant_id {
            return i + 1;
        }
    }
    0
}

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
    debug_assert!(result.is_finite(), "MRR must be finite; check input rrs for NaN/Inf");
    result
}

/// Compute Precision@K: fraction of the top-K results that are relevant.
///
/// For single-relevant-document evaluation (one qrel per query), this is
/// 1/K if the relevant doc appears in the top K, else 0.
///
/// Returns 0.0 for an empty ranked list or K=0.
///
/// # Arguments
/// * `ranked` — ordered list of results (best first)
/// * `relevant_id` — the single relevant document for this query
/// * `k` — cutoff rank
#[must_use]
pub fn precision_at_k(ranked: &[FileId], relevant_id: FileId, k: usize) -> f64 {
    if k == 0 || ranked.is_empty() {
        return 0.0;
    }
    let top_k = ranked.iter().take(k);
    let hits = top_k.filter(|&&fid| fid == relevant_id).count();
    hits as f64 / k as f64
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code — unwrap acceptable for test assertions
mod tests {
    use super::*;

    #[test]
    fn reciprocal_rank_first_position() {
        let ranked = [FileId(1), FileId(2), FileId(3)];
        assert!((reciprocal_rank(&ranked, FileId(1)) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reciprocal_rank_second_position() {
        let ranked = [FileId(1), FileId(2), FileId(3)];
        assert!((reciprocal_rank(&ranked, FileId(2)) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn reciprocal_rank_third_position() {
        let ranked = [FileId(1), FileId(2), FileId(3)];
        let rr = reciprocal_rank(&ranked, FileId(3));
        assert!((rr - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn reciprocal_rank_not_found() {
        let ranked = [FileId(1), FileId(2), FileId(3)];
        assert!((reciprocal_rank(&ranked, FileId(99)) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reciprocal_rank_empty_list() {
        assert!((reciprocal_rank(&[], FileId(1)) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rank_of_found() {
        let ranked = [FileId(10), FileId(20), FileId(30)];
        assert_eq!(rank_of(&ranked, FileId(20)), 2);
    }

    #[test]
    fn rank_of_not_found() {
        let ranked = [FileId(10), FileId(20)];
        assert_eq!(rank_of(&ranked, FileId(99)), 0);
    }

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

    #[test]
    fn precision_at_k_found_in_top_k() {
        let ranked = [FileId(1), FileId(2), FileId(3), FileId(4), FileId(5)];
        // relevant is at rank 3, k=5 → precision = 1/5
        assert!((precision_at_k(&ranked, FileId(3), 5) - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn precision_at_k_not_found_in_top_k() {
        let ranked = [FileId(1), FileId(2), FileId(3), FileId(4), FileId(5)];
        // relevant is id=99, not in top 5 → 0
        assert!((precision_at_k(&ranked, FileId(99), 5) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn precision_at_k_zero() {
        let ranked = [FileId(1)];
        assert!((precision_at_k(&ranked, FileId(1), 0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn precision_at_k_k_exceeds_list_len() {
        let ranked = [FileId(1), FileId(2)];
        // k=10 but only 2 results; relevant at rank 1 → 1/10
        assert!((precision_at_k(&ranked, FileId(1), 10) - 0.1).abs() < f64::EPSILON);
    }
}
