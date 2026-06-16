//! Multi-layer result intersection and composite re-ranking.
//!
//! # Algorithm (Wave 4a, #198)
//!
//! 1. **Intersect** lexical and AST results by `FileId` via a linear merge-join.
//!    Both layers emit FileId-ASC (by contract) so the join is O(n+m).
//! 2. **Fuse** via weighted Reciprocal Rank Fusion (RRF):
//!    `score(d) = Σᵢ wᵢ / (RRF_K + rankᵢ(d))`
//!    where `rankᵢ(d)` is d's 1-based position in layer i's DESC-sorted list
//!    and a layer in which d is absent contributes 0 (graceful absence).
//! 3. **Sort** DESC by fused score, then FileId-ASC as a deterministic tiebreaker.
//!
//! # Design invariants (cross-ticket, shared with #200)
//!
//! * `RRF_K ≈ 60` — the IR-standard damping constant (Cormack et al., SIGIR 2009).
//! * Equal-weight default — both layers contribute identically.
//! * Scale-free — only rank, never raw score magnitude, drives fusion.
//! * NaN-safe — denominator `RRF_K + rank` is always strictly positive.
//! * Deterministic tiebreaker — FileId-ASC for equal composite scores.
//! * Pure / I/O-free — all lookups are injected via `impl Fn` closures; no
//!   reader, DB, or filesystem access inside this module.
//!
//! # Extension points for #200
//!
//! `CompositeWeights` holds the per-layer weights so #200 can add temporal and
//! graph signal ranked lists into the same RRF without changing the fusion
//! kernel.  The equal-weight two-layer default defined here is the seed that
//! #200 extends additively.
//!
//! # References
//!
//! G. V. Cormack, C. L. A. Clarke, and S. Buettcher. Reciprocal rank fusion
//! outperforms condorcet and individual rank learning methods. *Proc. SIGIR
//! 2009*, pp. 758–759.  `RRF_K = 60` is the constant from that paper.

use std::collections::HashMap;

use crate::ast_index::StructuralMetrics;
use crate::types::{FileId, SearchResult};

// ============================================================================
// Constants
// ============================================================================

/// RRF damping constant.
///
/// `k ≈ 60` is the value from the original Cormack et al. SIGIR 2009 paper.
/// It prevents high-ranked results from dominating by capping the maximum
/// contribution of rank-1 to `w / (60 + 1)`.  Do NOT change this without
/// a measured lift on a real corpus.
pub const RRF_K: f64 = 60.0;

/// Weight for the lexical (BM25F) layer in the two-signal RRF blend.
///
/// Equal-weight default; #200 extends `CompositeWeights` with additional
/// signals.  Must be non-negative.
pub const WEIGHT_LEXICAL: f64 = 1.0;

/// Weight for the AST structural layer in the two-signal RRF blend.
///
/// Equal-weight default; see [`WEIGHT_LEXICAL`].
pub const WEIGHT_AST: f64 = 1.0;

// ============================================================================
// Weight container (extensible by #200)
// ============================================================================

/// Per-signal fusion weights for weighted RRF.
///
/// #198 defines the two-signal (lexical + AST) blend.  #200 extends this
/// struct additively to N signals (temporal, graph, ...).  Keep the equal-
/// weight default as the baseline; document each weight with its signal's
/// identity and the rationale for its value.
#[derive(Debug, Clone, Copy)]
pub struct CompositeWeights {
    /// Weight for the lexical (BM25F) ranked list.
    pub lexical: f64,
    /// Weight for the AST structural ranked list.
    pub ast: f64,
}

impl Default for CompositeWeights {
    fn default() -> Self {
        Self {
            lexical: WEIGHT_LEXICAL,
            ast: WEIGHT_AST,
        }
    }
}

// ============================================================================
// Core intersection + RRF fusion
// ============================================================================

/// Intersect and composite-rank lexical and AST search results.
///
/// # Inputs
///
/// * `lexical_scored` — results from the lexical layer, **sorted DESC by
///   score** (higher = better). FileIds need not be unique or sorted by FileId.
/// * `ast_scored` — results from the AST layer, **sorted ASC by FileId**,
///   unique FileIds, all scores > 0 (the frozen Wave-4 contract from #287).
/// * `structural_lookup` — pure closure: `FileId → Option<StructuralMetrics>`.
///   Used to refine the AST-layer rank by `max_depth` (depth-only in 4a;
///   richer metrics are available in v2 but grounded baselines for branch_count
///   etc. are deferred per ADR-003/ADR-004).  The closure must perform **no
///   I/O** — callers pre-fetch metrics before calling this function.
/// * `avg_max_depth` — corpus average max CST depth, from
///   `AstIndexReader::avg_max_depth()`.  Used as the ordering key baseline so
///   the structural refinement is grounded (avoids ADR-003 baseless magic).
/// * `weights` — per-signal RRF weights; use [`CompositeWeights::default()`]
///   for the equal-weight blend.
///
/// # Invariants enforced
///
/// * Only files present in **both** layers are returned (intersection gate).
/// * Empty intersection → `Ok(vec![])` (not an error).
/// * `u16` structural metrics are widened via `u32::from` / `f64::from` before
///   any arithmetic (avoids PF-004 overflow).
/// * The fused score denominator `RRF_K + rank` is always positive → NaN-safe.
/// * Equal composite scores are broken by FileId-ASC (deterministic, AC10).
/// * Stale FileId skew (FileId in AST not in lexical manifest) → silent drop,
///   consistent with `resolve_paths_and_snippets` pattern.
///
/// # Returns
///
/// `Vec<(FileId, f64)>` sorted DESC by composite score, then FileId-ASC.
/// The caller is responsible for resolving FileIds to paths and extracting
/// snippets from the lexical `SearchResult`s (AC11: carry the lexical result,
/// replace `.score` with the composite score).
///
/// # Doc: limit semantics
///
/// This function receives the **full** un-pre-limited candidate sets from both
/// layers. Truncation to `--limit` happens in the caller after composite ranking
/// (rank-then-truncate-LAST invariant; see Amendment in plan).
#[must_use]
pub fn intersect_and_rank(
    lexical_scored: &[SearchResult],
    ast_scored: &[(FileId, f64)],
    structural_lookup: impl Fn(FileId) -> Option<StructuralMetrics>,
    avg_max_depth: f32,
    weights: CompositeWeights,
) -> Vec<(FileId, f64)> {
    if lexical_scored.is_empty() || ast_scored.is_empty() {
        return vec![];
    }

    // --- Step 1: build rank maps from each layer ---

    // Lexical rank map: FileId → 1-based rank position in DESC-score order.
    // The lexical layer arrives pre-sorted DESC; index + 1 = rank.
    let lexical_rank: HashMap<FileId, usize> = lexical_scored
        .iter()
        .enumerate()
        .map(|(i, r)| (r.file_id, i + 1))
        .collect();

    // AST layer with optional structural refinement.
    // The AST scored list arrives FileId-ASC; we need to build a DESC-score
    // ordering (rank 1 = most structurally complex) to feed RRF.
    //
    // Structural refinement (4a, depth-only per ADR-003/ADR-004):
    // Sort the AST results DESC by (depth_ordering_key, ast_score) where
    //   depth_ordering_key = max_depth / (1 + avg_max_depth)
    // using a grounded baseline (`avg_max_depth` from the stored v2 header).
    // Because RRF consumes rank, not magnitude, no corpus divisor is strictly
    // needed — we only need a stable relative ordering.  Using the normalised
    // depth as the primary sort key and ast_score as tiebreaker achieves this
    // without arbitrary thresholds (ADR-003).
    // PF-004: widen u16→u32→f64 BEFORE any arithmetic.
    let avg_depth_f64 = f64::from(avg_max_depth);
    let mut ast_ranked: Vec<(FileId, f64)> = ast_scored
        .iter()
        .map(|&(fid, score)| {
            let ordering_key = if let Some(m) = structural_lookup(fid) {
                // avoids PF-004: u32::from(max_depth) before f64::from
                let depth = f64::from(u32::from(m.max_depth));
                // Normalise by (1 + avg_depth) — strictly positive denominator.
                depth / (1.0 + avg_depth_f64)
            } else {
                0.0
            };
            // Pack (ordering_key, ast_score) as a f64 for sort: use ordering_key
            // as primary and ast_score as tiebreaker.
            // We combine by sorting on (ordering_key, score) tuple below.
            let _ = ordering_key; // used in the sort closure below
            (fid, score)
        })
        .collect();

    // Sort AST candidates DESC by (structural depth key, ast_score) for rank assignment.
    ast_ranked.sort_unstable_by(|&(a_fid, a_score), &(b_fid, b_score)| {
        let a_depth = structural_lookup(a_fid)
            .map(|m| f64::from(u32::from(m.max_depth)))
            .unwrap_or(0.0);
        let b_depth = structural_lookup(b_fid)
            .map(|m| f64::from(u32::from(m.max_depth)))
            .unwrap_or(0.0);
        let a_key = a_depth / (1.0 + avg_depth_f64);
        let b_key = b_depth / (1.0 + avg_depth_f64);
        // Descending: higher key = lower rank number = better position.
        b_key
            .partial_cmp(&a_key)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                b_score
                    .partial_cmp(&a_score)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            // Tiebreaker: FileId-ASC for determinism.
            .then(a_fid.0.cmp(&b_fid.0))
    });

    // AST rank map: FileId → 1-based rank (after structural refinement).
    let ast_rank: HashMap<FileId, usize> = ast_ranked
        .iter()
        .enumerate()
        .map(|(i, &(fid, _))| (fid, i + 1))
        .collect();

    // --- Step 2: linear merge-join on FileId-ASC to build intersection ---
    //
    // Both `ast_scored` arrives FileId-ASC (frozen contract); lexical results
    // need to be sorted by FileId for the merge-join step.
    // We already have lexical_rank as a HashMap, so we use that for O(1) lookup
    // and collect the AST FileIds that appear in lexical_rank.

    let mut candidates: Vec<(FileId, f64)> = ast_scored
        .iter()
        .filter_map(|&(fid, _)| {
            // Only files in BOTH layers (intersection gate).
            let rank_lex = *lexical_rank.get(&fid)?;
            let rank_ast = *ast_rank.get(&fid)?;
            // Weighted RRF: Σᵢ wᵢ / (RRF_K + rankᵢ)
            // Denominator is always positive (RRF_K = 60, rank ≥ 1) → NaN-safe.
            let score = weights.lexical / (RRF_K + rank_lex as f64)
                + weights.ast / (RRF_K + rank_ast as f64);
            Some((fid, score))
        })
        .collect();

    // --- Step 3: sort DESC by composite score, FileId-ASC tiebreaker (AC10) ---
    candidates.sort_unstable_by(|&(a_fid, a_score), &(b_fid, b_score)| {
        b_score.total_cmp(&a_score).then(a_fid.0.cmp(&b_fid.0))
    });

    candidates
}

// ============================================================================
// Snippet-preserving result recomposition (AC11)
// ============================================================================

/// Recompose lexical `SearchResult`s with composite RRF scores.
///
/// For files in the intersection, carries the lexical `SearchResult` (with its
/// snippet and line_range) and replaces `.score` with the composite RRF score.
/// This preserves snippet + line-number data from the lexical layer (AC11).
///
/// Results are returned in the order defined by `ranked` (DESC by composite
/// score, FileId-ASC tiebreaker from [`intersect_and_rank`]).
///
/// Stale FileId skew (FileId in `ranked` not found in `lexical_scored`) →
/// silent drop, consistent with `resolve_paths_and_snippets`.
#[must_use]
pub fn recompose_with_lexical(
    ranked: &[(FileId, f64)],
    lexical_scored: &[SearchResult],
) -> Vec<SearchResult> {
    // Build a map FileId → &SearchResult for O(1) lookup.
    let lex_map: HashMap<FileId, &SearchResult> =
        lexical_scored.iter().map(|r| (r.file_id, r)).collect();

    ranked
        .iter()
        .filter_map(|&(fid, composite_score)| {
            let lex = lex_map.get(&fid)?;
            let mut result = (*lex).clone();
            result.score = composite_score;
            Some(result)
        })
        .collect()
}

// ============================================================================
// Tests (co-located in intersection_tests.rs, per repo convention)
// ============================================================================

#[cfg(test)]
#[path = "intersection_tests.rs"]
mod tests;
