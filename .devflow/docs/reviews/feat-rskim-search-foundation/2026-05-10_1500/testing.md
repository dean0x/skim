# Testing Review Report

**Branch**: feat-rskim-search-foundation -> main
**Date**: 2026-05-10T15:00

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing error variant test coverage (4 of 5 SearchError variants untested)** - `crates/rskim-search/src/types.rs:229-249`
**Confidence**: 92%
- Problem: `SearchError` defines 5 variants (`Core`, `IndexCorrupted`, `InvalidQuery`, `FileNotFound`, `Io`), but only `Core` (via `From<SkimError>`) is tested. The `From<io::Error>` conversion is untested. The display strings for `IndexCorrupted`, `InvalidQuery`, and `FileNotFound` are untested. These are error paths that future consumers will rely on -- if a display format changes or a `From` impl breaks, nothing catches it.
- Fix: Add tests for remaining error variants and the `From<io::Error>` conversion:
```rust
#[test]
fn test_search_error_from_io() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
    let search_err = SearchError::from(io_err);
    let display = format!("{search_err}");
    assert!(display.contains("missing"), "Display should propagate IO message, got: {display}");
}

#[test]
fn test_search_error_display_variants() {
    let err = SearchError::IndexCorrupted("bad checksum".into());
    assert_eq!(format!("{err}"), "Index corrupted: bad checksum");

    let err = SearchError::InvalidQuery("empty".into());
    assert_eq!(format!("{err}"), "Invalid query: empty");

    let err = SearchError::FileNotFound(FileId(99));
    assert_eq!(format!("{err}"), "File not found in index: 99");
}
```

**No tests for CLI stub behavior** - `crates/rskim/src/cmd/search.rs:25-38`
**Confidence**: 85%
- Problem: The `search::run` function has two code paths (help output on empty/--help args, "not yet implemented" on other args) but zero tests. While it is a stub, it is wired into the dispatch table and executed by users. If the function signature or exit code semantics change, no test catches the regression. Other stubs in the codebase (e.g., dispatch sync guard test at `cmd/mod.rs:1047`) verify routing but not the stub's own behavior.
- Fix: Add basic tests verifying the two exit paths:
```rust
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::analytics::AnalyticsConfig;

    #[test]
    fn test_search_help_returns_success() {
        let analytics = AnalyticsConfig::disabled();
        let result = run(&[], &analytics).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_unimplemented_returns_failure() {
        let analytics = AnalyticsConfig::disabled();
        let args = vec!["fn parse".to_string()];
        let result = run(&args, &analytics).unwrap();
        assert_eq!(result, ExitCode::FAILURE);
    }
}
```

### MEDIUM

**SearchResult lacks roundtrip deserialization test** - `crates/rskim-search/src/types.rs:136-150`
**Confidence**: 85%
- Problem: `SearchResult` derives `Serialize` but not `Deserialize`, and the serialization test only checks two fields via `json.contains()` substring matching rather than verifying the complete serialized structure. The `line_range` (which is `Range<usize>`) serializes as `{"start":10,"end":20}` in serde_json -- this is a non-obvious serialization format that should be explicitly asserted. If `SearchResult` later adds `Deserialize`, the lack of a roundtrip test means deserialization bugs could slip through.
- Fix: Strengthen the serialization test to assert the full JSON structure:
```rust
#[test]
fn test_search_result_serialization() {
    let result = SearchResult {
        file_id: FileId(1),
        score: 0.95,
        line_range: 10..20,
        match_positions: vec![5..10],
        field: SearchField::FunctionSignature,
        snippet: None,
    };
    let json = serde_json::to_string(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["file_id"], 1);
    assert_eq!(parsed["score"], 0.95);
    assert_eq!(parsed["line_range"]["start"], 10);
    assert_eq!(parsed["line_range"]["end"], 20);
    assert_eq!(parsed["match_positions"][0]["start"], 5);
    assert_eq!(parsed["match_positions"][0]["end"], 10);
    assert_eq!(parsed["field"], "FunctionSignature");
    assert!(parsed["snippet"].is_null());
}
```

**No edge-case tests for SearchQuery** - `crates/rskim-search/src/types.rs:114-126`
**Confidence**: 80%
- Problem: `SearchQuery::new` is tested once with a normal string `"test"`. There are no tests for edge cases: empty string queries, very long strings, or queries with filters set. Since `SearchQuery` is the primary input to the entire search pipeline, boundary validation tests are important to document expected behavior.
- Fix: Add edge-case tests:
```rust
#[test]
fn test_search_query_empty_string() {
    let q = SearchQuery::new("");
    assert_eq!(q.text, "");
}

#[test]
fn test_search_query_with_filters() {
    let mut q = SearchQuery::new("fn parse");
    q.lang = Some(rskim_core::Language::Rust);
    q.limit = Some(10);
    q.offset = Some(5);
    assert_eq!(q.text, "fn parse");
    assert_eq!(q.lang, Some(rskim_core::Language::Rust));
    assert_eq!(q.limit, Some(10));
    assert_eq!(q.offset, Some(5));
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No test for FileId ordering semantics** - `crates/rskim-search/src/types.rs:24` (Confidence: 65%) -- `FileId` derives `PartialOrd` and `Ord` but no test verifies the ordering is based on the inner `u32`. If used as a `BTreeMap` key, incorrect ordering would be a subtle bug.

- **SearchField deserialization untested** - `crates/rskim-search/src/types.rs:41` (Confidence: 70%) -- `SearchField` derives `Deserialize` but only serialization is tested. A roundtrip test (`serialize -> deserialize -> assert_eq`) would verify both directions.

- **test_public_api_accessible is a compile-time check posing as a runtime test** - `crates/rskim-search/src/lib.rs:22-38` (Confidence: 65%) -- The test creates values with `let _` bindings and defines unused `fn _assert_*` functions. These are compile-time checks that would catch missing re-exports. This is a valid pattern for API surface tests, but it could be more clearly documented as such and the trait assertion functions could use `const _: () = { ... }` blocks at module level instead.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 5/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The test suite covers 8 tests across the new `rskim-search` crate, which is a reasonable starting point for a foundation/types-only crate. Tests follow good patterns: they use Arrange-Act-Assert structure, test observable behavior (Display, Serialize, From conversions), and avoid mocking. The `#[allow(clippy::unwrap_used)]` scoping to test modules is a clean pattern.

However, coverage gaps are significant for a library that will serve as the foundation for the search pipeline:

1. **Error coverage is incomplete** -- 4 of 5 `SearchError` variants lack any test, including the `From<io::Error>` conversion that future I/O layers will depend on.
2. **CLI stub has zero tests** despite being wired into the dispatch table and reachable by users.
3. **Serialization tests are shallow** -- substring matching (`json.contains`) rather than structural assertions, and `Range<usize>` serialization format is unverified.
4. **No boundary/edge-case tests** for the primary query input type.

The score of 5 reflects a crate that has tests (positive) but leaves meaningful behavioral contracts unverified (negative). For a v0.1.0 foundation crate, the blocking issues should be addressed before merge to establish the right testing baseline for future layers built on top.
