# Reliability Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**JSON scanner `in_key_stack` grows without bound on deeply nested input** - `crates/rskim-search/src/fields/serde_fields.rs:68-77`
**Confidence**: 90%
- Problem: The `in_key_stack: Vec<bool>` in `classify_json` pushes on every `{` and pops on `}`. For malformed input with deeply nested or only opening braces (e.g., a crafted input with 10M `{` characters), this vector grows unboundedly. The upstream `MAX_SOURCE_BYTES` guard (100 MiB) limits total input size, so the stack cannot exceed ~100M entries (~100 MB of bools), but this is still significant. The `brace_depth` and `bracket_depth` counters similarly grow without limit, though as `usize` they are fixed-size and do not allocate.
- Fix: Add a depth guard. When `brace_depth` exceeds a reasonable maximum (e.g., 1024), stop pushing to `in_key_stack` and treat remaining content as `Other`. This bounds memory at O(max_depth) instead of O(input_size):
```rust
const MAX_JSON_DEPTH: usize = 1024;
// ...
b'{' => {
    brace_depth += 1;
    if brace_depth <= MAX_JSON_DEPTH {
        in_key_stack.push(true);
    }
    i += 1;
}
```

**Markdown `classify_markdown` bypasses `MAX_SOURCE_BYTES` guard** - `crates/rskim-search/src/fields/markdown.rs:43-101` and `crates/rskim-search/src/lexical/classifier.rs:146-163`
**Confidence**: 92%
- Problem: In `classify_source`, the `MAX_SOURCE_BYTES` check at line 139-144 executes before the format-specific dispatch at line 149-163, so JSON/YAML/TOML/Markdown all benefit from it. However, `classify_markdown` is declared `pub(crate)` and is also called directly from tests (lines 409, 422, 435, 449, 462, 474, 488, 501 of fields_tests.rs). Any future direct caller would bypass the size guard. The function's doc comment at line 38-42 claims the size guard comes from `classify_source`, but `classify_markdown` can be called independently. The serde scanners (`classify_json`, `classify_yaml`, `classify_toml`) have the same bypass pattern. Since these are `pub(crate)`, the blast radius is limited, but this is a defense-in-depth gap.
- Fix: Either add an independent size guard at the top of `classify_markdown` (and the serde scanners), or document that callers must ensure the size guard has been checked. A lightweight approach:
```rust
pub(crate) fn classify_markdown(source: &str) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    if source.len() > crate::lexical::classifier::MAX_SOURCE_BYTES {
        return Err(crate::SearchError::FileTooLarge {
            size: source.len(),
            limit: crate::lexical::classifier::MAX_SOURCE_BYTES,
        });
    }
    // ...
}
```

### MEDIUM

**TOML `find_toml_eq_sign` does not handle escape sequences in quoted keys** - `crates/rskim-search/src/fields/serde_fields.rs:587-608`
**Confidence**: 82%
- Problem: The function tracks `in_str` state but does not handle `\"` escapes inside double-quoted strings. A TOML key like `"key\"=val"` would incorrectly exit the string state at the escaped `"`, then find the `=` inside the string content. This could produce incorrect field classifications for keys containing escaped quotes. While uncommon, it is a correctness issue that could silently misclassify ranges.
- Fix: Add escape handling for `b'\\'` when inside a double-quoted string:
```rust
if in_str {
    if b == b'\\' && str_char == b'"' {
        // skip next byte (escaped character)
        // use enumerate().skip() or track index manually
        continue; // simplified — actual fix needs to skip one more byte
    }
    if b == str_char {
        in_str = false;
    }
}
```

**TOML scanner `eol + 1` can exceed `len` on input without trailing newline** - `crates/rskim-search/src/fields/serde_fields.rs:435,450,489`
**Confidence**: 85%
- Problem: When a TOML source string does not end with a newline, `eol` equals `len` (from `.unwrap_or(len)` on line 424). Then `i = eol + 1` sets `i` to `len + 1`. The outer `while i < len` loop exits safely because `len + 1 > len`, so this is not a panic or out-of-bounds access. However, `i` transiently holds an invalid index that exceeds the byte array length. This works correctly due to the loop guard, but it is a fragile pattern -- if any code were added between the `i = eol + 1` assignment and the loop guard, it could index out of bounds. The `usize` overflow is also not possible since `len <= MAX_SOURCE_BYTES` which is well below `usize::MAX`.
- Fix: Use `i = (eol + 1).min(len)` or restructure to avoid the +1 pattern when eol == len.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`count_atx_heading_level` could scan past reasonable heading depths** - `crates/rskim-search/src/fields/markdown.rs:137-145` (Confidence: 65%) -- The function counts `#` characters without an upper bound. In practice tree-sitter's `atx_heading` node will only fire for valid ATX headings (max 6 `#`), so the count is implicitly bounded by the parser. Only a concern if called outside the tree-sitter context.

- **`fill_gaps_and_merge` allocates `2 * ranges.len() + 1` capacity upfront** - `crates/rskim-search/src/fields/mod.rs:65` (Confidence: 60%) -- The `Vec::with_capacity(ranges.len() * 2 + 1)` heuristic is reasonable for typical files. For pathological input with many classified ranges, this pre-allocation is fine given the MAX_SOURCE_BYTES guard upstream. No action needed.

- **JSON scanner look-ahead whitespace loops are bounded only by `len`** - `crates/rskim-search/src/fields/serde_fields.rs:129-149` (Confidence: 62%) -- The two `while j < len` whitespace-skipping loops at lines 129-136 and 142-149 scan forward from the current key position. They terminate at `len` which is bounded by `MAX_SOURCE_BYTES`. No realistic concern, but the pattern of multiple unbounded-looking loops in sequence could be tightened with a helper function.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 0 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates good reliability patterns overall: all loops have implicit bounds via `while i < len`, all scanners handle malformed input gracefully (infallible return types), and the `fill_gaps_and_merge` post-processor includes clamping for safety. The FORMAT_VERSION bump correctly forces re-indexing.

The two HIGH issues are the main concerns: (1) the JSON scanner's `in_key_stack` can grow proportionally to input size on adversarial nesting, and (2) the format-specific classifiers can be called directly without the `MAX_SOURCE_BYTES` guard. Both are bounded by the 100 MiB upstream limit when called through `classify_source`, but defense-in-depth would strengthen reliability. The TOML escape handling gap is a correctness issue that could produce silent misclassification on edge-case inputs.
