//! Integration tests for BM25F scoring.
//!
//! Validates the scoring formula contract:
//! - IDF varies correctly with document frequency
//! - Field boosts from [`SearchField::default_boost`] affect ranking
//! - Edge cases (zero inputs) do not crash

use rskim_search::lexical::scoring::Bm25Scorer;
use rskim_search::lexical::Bm25Params;
use rskim_search::SearchField;

// ============================================================================
// Helpers
// ============================================================================

fn scorer(doc_count: u64) -> Bm25Scorer {
    Bm25Scorer::new(
        Bm25Params {
            k1: 1.2,
            b: 0.75,
            avg_doc_len: 100.0,
        },
        doc_count,
    )
}

fn scorer_with_avg(doc_count: u64, avg_doc_len: f32) -> Bm25Scorer {
    Bm25Scorer::new(
        Bm25Params { k1: 1.2, b: 0.75, avg_doc_len },
        doc_count,
    )
}

// ============================================================================
// IDF behavior
// ============================================================================

#[test]
fn low_df_produces_higher_idf_than_high_df() {
    let s = scorer(1000);
    let rare = s.score_term(&[(SearchField::FunctionSignature, 1)], 100, 1);
    let common = s.score_term(&[(SearchField::FunctionSignature, 1)], 100, 800);
    assert!(
        rare > common,
        "rare term (df=1) should outscore common term (df=800): rare={rare}, common={common}"
    );
}

#[test]
fn df_equal_to_doc_count_gives_near_zero_score() {
    let s = scorer(100);
    let score = s.score_term(&[(SearchField::FunctionBody, 5)], 100, 100);
    // IDF for df=N is ln((0.5 / 100.5) + 1) ≈ very small positive number.
    assert!(score >= 0.0);
    assert!(score < 0.05);
}

#[test]
fn df_zero_returns_zero() {
    let s = scorer(100);
    let score = s.score_term(&[(SearchField::TypeDefinition, 3)], 50, 0);
    assert_eq!(score, 0.0, "df=0 must return 0.0");
}

// ============================================================================
// Field boost effects
// ============================================================================

#[test]
fn type_definition_outscores_string_literal_same_tf() {
    // TypeDefinition boost = 5.0, StringLiteral boost = 0.5
    let s = scorer(200);
    let type_def = s.score_term(&[(SearchField::TypeDefinition, 2)], 100, 10);
    let str_lit = s.score_term(&[(SearchField::StringLiteral, 2)], 100, 10);
    assert!(
        type_def > str_lit,
        "TypeDefinition boost 5.0 must beat StringLiteral boost 0.5: {type_def} vs {str_lit}"
    );
}

#[test]
fn function_signature_outscores_function_body() {
    // FunctionSignature boost = 4.0 > FunctionBody boost = 1.0
    let s = scorer(200);
    let sig = s.score_term(&[(SearchField::FunctionSignature, 1)], 100, 5);
    let body = s.score_term(&[(SearchField::FunctionBody, 1)], 100, 5);
    assert!(sig > body, "FunctionSignature (boost 4.0) must outscore FunctionBody (boost 1.0)");
}

#[test]
fn multi_field_match_outscores_single_field() {
    let s = scorer(100);
    let multi = s.score_term(
        &[
            (SearchField::TypeDefinition, 1),
            (SearchField::SymbolName, 1),
        ],
        100,
        5,
    );
    let single = s.score_term(&[(SearchField::TypeDefinition, 1)], 100, 5);
    assert!(multi > single, "multi-field match must outscore single-field match");
}

// ============================================================================
// Empty / zero inputs (must not crash)
// ============================================================================

#[test]
fn empty_field_tfs_returns_zero() {
    let s = scorer(100);
    assert_eq!(s.score_term(&[], 50, 5), 0.0);
}

#[test]
fn doc_len_zero_does_not_crash() {
    let s = scorer(100);
    let score = s.score_term(&[(SearchField::FunctionSignature, 1)], 0, 5);
    assert!(score.is_finite(), "doc_len=0 must not produce NaN or inf");
}

#[test]
fn avg_doc_len_zero_does_not_crash() {
    let s = scorer_with_avg(100, 0.0);
    let score = s.score_term(&[(SearchField::TypeDefinition, 2)], 100, 5);
    assert!(score.is_finite(), "avg_doc_len=0 must not produce NaN or inf");
    assert!(score > 0.0, "avg_doc_len=0 should still yield a positive score");
}

#[test]
fn doc_count_one_and_df_one_gives_low_score() {
    // Single-doc collection with df=1 → IDF = ln((0.5/1.5)+1) ≈ 0.405
    let s = scorer(1);
    let score = s.score_term(&[(SearchField::FunctionSignature, 3)], 50, 1);
    assert!(score.is_finite());
    assert!(score >= 0.0);
}

// ============================================================================
// Score is always non-negative and finite
// ============================================================================

#[test]
fn score_is_always_non_negative_and_finite() {
    let s = scorer(500);
    let cases: &[(&[(SearchField, u16)], u32, u64)] = &[
        (&[(SearchField::TypeDefinition, 1)], 100, 1),
        (&[(SearchField::Comment, 10)], 5000, 499),
        (&[(SearchField::SymbolName, 0)], 50, 20),
        (&[(SearchField::ImportExport, 7)], 200, 50),
        (&[(SearchField::FunctionBody, 100)], 1, 1),
    ];
    for (field_tfs, doc_len, df) in cases {
        let score = s.score_term(field_tfs, *doc_len, *df);
        assert!(score.is_finite(), "score must be finite: {score}");
        assert!(score >= 0.0, "score must be non-negative: {score}");
    }
}
