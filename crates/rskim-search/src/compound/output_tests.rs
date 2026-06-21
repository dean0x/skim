//! Unit tests for `compound::output` — formatter purity and exact-byte assertions.
//!
//! All tests use a `Vec<u8>` sink and zero filesystem access, satisfying the
//! AC-API1 purity contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{AstResult, format_ast_json, format_ast_text};

// ============================================================================
// Helpers
// ============================================================================

fn make_result_with_line(path: &str, score: f64, line: u32, snippet: &str) -> AstResult {
    AstResult::ast_only(
        path.to_string(),
        score,
        Some(line),
        Some(snippet.to_string()),
    )
}

fn make_result_no_line(path: &str, score: f64) -> AstResult {
    AstResult::ast_only(path.to_string(), score, None, None)
}

// ============================================================================
// AC-API1: formatter purity and empty-slice handling
// ============================================================================

#[test]
fn format_text_empty_slice_writes_no_match_line() {
    let mut buf: Vec<u8> = Vec::new();
    format_ast_text(&[], "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(
        out.contains("no files match"),
        "empty slice must write 'no files match'; got: {out}"
    );
    assert!(
        !out.is_empty(),
        "empty slice must produce non-empty output (AC-F8)"
    );
}

#[test]
fn format_json_empty_slice_writes_valid_json_total_zero() {
    let mut buf: Vec<u8> = Vec::new();
    format_ast_json(&[], "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).expect("must be valid JSON");
    assert_eq!(v["mode"], "ast", "mode must be 'ast'");
    assert_eq!(v["total"], 0, "total must be 0 for empty slice");
    assert!(
        v["results"].as_array().unwrap().is_empty(),
        "results must be empty array"
    );
}

#[test]
fn format_text_returns_ok_for_empty_and_populated_slices() {
    // AC-API1: formatter returns Ok(()) for any well-formed input.
    let mut buf: Vec<u8> = Vec::new();
    let r = format_ast_text(&[], "x", "", &mut buf);
    assert!(r.is_ok(), "empty slice must return Ok");

    let results = vec![make_result_with_line("src/foo.rs", 2.5, 10, "  fn foo() {")];
    let mut buf2: Vec<u8> = Vec::new();
    let r2 = format_ast_text(&results, "x", "", &mut buf2);
    assert!(r2.is_ok(), "populated slice must return Ok");
}

#[test]
fn format_json_returns_ok_for_empty_and_populated_slices() {
    let mut buf: Vec<u8> = Vec::new();
    assert!(format_ast_json(&[], "x", "", &mut buf).is_ok());
    let results = vec![make_result_no_line("src/bar.rs", 1.2)];
    let mut buf2: Vec<u8> = Vec::new();
    assert!(format_ast_json(&results, "x", "", &mut buf2).is_ok());
}

// ============================================================================
// AC-F1: terminal output with recovered line
// ============================================================================

#[test]
fn format_text_with_line_has_colon_suffix_and_snippet() {
    let results = vec![make_result_with_line(
        "src/auth.rs",
        0.87,
        42,
        "  fn handle_auth() {",
    )];
    let mut buf: Vec<u8> = Vec::new();
    format_ast_text(&results, "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();

    // Must contain path:line suffix (AC-F1 POSITIVE).
    assert!(
        out.contains("src/auth.rs:42"),
        "text output must contain 'src/auth.rs:42'; got:\n{out}"
    );
    // Must contain the snippet line.
    assert!(
        out.contains("fn handle_auth()"),
        "text output must contain snippet; got:\n{out}"
    );
    // Must contain the score.
    assert!(
        out.contains("0.870"),
        "text output must contain score; got:\n{out}"
    );
}

// ============================================================================
// AC-F2: fail-soft degrade — no :line on degraded rows, no :0 or :1 placeholder
// ============================================================================

#[test]
fn format_text_degraded_row_no_colon_line_suffix() {
    let results = vec![make_result_no_line("src/models/user.rs", 0.72)];
    let mut buf: Vec<u8> = Vec::new();
    format_ast_text(&results, "try-catch", "desc", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();

    // Must contain the path.
    assert!(
        out.contains("src/models/user.rs"),
        "must contain path; got:\n{out}"
    );
    // Must NOT have a :line suffix (AC-F2 NEGATIVE).
    assert!(
        !out.contains("src/models/user.rs:"),
        "degraded row MUST NOT have :line suffix (AC-F2); got:\n{out}"
    );
    // No fabricated :0 or :1 line number.
    assert!(
        !out.contains(":0"),
        "degraded row must not emit :0 placeholder; got:\n{out}"
    );
    assert!(
        !out.contains(":1"),
        "degraded row must not emit :1 placeholder; got:\n{out}"
    );
    // No snippet line for degraded rows.
    assert!(
        !out.contains("snippet"),
        "degraded row must not emit snippet text; got:\n{out}"
    );
}

// ============================================================================
// AC-F4: JSON additive + key absence on degraded rows
// ============================================================================

#[test]
fn format_json_with_line_has_line_and_snippet_keys() {
    let results = vec![make_result_with_line(
        "src/auth.rs",
        0.87,
        42,
        "  fn foo() {",
    )];
    let mut buf: Vec<u8> = Vec::new();
    format_ast_json(&results, "try-catch", "desc", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).expect("must be valid JSON");

    let first = &v["results"][0];
    assert!(
        first["line"].is_number() && first["line"].as_u64().unwrap() > 0,
        "line must be a positive integer; got: {first}"
    );
    assert!(
        first["snippet"].is_string(),
        "snippet must be present as a string; got: {first}"
    );
    assert_eq!(first["path"], "src/auth.rs", "path must match");
    assert!(first["score"].is_number(), "score must be a number");
}

#[test]
fn format_json_degraded_row_line_and_snippet_keys_absent() {
    // AC-F4 NEGATIVE: degraded row → line and snippet keys ABSENT (not null, not 0).
    let results = vec![make_result_no_line("src/models/user.rs", 0.72)];
    let mut buf: Vec<u8> = Vec::new();
    format_ast_json(&results, "try-catch", "desc", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).expect("must be valid JSON");

    let first = &v["results"][0];
    assert!(
        first.get("line").is_none(),
        "degraded row: 'line' key must be ABSENT; got: {first}"
    );
    assert!(
        first.get("snippet").is_none(),
        "degraded row: 'snippet' key must be ABSENT; got: {first}"
    );
    // path and score must still be present (AC-API4 back-compat).
    assert_eq!(first["path"], "src/models/user.rs");
    assert!(first["score"].is_number());
}

// ============================================================================
// AC-F5: layers_matched == ["ast"] on standalone AST-only
// ============================================================================

#[test]
fn ast_only_constructor_sets_layers_matched_to_ast() {
    let r = AstResult::ast_only("src/foo.rs".to_string(), 2.0, None, None);
    assert_eq!(
        r.layers_matched,
        vec!["ast"],
        "AC-F5: layers_matched must be [\"ast\"]"
    );
}

#[test]
fn ast_only_never_includes_lexical_or_temporal() {
    let r = AstResult::ast_only(
        "src/foo.rs".to_string(),
        2.0,
        Some(5),
        Some("x".to_string()),
    );
    assert!(
        !r.layers_matched.contains(&"lexical"),
        "AC-F5 NEGATIVE: ast_only must not include 'lexical'"
    );
    assert!(
        !r.layers_matched.contains(&"temporal"),
        "AC-F5 NEGATIVE: ast_only must not include 'temporal'"
    );
    assert!(
        !r.layers_matched.is_empty(),
        "layers_matched must not be empty"
    );
}

#[test]
fn format_json_layers_matched_is_present_on_every_row() {
    let results = vec![
        make_result_with_line("src/a.rs", 1.0, 5, "fn a() {}"),
        make_result_no_line("src/b.rs", 0.5),
    ];
    let mut buf: Vec<u8> = Vec::new();
    format_ast_json(&results, "try-catch", "", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();

    for (i, row) in v["results"].as_array().unwrap().iter().enumerate() {
        assert!(
            row.get("layers_matched").is_some(),
            "row {i}: layers_matched must be present on every row"
        );
        let arr = row["layers_matched"].as_array().unwrap();
        assert!(
            arr.iter().any(|x| x == "ast"),
            "row {i}: layers_matched must include 'ast'; got: {arr:?}"
        );
    }
}

// ============================================================================
// AC-F6: layers_matched for text+AST intersection
// ============================================================================

#[test]
fn lexical_ast_constructor_sets_layers_matched_to_lexical_and_ast() {
    let r = AstResult::lexical_ast(
        "src/foo.rs".to_string(),
        1.0,
        Some(3),
        Some("x".to_string()),
    );
    assert_eq!(
        r.layers_matched,
        vec!["lexical", "ast"],
        "AC-F6: text+ast result must have layers_matched == [\"lexical\",\"ast\"]"
    );
}

// ============================================================================
// AC-API4: back-compat — path and score keys retained
// ============================================================================

#[test]
fn format_json_path_and_score_always_present() {
    let results = vec![
        make_result_with_line("src/a.rs", 2.5, 10, "fn a() {}"),
        make_result_no_line("src/b.rs", 1.2),
    ];
    let mut buf: Vec<u8> = Vec::new();
    format_ast_json(&results, "try-catch", "", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();

    for (i, row) in v["results"].as_array().unwrap().iter().enumerate() {
        assert!(
            row["path"].is_string(),
            "row {i}: 'path' must be present and a string"
        );
        assert!(
            row["score"].is_number(),
            "row {i}: 'score' must be present and a number"
        );
    }
}

// ============================================================================
// Header formatting
// ============================================================================

#[test]
fn format_text_header_contains_pattern_name() {
    let mut buf: Vec<u8> = Vec::new();
    format_ast_text(&[], "nested-loop", "Nested loop", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(
        out.contains("AST pattern: nested-loop"),
        "header must contain pattern name; got:\n{out}"
    );
}

#[test]
fn format_text_header_without_description_has_no_em_dash() {
    let mut buf: Vec<u8> = Vec::new();
    format_ast_text(&[], "containment-query", "", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    // When description is empty, no em dash should appear.
    assert!(
        !out.contains(" — "),
        "empty description must not emit em-dash; got:\n{out}"
    );
}

#[test]
fn format_json_mode_is_always_ast() {
    let mut buf: Vec<u8> = Vec::new();
    format_ast_json(&[], "whatever", "", &mut buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8(buf).unwrap()).unwrap();
    assert_eq!(v["mode"], "ast");
}

// ============================================================================
// temporal field is absent when None (AC-API4 additive)
// ============================================================================

#[test]
fn format_json_temporal_absent_when_none() {
    let results = vec![make_result_no_line("src/foo.rs", 1.0)];
    let mut buf: Vec<u8> = Vec::new();
    format_ast_json(&results, "x", "", &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let first = &v["results"][0];
    assert!(
        first.get("temporal").is_none(),
        "temporal must be absent when None (additive, skip_serializing_if); got: {first}"
    );
}
