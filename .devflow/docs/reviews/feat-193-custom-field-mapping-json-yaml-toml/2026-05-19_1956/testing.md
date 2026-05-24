# Testing Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Scope**: Incremental (commits 13e13e9, 0468ade)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Missing test for MAX_JSON_DEPTH cap behavior** - `crates/rskim-search/src/fields/serde_fields.rs:62`
**Confidence**: 85%
- Problem: The new `MAX_JSON_DEPTH` constant (1024) introduces a behavioral branch — when `brace_depth > MAX_JSON_DEPTH`, the `in_key_stack` stops tracking state. No test validates that input at or beyond this depth still classifies correctly (contiguous, no panic, reasonable field assignment). The existing F-JSON-06 deep nesting test only goes 3 levels deep, far below the threshold.
- Fix: Add a test that constructs a JSON string with nesting beyond 1024 levels and verifies (a) no panic, (b) contiguous output, (c) the ranges sum to source length. Example:
```rust
#[test]
fn f_json_09_depth_beyond_max_json_depth_cap() {
    // Build JSON nested to MAX_JSON_DEPTH + 10 to exercise the cap.
    let depth = 1024 + 10;
    let mut source = String::new();
    for _ in 0..depth {
        source.push_str("{\"k\":");
    }
    source.push_str("\"v\"");
    for _ in 0..depth {
        source.push('}');
    }
    let ranges = classify_json(&source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}
```

**Missing test for YAML newline-trimming fix** - `crates/rskim-search/src/fields/serde_fields.rs:345-351`
**Confidence**: 82%
- Problem: The YAML scanner now trims trailing `\n` from quoted string values (lines 345-351), which was a bugfix to avoid boosting newline bytes with StringLiteral weight in BM25F. However, existing test F-YAML-03 (`name: "skim"\n`) passes but does not explicitly assert that the newline is excluded from the StringLiteral range. The behavioral change (StringLiteral range no longer includes trailing `\n`) is untested at the boundary level.
- Fix: Add a test that verifies the StringLiteral range for a quoted YAML value ends before the newline byte:
```rust
#[test]
fn f_yaml_07_quoted_string_excludes_trailing_newline() {
    let source = "name: \"skim\"\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());
    let str_ranges: Vec<_> = ranges.iter()
        .filter(|(_, f)| *f == SearchField::StringLiteral)
        .collect();
    assert!(!str_ranges.is_empty(), "should have StringLiteral");
    for (r, _) in &str_ranges {
        let text = &source[r.clone()];
        assert!(!text.ends_with('\n'),
            "StringLiteral must not include trailing newline; got: {text:?}");
    }
}
```

### MEDIUM

**Missing test for TOML escape fix in find_toml_eq_sign** - `crates/rskim-search/src/fields/serde_fields.rs:625-634`
**Confidence**: 83%
- Problem: `find_toml_eq_sign` was changed from a `for` loop to a `while` loop to handle backslash escapes in double-quoted TOML keys (e.g., a key like `"key=\"with=equals"` should not treat the `=` inside the string as the key-value separator). The new triple-quote tests (F-TOML-07 through F-TOML-10) test multi-line strings but do not exercise the specific fix to `find_toml_eq_sign` with escaped characters in single-line quoted keys containing `=`.
- Fix: Add a targeted test:
```rust
#[test]
fn f_toml_11_escaped_eq_in_quoted_key() {
    // The `=` inside the escaped string must not be treated as the key-value separator.
    let source = "\"path=here\" = \"value\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("path=here")),
        "quoted key with = should be a single SymbolName; sym texts: {sym_texts:?}"
    );
}
```

**Missing test for classify_markdown size guard** - `crates/rskim-search/src/fields/markdown.rs:50-55`
**Confidence**: 80%
- Problem: `classify_markdown` added a `MAX_SOURCE_BYTES` size guard (lines 50-55) that returns `SearchError::FileTooLarge`. The existing size-guard test in `classifier_tests.rs` uses JSON, not Markdown. No test validates that the Markdown classifier's own size guard fires correctly and returns the right error variant. The `classify_source` dispatcher calls `classify_markdown` for Markdown, which means both `classify_source`'s guard and `classify_markdown`'s guard exist — a test should confirm the Markdown-specific guard works in isolation.
- Fix: This is lower priority because the `classify_source` guard fires first (same constant), making the Markdown guard defense-in-depth. However, a unit test of `classify_markdown` directly with oversized input would confirm the guard works if the dispatch path ever changes. Mark it `#[ignore]` like the existing boundary test if allocation is a concern.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No test for YAML \r\n newline trimming** - `crates/rskim-search/src/fields/serde_fields.rs:349` (Confidence: 70%) — The newline trim only strips `\n`, not `\r\n`. A YAML value ending with `\r\n` would have its `\n` stripped but leave the `\r` in the StringLiteral range. This may or may not be intentional; a test clarifying the expected behavior for `\r\n` line endings would document the design choice.

- **extract_json_helpers / extract_yaml_helpers mentioned in commit message but not tested in isolation** - `fields_tests.rs` (Confidence: 65%) — The commit message for 0468ade mentions "extract JSON/YAML helpers". The extracted helpers (`classify_json_key_at_depth0`, `skip_json_whitespace`, `strip_list_prefix`) are tested only through the public `classify_json`/`classify_yaml` functions, which is the correct behavior-focused approach. No separate unit tests needed — this is fine as-is.

- **TOML `(eol + 1).min(len)` fix has no targeted boundary test** - `crates/rskim-search/src/fields/serde_fields.rs:470,485,524` (Confidence: 65%) — The `eol + 1` overflow fix (`.min(len)`) was applied in three places. This fires when the last line has no trailing newline. The existing malformed-TOML test C-09 may exercise this, but a targeted test with a no-trailing-newline TOML file would make the fix regression-proof.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new TOML triple-quote tests (F-TOML-07 through F-TOML-10) are well-structured: they follow the established naming convention, use the shared `assert_contiguous`/`assert_field_lengths_sum` helpers, include descriptive assertion messages with debug context, and test meaningful edge cases (embedded quotes, literal backslash). The test design follows correct AAA structure and tests behavior rather than implementation.

The two HIGH findings are genuine coverage gaps: the `MAX_JSON_DEPTH` cap introduces a new code path that is completely untested, and the YAML newline-trimming bugfix changes observable output without a test that verifies the new boundary. Both are straightforward to address with the suggested test patterns.
