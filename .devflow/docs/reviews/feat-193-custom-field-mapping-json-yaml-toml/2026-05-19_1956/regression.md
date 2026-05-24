# Regression Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Scope**: Incremental (commits 13e13e9, 0468ade, 071f1e5 since f65b652)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**JSON depth cap may corrupt key-tracking state for depths > 1024** - `crates/rskim-search/src/fields/serde_fields.rs:79-91`
**Confidence**: 82%
- Problem: When `brace_depth` exceeds `MAX_JSON_DEPTH` (1024), the `{` handler skips the `in_key_stack.push(true)` call (line 83-84), but the matching `}` handler unconditionally calls `in_key_stack.pop()` (line 90). This means closing braces at depths > 1024 will pop entries belonging to shallower nesting levels, corrupting key-vs-value tracking for the rest of the document. The `}` handler should guard its pop symmetrically: only pop when `brace_depth` was within the tracked range before decrementing.
- Fix: Guard the pop to match the push condition:
```rust
b'}' => {
    if brace_depth <= MAX_JSON_DEPTH {
        in_key_stack.pop();
    }
    brace_depth = brace_depth.saturating_sub(1);
    i += 1;
}
```
Note: the `brace_depth` check must happen *before* the `saturating_sub` so it tests the pre-decrement depth. Alternatively, pop only when `in_key_stack.len()` matches `brace_depth` after decrement.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**YAML newline trimming does not handle `\r\n` line endings** - `crates/rskim-search/src/fields/serde_fields.rs:348-351`
**Confidence**: 80%
- Problem: The new newline trimming logic strips only a trailing `\n` byte from quoted string values. On files with Windows-style `\r\n` line endings, the `\r` byte remains inside the `StringLiteral` range, which slightly inflates BM25F scores. The intent of the fix is to exclude line-terminator bytes from boosted fields; `\r` is also a line-terminator byte.
- Fix: After stripping `\n`, also strip `\r`:
```rust
let mut str_end = line_end;
if str_end > actual_val_start && bytes[str_end - 1] == b'\n' {
    str_end -= 1;
}
if str_end > actual_val_start && bytes[str_end - 1] == b'\r' {
    str_end -= 1;
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none -- all identified items met the 80% confidence threshold)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The incremental changes are well-structured -- refactorings (extract `classify_json_key_at_depth0`, `strip_list_prefix`, `skip_json_whitespace`) are behavior-preserving, the TOML `eol + 1` bounding correctly prevents a potential out-of-bounds on no-final-newline files, the TOML escape fix in `find_toml_eq_sign` correctly handles `\"` inside quoted keys, the Markdown size guard adds defense-in-depth, new TOML triple-quote tests cover important edge cases, and all 292 package tests pass. The one blocking MEDIUM issue is the asymmetric push/pop guard on the JSON depth cap which can corrupt classification state for pathologically deep documents (> 1024 levels). While unlikely in practice, the fix is trivial and the intent of the depth cap is to be transparent -- parsing should degrade gracefully, not corrupt shallower state.
