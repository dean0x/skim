//! Tests for BM25F scoring functions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::SearchField;
use crate::lexical::config::{BM25FConfig, FIELD_COUNT};

fn zero_field_tfs() -> [f32; FIELD_COUNT] {
    [0.0; FIELD_COUNT]
}

fn zero_field_lengths() -> [u32; FIELD_COUNT] {
    [0; FIELD_COUNT]
}

fn avg_lengths(v: f32) -> [f32; FIELD_COUNT] {
    [v; FIELD_COUNT]
}

// -----------------------------------------------------------------------
// bm25f_score
// -----------------------------------------------------------------------

#[test]
fn test_zero_tf_returns_zero() {
    // No term occurrences → score must be exactly 0.0
    let cfg = BM25FConfig::default();
    let score = bm25f_score(
        5.0,
        &zero_field_tfs(),
        &zero_field_lengths(),
        &avg_lengths(100.0),
        &cfg,
    );
    assert_eq!(score, 0.0, "zero TF should give zero score");
}

#[test]
fn test_single_field_positive_score() {
    let cfg = BM25FConfig::default();
    let mut tfs = zero_field_tfs();
    tfs[0] = 3.0; // TypeDefinition field
    let mut lengths = zero_field_lengths();
    lengths[0] = 200;

    let score = bm25f_score(5.0, &tfs, &lengths, &avg_lengths(200.0), &cfg);
    assert!(
        score > 0.0,
        "positive TF and IDF should give positive score"
    );
    assert!(score.is_finite(), "score must be finite");
}

#[test]
fn test_higher_boost_increases_score() {
    // TypeDefinition field (boost 5.0) vs FunctionBody field (boost 1.0),
    // same TF and document lengths.
    let mut cfg = BM25FConfig {
        field_boosts: [0.0; FIELD_COUNT],
        ..BM25FConfig::default()
    };
    // All boosts 0 except index 0 → isolate TypeDefinition
    cfg.field_boosts[0] = 5.0;

    let mut tfs_high = zero_field_tfs();
    tfs_high[0] = 2.0;
    let high = bm25f_score(
        2.0,
        &tfs_high,
        &[100; FIELD_COUNT],
        &avg_lengths(100.0),
        &cfg,
    );

    cfg.field_boosts[0] = 1.0;
    let low = bm25f_score(
        2.0,
        &tfs_high,
        &[100; FIELD_COUNT],
        &avg_lengths(100.0),
        &cfg,
    );

    assert!(
        high > low,
        "higher boost should increase score: {high} vs {low}"
    );
}

#[test]
fn test_zero_boost_field_ignored() {
    let cfg = BM25FConfig {
        // Zero out all boosts — every field is disabled.
        field_boosts: [0.0; FIELD_COUNT],
        ..BM25FConfig::default()
    };

    let mut tfs = zero_field_tfs();
    tfs[3] = 10.0; // many occurrences in ImportExport field
    let score = bm25f_score(5.0, &tfs, &[500; FIELD_COUNT], &avg_lengths(200.0), &cfg);
    assert_eq!(score, 0.0, "zero boost should yield zero contribution");
}

#[test]
fn test_zero_avg_field_length_no_panic() {
    // avg_field_lengths = 0 should not panic; treated as 1.0 internally.
    let cfg = BM25FConfig::default();
    let mut tfs = zero_field_tfs();
    tfs[1] = 2.0;
    let score = bm25f_score(3.0, &tfs, &[50; FIELD_COUNT], &[0.0; FIELD_COUNT], &cfg);
    assert!(
        score.is_finite(),
        "score must be finite even with avg_len=0"
    );
}

#[test]
fn test_k1_zero_acts_as_binary_presence() {
    // k1=0 → tf_weighted / (tf_weighted + 0) = 1.0 → score = idf * 1.0
    let mut cfg = BM25FConfig {
        k1: 0.0,
        // Use all boosts = 0 except one to isolate
        field_boosts: [0.0; FIELD_COUNT],
        field_b: [0.0; FIELD_COUNT], // no length normalisation
    };
    cfg.field_boosts[0] = 1.0;

    let mut tfs = zero_field_tfs();
    tfs[0] = 1.0;
    let idf = 3.0_f64;
    let score = bm25f_score(idf, &tfs, &[100; FIELD_COUNT], &avg_lengths(100.0), &cfg);
    // With k1=0, b=0: tf_weighted = 1.0 * 1.0 / (1.0 - 0 + 0) = 1.0
    // score = idf * 1.0 / (1.0 + 0.0) = idf
    assert!(
        (score - idf).abs() < 1e-9,
        "k1=0 score should equal idf: got {score}, expected {idf}"
    );
}

#[test]
fn test_b_zero_no_length_normalisation() {
    // b=0 means field length has no effect.
    let mut cfg = BM25FConfig {
        field_b: [0.0; FIELD_COUNT],
        field_boosts: [0.0; FIELD_COUNT],
        ..BM25FConfig::default()
    };
    cfg.field_boosts[0] = 1.0;

    let mut tfs = zero_field_tfs();
    tfs[0] = 2.0;

    // Two documents: short (dl=10) and long (dl=10000) — both should score identically.
    let mut short_lengths = zero_field_lengths();
    short_lengths[0] = 10;
    let score_short = bm25f_score(2.0, &tfs, &short_lengths, &avg_lengths(200.0), &cfg);

    let mut long_lengths = zero_field_lengths();
    long_lengths[0] = 10_000;
    let score_long = bm25f_score(2.0, &tfs, &long_lengths, &avg_lengths(200.0), &cfg);

    assert!(
        (score_short - score_long).abs() < 1e-9,
        "b=0 should make length irrelevant: short={score_short}, long={score_long}"
    );
}

#[test]
fn test_extreme_length_ratio_finite() {
    let cfg = BM25FConfig::default();
    let mut tfs = zero_field_tfs();
    tfs[0] = 1.0;
    let mut lengths = zero_field_lengths();
    lengths[0] = u32::MAX;
    let score = bm25f_score(2.0, &tfs, &lengths, &avg_lengths(1.0), &cfg);
    assert!(
        score.is_finite(),
        "extreme length ratio must not produce NaN/inf"
    );
}

#[test]
fn test_zero_field_length_with_b_one_no_nan() {
    // b=1.0 and dl=0 produces norm=0.0 in the formula.
    // The guard should prevent NaN/Inf.
    let mut cfg = BM25FConfig {
        field_b: [1.0; FIELD_COUNT], // full normalisation
        field_boosts: [0.0; FIELD_COUNT],
        ..BM25FConfig::default()
    };
    cfg.field_boosts[0] = 1.0;

    let mut tfs = zero_field_tfs();
    tfs[0] = 2.0; // term appears in field with zero length (edge case)
    let lengths = zero_field_lengths(); // dl=0 for all fields
    let avgs = avg_lengths(100.0);

    let score = bm25f_score(3.0, &tfs, &lengths, &avgs, &cfg);
    assert!(
        score.is_finite(),
        "b=1.0 with dl=0 must not produce NaN/Inf, got {score}"
    );
    assert!(score > 0.0, "score should still be positive: {score}");
}

// -----------------------------------------------------------------------
// bm25f_per_field_saturated_score (AD-411-3)
// -----------------------------------------------------------------------

/// Zero TF in all fields → score must be exactly 0.0.
#[test]
fn test_per_field_saturated_zero_tf_returns_zero() {
    let cfg = BM25FConfig::for_exact_symbol();
    let score = bm25f_per_field_saturated_score(1.0, &zero_field_tfs(), &cfg);
    assert_eq!(score, 0.0, "all-zero TF should give 0.0 score");
}

/// idf = 0.0 → score must be 0.0 regardless of TFs.
#[test]
fn test_per_field_saturated_zero_idf_returns_zero() {
    let cfg = BM25FConfig::for_exact_symbol();
    let mut tfs = zero_field_tfs();
    tfs[1] = 10.0; // FunctionSignature
    let score = bm25f_per_field_saturated_score(0.0, &tfs, &cfg);
    assert_eq!(score, 0.0, "zero idf should give 0.0 score");
}

/// AC4 (N=16): 1 occurrence in FunctionSignature (boost 8.0) must score
/// higher than 16 occurrences in FunctionBody (boost 1.0).
///
/// Math: 1 FnSig → 8 × (1/2.2) ≈ 3.636
///       16 FnBody → 1 × (16/17.2) ≈ 0.930   → def wins
#[test]
fn test_per_field_saturated_one_fnsig_beats_sixteen_fnbody() {
    let cfg = BM25FConfig::for_exact_symbol();
    let idf = 1.0_f64;

    let mut def_tfs = zero_field_tfs();
    def_tfs[SearchField::FunctionSignature.discriminant() as usize] = 1.0;

    let mut body_tfs = zero_field_tfs();
    body_tfs[SearchField::FunctionBody.discriminant() as usize] = 16.0;

    let def_score = bm25f_per_field_saturated_score(idf, &def_tfs, &cfg);
    let body_score = bm25f_per_field_saturated_score(idf, &body_tfs, &cfg);

    assert!(
        def_score > body_score,
        "AC4: 1 FnSig occurrence ({def_score:.4}) must beat 16 FnBody occurrences ({body_score:.4})"
    );
}

/// AC4 (N=52): 1 occurrence in FunctionSignature must beat 52 in FunctionBody.
/// This is the extreme case from the plan's example.
#[test]
fn test_per_field_saturated_one_fnsig_beats_fifty_two_fnbody() {
    let cfg = BM25FConfig::for_exact_symbol();
    let idf = 1.0_f64;

    let mut def_tfs = zero_field_tfs();
    def_tfs[SearchField::FunctionSignature.discriminant() as usize] = 1.0;

    let mut body_tfs = zero_field_tfs();
    body_tfs[SearchField::FunctionBody.discriminant() as usize] = 52.0;

    let def_score = bm25f_per_field_saturated_score(idf, &def_tfs, &cfg);
    let body_score = bm25f_per_field_saturated_score(idf, &body_tfs, &cfg);

    assert!(
        def_score > body_score,
        "AC4: 1 FnSig ({def_score:.4}) must beat 52 FnBody ({body_score:.4})"
    );
}

/// FunctionSignature boost (8.0) > TypeDefinition boost (4.0) ensures code
/// definitions outrank doc headings (OD3). Verify the per-field saturated scorer
/// respects this ordering.
#[test]
fn test_per_field_saturated_fnsig_boost_beats_typedef_boost() {
    let cfg = BM25FConfig::for_exact_symbol();
    let idf = 1.0_f64;

    let mut code_tfs = zero_field_tfs();
    code_tfs[SearchField::FunctionSignature.discriminant() as usize] = 1.0;

    let mut doc_tfs = zero_field_tfs();
    doc_tfs[SearchField::TypeDefinition.discriminant() as usize] = 1.0;

    let code_score = bm25f_per_field_saturated_score(idf, &code_tfs, &cfg);
    let doc_score = bm25f_per_field_saturated_score(idf, &doc_tfs, &cfg);

    assert!(
        code_score > doc_score,
        "OD3: FnSig ({code_score:.4}) must beat TypeDef ({doc_score:.4})"
    );
}

/// Score is finite for any reasonable input combination.
#[test]
fn test_per_field_saturated_finite_for_large_tf() {
    let cfg = BM25FConfig::for_exact_symbol();
    let mut tfs = zero_field_tfs();
    tfs[4] = 100_000.0; // very high TF in FunctionBody
    let score = bm25f_per_field_saturated_score(1.0, &tfs, &cfg);
    assert!(score.is_finite(), "score must be finite for large TF");
    // Should be close to asymptote: boost × (tf → ∞) → boost × 1.0
    let boost_fnbody = cfg.field_boosts[4] as f64;
    assert!(
        score <= boost_fnbody * 1.0001,
        "score must not exceed the asymptote {boost_fnbody}"
    );
}

// -----------------------------------------------------------------------
// dominant_field
// -----------------------------------------------------------------------

#[test]
fn test_dominant_field_all_zero_returns_other() {
    // All TFs zero → fallback to Other (field 7 = lowest non-match)
    let result = dominant_field(&zero_field_tfs());
    // With all zeros, we never beat 0.0 > 0.0, so best_field stays Other.
    assert_eq!(result, SearchField::Other);
}

#[test]
fn test_dominant_field_single_field() {
    let mut tfs = zero_field_tfs();
    tfs[1] = 5.0; // FunctionSignature
    assert_eq!(dominant_field(&tfs), SearchField::FunctionSignature);
}

#[test]
fn test_dominant_field_picks_highest() {
    let mut tfs = zero_field_tfs();
    tfs[0] = 1.0; // TypeDefinition
    tfs[2] = 3.0; // SymbolName — highest
    tfs[4] = 2.0; // FunctionBody
    assert_eq!(dominant_field(&tfs), SearchField::SymbolName);
}

#[test]
fn test_dominant_field_tie_picks_lowest_discriminant() {
    // Equal TF in fields 1 (FunctionSignature) and 4 (FunctionBody) —
    // field 1 has lower discriminant → wins.
    let mut tfs = zero_field_tfs();
    tfs[1] = 2.0;
    tfs[4] = 2.0;
    assert_eq!(dominant_field(&tfs), SearchField::FunctionSignature);
}
