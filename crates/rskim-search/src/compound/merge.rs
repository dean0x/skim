//! N-signal weighted Reciprocal Rank Fusion over the UNION of file sets (#200).
//!
//! # Algorithm
//!
//! For each candidate file `d` across the **UNION** of all input-layer FileIds:
//!
//! ```text
//! score(d) = Σᵢ wᵢ / (RRF_K + rankᵢ(d))
//! ```
//!
//! where:
//! - `rankᵢ(d)` is `d`'s 1-based position in layer `i`'s DESC-score ordering.
//! - A layer in which `d` is **absent** contributes `0` (graceful absence).
//!
//! This is a direct generalisation of the 2-signal `intersect_and_rank` in
//! `compound::intersection`: that function fuses lexical+AST over their
//! **intersection**; this function fuses N signals over their **UNION**.
//! The UNION mode enables co-change-only files (present in the temporal ranked
//! list but absent from the lexical list) to appear in blast-radius results.
//!
//! # Design invariants
//!
//! - **Scale-free**: only ranks, never raw scores, drive fusion.
//! - **NaN-safe**: the denominator `RRF_K + rank` is always strictly positive
//!   (`RRF_K = 60`, `rank ≥ 1`) so division never yields NaN or ±inf.
//! - **Deterministic**: the output is sorted DESC by fused score, then
//!   `FileId`-ASC as a total comparator for equal scores.  `total_cmp` is used
//!   throughout so NaN (which cannot occur in the RRF denominator) is handled
//!   safely as defence in depth.
//! - **UNION semantics**: a FileId present in only one layer is INCLUDED in the
//!   output, ranked by its single rank term (graceful-absence).
//! - **No HashMap iteration in output**: accumulation uses a `HashMap` but the
//!   output is collected to `Vec` and sorted with a total comparator, so the
//!   output order is deterministic even though `HashMap` iteration is not.
//! - **Empty-layer safety**: a layer with zero entries contributes 0 to every
//!   file's score; the weight is NOT redistributed.
//!
//! # References
//!
//! G. V. Cormack, C. L. A. Clarke, and S. Buettcher.  Reciprocal rank fusion
//! outperforms condorcet and individual rank learning methods.  *Proc. SIGIR
//! 2009*, pp. 758–759.  `RRF_K = 60` is the constant from that paper.

use std::collections::HashMap;

use crate::types::FileId;

use super::intersection::RRF_K;
use super::weights::CompositeWeights6;

// ============================================================================
// Core fusion function
// ============================================================================

/// Fuse N ranked signal layers into one composite ranking over the **UNION** of
/// all FileIds via weighted Reciprocal Rank Fusion.
///
/// # Inputs
///
/// * `layers` — ordered slice of `(signal_scores, weight)` pairs.  Each
///   `signal_scores` is a `Vec<(FileId, f64)>` where higher scores are better.
///   The weight is the corresponding `wᵢ` coefficient.
///
///   Layers MAY contain duplicate FileIds (within a single layer); the first
///   occurrence (highest score in a DESC-sorted layer) determines rank.
///   The function assigns 1-based ranks after sorting each layer DESC by score.
///
/// * Layers with zero entries are silently skipped (contribute 0 to every
///   file's score; weight NOT redistributed).
///
/// # Output
///
/// `Vec<(FileId, f64)>` sorted DESC by composite score, then FileId-ASC for
/// equal-score tiebreaking (deterministic, AC3).  Finite scores guaranteed
/// (AC4).  Every FileId that appears in at least one layer is present in the
/// output (UNION semantics, AC4).
///
/// Returns an empty `Vec` when all layers are empty.
///
/// # Performance
///
/// - O(Σ nᵢ log nᵢ) for the per-layer rank sorts (nᵢ = entries in layer i).
/// - O(N_union log N_union) for the final output sort (N_union = union size).
/// - No per-pair allocation — the HashMap accumulates scores in-place.
/// - Bounded by `MAX_FILES_FOR_EVALUATION` on the benchmark side; no explicit
///   cap inside this pure function (callers are responsible for bounding
///   layer sizes).
#[must_use]
pub fn merge_layer_scores(layers: &[(Vec<(FileId, f64)>, f64)]) -> Vec<(FileId, f64)> {
    // Accumulate fused scores keyed by FileId.
    // Using HashMap for O(1) per-file accumulation; iteration order is NOT
    // used for output (we sort explicitly before returning).
    let mut scores: HashMap<FileId, f64> = HashMap::new();

    for (layer, weight) in layers {
        if layer.is_empty() || *weight == 0.0 {
            // Empty layer or zero-weight signal: contributes nothing.
            // Weight is NOT redistributed to remaining signals — the graceful
            // absence that enables UNION-mode co-change-only files to score.
            continue;
        }

        // Sort this layer DESC by score to derive 1-based ranks.
        // We sort a local owned clone so we don't mutate the caller's data.
        // For large layers the sort dominates; for small layers the alloc
        // dominates — both are bounded by the caller's layer sizes.
        let mut sorted_layer: Vec<(FileId, f64)> = layer.clone();
        sorted_layer.sort_unstable_by(|&(_, a), &(_, b)| {
            // DESC by score; NaN goes last (defence in depth — callers should
            // not produce NaN raw scores, but total_cmp handles it safely).
            b.total_cmp(&a)
        });

        // Assign 1-based ranks and accumulate RRF contributions.
        // For duplicate FileIds within the same layer, the first occurrence
        // (highest score after DESC sort) wins — track via a per-layer seen set.
        let mut seen_in_layer: std::collections::HashSet<FileId> = std::collections::HashSet::new();
        for (i, &(fid, _)) in sorted_layer.iter().enumerate() {
            // Skip non-first occurrences within this layer.
            if !seen_in_layer.insert(fid) {
                continue;
            }
            // Accumulate weighted RRF term: wᵢ / (RRF_K + rankᵢ(d)).
            // rank = i+1 (1-based); RRF_K = 60 → denominator ≥ 61 → never NaN.
            let contribution = weight / (RRF_K + (i + 1) as f64);
            *scores.entry(fid).or_insert(0.0) += contribution;
        }
    }

    // Collect to Vec and sort with a total comparator for determinism (AC3).
    let mut result: Vec<(FileId, f64)> = scores.into_iter().collect();
    result.sort_unstable_by(|&(a_fid, a_score), &(b_fid, b_score)| {
        // DESC by fused score, then ASC by FileId as deterministic tiebreaker.
        b_score
            .total_cmp(&a_score)
            .then_with(|| a_fid.0.cmp(&b_fid.0))
    });

    result
}

/// Convenience wrapper: fuse the standard 6-signal profile from
/// [`CompositeWeights6`].
///
/// The caller supplies one `Vec<(FileId, f64)>` per signal in the order:
/// `[lexical, ast, temporal, import_graph, dir_proximity, structural_coupling]`.
///
/// Signals with weight 0.0 (the default for extended signals) are included
/// in the layer slice but contribute nothing to the fused score (graceful
/// absence preserves UNION semantics for non-zero layers).
///
/// # Returns
///
/// See [`merge_layer_scores`] — same guarantees.
#[must_use]
pub fn merge_composite(
    lexical: Vec<(FileId, f64)>,
    ast: Vec<(FileId, f64)>,
    temporal: Vec<(FileId, f64)>,
    import_graph: Vec<(FileId, f64)>,
    dir_proximity: Vec<(FileId, f64)>,
    structural_coupling: Vec<(FileId, f64)>,
    weights: CompositeWeights6,
) -> Vec<(FileId, f64)> {
    let layers = [
        (lexical, weights.lexical),
        (ast, weights.ast),
        (temporal, weights.temporal),
        (import_graph, weights.import_graph),
        (dir_proximity, weights.dir_proximity),
        (structural_coupling, weights.structural_coupling),
    ];
    merge_layer_scores(&layers)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
