//! Tests for BM25F scoring functions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

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
    let mut cfg = BM25FConfig::default();
    // All boosts 0 except index 0 → isolate TypeDefinition
    cfg.field_boosts = [0.0; FIELD_COUNT];
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
    let mut cfg = BM25FConfig::default();
    // Zero out all boosts — every field is disabled.
    cfg.field_boosts = [0.0; FIELD_COUNT];

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
    let mut cfg = BM25FConfig::default();
    cfg.k1 = 0.0;
    // Use all boosts = 0 except one to isolate
    cfg.field_boosts = [0.0; FIELD_COUNT];
    cfg.field_boosts[0] = 1.0;
    cfg.field_b = [0.0; FIELD_COUNT]; // no length normalisation

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
    let mut cfg = BM25FConfig::default();
    cfg.field_b = [0.0; FIELD_COUNT];
    cfg.field_boosts = [0.0; FIELD_COUNT];
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
    let mut cfg = BM25FConfig::default();
    cfg.field_b = [1.0; FIELD_COUNT]; // full normalisation
    cfg.field_boosts = [0.0; FIELD_COUNT];
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

#[test]
fn test_determinism() {
    // Calling bm25f_score 100 times with the same inputs must give identical results.
    let cfg = BM25FConfig::default();
    let mut tfs = zero_field_tfs();
    tfs[0] = 3.0;
    tfs[1] = 1.0;
    let lengths = [200u32; FIELD_COUNT];
    let avgs = [180.0f32; FIELD_COUNT];

    let first = bm25f_score(4.0, &tfs, &lengths, &avgs, &cfg);
    for _ in 0..100 {
        let s = bm25f_score(4.0, &tfs, &lengths, &avgs, &cfg);
        assert!(
            (s - first).abs() < 1e-15,
            "bm25f_score is not deterministic: {first} vs {s}"
        );
    }
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

#[test]
fn test_dominant_field_deterministic() {
    let mut tfs = zero_field_tfs();
    tfs[3] = 4.0;
    let first = dominant_field(&tfs);
    for _ in 0..50 {
        assert_eq!(
            dominant_field(&tfs),
            first,
            "dominant_field must be deterministic"
        );
    }
}
