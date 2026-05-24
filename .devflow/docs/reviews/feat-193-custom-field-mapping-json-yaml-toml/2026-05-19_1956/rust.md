# Rust Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19T19:56
**Scope**: Incremental review (commits 13e13e9..0468ade)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**JSON depth cap causes incorrect key/value classification when unwinding from >1024 depth** - `serde_fields.rs:79-91`
**Confidence**: 82%
- Problem: When `brace_depth > MAX_JSON_DEPTH`, `b'{'` increments `brace_depth` but skips the `in_key_stack.push()`. However, the `b'}'` handler unconditionally calls `in_key_stack.pop()`. When unwinding from deep nesting, each `}` pops a stack entry that belongs to a shallower (still-open) brace scope. After returning from depth >1024 back to depth 1024, the stack is empty and all remaining keys at depths 1-1024 are misclassified as values (`in_key_stack.last()` returns `None` -> `unwrap_or(false)`). The comment on line 59-61 says "we still parse correctly" which is not strictly accurate for intermediate depths during the unwind.
- Fix: Guard the `pop()` to only fire when the stack was tracking that depth:
```rust
b'}' => {
    if brace_depth <= MAX_JSON_DEPTH {
        in_key_stack.pop();
    }
    brace_depth = brace_depth.saturating_sub(1);
    i += 1;
}
```
This keeps the pop and decrement in sync. The `saturating_sub` must happen after the guard check (or use `brace_depth > MAX_JSON_DEPTH` before decrement) to maintain the invariant. At MAX_JSON_DEPTH=1024 the bug is practically unreachable, but the fix is trivial and makes the code correct by construction.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **YAML newline trim does not handle `\r\n` line endings** - `serde_fields.rs:348-351` (Confidence: 65%) -- The new code trims trailing `\n` from StringLiteral ranges but does not trim a preceding `\r` on Windows-style line endings. The `\r` byte would be counted with StringLiteral boost weight. Consider adding `if str_end > actual_val_start && bytes[str_end - 1] == b'\r' { str_end -= 1; }` after the `\n` trim.

- **`classify_json_key_at_depth0` duplicates whitespace-skip logic** - `serde_fields.rs:158-170` (Confidence: 62%) -- The extracted helper uses inline while loops for whitespace skipping. The same file already has `skip_json_whitespace` (line 177) which does exactly this. The helper calls `skip_json_whitespace` in the current code (line 160, 164) so this is already resolved in the final file -- the diff showed the intermediate state. No action needed.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Condition
The JSON depth-cap pop desync (MEDIUM) is a correctness issue in a defensive code path. While practically unreachable at depth 1024, the fix is a one-line guard and eliminates the comment inaccuracy. Recommend fixing before merge.

### Positive observations

1. **Bounded loops and stack**: The `MAX_JSON_DEPTH` cap (line 62) and `(eol + 1).min(len)` guards (lines 469, 481, 518) demonstrate good adherence to reliability principles -- every loop and data structure has an explicit bound.
2. **`find_toml_eq_sign` backslash handling**: Converting from `for` iterator to manual `while` loop with `i += 2` skip for escaped characters inside double-quoted strings (lines 625-634) correctly implements the TOML spec distinction between basic strings (escapes) and literal strings (no escapes). The bounds check at the top of the loop prevents OOB when a backslash is the last byte.
3. **`strip_list_prefix` extraction**: Moving the inline list-prefix logic into a named function (lines 376-392) improves readability and makes the YAML scanner's main loop easier to follow. Lifetime annotation `'a` on the return slice is correct.
4. **`scan_triple_quote`**: Correctly handles embedded single and double quotes within triple-quoted strings, and respects the TOML spec that literal strings (single-quoted) do not process backslash escapes (line 690-693).
5. **Test coverage**: Four new TOML triple-quote tests (F-TOML-07 through F-TOML-10) cover basic multi-line, literal multi-line, embedded quotes, and embedded backslashes -- thorough edge-case coverage for the new scanner feature.
