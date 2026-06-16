//! Behavior tests for `compound::intersection` (Wave 4a, #198).
//!
//! Every test asserts observable behaviour — not just exit 0 (avoids PF-007).

#![allow(clippy::unwrap_used)]

use std::ops::Range;

use crate::ast_index::StructuralMetrics;
use crate::compound::intersection::{
    CompositeWeights, RRF_K, WEIGHT_AST, WEIGHT_LEXICAL, intersect_and_rank, recompose_with_lexical,
};
use crate::types::{FileId, SearchField, SearchResult};

// ============================================================================
// Helpers
// ============================================================================

/// Construct a minimal `SearchResult` for testing.
fn make_lex_result(file_id: u32, score: f64) -> SearchResult {
    SearchResult {
        file_id: FileId(file_id),
        score,
        line_range: Range { start: 1, end: 3 },
        match_positions: vec![Range { start: 0, end: 4 }],
        field: SearchField::FunctionSignature,
        snippet: Some(format!("snippet for file {file_id}")),
    }
}

/// Minimal structural metrics — all zeroes except `max_depth`.
fn metrics_with_depth(max_depth: u16) -> StructuralMetrics {
    StructuralMetrics {
        max_depth,
        max_block_stmts: 0,
        max_params: 0,
        branch_count: 0,
    }
}

/// No-op structural lookup (returns None for every FileId).
fn no_metrics(_fid: FileId) -> Option<StructuralMetrics> {
    None
}

// ============================================================================
// AC7 — pure / I/O-free compound signature
// ============================================================================

#[test]
fn ac7_compound_fn_is_pure_no_io() {
    // Construct inputs entirely from literals and closures — no reader, no DB,
    // no filesystem handle.  The fact this compiles and runs without I/O types
    // proves the purity contract (AC7).
    let lexical = vec![make_lex_result(1, 5.0), make_lex_result(2, 3.0)];
    let ast = vec![(FileId(1), 2.0_f64), (FileId(2), 1.5_f64)];

    let structural_lookup = |fid: FileId| {
        if fid.0 == 1 {
            Some(metrics_with_depth(10))
        } else {
            Some(metrics_with_depth(5))
        }
    };

    let ranked = intersect_and_rank(
        &lexical,
        &ast,
        structural_lookup,
        7.5_f32,
        CompositeWeights::default(),
    );

    assert!(
        !ranked.is_empty(),
        "AC7: pure fn must return results for overlapping inputs"
    );
    // Verify scores are finite (NaN-safe, AC9).
    for (_, score) in &ranked {
        assert!(score.is_finite(), "AC7: score must be finite, got {score}");
        assert!(!score.is_nan(), "AC7: score must not be NaN, got {score}");
    }
}

// ============================================================================
// AC1 — intersection is a strict subset gate
// ============================================================================

#[test]
fn ac1_intersection_strict_subset_gate() {
    // Lexical matches files {1,2,3,4,5}; AST matches {2,4,6}.
    // Result must be exactly {2,4} — not {1,2,3,4,5}, not {2,4,6}.
    let lexical = vec![
        make_lex_result(1, 10.0),
        make_lex_result(2, 8.0),
        make_lex_result(3, 6.0),
        make_lex_result(4, 4.0),
        make_lex_result(5, 2.0),
    ];
    // AST arrives FileId-ASC (contract).
    let ast = vec![(FileId(2), 3.0), (FileId(4), 2.0), (FileId(6), 1.0)];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    let fids: Vec<u32> = ranked.iter().map(|&(f, _)| f.0).collect();

    // Must contain exactly {2, 4}.
    assert!(
        fids.contains(&2),
        "AC1: file 2 (in both) must be present, got {fids:?}"
    );
    assert!(
        fids.contains(&4),
        "AC1: file 4 (in both) must be present, got {fids:?}"
    );
    // Must NOT contain files only in lexical.
    assert!(
        !fids.contains(&1),
        "AC1: file 1 (lexical-only) must be absent, got {fids:?}"
    );
    assert!(
        !fids.contains(&3),
        "AC1: file 3 (lexical-only) must be absent, got {fids:?}"
    );
    assert!(
        !fids.contains(&5),
        "AC1: file 5 (lexical-only) must be absent, got {fids:?}"
    );
    // Must NOT contain files only in AST.
    assert!(
        !fids.contains(&6),
        "AC1: file 6 (AST-only) must be absent, got {fids:?}"
    );
    // Strict subset: result set is smaller than lexical set.
    assert_eq!(
        fids.len(),
        2,
        "AC1: result must have exactly 2 files (strict subset), got {fids:?}"
    );
}

// ============================================================================
// AC2 — composite fusion actually reorders vs pure-lexical
// ============================================================================

/// Verify that the AST signal reaches composite scoring and can invert lexical order.
///
/// # Weight note
///
/// This test uses asymmetric weights (ast: 10.0 / lexical: 1.0) rather than
/// the production equal-weight default (`CompositeWeights::default()`).  With
/// exactly 2 intersecting files and symmetric complementary ranks (f1: lex-1/ast-2,
/// f2: lex-2/ast-1), equal weights yield identical RRF scores — impossible to
/// assert a deterministic ordering difference without the FileId-ASC tiebreaker.
/// Asymmetric weights make the AST signal decisive and the assertion unambiguous.
///
/// The equal-weight production path is covered by `ac3d_equal_weight_production_path_ast_signal_wired`,
/// which uses 3-file inputs where the ranks are NOT symmetric and the ordering
/// difference is provable under equal weights.
#[test]
fn ac2_composite_fusion_reorders_vs_lexical() {
    // lexical: 3 files — f1 (rank 1, score 10), f2 (rank 2, score 5), f3 (rank 3, score 1).
    // AST: 2 files — f2 (rank 1, score 9), f1 (rank 2, score 1).
    // (f3 is NOT in AST → intersection is {f1, f2})
    //
    // RRF scores with equal weights w=1, k=60:
    //   f1: lex_rank=1, ast_rank=2  → 1/(60+1) + 1/(60+2) = 1/61 + 1/62 ≈ 0.02780
    //   f2: lex_rank=2, ast_rank=1  → 1/(60+2) + 1/(60+1) = 1/62 + 1/61 ≈ 0.02780
    //
    // Note: with exactly 2 intersecting files, symmetric complementary ranks
    // yield identical RRF scores (rank-1 in one layer + rank-2 in the other).
    // To produce a provable ordering difference, we use ASYMMETRIC weights:
    // heavier AST weight means f2 (AST rank 1) beats f1 (AST rank 2).
    let lexical = vec![
        make_lex_result(1, 10.0), // lex rank 1
        make_lex_result(2, 5.0),  // lex rank 2
        make_lex_result(3, 1.0),  // lex rank 3 — NOT in AST
    ];
    let ast = vec![
        (FileId(1), 1.0), // FileId-ASC; AST rank 2 (lower score)
        (FileId(2), 9.0), // AST rank 1 (higher score)
    ];

    // Use asymmetric weights: AST weight >> lexical weight so f2 (AST rank 1) beats f1.
    let weights = CompositeWeights {
        lexical: 1.0,
        ast: 10.0,
    };
    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, weights);

    assert_eq!(ranked.len(), 2, "AC2: both intersecting files must appear");
    // With heavily-weighted AST: f2 (AST rank 1) must rank above f1 (AST rank 2).
    // f2: 1/(61) + 10/(61) = 11/61 ≈ 0.1803
    // f1: 1/(61) + 10/(62) = 1/61 + 10/62 ≈ 0.0164 + 0.1613 = 0.1777
    // f2 > f1 ✓
    assert_eq!(
        ranked[0].0,
        FileId(2),
        "AC2: f2 must rank first (strong AST rank, high AST weight), got {:?}",
        ranked
    );
    assert_eq!(ranked[1].0, FileId(1), "AC2: f1 must rank second");
    // Also verify the pure-lexical ordering was inverted: f1 ranked first in lexical
    // but f2 ranks first in composite. This proves the AST signal reaches scoring.
    assert!(
        ranked[0].1 > ranked[1].1,
        "AC2: composite scores must be strictly ordered (f2 > f1), got {:?}",
        ranked
    );
}

// ============================================================================
// AC3 — single-layer pass-through
// ============================================================================

/// AC3a: When only one layer is non-empty but the other is empty, the
/// intersection is empty (there are no files common to both layers).
/// Renamed from the incorrectly-labeled `ac3a_empty_ast_returns_empty` —
/// that test was covering the empty-input case (AC6 territory), not AC3.
#[test]
fn ac3a_empty_layer_yields_empty_intersection() {
    // Empty AST input → empty intersection (no files in both layers).
    let lexical = vec![make_lex_result(1, 5.0), make_lex_result(2, 3.0)];
    let ast: Vec<(FileId, f64)> = vec![];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    assert!(
        ranked.is_empty(),
        "AC3a: empty AST must yield empty intersection, got {ranked:?}"
    );
}

/// AC3b: Empty lexical input → empty intersection regardless of AST content.
#[test]
fn ac3b_empty_lexical_yields_empty_intersection() {
    // Empty lexical input → empty intersection.
    let lexical: Vec<SearchResult> = vec![];
    let ast = vec![(FileId(1), 2.0)];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    assert!(
        ranked.is_empty(),
        "AC3b: empty lexical must yield empty intersection, got {ranked:?}"
    );
}

/// AC3c: Pure-lexical pass-through — all lexical files appear in the intersection
/// when the AST set is a superset of the lexical set (no file is dropped).
/// This is AC3 proper: with the right inputs, the intersection does not shrink
/// the lexical set, proving no spurious gating.
#[test]
fn ac3c_all_lexical_files_pass_when_ast_is_superset() {
    // Lexical has {1, 2}; AST covers {1, 2, 3} (superset).
    // Intersection must be exactly {1, 2} — no files are dropped by the gate.
    let lexical = vec![make_lex_result(1, 5.0), make_lex_result(2, 3.0)];
    let ast = vec![(FileId(1), 2.0), (FileId(2), 1.5), (FileId(3), 1.0)];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    let fids: Vec<u32> = ranked.iter().map(|&(f, _)| f.0).collect();
    assert_eq!(
        ranked.len(),
        2,
        "AC3c: intersection must preserve all lexical files when AST is a superset, got {fids:?}"
    );
    assert!(fids.contains(&1), "AC3c: file 1 must be present");
    assert!(fids.contains(&2), "AC3c: file 2 must be present");
    assert!(!fids.contains(&3), "AC3c: AST-only file 3 must be absent");
}

/// AC3d: AST-only pass-through — with equal-weight default config, the composite
/// ordering is stable and both files appear.  Verifies that the AST signal reaches
/// the scoring layer (not just gating) even with equal weights.
///
/// This is the production path: `CompositeWeights::default()` (equal weights).
/// Two files with complementary rank positions produce *different* composite scores
/// only if one layer has more than 2 items — with exactly 2 intersecting files and
/// symmetric ranks the scores are equal and the tiebreaker (FileId-ASC) determines
/// order.  The test proves the AST signal is wired into the fusion path at all.
#[test]
fn ac3d_equal_weight_production_path_ast_signal_wired() {
    // 3 lexical results; AST has only files {2, 3} — so file 1 is dropped by the gate.
    // With equal weights:
    //   f2: lex_rank=2, ast_rank=1 (AST score 9.0) → 1/62 + 1/61 ≈ 0.02780
    //   f3: lex_rank=3, ast_rank=2 (AST score 3.0) → 1/63 + 1/62 ≈ 0.02751
    // f2 > f3 (AST signal pushes f2 above its lexical rank).
    let lexical = vec![
        make_lex_result(1, 10.0), // lex rank 1 — NOT in AST
        make_lex_result(2, 5.0),  // lex rank 2
        make_lex_result(3, 2.0),  // lex rank 3
    ];
    let ast = vec![
        (FileId(2), 9.0), // FileId-ASC; AST rank 1
        (FileId(3), 3.0), // AST rank 2
    ];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    assert_eq!(
        ranked.len(),
        2,
        "AC3d: only the 2 files in both layers must appear, got {:?}",
        ranked
    );

    // f1 must be absent (AST gate removes it even under equal weights).
    let fids: Vec<u32> = ranked.iter().map(|&(f, _)| f.0).collect();
    assert!(
        !fids.contains(&1),
        "AC3d: file 1 (lexical-only) must be absent under equal-weight default"
    );

    // f2 must rank above f3: lex_rank(f2)=2 + ast_rank(f2)=1 outscores
    // lex_rank(f3)=3 + ast_rank(f3)=2 — the AST signal contributes to the ordering.
    assert_eq!(
        ranked[0].0,
        FileId(2),
        "AC3d: f2 must rank first (better composite score), got {ranked:?}"
    );
    assert_eq!(ranked[1].0, FileId(3), "AC3d: f3 must rank second");

    // Verify scores are strictly ordered (not equal — the ranks are NOT symmetric).
    assert!(
        ranked[0].1 > ranked[1].1,
        "AC3d: equal-weight composite scores must be strictly ordered (f2 > f3), got {ranked:?}"
    );
}

// ============================================================================
// AC6 — empty intersection returns empty Vec, not error
// ============================================================================

#[test]
fn ac6_disjoint_inputs_return_empty_not_error() {
    // Lexical matches {1}; AST matches {9} — disjoint.
    let lexical = vec![make_lex_result(1, 5.0)];
    let ast = vec![(FileId(9), 2.0)];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    assert!(
        ranked.is_empty(),
        "AC6: disjoint inputs must return empty Vec (not error), got {ranked:?}"
    );
}

// ============================================================================
// AC8 — rank-based RRF prevents scale domination
// ============================================================================

#[test]
fn ac8_rrf_is_scale_free() {
    // Prove scale-freedom with 3 files:
    //   lexical: f1 (rank 1, score 50), f2 (rank 2, score 48), f3 (rank 3, score 1)
    //   AST:     f3 NOT present; f2 (rank 1, high score), f1 (rank 2, low score)
    //   intersection: {f1, f2} (f3 not in AST)
    //
    // RRF with equal weights, 2-file intersection:
    //   f1: lex=rank1, ast=rank2 → 1/61 + 1/62
    //   f2: lex=rank2, ast=rank1 → 1/62 + 1/61
    //   Tied! (symmetric complementary ranks)
    //
    // So use asymmetric weights to prove scale-freedom AND ordering:
    //   f1: 1/61 + 0.1/62 = 0.01639 + 0.00161 = 0.01800
    //   f2: 1/62 + 0.1/61 = 0.01613 + 0.00164 = 0.01777
    //   → f1 ranks above f2 with heavily-weighted lexical.
    //
    // Invocation B: same rank ORDER, different raw magnitudes → identical composite.
    // This proves RRF, not raw magnitude summation.
    let lexical = vec![
        make_lex_result(1, 50.0), // lexical rank 1
        make_lex_result(2, 48.0), // lexical rank 2
        make_lex_result(3, 1.0),  // lexical rank 3 (NOT in AST)
    ];
    // AST invocation A: high magnitudes.
    // FileId-ASC: f1 (score 0.1, AST rank 2), f2 (score 2.0, AST rank 1)
    let ast_a = vec![
        (FileId(1), 0.1), // low score → AST rank 2
        (FileId(2), 2.0), // high score → AST rank 1
    ];
    // Use lexical-heavy weights to make f1 (lex rank 1) beat f2 (lex rank 2).
    let lex_heavy = CompositeWeights {
        lexical: 1.0,
        ast: 0.1,
    };
    let ranked_a = intersect_and_rank(&lexical, &ast_a, no_metrics, 0.0, lex_heavy);

    assert_eq!(
        ranked_a.len(),
        2,
        "AC8: both intersecting files must appear (A)"
    );
    assert_eq!(
        ranked_a[0].0,
        FileId(1),
        "AC8: f1 must rank first with lex-heavy weights (lex rank 1 dominates); got {ranked_a:?}"
    );

    // Invocation B: same rank ORDER in AST, completely different raw magnitudes.
    let ast_b = vec![
        (FileId(1), 0.0001), // still AST rank 2
        (FileId(2), 0.0002), // still AST rank 1
    ];
    let ranked_b = intersect_and_rank(&lexical, &ast_b, no_metrics, 0.0, lex_heavy);

    // Rank-invariance: same rank order → same fused output.
    assert_eq!(
        ranked_a.len(),
        ranked_b.len(),
        "AC8: rank-invariance — length must match"
    );
    for ((fid_a, score_a), (fid_b, score_b)) in ranked_a.iter().zip(ranked_b.iter()) {
        assert_eq!(
            fid_a, fid_b,
            "AC8: rank-invariance — FileId order must be identical when only magnitudes change"
        );
        assert!(
            (score_a - score_b).abs() < 1e-12,
            "AC8: rank-invariance — composite score must be identical when only magnitudes change, \
             got {score_a} vs {score_b} for FileId({fid_a:?})"
        );
    }

    // Sanity: confirm named consts exist and are referenced.
    let _ = RRF_K;
    let _ = WEIGHT_LEXICAL;
    let _ = WEIGHT_AST;

    // Additional AC8 assertion: with AST-heavy weights, the ordering inverts.
    // Proves that weights control which layer dominates — not raw magnitudes.
    let ast_heavy = CompositeWeights {
        lexical: 0.1,
        ast: 1.0,
    };
    let ranked_c = intersect_and_rank(&lexical, &ast_a, no_metrics, 0.0, ast_heavy);
    assert_eq!(
        ranked_c[0].0,
        FileId(2),
        "AC8: f2 must rank first with AST-heavy weights (AST rank 1 for f2 dominates); got {ranked_c:?}"
    );
}

// ============================================================================
// AC9 — NaN-safe fusion on degenerate input
// ============================================================================

#[test]
fn ac9_nan_safe_all_equal_scores() {
    // All-equal scores in both layers — no NaN, deterministic across invocations.
    let lexical = vec![
        make_lex_result(1, 3.0),
        make_lex_result(2, 3.0),
        make_lex_result(3, 3.0),
    ];
    let ast = vec![(FileId(1), 7.0), (FileId(2), 7.0), (FileId(3), 7.0)];

    let ranked_1 = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());
    let ranked_2 = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    assert_eq!(
        ranked_1.len(),
        3,
        "AC9: all 3 files must appear (all in both layers)"
    );
    for &(_, score) in &ranked_1 {
        assert!(
            !score.is_nan(),
            "AC9: score must not be NaN (RRF denominator always positive)"
        );
        assert!(score.is_finite(), "AC9: score must be finite");
    }
    // Determinism: two invocations with identical inputs must produce identical output.
    assert_eq!(
        ranked_1, ranked_2,
        "AC9: fused output must be deterministic across invocations"
    );
}

#[test]
fn ac9_nan_safe_single_element_layer() {
    // Single-element layer — denominator RRF_K + 1 is positive, no NaN.
    let lexical = vec![make_lex_result(1, 7.0)];
    let ast = vec![(FileId(1), 3.0)];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    assert_eq!(
        ranked.len(),
        1,
        "AC9: single-file intersection must have 1 result"
    );
    let (_, score) = ranked[0];
    assert!(
        !score.is_nan(),
        "AC9: score must not be NaN for single-element layer"
    );
    assert!(score.is_finite(), "AC9: score must be finite");

    // Verify the score matches the expected RRF formula (both rank-1):
    // score = WEIGHT_LEXICAL / (RRF_K + 1) + WEIGHT_AST / (RRF_K + 1)
    let expected = WEIGHT_LEXICAL / (RRF_K + 1.0) + WEIGHT_AST / (RRF_K + 1.0);
    assert!(
        (score - expected).abs() < 1e-12,
        "AC9: expected score {expected:.6}, got {score:.6}"
    );
}

// ============================================================================
// AC10 — deterministic tiebreaker on equal composite scores
// ============================================================================

#[test]
fn ac10_deterministic_tiebreaker_fileid_asc() {
    // Two files with equal rank in BOTH layers → equal composite scores.
    // Use symmetric complementary ranks (f10 lex-rank-1/ast-rank-2 and
    // f20 lex-rank-2/ast-rank-1) — these yield identical RRF scores:
    //   f10: 1/(60+1) + 1/(60+2) == f20: 1/(60+2) + 1/(60+1)
    // FileId-ASC tiebreaker must put FileId(10) before FileId(20).
    let lexical = vec![
        make_lex_result(10, 10.0), // lex rank 1
        make_lex_result(20, 5.0),  // lex rank 2
    ];
    // AST: f10 score 1.0 (AST rank 2), f20 score 9.0 (AST rank 1) — FileId-ASC order.
    // Symmetric complementary ranks → equal composite scores.
    let ast = vec![(FileId(10), 1.0), (FileId(20), 9.0)];

    let ranked_1 = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());
    let ranked_2 = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    assert_eq!(
        ranked_1, ranked_2,
        "AC10: output must be deterministic across invocations"
    );
    assert_eq!(ranked_1.len(), 2, "AC10: both files must appear");

    // Scores must be identical (symmetric complementary ranks → equal RRF scores).
    let (_, s0) = ranked_1[0];
    let (_, s1) = ranked_1[1];
    assert!(
        (s0 - s1).abs() < 1e-12,
        "AC10: symmetric complementary ranks must yield equal composite scores, got {s0} vs {s1}"
    );

    // FileId-ASC tiebreaker: FileId(10) < FileId(20) → FileId(10) first.
    assert_eq!(
        ranked_1[0].0,
        FileId(10),
        "AC10: FileId(10) must come first (FileId-ASC tiebreaker), got {:?}",
        ranked_1
    );
    assert_eq!(ranked_1[1].0, FileId(20), "AC10: FileId(20) must be second");
}

// ============================================================================
// AC11 — snippets preserved on intersection (recompose_with_lexical)
// ============================================================================

#[test]
fn ac11_snippets_preserved_from_lexical() {
    let lexical = vec![make_lex_result(1, 10.0), make_lex_result(2, 5.0)];
    let ast = vec![(FileId(1), 3.0), (FileId(2), 1.0)];

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());
    let recomposed = recompose_with_lexical(&ranked, &lexical);

    assert_eq!(
        recomposed.len(),
        2,
        "AC11: both intersected files must be in output"
    );

    for result in &recomposed {
        let fid = result.file_id.0;
        assert!(
            result.snippet.is_some(),
            "AC11: snippet must be preserved from lexical layer for FileId({fid})"
        );
        assert_eq!(
            result.snippet.as_deref(),
            Some(format!("snippet for file {fid}").as_str()),
            "AC11: snippet content must match lexical layer's snippet for FileId({fid})"
        );
        // Line range must be non-zero (lexical layer set 1..3).
        assert_ne!(
            result.line_range,
            (0..0),
            "AC11: line_range must be non-zero (from lexical layer) for FileId({fid})"
        );
    }
}

// ============================================================================
// AC12 — u16/u32 metric widening (no overflow)
// ============================================================================

/// Verify that u16/u32 structural metrics do not overflow when widened.
///
/// # Note: injectable path, deferred production wiring (#290)
///
/// This test exercises the `structural_lookup` injection seam with real
/// `StructuralMetrics` values.  On the production CLI path in Wave 4a,
/// `run_compound_query` passes `|_| None` and `avg_max_depth = 0.0`, so this
/// code path is never reached in production.  The test validates the overflow-
/// free widening logic for the #290 milestone that will wire a real lookup.
#[test]
fn ac12_u16_max_metrics_no_overflow() {
    // Feed StructuralMetrics with max values — must not overflow or panic.
    let max_metrics = StructuralMetrics {
        max_depth: u16::MAX,
        max_block_stmts: u16::MAX,
        max_params: u16::MAX,
        branch_count: u32::MAX,
    };

    let lexical = vec![make_lex_result(1, 5.0), make_lex_result(2, 3.0)];
    let ast = vec![(FileId(1), 2.0), (FileId(2), 1.0)];

    // Small avg_max_depth so the normalised key is large but finite.
    let structural_lookup = |_fid: FileId| Some(max_metrics);

    // Must not panic (no overflow), and scores must be finite.
    let ranked = intersect_and_rank(
        &lexical,
        &ast,
        structural_lookup,
        1.0_f32,
        CompositeWeights::default(),
    );

    assert_eq!(
        ranked.len(),
        2,
        "AC12: both files must appear after widened metric computation"
    );
    for &(fid, score) in &ranked {
        assert!(
            score.is_finite(),
            "AC12: score must be finite with u16::MAX metrics for FileId({:?})",
            fid
        );
        assert!(!score.is_nan(), "AC12: score must not be NaN");
    }
}

// ============================================================================
// AC13 — core signature is infallible (returns plain Vec, not Result)
// ============================================================================

#[test]
fn ac13_core_fn_returns_plain_vec() {
    // Compile-time proof: the explicit type annotation fails to compile if the
    // return type is changed to Result<Vec<_>, _>.
    let lexical = vec![make_lex_result(1, 1.0)];
    let ast = vec![(FileId(1), 1.0)];
    let _: Vec<(FileId, f64)> =
        intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());
}

// ============================================================================
// Structural refinement changes ordering
// ============================================================================

/// Verify that depth-based structural refinement correctly re-ranks the AST list.
///
/// # Note: injectable path, deferred production wiring (#290)
///
/// This test injects a real `structural_lookup` closure and a non-zero
/// `avg_max_depth`, exercising the depth-based AST re-ranking logic inside
/// `intersect_and_rank`.  On the production CLI path in Wave 4a,
/// `run_compound_query` always passes `|_| None` and `0.0_f32`, so the
/// depth key is 0.0 for every file and the AST decorate-sort reduces to
/// pure `ast_score`-DESC order.  The test validates the correctness of the
/// structural-refinement logic against the #290 milestone; it does NOT
/// represent live production behaviour.
#[test]
fn structural_depth_refines_ast_rank() {
    // Structural refinement changes the AST ranked list: files with higher
    // max_depth get better AST rank, which shifts the composite ordering.
    //
    // Setup:
    //   lexical: 3 files — f1 (rank 1, score 5), f2 (rank 2, score 4), f3 NOT in AST
    //   AST raw scores: f1=3.0, f2=1.0 (f1 would rank 1 by raw score alone)
    //   Structural: f2 has max_depth=100, f1 has max_depth=5
    //   → With structural refinement, f2 gets AST rank 1 (depth dominates)
    //
    // With lexical-heavy weights (to avoid the 2-file symmetric tie):
    //   f1: lex_rank=1, ast_rank=2 → 1/(61) + 0.1/(62) ≈ 0.01639 + 0.00161 = 0.01800
    //   f2: lex_rank=2, ast_rank=1 → 1/(62) + 0.1/(61) ≈ 0.01613 + 0.00164 = 0.01777
    //   → f1 > f2 with lex-heavy weight.
    //
    // Without structural refinement (raw score order: f1 rank 1):
    //   Same as above — f1 (lex rank 1, ast rank 1) beats f2 (lex rank 2, ast rank 2):
    //   f1: 1/61 + 0.1/61, f2: 1/62 + 0.1/62 → f1 wins.
    //
    // With structural refinement (depth order inverts AST ranks: f2 rank 1):
    //   f1: lex_rank=1, ast_rank=2 → 1/61 + 0.1/62 ≈ 0.01800
    //   f2: lex_rank=2, ast_rank=1 → 1/62 + 0.1/61 ≈ 0.01777
    //   → f1 still wins (lex-heavy).
    //
    // To make the structural refinement decisive, use AST-heavy weights:
    //   f1: 0.1/61 + 10/62 → 0.00164 + 0.16129 = 0.16293  (ast_rank=2 hurts a lot)
    //   f2: 0.1/62 + 10/61 → 0.00161 + 0.16393 = 0.16554  (ast_rank=1 helps a lot)
    //   → f2 > f1 ✓ — depth-based structural refinement changed the outcome.
    let lexical = vec![
        make_lex_result(1, 5.0), // lex rank 1
        make_lex_result(2, 4.0), // lex rank 2
        make_lex_result(3, 1.0), // lex rank 3 — NOT in AST
    ];
    let ast = vec![
        (FileId(1), 3.0), // f1: higher raw AST score (would be rank 1 without structure)
        (FileId(2), 1.0), // f2: lower raw AST score
    ];

    // Structural: f2 is much deeper → structural refinement inverts AST rank.
    let structural_lookup = |fid: FileId| {
        Some(if fid.0 == 1 {
            metrics_with_depth(5) // shallow
        } else {
            metrics_with_depth(100) // much deeper → structural AST rank 1
        })
    };

    let ast_heavy = CompositeWeights {
        lexical: 0.1,
        ast: 10.0,
    };
    let ranked = intersect_and_rank(&lexical, &ast, structural_lookup, 10.0_f32, ast_heavy);

    assert_eq!(
        ranked.len(),
        2,
        "structural refinement test: both files must appear"
    );
    // With AST-heavy weight and structural refinement promoting f2 to AST rank 1,
    // f2 must rank above f1 in composite output.
    assert_eq!(
        ranked[0].0,
        FileId(2),
        "structural refinement: f2 (deeper, AST rank 1 after depth-refinement) must rank first; got {ranked:?}"
    );
    assert!(
        ranked[0].1 > ranked[1].1,
        "structural refinement: composite scores must be strictly ordered (f2 > f1), got {ranked:?}"
    );
}

// ============================================================================
// AC14 — O(n+m) intersection (no nested scan)
// ============================================================================

/// Verifies the intersection produces correct results for larger inputs.
///
/// # Algorithmic note
///
/// AC14 requires the intersection to be O(n+m), not O(n*m).  The implementation
/// achieves this via a `HashMap<FileId, rank>` built from the lexical layer (O(n))
/// followed by a single pass over the AST layer with O(1) HashMap lookups (O(m)).
///
/// This test verifies the correctness of that approach for 100 lexical × 50 AST
/// entries (intersection = 50 even FileIds).  The linear complexity is enforced
/// at the implementation level; the test proves correct membership and count.
/// It would pass an O(n*m) implementation too, but the code review and the
/// `debug_assert!` input-contract guards document and enforce the invariant.
#[test]
fn ac14_intersection_is_correct_for_larger_inputs() {
    // 100 lexical results, 50 AST results — intersection should be the 50 even FileIds.
    let lexical: Vec<SearchResult> = (0u32..100)
        .map(|i| make_lex_result(i, 100.0 - i as f64))
        .collect();
    // AST covers even FileIds 0,2,4,...,98 → 50 entries, FileId-ASC (contract).
    let ast: Vec<(FileId, f64)> = (0u32..50)
        .map(|i| (FileId(i * 2), 50.0 - i as f64))
        .collect();

    let ranked = intersect_and_rank(&lexical, &ast, no_metrics, 0.0, CompositeWeights::default());

    // Intersection: lexical has all 0..100, AST has 0,2,4,...,98 → 50 files.
    assert_eq!(
        ranked.len(),
        50,
        "AC14: intersection must have exactly 50 files"
    );
    // Every FileId in result must be even (all are in AST).
    for &(fid, _) in &ranked {
        assert_eq!(
            fid.0 % 2,
            0,
            "AC14: only even FileIds must appear, got {}",
            fid.0
        );
    }
    // All scores must be finite and positive (RRF denominator is always positive).
    for &(_, score) in &ranked {
        assert!(
            score.is_finite() && score > 0.0,
            "AC14: all composite scores must be finite and positive"
        );
    }
}
