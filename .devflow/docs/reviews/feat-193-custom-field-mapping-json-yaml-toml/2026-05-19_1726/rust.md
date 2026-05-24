# Rust Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**`find_toml_eq_sign` does not handle escaped quotes in quoted TOML keys** - `crates/rskim-search/src/fields/serde_fields.rs:587-608`
**Confidence**: 85%
- Problem: The function tracks `in_str` state by toggling on every quote character, but never skips `\"` (escaped quote) sequences inside basic strings. TOML allows quoted keys like `"key\"with=eq" = "value"`. An escaped quote inside a quoted key will prematurely close the string-tracking state, causing the scanner to find the `=` inside the key text and misclassify the key/value boundary. Literal strings (`'...'`) do not have escapes so they are unaffected.
- Impact: Misclassification of key and value byte ranges for TOML files with quoted keys containing escaped quotes. Since TOML quoted keys with backslash escapes are uncommon in practice (most keys are bare), the severity is HIGH rather than CRITICAL.
- Fix: Add backslash-escape handling for basic strings (`"..."`) in the `in_str` branch:
```rust
fn find_toml_eq_sign(content: &[u8]) -> Option<usize> {
    let mut in_str = false;
    let mut str_char = b'"';
    let mut i = 0;
    while i < content.len() {
        let b = content[i];
        if in_str {
            if b == b'\\' && str_char == b'"' {
                i += 2; // skip escaped character
                continue;
            }
            if b == str_char {
                in_str = false;
            }
        } else {
            match b {
                b'"' | b'\'' => {
                    in_str = true;
                    str_char = b;
                }
                b'=' => return Some(i),
                b'#' => return None,
                _ => {}
            }
        }
        i += 1;
    }
    None
}
```

### MEDIUM

**YAML quoted string value range includes trailing newline** - `crates/rskim-search/src/fields/serde_fields.rs:347`
**Confidence**: 82%
- Problem: When a YAML line contains a quoted string value (e.g., `name: "skim"\n`), the range `actual_val_start..line_end` is pushed as `StringLiteral`. Since `line_end` includes the trailing `\n` character, the newline byte is classified as `StringLiteral` rather than `Other`. For BM25F scoring, newline characters in the `StringLiteral` field artificially inflate the string field's byte count and term frequency, slightly skewing relevance scores.
- Impact: Minor scoring inaccuracy. The newline is unlikely to match any search terms so the practical effect on search quality is small, but it violates the principle that only actual string content should be in the `StringLiteral` field.
- Fix: Trim the trailing newline (and optional `\r`) from the range before pushing:
```rust
let mut str_end = line_end;
// Trim trailing newline from StringLiteral range.
if str_end > actual_val_start && bytes[str_end - 1] == b'\n' {
    str_end -= 1;
}
if str_end > actual_val_start && bytes[str_end - 1] == b'\r' {
    str_end -= 1;
}
ranges.push((actual_val_start..str_end, SearchField::StringLiteral));
```

**TOML full-line comment range includes newline not covered by `eol`** - `crates/rskim-search/src/fields/serde_fields.rs:434-435`
**Confidence**: 80%
- Problem: The TOML scanner sets `eol` to the position of `\n` (exclusive), then pushes range `i..eol` for full-line comments and advances `i = eol + 1`. The `eol` value does not include the newline, so the newline byte between `eol` and `eol + 1` falls into a gap filled by `Other`. This is functionally correct (the gap-fill handles it), but it means `i = eol + 1` can skip past the end of input when the last line has no trailing newline: if `eol == len - 1` and that byte is `\n`, then `i = len` is fine, but if the file ends without `\n`, `eol` is already `len` via the `unwrap_or(len)`, and `i = len + 1` causes the while-loop condition `i < len` to terminate normally (no UB), but it does mean the `eol + 1` arithmetic is technically beyond bounds. The `while i < len` guard prevents any actual out-of-bounds access.
- Impact: No functional bug -- `fill_gaps_and_merge` handles the gap correctly. The `eol + 1` when `eol == len` produces `i = len + 1` but the loop exits immediately. This is safe but asymmetric with the JSON scanner which handles this more cleanly.
- Fix: Guard `eol + 1` with `(eol + 1).min(len)` for clarity, or use `eol` directly since the newline handling is already covered by gap-fill. Low priority.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`classify_toml_inline_comment` may false-positive on `#` inside inline table string values** - `crates/rskim-search/src/fields/serde_fields.rs:556` (Confidence: 65%) -- For non-string values like inline tables `{key = "val # not comment"}`, the function scans for `space + #` in the raw bytes of the value region, which could match a `#` inside a nested string. Since inline TOML tables on key-value lines are uncommon in config files and the consequence is only a minor classification error (some value bytes become Comment), this is low-risk.

- **`find_yaml_key_colon` does not skip colons inside quoted YAML keys** - `crates/rskim-search/src/fields/serde_fields.rs:367` (Confidence: 70%) -- A YAML key like `"key: with colon": value` would cause `find_yaml_key_colon` to match the colon inside the quoted key text. This is a known YAML spec edge case, and the PR description explicitly documents that flow-style and complex keys fall to `Other`. Still, quoted keys with embedded colons are valid block-style YAML and could silently misclassify.

- **`in_key_stack` Vec allocation for JSON scanner grows with nesting depth** - `crates/rskim-search/src/fields/serde_fields.rs:68` (Confidence: 62%) -- The `in_key_stack` Vec is unbounded and grows one element per `{`. For deeply nested JSON (hundreds or thousands of levels), this allocates a Vec entry per level. The 100 MiB `MAX_SOURCE_BYTES` guard limits total input size but not nesting depth relative to input size. A `SmallVec<[bool; 32]>` or a depth cap would be more defensive. In practice, typical JSON files have modest nesting depth, so this is unlikely to cause issues.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The code is well-structured with clean separation between format-specific scanners and the shared `fill_gaps_and_merge` post-processing. The Markdown classifier correctly reuses `build_field_ranges` for overlapping tree-sitter nodes, while JSON/YAML/TOML use the simpler gap-fill path. Error handling follows the right pattern: serde scanners are infallible (return Vec, not Result), malformed input degrades gracefully, and the existing `MAX_SOURCE_BYTES` guard protects against oversized inputs. The test suite is thorough with contiguity contract assertions applied uniformly. The FORMAT_VERSION bump and test updates are done correctly. The one blocking issue (`find_toml_eq_sign` missing escape handling) is a real but low-frequency bug that should be fixed before merge.
