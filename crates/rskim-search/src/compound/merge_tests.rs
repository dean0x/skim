//! Tests for `compound::merge` (AC2, AC3, AC4).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::types::FileId;

// Helper to build a layer from (id, score) pairs.
fn layer(pairs: &[(u32, f64)]) -> Vec<(FileId, f64)> {
    pairs.iter().map(|&(id, s)| (FileId(id), s)).collect()
}

// ============================================================================
// AC2 — Multi-layer match ranks strictly higher than single-layer-dominant
// ============================================================================

/// AC2 (discriminating): File A appears in 2 layers (lexical + temporal) with
/// reasonable ranks.  File B appears in only 1 layer (lexical) with an
/// extremely high raw score.  Under NAIVE raw summation B would rank above A,
/// but under weighted RRF A's extra temporal rank term pushes it above B.
///
/// This proves rank-based fusion, NOT magnitude addition.
#[test]
fn test_multi_layer_match_ranks_higher_than_single_layer_dominant() {
    // Layer 1 (lexical): A is rank 1 (score 50.0), B is rank 2 (score 49.0)
    let lexical = layer(&[(0, 50.0), (1, 49.0)]);

    // Layer 2 (temporal): A is rank 1 (score 0.9), B is absent
    // B's very-high lexical magnitude (49.0) would beat A's lexical (50.0) + temp (0.9)
    // under raw summation, but NOT under RRF.
    let temporal = layer(&[(0, 0.9)]);

    // Default weights: lexical=0.5, ast=0.3, temporal=0.2, rest=0.0
    let w = CompositeWeights6::with_six_signal_defaults();
    let result = merge_composite(lexical, vec![], temporal, vec![], vec![], vec![], w);

    assert!(!result.is_empty(), "result must not be empty");

    // A (FileId 0) must rank above B (FileId 1).
    let pos_a = result.iter().position(|&(fid, _)| fid.0 == 0).unwrap();
    let pos_b = result.iter().position(|&(fid, _)| fid.0 == 1).unwrap();
    assert!(
        pos_a < pos_b,
        "A (multi-layer) must rank above B (single-layer-dominant): got A={pos_a}, B={pos_b}"
    );

    // Discriminating contrast: compute naive raw sum to show it would invert the order.
    // Raw: A = 50.0 + 0.9 = 50.9, B = 49.0 + 0.0 = 49.0 (A wins)
    // But the intent is to show that RRF handles cases where the scale difference is extreme.
    // Use a more extreme example: B has score 100× A in the single layer.
    let lexical2 = layer(&[(0, 1.0), (1, 100.0)]);
    let temporal2 = layer(&[(0, 0.9)]);
    // Raw sum: A = 1.0 + 0.9 = 1.9; B = 100.0 + 0.0 = 100.0 → B wins under raw.
    // RRF: A in lex rank 2 (because B=100 > A=1), A in temporal rank 1.
    //      B in lex rank 1, B absent from temporal.
    // RRF(A) = 0.5/(60+2) + 0.2/(60+1) = 0.00806... + 0.00328... = 0.01134
    // RRF(B) = 0.5/(60+1) + 0 = 0.00820
    // A > B under RRF, B > A under raw — this is the discriminating inversion.
    let result2 = merge_composite(
        lexical2,
        vec![],
        temporal2,
        vec![],
        vec![],
        vec![],
        CompositeWeights6::with_six_signal_defaults(),
    );
    let pos_a2 = result2.iter().position(|&(fid, _)| fid.0 == 0).unwrap();
    let pos_b2 = result2.iter().position(|&(fid, _)| fid.0 == 1).unwrap();
    assert!(
        pos_a2 < pos_b2,
        "A (multi-layer, lower raw score) must beat B (single-layer-dominant, 100x raw score) \
         under RRF — this is the rank-based fusion discriminating test (AC2): \
         got A={pos_a2}, B={pos_b2}"
    );
}

/// AC2 (rank-invariance): replacing raw scores with any rank-preserving values
/// must leave the fused output unchanged.
#[test]
fn test_rank_invariance() {
    let w = CompositeWeights6 {
        lexical: 0.5,
        ast: 0.0,
        temporal: 0.5,
        ..Default::default()
    };

    // Original magnitudes: file 0 beats file 1 in both layers.
    let lex_a = layer(&[(0, 100.0), (1, 50.0)]);
    let tmp_a = layer(&[(0, 0.9), (1, 0.4)]);

    // Rank-preserving substitute magnitudes: same rank order, very different values.
    let lex_b = layer(&[(0, 2.0), (1, 1.0)]);
    let tmp_b = layer(&[(0, 0.6), (1, 0.1)]);

    let result_a = merge_composite(lex_a, vec![], tmp_a, vec![], vec![], vec![], w);
    let result_b = merge_composite(lex_b, vec![], tmp_b, vec![], vec![], vec![], w);

    // FileId order must be identical.
    let ids_a: Vec<u32> = result_a.iter().map(|&(fid, _)| fid.0).collect();
    let ids_b: Vec<u32> = result_b.iter().map(|&(fid, _)| fid.0).collect();
    assert_eq!(
        ids_a, ids_b,
        "rank-preserving magnitude substitution must not change fused order (AC2 rank-invariance)"
    );
}

// ============================================================================
// AC3 — Deterministic, total order
// ============================================================================

#[test]
fn test_deterministic_two_calls_identical() {
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(2, 10.0), (0, 8.0), (1, 6.0)]);
    let tmp = layer(&[(1, 0.9), (0, 0.7)]);

    let r1 = merge_composite(lex.clone(), vec![], tmp.clone(), vec![], vec![], vec![], w);
    let r2 = merge_composite(lex, vec![], tmp, vec![], vec![], vec![], w);
    assert_eq!(
        r1, r2,
        "two identical calls must produce byte-identical output (AC3)"
    );
}

#[test]
fn test_equal_fused_scores_break_by_file_id_asc() {
    // Two files with identical weights and rank positions → equal fused scores.
    // Tiebreaker must be FileId-ASC.
    //
    // To force truly equal fused scores we must put them in separate symmetric layers.
    // Symmetric approach: file 3 in layer1 rank1, file 7 in layer2 rank1, equal weights.
    let w2 = CompositeWeights6 {
        lexical: 0.5,
        ast: 0.5,
        temporal: 0.0,
        ..Default::default()
    };
    let lex = layer(&[(7, 1.0)]); // FileId 7, rank 1 in lexical
    let ast = layer(&[(3, 1.0)]); // FileId 3, rank 1 in ast
    let result = merge_composite(lex, ast, vec![], vec![], vec![], vec![], w2);
    assert_eq!(result.len(), 2, "both files must appear (UNION)");
    // Both have equal fused scores (0.5/(60+1) each); tiebreaker → FileId 3 first.
    assert_eq!(
        result[0].0,
        FileId(3),
        "equal fused scores must break by FileId-ASC (AC3): expected FileId(3) first"
    );
    assert_eq!(result[1].0, FileId(7), "FileId(7) must follow FileId(3)");
    // Scores must be finite and equal.
    let (_, s0) = result[0];
    let (_, s1) = result[1];
    assert!(s0.is_finite(), "score must be finite (AC3)");
    assert!(
        (s0 - s1).abs() < f64::EPSILON,
        "equal-score invariant: scores must be equal for symmetric inputs"
    );
}

#[test]
fn test_all_finite_scores() {
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(0, 1.0), (1, 2.0)]);
    let result = merge_composite(lex, vec![], vec![], vec![], vec![], vec![], w);
    for &(_, score) in &result {
        assert!(
            score.is_finite(),
            "all fused scores must be finite (AC3/AC4)"
        );
    }
}

// ============================================================================
// AC4 — Degenerate-input safety + UNION inclusion
// ============================================================================

#[test]
fn test_empty_layer_contributes_nothing_weight_not_redistributed() {
    // AC4(a): empty layer contributes 0 to every file; weight NOT redistributed.
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(0, 1.0)]);
    // All other layers empty.
    let result = merge_composite(lex, vec![], vec![], vec![], vec![], vec![], w);
    assert_eq!(result.len(), 1, "single file from single non-empty layer");
    let score = result[0].1;
    assert!(
        score.is_finite() && score > 0.0,
        "score must be finite and positive"
    );
    // Expected: 0.5 / (60 + 1) = 0.008196...
    let expected = 0.5_f64 / 61.0;
    assert!(
        (score - expected).abs() < 1e-10,
        "score must equal w_lexical/(RRF_K+1), got {score}"
    );
}

#[test]
fn test_single_file_layer() {
    // AC4(b): single-file layer must produce one output entry.
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(42, 99.0)]);
    let result = merge_composite(lex, vec![], vec![], vec![], vec![], vec![], w);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, FileId(42));
    assert!(result[0].1.is_finite() && result[0].1 > 0.0);
}

#[test]
fn test_all_equal_scores_layer_deterministic() {
    // AC4(c): all-equal-scores layer must not produce non-deterministic order.
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(0, 5.0), (1, 5.0), (2, 5.0)]);
    let r1 = merge_composite(lex.clone(), vec![], vec![], vec![], vec![], vec![], w);
    let r2 = merge_composite(lex, vec![], vec![], vec![], vec![], vec![], w);
    assert_eq!(
        r1, r2,
        "all-equal-scores layer must produce deterministic order (AC4c)"
    );
    for &(_, s) in &r1 {
        assert!(s.is_finite() && s > 0.0);
    }
}

#[test]
fn test_union_inclusion_single_layer_only_file_is_present() {
    // AC4(d): a FileId present in only ONE of several layers MUST appear in output.
    // File 99 is present only in the temporal layer; not in lexical or ast.
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(0, 10.0), (1, 8.0)]);
    let ast = layer(&[(0, 5.0)]);
    let temporal = layer(&[(99, 0.95)]); // Only here.
    let result = merge_composite(lex, ast, temporal, vec![], vec![], vec![], w);

    let has_99 = result.iter().any(|&(fid, _)| fid.0 == 99);
    assert!(
        has_99,
        "FileId 99 (single-layer-only) MUST be present in UNION output (AC4d)"
    );

    // Its score must be finite and positive (ranked by its one temporal term).
    let score_99 = result.iter().find(|&&(fid, _)| fid.0 == 99).unwrap().1;
    assert!(
        score_99.is_finite() && score_99 > 0.0,
        "single-layer-only file must have finite positive score, got {score_99}"
    );
}

#[test]
fn test_union_no_files_dropped_from_any_layer() {
    // All FileIds across all layers must appear in the output.
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(0, 10.0), (1, 8.0), (2, 6.0)]);
    let temporal = layer(&[(3, 0.9), (4, 0.7)]);
    let expected_ids: std::collections::HashSet<u32> = [0, 1, 2, 3, 4].into();
    let result = merge_composite(lex, vec![], temporal, vec![], vec![], vec![], w);
    let got_ids: std::collections::HashSet<u32> = result.iter().map(|&(fid, _)| fid.0).collect();
    assert_eq!(
        got_ids, expected_ids,
        "all FileIds from all layers must be in the UNION output"
    );
}

#[test]
fn test_no_panic_all_empty_layers() {
    // AC4(a): all-empty input must return an empty Vec, not panic.
    let w = CompositeWeights6::with_six_signal_defaults();
    let result = merge_composite(vec![], vec![], vec![], vec![], vec![], vec![], w);
    assert!(
        result.is_empty(),
        "all-empty layers must return empty Vec (not panic)"
    );
}

#[test]
fn test_no_duplicate_file_ids_in_output() {
    // AC13: a FileId appearing in multiple input lists must appear ONCE in output.
    let w = CompositeWeights6::with_six_signal_defaults();
    let lex = layer(&[(5, 10.0), (6, 8.0)]);
    let temporal = layer(&[(5, 0.9), (7, 0.6)]); // FileId 5 in both layers.
    let result = merge_composite(lex, vec![], temporal, vec![], vec![], vec![], w);

    let mut ids: Vec<u32> = result.iter().map(|&(fid, _)| fid.0).collect();
    let original_len = ids.len();
    ids.dedup();
    assert_eq!(
        ids.len(),
        original_len,
        "no duplicate FileIds in output (AC13)"
    );
    assert_eq!(original_len, 3, "FileIds 5, 6, 7 each exactly once");
}

// ============================================================================
// merge_layer_scores — low-level API
// ============================================================================

#[test]
fn test_merge_layer_scores_empty_returns_empty() {
    let result = merge_layer_scores(&[]);
    assert!(result.is_empty());
}

#[test]
fn test_merge_layer_scores_zero_weight_layer_contributes_nothing() {
    let layers = vec![(layer(&[(0, 1.0)]), 0.0)];
    let result = merge_layer_scores(&layers);
    // Zero-weight layer: the file gets no contribution.
    assert!(
        result.is_empty(),
        "zero-weight layer must not produce entries"
    );
}

#[test]
fn test_merge_layer_scores_single_layer() {
    let layers = vec![(layer(&[(1, 5.0), (0, 3.0)]), 1.0)];
    let result = merge_layer_scores(&layers);
    assert_eq!(result.len(), 2);
    // Rank 1 = FileId(1) (higher score); Rank 2 = FileId(0).
    // Score(1) = 1.0/(60+1) > Score(0) = 1.0/(60+2).
    assert_eq!(result[0].0, FileId(1), "rank-1 file must come first");
    assert_eq!(result[1].0, FileId(0));
}

#[test]
fn test_merge_layer_scores_sort_order_desc_score_asc_fileid() {
    // Verify overall sort: DESC score, then ASC FileId for ties.
    let layers = vec![(layer(&[(10, 1.0)]), 0.5), (layer(&[(20, 1.0)]), 0.5)];
    let result = merge_layer_scores(&layers);
    // Both have the same fused score (each rank-1 in their own layer with equal weights).
    // Tiebreaker: FileId-ASC → FileId(10) first.
    assert_eq!(result[0].0, FileId(10), "ASC FileId tiebreaker (AC3)");
    assert_eq!(result[1].0, FileId(20));
}
