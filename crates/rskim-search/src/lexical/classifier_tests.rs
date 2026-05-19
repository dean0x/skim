//! Tests for classify_source().

#![allow(clippy::unwrap_used)]

use super::*;
use crate::SearchField;

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Assert that ranges cover the full source length without gaps or overlaps.
fn assert_contiguous(ranges: &[(std::ops::Range<usize>, SearchField)], source_len: usize) {
    if source_len == 0 {
        assert!(
            ranges.is_empty(),
            "empty source should produce empty ranges"
        );
        return;
    }
    let mut expected = 0usize;
    for (range, _) in ranges {
        assert_eq!(
            range.start, expected,
            "gap at {expected}: range starts at {}",
            range.start
        );
        assert!(range.end > range.start, "range must be non-empty");
        expected = range.end;
    }
    assert_eq!(
        expected, source_len,
        "ranges do not cover full source: covered {expected}, source len {source_len}"
    );
}

/// Assert that field_lengths_sum equals doc_length.
fn assert_field_lengths_sum(ranges: &[(std::ops::Range<usize>, SearchField)], source_len: usize) {
    let total: usize = ranges.iter().map(|(r, _)| r.end - r.start).sum();
    assert_eq!(
        total, source_len,
        "sum of range lengths {total} != source len {source_len}"
    );
}

// -----------------------------------------------------------------------
// Empty source
// -----------------------------------------------------------------------

#[test]
fn test_empty_source_returns_empty() {
    let ranges = classify_source("", rskim_core::Language::Rust).unwrap();
    assert!(ranges.is_empty(), "empty source should return empty vec");
}

// -----------------------------------------------------------------------
// Size limit guard
// -----------------------------------------------------------------------

#[test]
fn test_source_exceeding_limit_returns_error() {
    use crate::SearchError;

    // Build a source that is exactly one byte over the limit.
    // We use a byte string of spaces so allocation stays minimal in tests;
    // the limit check fires before any tree-sitter work.
    let oversized = " ".repeat(MAX_SOURCE_BYTES + 1);
    let err = classify_source(&oversized, rskim_core::Language::Rust)
        .expect_err("sources over MAX_SOURCE_BYTES must return Err");

    assert!(
        matches!(err, SearchError::FileTooLarge { size, limit }
            if size == MAX_SOURCE_BYTES + 1 && limit == MAX_SOURCE_BYTES),
        "expected FileTooLarge with correct size/limit, got: {err:?}"
    );
}

/// Verifies that a source of exactly MAX_SOURCE_BYTES is accepted, not rejected.
///
/// This test allocates ~100 MiB, which is excessive for a normal CI run.
/// Run it explicitly with `cargo test -- --ignored` when changing the size guard.
#[test]
#[ignore = "allocates 100 MiB — run explicitly with --ignored"]
fn test_source_at_limit_boundary_does_not_error() {
    // A source of exactly MAX_SOURCE_BYTES bytes must NOT be rejected.
    // We use JSON (non-tree-sitter) so this stays fast even at 100 MiB;
    // it returns a single Other range without touching the parser.
    let at_limit = " ".repeat(MAX_SOURCE_BYTES);
    let result = classify_source(&at_limit, rskim_core::Language::Json);
    // Json parser returns an error (unsupported for tree-sitter), but the
    // size guard must not fire — the error, if any, comes from the parser,
    // not from FileTooLarge.
    match result {
        Err(crate::SearchError::FileTooLarge { .. }) => {
            panic!("MAX_SOURCE_BYTES itself must not trigger FileTooLarge");
        }
        _ => {} // Ok or a parser error — both are acceptable here.
    }
}

// -----------------------------------------------------------------------
// Non-tree-sitter languages (JSON, YAML, TOML)
// -----------------------------------------------------------------------

/// JSON is now classified with format-specific field mapping (not single-Other).
/// Verifies contiguity and presence of structural fields.
#[test]
fn test_json_field_mapping_non_trivial() {
    let source = r#"{"key": "value"}"#;
    let ranges = classify_source(source, rskim_core::Language::Json).unwrap();
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
    // With format-specific classification, "key" must be SymbolName.
    let has_symbol = ranges.iter().any(|(_, f)| *f == SearchField::SymbolName);
    assert!(
        has_symbol,
        "JSON classify_source must produce SymbolName for key; got: {ranges:?}"
    );
}

/// YAML is now classified with format-specific field mapping (not single-Other).
/// Verifies contiguity and presence of structural fields.
#[test]
fn test_yaml_field_mapping_non_trivial() {
    let source = "key: value\n";
    let ranges = classify_source(source, rskim_core::Language::Yaml).unwrap();
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
    // "key" at indent-0 must be TypeDefinition.
    let has_type_def = ranges
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_type_def,
        "YAML classify_source must produce TypeDefinition for root key; got: {ranges:?}"
    );
}

/// TOML is now classified with format-specific field mapping (not single-Other).
/// Verifies contiguity and presence of structural fields.
#[test]
fn test_toml_field_mapping_non_trivial() {
    let source = "[package]\nname = \"skim\"\n";
    let ranges = classify_source(source, rskim_core::Language::Toml).unwrap();
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
    // [package] must be TypeDefinition.
    let has_type_def = ranges
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_type_def,
        "TOML classify_source must produce TypeDefinition for section; got: {ranges:?}"
    );
}

// -----------------------------------------------------------------------
// Rust language
// -----------------------------------------------------------------------

#[test]
fn test_rust_struct_contains_type_definition() {
    let source = "struct UserService { name: String }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    let has_type_def = ranges
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_type_def,
        "Rust struct should produce TypeDefinition field range; got: {ranges:?}"
    );
}

#[test]
fn test_rust_function_contains_function_signature() {
    let source = "fn compute(x: u32) -> u32 { x * 2 }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());

    let has_fn_sig = ranges
        .iter()
        .any(|(_, f)| *f == SearchField::FunctionSignature);
    assert!(
        has_fn_sig,
        "Rust function should produce FunctionSignature range; got: {ranges:?}"
    );
}

#[test]
fn test_non_overlapping_invariant() {
    let source = "struct Foo { fn bar() {} }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    // Verify no two ranges overlap.
    for i in 0..ranges.len().saturating_sub(1) {
        assert_eq!(
            ranges[i].0.end,
            ranges[i + 1].0.start,
            "ranges overlap or gap between index {i} and {}: {:?} vs {:?}",
            i + 1,
            ranges[i],
            ranges[i + 1]
        );
    }
}

#[test]
fn test_innermost_wins() {
    // A struct body contains field declarations — the struct itself should be
    // TypeDefinition, but interior identifier nodes might narrow to SymbolName.
    // The key property: every byte belongs to exactly one range.
    let source = "struct Point { x: f64, y: f64 }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());
    // At least TypeDefinition should appear somewhere.
    assert!(
        ranges
            .iter()
            .any(|(_, f)| *f == SearchField::TypeDefinition),
        "struct should have TypeDefinition range; got: {ranges:?}"
    );
}

#[test]
fn test_field_lengths_sum_equals_source_len_rust() {
    let source = "fn main() { let x: u32 = 42; }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_field_lengths_sum(&ranges, source.len());
}

// -----------------------------------------------------------------------
// Body block classification
// -----------------------------------------------------------------------

/// Body blocks (`{ ... }`) must be classified as FunctionBody, not
/// FunctionSignature. Before this fix, block/statement_block nodes returned
/// Other from map_priority_to_field and were overwritten by the parent
/// function node's FunctionSignature stamp, inflating FunctionSignature lengths.
#[test]
fn test_rust_function_body_classified_as_function_body() {
    // The block `{ x * 2 }` is the function body.
    let source = "fn compute(x: u32) -> u32 { x * 2 }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());

    let has_body = ranges.iter().any(|(_, f)| *f == SearchField::FunctionBody);
    assert!(
        has_body,
        "Rust function body should produce FunctionBody range; got: {ranges:?}"
    );
}

#[test]
fn test_typescript_function_body_classified_as_function_body() {
    let source = "function greet(name: string): string { return name; }\n";
    let ranges = classify_source(source, rskim_core::Language::TypeScript).unwrap();
    assert_contiguous(&ranges, source.len());

    let has_body = ranges.iter().any(|(_, f)| *f == SearchField::FunctionBody);
    assert!(
        has_body,
        "TypeScript function body should produce FunctionBody range; got: {ranges:?}"
    );
}

#[test]
fn test_c_compound_statement_classified_as_function_body() {
    let source = "int add(int a, int b) { return a + b; }\n";
    let ranges = classify_source(source, rskim_core::Language::C).unwrap();
    assert_contiguous(&ranges, source.len());

    let has_body = ranges.iter().any(|(_, f)| *f == SearchField::FunctionBody);
    assert!(
        has_body,
        "C compound_statement should produce FunctionBody range; got: {ranges:?}"
    );
}

// -----------------------------------------------------------------------
// Other languages
// -----------------------------------------------------------------------

#[test]
fn test_python_function_contains_function_signature() {
    let source = "def greet(name: str) -> str:\n    return name\n";
    let ranges = classify_source(source, rskim_core::Language::Python).unwrap();
    assert_contiguous(&ranges, source.len());
    let has_fn = ranges
        .iter()
        .any(|(_, f)| *f == SearchField::FunctionSignature);
    assert!(
        has_fn,
        "Python def should produce FunctionSignature range; got: {ranges:?}"
    );
}

#[test]
fn test_typescript_import_contains_import_export() {
    let source = "import { Component } from '@angular/core';\n";
    let ranges = classify_source(source, rskim_core::Language::TypeScript).unwrap();
    assert_contiguous(&ranges, source.len());
    let has_import = ranges.iter().any(|(_, f)| *f == SearchField::ImportExport);
    assert!(
        has_import,
        "TypeScript import should produce ImportExport range; got: {ranges:?}"
    );
}

#[test]
fn test_field_lengths_sum_multi_language() {
    let cases: &[(&str, rskim_core::Language)] = &[
        ("def f(x): return x", rskim_core::Language::Python),
        ("fn x() {}", rskim_core::Language::Rust),
        (
            "function y() { return 1; }",
            rskim_core::Language::JavaScript,
        ),
    ];
    for (source, lang) in cases {
        let ranges = classify_source(source, *lang).unwrap();
        assert_field_lengths_sum(&ranges, source.len());
    }
}
