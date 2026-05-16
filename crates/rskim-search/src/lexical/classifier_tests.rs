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
        assert!(ranges.is_empty(), "empty source should produce empty ranges");
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
// Non-tree-sitter languages (JSON, YAML, TOML)
// -----------------------------------------------------------------------

#[test]
fn test_json_classified_as_single_other() {
    let source = r#"{"key": "value"}"#;
    let ranges = classify_source(source, rskim_core::Language::Json).unwrap();
    assert_eq!(ranges.len(), 1, "JSON should return single range");
    assert_eq!(ranges[0].1, SearchField::Other, "JSON range should be Other");
    assert_eq!(ranges[0].0.start, 0);
    assert_eq!(ranges[0].0.end, source.len());
}

#[test]
fn test_yaml_classified_as_single_other() {
    let source = "key: value\n";
    let ranges = classify_source(source, rskim_core::Language::Yaml).unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].1, SearchField::Other);
}

#[test]
fn test_toml_classified_as_single_other() {
    let source = "[package]\nname = \"skim\"\n";
    let ranges = classify_source(source, rskim_core::Language::Toml).unwrap();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].1, SearchField::Other);
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
            ranges[i].0.end, ranges[i + 1].0.start,
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
        ranges.iter().any(|(_, f)| *f == SearchField::TypeDefinition),
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
    let has_import = ranges
        .iter()
        .any(|(_, f)| *f == SearchField::ImportExport);
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
        ("function y() { return 1; }", rskim_core::Language::JavaScript),
    ];
    for (source, lang) in cases {
        let ranges = classify_source(source, *lang).unwrap();
        assert_field_lengths_sum(&ranges, source.len());
    }
}
