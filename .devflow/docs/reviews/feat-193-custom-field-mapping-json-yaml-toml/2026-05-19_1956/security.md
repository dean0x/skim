# Security Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Scope**: Incremental (commits 13e13e9..0468ade)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**JSON depth cap causes misaligned key/value tracking beyond MAX_JSON_DEPTH** - `serde_fields.rs:88-91`
**Confidence**: 82%
- Problem: The `in_key_stack.pop()` on line 90 is called unconditionally on every `}`, but `in_key_stack.push(true)` is skipped when `brace_depth > MAX_JSON_DEPTH` (line 83-85). For JSON with nesting depth exceeding 1024, the stack becomes misaligned: closing braces at depths > 1024 pop entries belonging to shallower depths. This means keys and values at depths 1..1024 may be misclassified (keys treated as values or vice versa) after exiting the deeply nested region.
- Impact: Not a memory safety issue (`Vec::pop()` on empty returns `None`, no panic or UB). However, for adversarial JSON crafted to exploit this, the misalignment propagates upward, causing field misclassification at all remaining depths. The practical impact is limited to BM25F scoring accuracy, not data integrity or code execution.
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
Note: the `brace_depth` decrement must happen *after* the guard check (or use `brace_depth > 0 && brace_depth <= MAX_JSON_DEPTH`) so the pop condition mirrors the push condition at the same depth value.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **TOML `find_toml_eq_sign` escape skip could overshoot** - `serde_fields.rs:625-628` (Confidence: 65%) -- When `i` is at the last byte of `content` and that byte is `\\` inside a double-quoted string, `i += 2; continue;` overshoots the buffer by one position. The `while i < content.len()` guard prevents any out-of-bounds access, so this is safe but silently drops the last character of a malformed (unterminated) string. Same pattern applies at lines 660-663 and 690-692. No action needed unless the scanners must handle truncated input precisely.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Security Observations

1. **MAX_JSON_DEPTH cap (line 62)** -- Bounds `in_key_stack` heap growth to prevent DoS via pathologically deep JSON. The constant of 1024 is reasonable (RFC 7159 does not mandate a depth limit, but most parsers cap at 512-1024).

2. **MAX_SOURCE_BYTES guard added to classify_markdown (lines 50-55)** -- Closes a bypass where `classify_markdown` could be called directly without the size guard in `classify_source`. Defense in depth.

3. **`(eol + 1).min(len)` bounds (lines 469, 481, 518)** -- Fixes potential off-by-one when the last line has no trailing newline. The previous `eol + 1` could set `i` to `len + 1`, which while not causing UB in Rust (the next loop iteration would just exit), was technically incorrect.

4. **TOML escape handling in `find_toml_eq_sign` (lines 623-628)** -- Prevents `\"` inside a TOML basic string from being treated as a closing quote, which would cause the `=` search to misidentify characters inside string literals. Correctly limited to double-quoted strings only (single-quoted TOML literal strings do not support escapes).

### Conditions for Approval

The Should-Fix item (JSON depth cap stack misalignment) is a correctness issue in adversarial input handling, not a safety vulnerability. It should be addressed but does not block merge.
