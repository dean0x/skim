//! Tests for BM25FConfig.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn test_default_k1() {
    let cfg = BM25FConfig::default();
    assert!(
        (cfg.k1 - 1.2).abs() < f32::EPSILON,
        "default k1 should be 1.2"
    );
}

#[test]
fn test_default_boosts() {
    let cfg = BM25FConfig::default();
    assert!(
        (cfg.field_boosts[0] - 5.0).abs() < f32::EPSILON,
        "TypeDefinition boost"
    );
    assert!(
        (cfg.field_boosts[1] - 4.0).abs() < f32::EPSILON,
        "FunctionSignature boost"
    );
    assert!(
        (cfg.field_boosts[2] - 3.5).abs() < f32::EPSILON,
        "SymbolName boost"
    );
    assert!(
        (cfg.field_boosts[3] - 3.0).abs() < f32::EPSILON,
        "ImportExport boost"
    );
    assert!(
        (cfg.field_boosts[4] - 1.0).abs() < f32::EPSILON,
        "FunctionBody boost"
    );
    assert!(
        (cfg.field_boosts[5] - 0.8).abs() < f32::EPSILON,
        "Comment boost"
    );
    assert!(
        (cfg.field_boosts[6] - 0.5).abs() < f32::EPSILON,
        "StringLiteral boost"
    );
    assert!(
        (cfg.field_boosts[7] - 1.0).abs() < f32::EPSILON,
        "Other boost"
    );
}

#[test]
fn test_default_b_values() {
    let cfg = BM25FConfig::default();
    for (i, &b) in cfg.field_b.iter().enumerate() {
        assert!(
            (b - 0.75).abs() < f32::EPSILON,
            "field_b[{i}] should be 0.75"
        );
    }
}

#[test]
fn test_validate_ok_defaults() {
    let cfg = BM25FConfig::default();
    assert!(cfg.validate().is_ok(), "default config should be valid");
}

#[test]
fn test_validate_rejects_negative_k1() {
    let cfg = BM25FConfig {
        k1: -0.1,
        ..BM25FConfig::default()
    };
    let result = cfg.validate();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("k1"), "error should mention k1: {msg}");
}

#[test]
fn test_validate_rejects_negative_boost() {
    let mut cfg = BM25FConfig::default();
    cfg.field_boosts[2] = -1.0;
    let result = cfg.validate();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("field_boosts[2]"),
        "error should mention field index: {msg}"
    );
}

#[test]
fn test_validate_rejects_b_above_one() {
    let mut cfg = BM25FConfig::default();
    cfg.field_b[0] = 1.1;
    let result = cfg.validate();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("field_b[0]"),
        "error should mention field index: {msg}"
    );
}

#[test]
fn test_validate_rejects_b_below_zero() {
    let mut cfg = BM25FConfig::default();
    cfg.field_b[3] = -0.01;
    let result = cfg.validate();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("field_b[3]"),
        "error should mention field index: {msg}"
    );
}

#[test]
fn test_validate_accepts_zero_k1() {
    // k1 = 0.0 is a legal degenerate case (no TF saturation)
    let cfg = BM25FConfig {
        k1: 0.0,
        ..BM25FConfig::default()
    };
    assert!(cfg.validate().is_ok(), "k1=0.0 should be valid");
}

#[test]
fn test_validate_accepts_zero_boost() {
    // zero boost means field is ignored — valid
    let mut cfg = BM25FConfig::default();
    cfg.field_boosts[5] = 0.0;
    assert!(cfg.validate().is_ok(), "zero boost should be valid");
}

#[test]
fn test_serde_roundtrip() {
    let original = BM25FConfig::default();
    let json = serde_json::to_string(&original).unwrap();
    let restored: BM25FConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(original, restored, "serde roundtrip should be lossless");
}

#[test]
fn test_partial_eq() {
    let a = BM25FConfig::default();
    let b = BM25FConfig::default();
    assert_eq!(a, b);

    let c = BM25FConfig {
        k1: 2.0,
        ..BM25FConfig::default()
    };
    assert_ne!(a, c);
}

#[test]
fn test_field_count_matches_search_field_variants() {
    // SearchField has 8 variants (0..=7). FIELD_COUNT must match.
    assert_eq!(
        FIELD_COUNT, 8,
        "FIELD_COUNT must equal the number of SearchField variants"
    );
}

// -----------------------------------------------------------------------
// BM25FConfig::for_exact_symbol (AD-411-4)
// -----------------------------------------------------------------------

/// `for_exact_symbol()` must pass validate() (all parameters in legal ranges).
#[test]
fn test_for_exact_symbol_validates() {
    let cfg = BM25FConfig::for_exact_symbol();
    assert!(
        cfg.validate().is_ok(),
        "for_exact_symbol() must satisfy validate(); errors: {:?}",
        cfg.validate()
    );
}

/// field_b must be all 0.0 — no length normalisation on the exact-symbol path
/// (preserves AD-372-6 length-norm-free property, OD-3/4 guarantee).
#[test]
fn test_for_exact_symbol_field_b_all_zero() {
    let cfg = BM25FConfig::for_exact_symbol();
    for (i, &b) in cfg.field_b.iter().enumerate() {
        assert_eq!(
            b, 0.0,
            "for_exact_symbol: field_b[{i}] must be 0.0 (length-norm-free), got {b}"
        );
    }
}

/// FunctionSignature boost (index 1) > TypeDefinition boost (index 0).
/// Required so code definitions outrank Markdown headings (OD3, AC14).
#[test]
fn test_for_exact_symbol_fnsig_boost_greater_than_typedef_boost() {
    let cfg = BM25FConfig::for_exact_symbol();
    let fnsig = cfg.field_boosts[1]; // FunctionSignature
    let typedef = cfg.field_boosts[0]; // TypeDefinition
    assert!(
        fnsig > typedef,
        "FunctionSignature boost ({fnsig}) must exceed TypeDefinition boost ({typedef}) (OD3)"
    );
}

/// FunctionSignature boost > SymbolName boost > FunctionBody boost.
/// Ensures definition tier > degradation tier > call-site tier.
#[test]
fn test_for_exact_symbol_boost_ordering_invariants() {
    let cfg = BM25FConfig::for_exact_symbol();
    let fnsig = cfg.field_boosts[1]; // FunctionSignature
    let symbol = cfg.field_boosts[2]; // SymbolName
    let body = cfg.field_boosts[4]; // FunctionBody
    assert!(
        fnsig > symbol,
        "FnSig boost ({fnsig}) must exceed SymbolName boost ({symbol})"
    );
    assert!(
        symbol > body,
        "SymbolName boost ({symbol}) must exceed FunctionBody boost ({body})"
    );
}

/// `for_exact_symbol()` must return a DIFFERENT config from `Default`.
/// (If they were identical the method would be dead code.)
#[test]
fn test_for_exact_symbol_differs_from_default() {
    let exact = BM25FConfig::for_exact_symbol();
    let def = BM25FConfig::default();
    // At minimum, field_b differs (exact=0, default=0.75).
    assert_ne!(
        exact.field_b, def.field_b,
        "for_exact_symbol field_b must differ from default"
    );
}

// -----------------------------------------------------------------------
// NaN / Infinity rejection
// -----------------------------------------------------------------------

#[test]
fn test_validate_rejects_nan_k1() {
    let cfg = BM25FConfig {
        k1: f32::NAN,
        ..BM25FConfig::default()
    };
    let result = cfg.validate();
    assert!(result.is_err(), "NaN k1 should be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("k1"), "error should mention k1: {msg}");
}

#[test]
fn test_validate_rejects_infinity_k1() {
    let cfg = BM25FConfig {
        k1: f32::INFINITY,
        ..BM25FConfig::default()
    };
    let result = cfg.validate();
    assert!(result.is_err(), "infinity k1 should be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("k1"), "error should mention k1: {msg}");
}

#[test]
fn test_validate_rejects_negative_infinity_k1() {
    let cfg = BM25FConfig {
        k1: f32::NEG_INFINITY,
        ..BM25FConfig::default()
    };
    let result = cfg.validate();
    assert!(result.is_err(), "negative infinity k1 should be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("k1"), "error should mention k1: {msg}");
}

#[test]
fn test_validate_rejects_nan_field_boost() {
    let mut cfg = BM25FConfig::default();
    cfg.field_boosts[3] = f32::NAN;
    let result = cfg.validate();
    assert!(result.is_err(), "NaN field_boosts should be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("field_boosts[3]"),
        "error should mention field index: {msg}"
    );
}

#[test]
fn test_validate_rejects_infinity_field_boost() {
    let mut cfg = BM25FConfig::default();
    cfg.field_boosts[0] = f32::INFINITY;
    let result = cfg.validate();
    assert!(result.is_err(), "infinity field_boosts should be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("field_boosts[0]"),
        "error should mention field index: {msg}"
    );
}

#[test]
fn test_validate_rejects_nan_field_b() {
    let mut cfg = BM25FConfig::default();
    cfg.field_b[5] = f32::NAN;
    let result = cfg.validate();
    assert!(result.is_err(), "NaN field_b should be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("field_b[5]"),
        "error should mention field index: {msg}"
    );
}

#[test]
fn test_validate_rejects_infinity_field_b() {
    let mut cfg = BM25FConfig::default();
    cfg.field_b[2] = f32::INFINITY;
    let result = cfg.validate();
    assert!(result.is_err(), "infinity field_b should be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("field_b[2]"),
        "error should mention field index: {msg}"
    );
}
