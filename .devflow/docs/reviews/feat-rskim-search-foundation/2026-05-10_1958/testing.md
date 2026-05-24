# Testing Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10

## Issues in Your Changes (BLOCKING)

### HIGH

**SearchResult Deserialize added but no roundtrip deserialization test** - `crates/rskim-search/src/types.rs:144`
**Confidence**: 90%
- Problem: `SearchResult` gained `Deserialize` in this PR (changed from `#[derive(Debug, Clone, Serialize)]` to `#[derive(Debug, Clone, Serialize, Deserialize)]`), but both serialization tests (`test_search_result_serialization` at line 290 and `test_search_result_serialization_null_snippet` at line 314) only deserialize into `serde_json::Value`, not back into `SearchResult`. This means the `Deserialize` impl is untested. If a field rename, type change, or serde attribute breaks deserialization in the future, no test will catch it. The PR description states this crate provides types for `--json` CLI output, which implies results will be deserialized by consumers.
- Fix: Add a roundtrip test that serializes a `SearchResult` to JSON and deserializes it back into a `SearchResult`, then asserts field-level equality:
```rust
#[test]
fn test_search_result_serde_roundtrip() {
    let original = SearchResult {
        file_id: FileId(1),
        score: 0.95,
        line_range: 10..20,
        match_positions: vec![5..10],
        field: SearchField::FunctionSignature,
        snippet: Some("fn foo()".to_string()),
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: SearchResult = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.file_id, original.file_id);
    assert!((deserialized.score - original.score).abs() < f64::EPSILON);
    assert_eq!(deserialized.line_range, original.line_range);
    assert_eq!(deserialized.match_positions, original.match_positions);
    assert_eq!(deserialized.field, original.field);
    assert_eq!(deserialized.snippet, original.snippet);
}
```

### MEDIUM

**IndexStats has no test coverage** - `crates/rskim-search/src/types.rs:166-175`
**Confidence**: 85%
- Problem: `IndexStats` is a public struct with `Serialize` and `Deserialize` derives, an `Option<u64>` field, and 4 fields total. There is zero test coverage for its serialization behavior. Every other public type in this module (`FileId`, `SearchField`, `SearchQuery`, `SearchResult`, `TemporalFlags`, `SearchError`) has at least one test.
- Fix: Add a basic serialization test:
```rust
#[test]
fn test_index_stats_serialization() {
    let stats = IndexStats {
        file_count: 42,
        total_ngrams: 1_000_000,
        index_size_bytes: 4096,
        last_updated: Some(1_700_000_000),
    };
    let json = serde_json::to_string(&stats).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["file_count"], serde_json::json!(42));
    assert_eq!(v["total_ngrams"], serde_json::json!(1_000_000));
    assert_eq!(v["last_updated"], serde_json::json!(1_700_000_000));
}
```

**SearchField deserialization not tested (only serialization)** - `crates/rskim-search/src/types.rs:402-417`
**Confidence**: 82%
- Problem: `test_search_field_serialization` at line 402 verifies that `SearchField::TypeDefinition` serializes to `"type_definition"`, but never tests the reverse. The `#[serde(rename_all = "snake_case")]` attribute (added in this PR at line 47) affects both directions. If deserialization from API input or persisted data uses `SearchField`, a missing roundtrip test means rename regressions go undetected.
- Fix: Add a deserialization assertion to the existing test:
```rust
// In test_search_field_serialization
let deserialized: SearchField = serde_json::from_str("\"type_definition\"").unwrap();
assert_eq!(deserialized, SearchField::TypeDefinition);
let deserialized: SearchField = serde_json::from_str("\"function_signature\"").unwrap();
assert_eq!(deserialized, SearchField::FunctionSignature);
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Search CLI stub test does not verify `--help` flag path** - `crates/rskim/src/cmd/search.rs:84-89`
**Confidence**: 80%
- Problem: `test_search_help_returns_success` tests the empty-args path (line 87: `run(&[], ...)`), but the explicit `--help` flag path (line 30: `args.iter().any(|a| matches!(a.as_str(), "--help" | "-h"))`) is not tested. There are two distinct code paths in the `if` condition that both lead to `print_help()`, but only one is exercised.
- Fix: Add a test for the explicit flag:
```rust
#[test]
fn test_search_help_flag_returns_success() {
    let args = vec!["--help".to_string()];
    let result = run(&args, &TEST_ANALYTICS).unwrap();
    assert_eq!(result, ExitCode::SUCCESS);
}
```

## Pre-existing Issues (Not Blocking)

No pre-existing CRITICAL issues found in unchanged code.

## Suggestions (Lower Confidence)

- **TemporalFlags has no dedicated test** - `crates/rskim-search/src/types.rs:91-95` (Confidence: 65%) -- `TemporalFlags` derives `Default` but no test verifies the default value is `modified_within_days: None`. Low priority since it is tested indirectly via `test_search_query_new`.

- **FieldClassifier and LayerBuilder traits have no compile-time trait-object safety tests** - `crates/rskim-search/src/types.rs:201-224` (Confidence: 62%) -- The traits are defined with `Send`/`Sync` bounds and `where Self: Sized` on `build()`. A compile-time assertion like `fn _assert_object_safe(_: &dyn SearchLayer) {}` would catch accidental breakage of trait-object compatibility. The original `lib.rs` had these assertions but they were removed in this PR.

- **No negative deserialization test for SearchField** - `crates/rskim-search/src/types.rs:46-47` (Confidence: 60%) -- With `rename_all = "snake_case"`, deserializing the old PascalCase format (`"TypeDefinition"`) should fail. A test proving this rejection would document the breaking change and prevent regression.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite for the new `rskim-search` crate is solid for a Wave 0 types-only PR: 11 tests covering error conversion, serialization, display, field names, and query construction. The tests follow AAA structure, have clear naming, and validate behavior rather than implementation details. The main gap is the untested `Deserialize` capability -- since this PR specifically added `Deserialize` to `SearchResult` and `#[serde(rename_all = "snake_case")]` to `SearchField`, roundtrip tests should verify these work correctly before consumers rely on them.
