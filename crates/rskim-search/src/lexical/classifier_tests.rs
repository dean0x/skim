//! Tests for classify_source().

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    // We use JSON so this stays fast even at 100 MiB; the format-specific
    // scanner classifies without tree-sitter overhead.
    let at_limit = " ".repeat(MAX_SOURCE_BYTES);
    let result = classify_source(&at_limit, rskim_core::Language::Json);
    // The size guard must not fire at exactly the limit — any result other
    // than FileTooLarge (Ok or a scanner error) is acceptable here.
    if let Err(crate::SearchError::FileTooLarge { .. }) = result {
        panic!("MAX_SOURCE_BYTES itself must not trigger FileTooLarge");
    } // Ok or a parser error — both are acceptable here.
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

// -----------------------------------------------------------------------
// AD-411-1: context-aware identifier classification
// OD5: all 14 tree-sitter grammars have declaration-name fixtures
// -----------------------------------------------------------------------

/// Return the field assigned to the byte at `pos` in `ranges`.
fn field_at_byte(ranges: &[(std::ops::Range<usize>, SearchField)], pos: usize) -> SearchField {
    for (range, field) in ranges {
        if range.contains(&pos) {
            return *field;
        }
    }
    SearchField::Other
}

// --- Rust ---------------------------------------------------------------

/// Rust `fn compute()` — the declaration name "compute" must be in
/// FunctionSignature (AD-411-1 priority-4 path).
#[test]
fn test_411_rust_fn_def_name_is_fnsig() {
    // "fn " = 3 bytes → "compute" starts at byte 3
    let source = "fn compute() {}";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 3); // first byte of "compute"
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "Rust fn name should be FunctionSignature; got {field:?}"
    );
}

/// Rust call-site identifier (inside a function body) must be FunctionBody
/// (AD-411-1 non-declaration parent path).
#[test]
fn test_411_rust_call_site_is_fnbody() {
    // "fn main() { " = 12 bytes → "compute" in body starts at byte 12
    let source = "fn main() { compute() }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 12); // first byte of call-site "compute"
    assert_eq!(
        field,
        SearchField::FunctionBody,
        "Rust call site should be FunctionBody; got {field:?}"
    );
}

/// Rust `const MAX: u32 = 100;` — the declaration name "MAX" must be in
/// FunctionSignature (AD-411-1 + AD-411-6 option a: const names → FnSig).
#[test]
fn test_411_rust_const_def_name_is_fnsig() {
    // "const " = 6 bytes → "MAX" starts at byte 6
    let source = "const MAX: u32 = 100;";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 6); // first byte of "MAX"
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "Rust const name should be FunctionSignature (AD-411-6 option a); got {field:?}"
    );
}

/// Non-regression: the Rust struct name should still be TypeDefinition
/// (existing behaviour, not broken by AD-411-1).
#[test]
fn test_411_rust_struct_name_still_typedefinition() {
    // "struct " = 7 bytes → "Foo" starts at byte 7
    let source = "struct Foo { x: u32 }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 7); // first byte of "Foo"
    assert_eq!(
        field,
        SearchField::TypeDefinition,
        "Rust struct name should be TypeDefinition; got {field:?}"
    );
}

// --- TypeScript ---------------------------------------------------------

/// TS function declaration name → FunctionSignature.
#[test]
fn test_411_typescript_fn_def_name_is_fnsig() {
    // "function " = 9 bytes → "greet" starts at byte 9
    let source = "function greet(): void {}";
    let ranges = classify_source(source, rskim_core::Language::TypeScript).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 9);
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "TS function name should be FunctionSignature; got {field:?}"
    );
}

/// TS call site inside a function body → FunctionBody.
#[test]
fn test_411_typescript_call_site_is_fnbody() {
    // "function main(): void { " = 24 bytes → "greet" starts at byte 24
    let source = "function main(): void { greet() }";
    let ranges = classify_source(source, rskim_core::Language::TypeScript).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 24);
    assert_ne!(
        field,
        SearchField::FunctionSignature,
        "TS call site should not be FunctionSignature; got {field:?}"
    );
    // FunctionBody OR Other (either is acceptable — the key is it's NOT a def tier)
    assert!(
        matches!(field, SearchField::FunctionBody | SearchField::Other),
        "TS call site should be FunctionBody or Other; got {field:?}"
    );
}

// --- JavaScript ---------------------------------------------------------

/// JS function declaration name → FunctionSignature.
#[test]
fn test_411_javascript_fn_def_name_is_fnsig() {
    // "function " = 9 bytes → "greet" starts at byte 9
    let source = "function greet() {}";
    let ranges = classify_source(source, rskim_core::Language::JavaScript).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 9);
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "JS function name should be FunctionSignature; got {field:?}"
    );
}

// --- Python -------------------------------------------------------------

/// Python `def greet():` — name identifier → FunctionSignature.
#[test]
fn test_411_python_fn_def_name_is_fnsig() {
    // "def " = 4 bytes → "greet" starts at byte 4
    let source = "def greet(): pass";
    let ranges = classify_source(source, rskim_core::Language::Python).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 4);
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "Python def name should be FunctionSignature; got {field:?}"
    );
}

// --- Go -----------------------------------------------------------------

/// Go `func greet() {}` — name identifier → FunctionSignature.
#[test]
fn test_411_go_fn_def_name_is_fnsig() {
    // "func " = 5 bytes → "greet" starts at byte 5
    let source = "func greet() {}";
    let ranges = classify_source(source, rskim_core::Language::Go).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 5);
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "Go func name should be FunctionSignature; got {field:?}"
    );
}

// --- Java ---------------------------------------------------------------

/// Java method declaration name → FunctionSignature (OD5).
#[test]
fn test_411_java_method_def_name_is_not_fnbody() {
    let source = "class Foo { void bar() {} }";
    let ranges = classify_source(source, rskim_core::Language::Java).unwrap();
    assert_contiguous(&ranges, source.len());
    // "void " = 5 bytes inside "class Foo { " (12) → "bar" at byte 12+5=17
    let field = field_at_byte(&ranges, 17);
    assert_ne!(
        field,
        SearchField::FunctionBody,
        "Java method name should not be FunctionBody; got {field:?}"
    );
}

// --- C ------------------------------------------------------------------

/// C function declarator name — falls to SymbolName (degradation, OD4).
/// The C grammar uses `declarator:` field (not `name:`), so the name-child
/// receives SymbolName, not FunctionBody.
#[test]
fn test_411_c_fn_def_name_is_not_fnbody() {
    let source = "int add(int a) { return a; }";
    let ranges = classify_source(source, rskim_core::Language::C).unwrap();
    assert_contiguous(&ranges, source.len());
    // "int " = 4 bytes → "add" starts at byte 4
    let field = field_at_byte(&ranges, 4);
    assert_ne!(
        field,
        SearchField::FunctionBody,
        "C function name should not be FunctionBody (should be SymbolName); got {field:?}"
    );
}

/// C call site inside a function body → FunctionBody.
#[test]
fn test_411_c_call_site_is_fnbody() {
    // "int main() { " = 13 bytes → "add" at byte 13
    let source = "int main() { add(1); return 0; }";
    let ranges = classify_source(source, rskim_core::Language::C).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 13);
    assert_eq!(
        field,
        SearchField::FunctionBody,
        "C call site should be FunctionBody; got {field:?}"
    );
}

// --- C++ ----------------------------------------------------------------

/// C++ function name uses `declarator:` field → SymbolName (degradation, OD4).
#[test]
fn test_411_cpp_fn_def_name_is_not_fnbody() {
    let source = "int add(int a) { return a; }";
    let ranges = classify_source(source, rskim_core::Language::Cpp).unwrap();
    assert_contiguous(&ranges, source.len());
    // "int " = 4 bytes → "add" starts at byte 4
    let field = field_at_byte(&ranges, 4);
    assert_ne!(
        field,
        SearchField::FunctionBody,
        "C++ function name should not be FunctionBody; got {field:?}"
    );
}

// --- C# -----------------------------------------------------------------

/// C# method declaration name → not FunctionBody (OD5).
#[test]
fn test_411_csharp_method_def_name_is_not_fnbody() {
    let source = "class Foo { void Bar() {} }";
    let ranges = classify_source(source, rskim_core::Language::CSharp).unwrap();
    assert_contiguous(&ranges, source.len());
    // "void " = 5 bytes inside "class Foo { " (12) → "Bar" at byte 17
    let field = field_at_byte(&ranges, 17);
    assert_ne!(
        field,
        SearchField::FunctionBody,
        "C# method name should not be FunctionBody; got {field:?}"
    );
}

// --- Ruby ---------------------------------------------------------------

/// Ruby `def greet` — name identifier → FunctionSignature (OD5).
#[test]
fn test_411_ruby_method_def_name_is_fnsig() {
    // "def " = 4 bytes → "greet" starts at byte 4
    let source = "def greet\nend\n";
    let ranges = classify_source(source, rskim_core::Language::Ruby).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 4);
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "Ruby def name should be FunctionSignature; got {field:?}"
    );
}

// --- SQL ----------------------------------------------------------------

/// SQL `CREATE TABLE users` — the table name must not be FunctionBody (OD5).
/// The exact field depends on whether the grammar uses `name:` or another
/// field for the table identifier; at minimum it should not be a call-site tier.
#[test]
fn test_411_sql_table_def_name_is_not_fnbody() {
    let source = "CREATE TABLE users (id INT)";
    let ranges = classify_source(source, rskim_core::Language::Sql).unwrap();
    assert_contiguous(&ranges, source.len());
    // "CREATE TABLE " = 13 bytes → "users" starts at byte 13
    let field = field_at_byte(&ranges, 13);
    assert_ne!(
        field,
        SearchField::FunctionBody,
        "SQL table name should not be FunctionBody; got {field:?}"
    );
}

// --- Kotlin -------------------------------------------------------------

/// Kotlin `fun greet()` — name identifier → FunctionSignature (OD5).
#[test]
fn test_411_kotlin_fn_def_name_is_fnsig() {
    // "fun " = 4 bytes → "greet" starts at byte 4
    let source = "fun greet(): Unit {}";
    let ranges = classify_source(source, rskim_core::Language::Kotlin).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 4);
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "Kotlin fun name should be FunctionSignature; got {field:?}"
    );
}

// --- Swift --------------------------------------------------------------

/// Swift `func greet()` — name identifier → FunctionSignature (OD5).
#[test]
fn test_411_swift_fn_def_name_is_fnsig() {
    // "func " = 5 bytes → "greet" starts at byte 5
    let source = "func greet() {}";
    let ranges = classify_source(source, rskim_core::Language::Swift).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 5);
    assert_eq!(
        field,
        SearchField::FunctionSignature,
        "Swift func name should be FunctionSignature; got {field:?}"
    );
}

// --- Markdown -----------------------------------------------------------

/// Markdown heading → TypeDefinition (non-regression for classify_markdown).
/// Markdown is handled by a format-specific classifier, not the tree-sitter
/// identifier path, so this tests the Markdown path remains unaffected (OD5).
#[test]
fn test_411_markdown_heading_is_type_definition() {
    let source = "# Heading\n";
    let ranges = classify_source(source, rskim_core::Language::Markdown).unwrap();
    assert_contiguous(&ranges, source.len());
    let has_typedef = ranges
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_typedef,
        "Markdown heading should produce TypeDefinition; got: {ranges:?}"
    );
}

// -----------------------------------------------------------------------
// AD-411-1 container rule: class/module names must be TypeDefinition
// (cross-language consistency fix for priority-2 container kinds)
//
// Before the fix, class_declaration (TS/JS/Java/C#) and class_specifier (C++)
// had priority 2 in node_kind_info — below the `>= 3` guard in
// map_identifier_to_field — so their name-child identifiers fell through to
// FunctionBody (call-site tier), making `class Foo {}` rank at the same level
// as a call site.  That contradicts Rust `struct Foo` (struct_item = priority 5
// → TypeDefinition) and Python `class Foo` (class_definition = priority 5 →
// TypeDefinition) for semantically-equivalent constructs.
// (applies ADR-001: fix immediately; applies ADR-007: dog-food quality gate)
// -----------------------------------------------------------------------

/// TS `class Foo {}` — the class name "Foo" must be TypeDefinition, NOT FunctionBody.
/// Before the fix, class_declaration's priority-2 rating caused "Foo" to fall
/// through the `>= 3` guard in map_identifier_to_field and land at FunctionBody.
#[test]
fn test_411_typescript_class_def_name_is_typedefinition() {
    // "class " = 6 bytes → "Foo" starts at byte 6
    let source = "class Foo {}";
    let ranges = classify_source(source, rskim_core::Language::TypeScript).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 6);
    assert_eq!(
        field,
        SearchField::TypeDefinition,
        "TS class name must be TypeDefinition (was FunctionBody before container-rule fix); \
         got {field:?}\nranges: {ranges:?}"
    );
}

/// JS `class Foo {}` — the class name "Foo" must be TypeDefinition.
#[test]
fn test_411_javascript_class_def_name_is_typedefinition() {
    // "class " = 6 bytes → "Foo" starts at byte 6
    let source = "class Foo {}";
    let ranges = classify_source(source, rskim_core::Language::JavaScript).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 6);
    assert_eq!(
        field,
        SearchField::TypeDefinition,
        "JS class name must be TypeDefinition; got {field:?}\nranges: {ranges:?}"
    );
}

/// Java `class Foo {}` — the class name "Foo" must be TypeDefinition.
/// Before the fix: class_declaration (priority 2) → name identifier → FunctionBody.
#[test]
fn test_411_java_class_def_name_is_typedefinition() {
    // "class " = 6 bytes → "Foo" starts at byte 6
    let source = "class Foo {}";
    let ranges = classify_source(source, rskim_core::Language::Java).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 6);
    assert_eq!(
        field,
        SearchField::TypeDefinition,
        "Java class name must be TypeDefinition; got {field:?}\nranges: {ranges:?}"
    );
}

/// C++ `class Foo {};` — the class name "Foo" must be TypeDefinition.
/// C++ uses class_specifier (not class_declaration) for class definitions.
/// Before the fix: class_specifier (priority 2) → name type_identifier → FunctionBody.
#[test]
fn test_411_cpp_class_def_name_is_typedefinition() {
    // "class " = 6 bytes → "Foo" starts at byte 6
    let source = "class Foo {};";
    let ranges = classify_source(source, rskim_core::Language::Cpp).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 6);
    assert_eq!(
        field,
        SearchField::TypeDefinition,
        "C++ class name must be TypeDefinition (via class_specifier); \
         got {field:?}\nranges: {ranges:?}"
    );
}

/// C# `class Foo {}` — the class name "Foo" must be TypeDefinition.
/// Before the fix: class_declaration (priority 2) → name identifier → FunctionBody.
#[test]
fn test_411_csharp_class_def_name_is_typedefinition() {
    // "class " = 6 bytes → "Foo" starts at byte 6
    let source = "class Foo {}";
    let ranges = classify_source(source, rskim_core::Language::CSharp).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 6);
    assert_eq!(
        field,
        SearchField::TypeDefinition,
        "C# class name must be TypeDefinition; got {field:?}\nranges: {ranges:?}"
    );
}

/// Non-regression: within a Java file both `class Foo` and `interface Bar` must be
/// at TypeDefinition or above.  interface_declaration already has priority 5
/// (TypeDefinition), so this verifies that adding the container rule for
/// class_declaration does not accidentally lower the interface name rank.
#[test]
fn test_411_java_class_and_interface_both_typedefinition() {
    // class at byte 0: "class " = 6 → "Foo" at 6
    let source_class = "class Foo {}";
    let ranges_class = classify_source(source_class, rskim_core::Language::Java).unwrap();
    let class_field = field_at_byte(&ranges_class, 6);

    // interface at byte 0: "interface " = 10 → "Bar" at 10
    let source_iface = "interface Bar {}";
    let ranges_iface = classify_source(source_iface, rskim_core::Language::Java).unwrap();
    let iface_field = field_at_byte(&ranges_iface, 10);

    assert_eq!(
        class_field,
        SearchField::TypeDefinition,
        "Java class name must be TypeDefinition; got {class_field:?}"
    );
    assert_eq!(
        iface_field,
        SearchField::TypeDefinition,
        "Java interface name must be TypeDefinition; got {iface_field:?}"
    );
    // Both should be equal — cross-language consistency mandated by ADR-007.
    assert_eq!(
        class_field, iface_field,
        "Java `class` and `interface` names must rank at the same tier \
         (both TypeDefinition); got class={class_field:?}, interface={iface_field:?}"
    );
}

/// Non-regression: TS `interface Foo {}` uses interface_declaration (priority 5 →
/// TypeDefinition) which was already correct.  Verify the container-rule addition
/// does not disturb the existing priority-5 path.
#[test]
fn test_411_typescript_interface_def_name_still_typedefinition() {
    // "interface " = 10 bytes → "Foo" at byte 10
    let source = "interface Foo {}";
    let ranges = classify_source(source, rskim_core::Language::TypeScript).unwrap();
    assert_contiguous(&ranges, source.len());
    let field = field_at_byte(&ranges, 10);
    assert_eq!(
        field,
        SearchField::TypeDefinition,
        "TS interface name must remain TypeDefinition (priority-5 path unaffected); \
         got {field:?}\nranges: {ranges:?}"
    );
}

// -----------------------------------------------------------------------
// AC14: code definition outranks Markdown heading (OD3)
// Verified at field level here; the scoring-tier proof is in config_tests.
// -----------------------------------------------------------------------

/// A Rust const definition name is stamped FunctionSignature (boost 8.0 in
/// for_exact_symbol), while a Markdown heading is TypeDefinition (boost 4.0).
/// This field-level fixture proves AC14: code def > doc heading on the
/// exact-symbol path — the score arithmetic follows from the boosts.
#[test]
fn test_411_rust_const_def_field_outranks_markdown_heading_field() {
    use crate::lexical::BM25FConfig;

    // Rust const def → FunctionSignature (index 1)
    let source_rust = "const TOKEN: u32 = 1;";
    let ranges = classify_source(source_rust, rskim_core::Language::Rust).unwrap();
    // "const " = 6 bytes → "TOKEN" starts at byte 6
    let rust_field = field_at_byte(&ranges, 6);
    assert_eq!(
        rust_field,
        SearchField::FunctionSignature,
        "Rust const name should be FunctionSignature"
    );

    // Markdown heading → TypeDefinition (index 0)
    let source_md = "# TOKEN\n";
    let md_ranges = classify_source(source_md, rskim_core::Language::Markdown).unwrap();
    // "# " = 2 bytes → "TOKEN" heading content; any TypeDefinition byte is enough
    let md_has_typedef = md_ranges
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(md_has_typedef, "Markdown heading should be TypeDefinition");

    // Verify the BM25FConfig::for_exact_symbol() boost ordering ensures code > doc.
    let cfg = BM25FConfig::for_exact_symbol();
    let fnsig_boost = cfg.field_boosts[SearchField::FunctionSignature.discriminant() as usize];
    let typedef_boost = cfg.field_boosts[SearchField::TypeDefinition.discriminant() as usize];
    assert!(
        fnsig_boost > typedef_boost,
        "FunctionSignature boost ({fnsig_boost}) must exceed TypeDefinition boost ({typedef_boost}) \
         so code def scores above Markdown heading (OD3 / AC14)"
    );
}
