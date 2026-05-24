# Security Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Unbounded stack growth in JSON scanner `in_key_stack` for deeply nested input** - `crates/rskim-search/src/fields/serde_fields.rs:68`
**Confidence**: 82%
- Problem: The `in_key_stack: Vec<bool>` grows by one entry per `{` character encountered. A crafted JSON file consisting entirely of `{{{...` (100 MiB of opening braces within the MAX_SOURCE_BYTES limit) would allocate a ~100 MB Vec<bool>. While the upstream `MAX_SOURCE_BYTES` guard (100 MiB) ensures the input is bounded, the stack allocation is proportional to input size with no independent depth limit. This is a low-severity resource exhaustion vector: an adversary who can submit a file for indexing could trigger disproportionate memory use via a pathological JSON file.
- Fix: Add a depth guard that stops pushing onto `in_key_stack` beyond a reasonable limit (e.g., 128 or 256 levels). Beyond that depth, all keys can fall back to `SymbolName` classification:
```rust
const MAX_JSON_DEPTH: usize = 256;

b'{' => {
    brace_depth += 1;
    if in_key_stack.len() < MAX_JSON_DEPTH {
        in_key_stack.push(true);
    }
    i += 1;
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`find_toml_eq_sign` does not handle backslash escapes inside quoted keys** - `crates/rskim-search/src/fields/serde_fields.rs:587-608`
**Confidence**: 80%
- Problem: The function tracks whether it is inside a string (`in_str` flag) but does not handle backslash escapes. A TOML key like `"key\"=evil"` would cause the scanner to exit the string at the escaped `\"`, then interpret the `=` inside the string as the key-value separator. This leads to incorrect classification, not a crash, but in the context of a search index it could cause misclassification of user-controlled content. The impact is low (BM25F field misclassification, not code execution), but the root cause is a parser that can be confused by adversarial input.
- Fix: Add escape handling in the `in_str` branch:
```rust
if in_str {
    if b == b'\\' && str_char == b'"' {
        // Skip next byte (escaped character). Literal strings ('...') have no escapes in TOML.
        // This is safe because enumerate yields the next index naturally.
        continue; // The for loop will advance past the escaped char
    }
    if b == str_char {
        in_str = false;
    }
}
```
Note: A simple `continue` in a `for` loop won't skip the next byte. A proper fix needs an index-based loop or a `skip_next` flag.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **YAML quoted string values include trailing whitespace/newline in StringLiteral range** - `crates/rskim-search/src/fields/serde_fields.rs:347` (Confidence: 65%) -- The YAML scanner classifies `actual_val_start..line_end` as StringLiteral for quoted values. This includes the trailing newline in the range. While not a security issue, it means the trailing `\n` gets boosted with the StringLiteral field weight, which could subtly affect search ranking for adversarially crafted YAML files.

- **TOML inline comment detection may false-positive on `#` inside non-string values** - `crates/rskim-search/src/fields/serde_fields.rs:562-582` (Confidence: 68%) -- The `classify_toml_inline_comment` function looks for `# ` preceded by whitespace, but it operates on the region after a value ends. For inline tables or arrays containing `#` characters (e.g., `color = [0x#FF]`), this could misclassify part of a value as a comment, though the practical impact on search indexing is minimal.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong security posture overall:
- The `MAX_SOURCE_BYTES` guard (100 MiB) at the `classify_source` entry point protects all four format-specific scanners from unbounded input.
- All scanners are infallible and degrade gracefully on malformed input (no panics, no crashes).
- The `pub(crate)` visibility on scanner functions prevents external callers from bypassing the size guard.
- The `fill_gaps_and_merge` post-processing clamps ranges to source bounds, defending against off-by-one errors in scanners.
- The `FORMAT_VERSION` bump correctly forces re-indexing, preventing stale field classifications from persisting.
- No hardcoded secrets, no user-controlled file writes, no injection vectors.

The two MEDIUM findings (JSON depth amplification and TOML escape handling) are both low-impact in practice -- they affect search classification accuracy, not code execution or data integrity. The JSON depth issue is mitigated by the upstream 100 MiB size guard. Both should be addressed for defense-in-depth but neither blocks merge.
