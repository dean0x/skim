# Testing Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Stale comment in `test_source_at_limit_boundary_does_not_error`** - `crates/rskim-search/src/lexical/classifier_tests.rs:86-87`
**Confidence**: 85%
- Problem: The comment says "it returns a single Other range without touching the parser" for JSON, but after this PR, JSON is dispatched to `classify_json()` which runs a byte-level state machine -- no longer "without touching the parser." While the ignored test still passes (the size guard fires before format dispatch in the non-at-limit case), the comment is now misleading to future developers.
- Fix: Update the comment to reflect that JSON now runs through the format-specific classifier:
```rust
// We use JSON so this stays fast even at 100 MiB; the format-specific
// classifier scans bytes without tree-sitter overhead.
```

**Missing test: TOML multi-line triple-quoted strings** - `crates/rskim-search/src/fields/fields_tests.rs`
**Confidence**: 88%
- Problem: The production code in `serde_fields.rs:520-558` handles triple-quoted strings (`"""..."""` and `'''...'''`) with dedicated `scan_triple_quote` logic including escape handling and cross-line scanning. No test exercises this path. This is a non-trivial parser path with escape handling that could silently regress.
- Fix: Add at least one test per triple-quote variant:
```rust
#[test]
fn f_toml_07_triple_quoted_basic_string() {
    let source = "[pkg]\ndesc = \"\"\"\nA multi-line\ndescription\n\"\"\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert!(
        has_field(&ranges, SearchField::StringLiteral),
        "triple-quoted basic string should be StringLiteral; ranges: {ranges:?}"
    );
}

#[test]
fn f_toml_08_triple_quoted_literal_string() {
    let source = "[pkg]\npath = '''C:\\path\\to\\file'''\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert!(
        has_field(&ranges, SearchField::StringLiteral),
        "triple-quoted literal string should be StringLiteral; ranges: {ranges:?}"
    );
}
```

**Missing test: empty Markdown input** - `crates/rskim-search/src/fields/fields_tests.rs`
**Confidence**: 82%
- Problem: Empty source tests exist for JSON (C-04), YAML (C-05), and TOML (C-06), but not for Markdown. The `classify_markdown("")` path returns `Ok(Vec::new())` on line 44-46 of `markdown.rs`. Without a test, a regression removing the empty guard would go undetected.
- Fix: Add a contract test:
```rust
#[test]
fn c_11_empty_markdown_empty_vec() {
    let ranges = classify_markdown("").unwrap();
    assert!(
        ranges.is_empty(),
        "empty Markdown source should return empty Vec; got: {ranges:?}"
    );
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Missing test: YAML inline comment on value lines** - `crates/rskim-search/src/fields/serde_fields.rs:344-347`
**Confidence**: 80%
- Problem: The YAML scanner has a documented TODO about inline comment classification and explicitly chooses NOT to detect inline comments to avoid false positives with URLs. However, there is no test verifying that a URL value like `host: http://example.com` does NOT produce a false-positive Comment field. The `find_yaml_key_colon` function correctly handles URL colons, but the behavior is untested.
- Fix: Add a regression test:
```rust
#[test]
fn f_yaml_07_url_value_no_false_positive_comment() {
    let source = "url: http://example.com\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());
    // URL with `://` must not produce a false Comment field.
    assert!(
        !has_field(&ranges, SearchField::Comment),
        "URL value must not produce Comment; ranges: {ranges:?}"
    );
}
```

**Missing test: YAML single-quoted string value** - `crates/rskim-search/src/fields/serde_fields.rs:342`
**Confidence**: 80%
- Problem: The YAML scanner classifies single-quoted string values (`'value'`) as StringLiteral (line 342 checks for `b'\''`), but only double-quoted values are tested in F-YAML-03. Single-quote handling is an explicit code path that lacks coverage.
- Fix:
```rust
#[test]
fn f_yaml_08_single_quoted_string_value() {
    let source = "name: 'skim'\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());
    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("skim")),
        "single-quoted value should be StringLiteral; str texts: {str_texts:?}; ranges: {ranges:?}"
    );
}
```

## Pre-existing Issues (Not Blocking)

No pre-existing CRITICAL issues found in unchanged test code.

## Suggestions (Lower Confidence)

- **Missing test: TOML key with escaped `=` in quoted key name** - `crates/rskim-search/src/fields/serde_fields.rs:587-608` (Confidence: 65%) -- The `find_toml_eq_sign` function handles quoted keys but there is no test for a key like `"a=b" = "val"` to verify the `=` inside quotes is skipped.

- **Missing test: JSON with unicode escape sequences** - `crates/rskim-search/src/fields/serde_fields.rs:196-203` (Confidence: 70%) -- The `scan_json_string` function handles `\uXXXX` escapes, but no test exercises this path. A test like `{"key": "A"}` would verify the scanner does not break contiguity on unicode escapes.

- **Duplicate helper functions** - `crates/rskim-search/src/fields/fields_tests.rs:20-44` and `crates/rskim-search/src/lexical/classifier_tests.rs:13-44` (Confidence: 75%) -- The `assert_contiguous` and `assert_field_lengths_sum` helpers are duplicated between the two test files. Could be extracted to a shared test utility module, but this is an organizational preference, not a correctness issue.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Assessment

The test suite is well-structured with strong characteristics:

1. **Behavior-focused testing**: Tests verify observable outputs (field classifications, contiguity invariants) rather than internal implementation. The test names clearly describe expected behavior (e.g., `f_json_02_depth0_key_with_object_value_is_type_definition`).

2. **Contract testing pattern**: The C-01 through C-10 tests enforce structural invariants (sorted, non-overlapping, contiguous, infallible) across all scanners. This is the right approach for parsers.

3. **Integration tests through dispatch**: I-01 through I-06 verify that `classify_source` correctly routes to format-specific classifiers and that the results satisfy the same invariants.

4. **Malformed input coverage**: C-07 through C-09 test graceful degradation for invalid JSON, YAML, and TOML.

5. **Manifest test updates are correct**: The two modified manifest tests (`test_load_stops_at_entry_cap` and `test_git_head_backward_compat_none`) correctly use `FileManifest::FORMAT_VERSION` instead of hardcoded `1`, and the stale-version test (`test_stale_version_manifest_triggers_cold_start`) properly validates that v1 manifests are rejected.

The conditions for approval are: adding test coverage for TOML triple-quoted strings (production code with escape handling that has zero test coverage) and updating the stale comment in the boundary test. The remaining items are lower priority but would strengthen the suite.
