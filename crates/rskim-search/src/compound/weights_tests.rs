//! Tests for `compound::weights` (AC1, AC5).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::types::SearchError;

// ============================================================================
// AC1 — Default values and field presence
// ============================================================================

#[test]
fn test_default_values() {
    let w = CompositeWeights6::default();
    // Core signals.
    assert_eq!(w.lexical, 0.5, "lexical default must be 0.5 (AC1)");
    assert_eq!(w.ast, 0.3, "ast default must be 0.3 (AC1)");
    assert_eq!(w.temporal, 0.2, "temporal default must be 0.2 (AC1)");
    // Extended signals default to 0.0 (ADR-003 gated until measured).
    assert_eq!(
        w.import_graph, 0.0,
        "import_graph must default to 0.0 (ADR-003 gated)"
    );
    assert_eq!(
        w.dir_proximity, 0.0,
        "dir_proximity must default to 0.0 (ADR-003 gated)"
    );
    assert_eq!(
        w.structural_coupling, 0.0,
        "structural_coupling must default to 0.0 (ADR-003 gated)"
    );
}

#[test]
fn test_default_validates_ok() {
    // AC1: validate() must return Ok for the default profile.
    assert!(
        CompositeWeights6::default().validate().is_ok(),
        "default weights must pass validate()"
    );
}

// ============================================================================
// AC1 — validate() accepts all-zero weights (legal: every signal disabled)
// ============================================================================

#[test]
fn test_all_zero_weights_validates_ok() {
    let w = CompositeWeights6 {
        lexical: 0.0,
        ast: 0.0,
        temporal: 0.0,
        import_graph: 0.0,
        dir_proximity: 0.0,
        structural_coupling: 0.0,
    };
    assert!(
        w.validate().is_ok(),
        "all-zero weights are valid (zero contribution, not an error)"
    );
}

// ============================================================================
// AC5 — Invalid weights rejected (NaN, inf, negative) — NEGATIVE
// ============================================================================

#[test]
fn test_validate_rejects_nan_lexical() {
    let w = CompositeWeights6 {
        lexical: f64::NAN,
        ..Default::default()
    };
    let err = w.validate().unwrap_err();
    assert!(
        matches!(err, SearchError::InvalidQuery(_)),
        "NaN lexical must return InvalidQuery, got {err:?}"
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("lexical"),
        "error message must name the offending field: {msg}"
    );
    assert!(msg.contains("NaN"), "error message must mention NaN: {msg}");
}

#[test]
fn test_validate_rejects_inf_ast() {
    let w = CompositeWeights6 {
        ast: f64::INFINITY,
        ..Default::default()
    };
    let err = w.validate().unwrap_err();
    assert!(matches!(err, SearchError::InvalidQuery(_)));
    let msg = format!("{err}");
    assert!(msg.contains("ast"), "must mention field 'ast': {msg}");
    assert!(msg.contains("infinite"), "must mention 'infinite': {msg}");
}

#[test]
fn test_validate_rejects_neg_inf_temporal() {
    let w = CompositeWeights6 {
        temporal: f64::NEG_INFINITY,
        ..Default::default()
    };
    let err = w.validate().unwrap_err();
    assert!(matches!(err, SearchError::InvalidQuery(_)));
    let msg = format!("{err}");
    assert!(
        msg.contains("temporal"),
        "must mention field 'temporal': {msg}"
    );
    assert!(msg.contains("infinite"), "must mention 'infinite': {msg}");
}

#[test]
fn test_validate_rejects_negative_import_graph() {
    let w = CompositeWeights6 {
        import_graph: -0.001,
        ..Default::default()
    };
    let err = w.validate().unwrap_err();
    assert!(matches!(err, SearchError::InvalidQuery(_)));
    let msg = format!("{err}");
    assert!(
        msg.contains("import_graph"),
        "must mention field 'import_graph': {msg}"
    );
    assert!(msg.contains("negative"), "must mention 'negative': {msg}");
}

#[test]
fn test_validate_rejects_negative_dir_proximity() {
    let w = CompositeWeights6 {
        dir_proximity: -1.0,
        ..Default::default()
    };
    let err = w.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("dir_proximity"), "{msg}");
}

#[test]
fn test_validate_rejects_negative_structural_coupling() {
    let w = CompositeWeights6 {
        structural_coupling: -0.5,
        ..Default::default()
    };
    let err = w.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("structural_coupling"), "{msg}");
}

// ============================================================================
// No panic in validate() — engineering rule (Result, never throw)
// ============================================================================

#[test]
fn test_validate_never_panics_on_any_f64() {
    // Construct extreme cases — none must panic; all must return a Result.
    let cases = [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -0.0,
        f64::MIN,
        f64::MAX,
        f64::EPSILON,
    ];
    for val in cases {
        let w = CompositeWeights6 {
            lexical: val,
            ..Default::default()
        };
        // Must not panic — Result can be Ok or Err.
        let _ = w.validate();
    }
}

// ============================================================================
// parse_weights_flag — AC5 (CLI weights parsing)
// ============================================================================

#[test]
fn test_parse_weights_flag_valid() {
    let w = CompositeWeights6::parse_weights_flag("0.5,0.3,0.2").unwrap();
    assert_eq!(w.lexical, 0.5);
    assert_eq!(w.ast, 0.3);
    assert_eq!(w.temporal, 0.2);
    // Extended signals stay at defaults.
    assert_eq!(w.import_graph, 0.0);
    assert_eq!(w.dir_proximity, 0.0);
    assert_eq!(w.structural_coupling, 0.0);
}

#[test]
fn test_parse_weights_flag_whitespace_trimmed() {
    // Values with leading/trailing whitespace must parse correctly.
    let w = CompositeWeights6::parse_weights_flag(" 0.5 , 0.3 , 0.2 ").unwrap();
    assert_eq!(w.lexical, 0.5);
    assert_eq!(w.temporal, 0.2);
}

#[test]
fn test_parse_weights_flag_too_few_parts_is_error() {
    let err = CompositeWeights6::parse_weights_flag("0.5,0.3").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("exactly 3"),
        "must mention '3 comma-separated values': {msg}"
    );
}

#[test]
fn test_parse_weights_flag_too_many_parts_is_error() {
    let err = CompositeWeights6::parse_weights_flag("0.5,0.3,0.2,0.1").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("exactly 3"), "{msg}");
}

#[test]
fn test_parse_weights_flag_non_numeric_is_error() {
    let err = CompositeWeights6::parse_weights_flag("abc,0.3,0.2").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not a valid number") || msg.contains("abc"),
        "must mention invalid value: {msg}"
    );
}

#[test]
fn test_parse_weights_flag_negative_is_error() {
    // AC5: negative weights must be rejected.
    let err = CompositeWeights6::parse_weights_flag("-0.5,0.3,0.2").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("negative") || msg.contains("lexical"),
        "must reject negative weight: {msg}"
    );
}

#[test]
fn test_parse_weights_flag_nan_string_is_error() {
    let err = CompositeWeights6::parse_weights_flag("NaN,0.3,0.2").unwrap_err();
    // NaN parses as f64 in Rust — validate() must catch it.
    let msg = format!("{err}");
    assert!(
        msg.contains("NaN") || msg.contains("lexical"),
        "must reject NaN: {msg}"
    );
}

#[test]
fn test_parse_weights_flag_inf_is_error() {
    // "inf" parses as f64::INFINITY in Rust.
    let err = CompositeWeights6::parse_weights_flag("inf,0.3,0.2").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("infinite") || msg.contains("lexical"),
        "must reject infinite: {msg}"
    );
}

#[test]
fn test_parse_weights_flag_zero_values_valid() {
    // All-zero is legal: every signal disabled.
    let w = CompositeWeights6::parse_weights_flag("0.0,0.0,0.0").unwrap();
    assert_eq!(w.lexical, 0.0);
    assert_eq!(w.ast, 0.0);
    assert_eq!(w.temporal, 0.0);
}
