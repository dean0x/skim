# Reliability Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Scope**: Incremental (commits 13e13e9..0468ade)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**JSON depth cap push/pop asymmetry may desync `in_key_stack` during recovery** - `serde_fields.rs:83-90`
**Confidence**: 82%
- Problem: When `brace_depth > MAX_JSON_DEPTH`, the `{` handler skips `in_key_stack.push()` (line 83-85), but the `}` handler always calls `in_key_stack.pop()` (line 90). For pathologically deep input (>1024 nested objects), closing braces will pop entries belonging to shallower levels, desyncing `in_key_stack` from the actual nesting. After recovery to depth <=1024, key/value classification will be incorrect for all remaining objects.
- Fix: Guard the pop symmetrically with the push:
```rust
b'}' => {
    if brace_depth <= MAX_JSON_DEPTH {
        in_key_stack.pop();
    }
    brace_depth = brace_depth.saturating_sub(1);
    i += 1;
}
```
Note: `brace_depth` must be compared *before* the saturating_sub so the guard mirrors the push condition. This ensures entries are only popped for depths that had corresponding pushes.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**YAML newline trim does not handle `\r\n` line endings** - `serde_fields.rs:348-351`
**Confidence**: 80%
- Problem: The new newline-trim logic (lines 348-350) only strips a trailing `\n` byte. On Windows-style `\r\n` input, after trimming `\n`, the `\r` byte remains inside the `StringLiteral` range. This `\r` byte receives the StringLiteral BM25F boost -- the same score-skewing issue the trim was introduced to fix, just for a different whitespace byte.
- Fix: Add a `\r` trim after the `\n` trim:
```rust
let mut str_end = line_end;
if str_end > actual_val_start && bytes[str_end - 1] == b'\n' {
    str_end -= 1;
}
if str_end > actual_val_start && bytes[str_end - 1] == b'\r' {
    str_end -= 1;
}
if str_end > actual_val_start {
    ranges.push((actual_val_start..str_end, SearchField::StringLiteral));
}
```

## Pre-existing Issues (Not Blocking)

No pre-existing reliability issues at CRITICAL severity in reviewed files.

## Suggestions (Lower Confidence)

- **`find_toml_eq_sign` escape skip may overshoot by 1** - `serde_fields.rs:627` (Confidence: 65%) -- When `\\` is the last byte in `content`, `i += 2` sets `i` to `content.len() + 1`, which exceeds the slice length by 1. The `while i < content.len()` loop guard prevents any out-of-bounds access, so this is safe in practice. A `.min(content.len())` clamp after `i += 2` would make the bound explicit.

- **`scan_triple_quote` unterminated string returns `len` without diagnostic** - `serde_fields.rs:696` (Confidence: 62%) -- If a triple-quoted string is never terminated, the function silently returns `len`, consuming all remaining input as StringLiteral. This is correct for the infallible-scanner contract, but on malformed TOML files it could cause entire file tails to be misclassified. Consider emitting a `SKIM_DEBUG` stderr warning for unterminated multi-line strings.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The depth-cap addition (`MAX_JSON_DEPTH`) and EOL-arithmetic bounds (`(eol + 1).min(len)`) are solid reliability improvements. The Markdown `MAX_SOURCE_BYTES` size guard correctly mirrors the generic classifier path. The two MEDIUM findings are both edge-case correctness issues in adversarial input (>1024-deep JSON, `\r\n` YAML) that do not cause panics or undefined behavior, but could produce subtly wrong field classification. The push/pop asymmetry (BLOCKING) should be fixed before merge as it violates the stack invariant the depth cap was introduced to protect.
