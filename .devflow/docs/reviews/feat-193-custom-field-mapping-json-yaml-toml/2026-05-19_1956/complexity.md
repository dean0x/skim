# Complexity Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Scope**: Incremental (commits 13e13e9, 0468ade)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`classify_yaml` function length: 115 lines** - `serde_fields.rs:250-365`
**Confidence**: 85%
- Problem: At 115 lines, `classify_yaml` exceeds the 50-line threshold for function length. The function handles blank lines, comments, document markers, list-item stripping, key detection, key trimming, value classification, and quoted-string newline trimming -- all in a single function body. The nesting depth reaches 5 levels in the value-classification branch (while -> if let -> if -> if -> if/if).
- Fix: The incremental changes (list prefix extraction to `strip_list_prefix`, newline trimming) actually *improved* this function by moving logic out. No action required in this PR. Consider a follow-up to extract the key-detection block (lines 305-358) into a `classify_yaml_key_value_line()` helper, which would bring `classify_yaml` to ~60 lines and reduce max nesting by 1 level.

**`classify_toml` function length: 93 lines** - `serde_fields.rs:432-525`
**Confidence**: 82%
- Problem: At 93 lines, `classify_toml` exceeds the 50-line threshold. The function handles whitespace skipping, blank lines, comments, section headers, key-value parsing (with trimming and value dispatching) in a single body. The key-value arm (lines 484-521) nests 4 levels deep (while -> match -> if let -> if).
- Fix: The incremental changes (EOL arithmetic bounds) were localized fixes that did not add complexity. No action required in this PR. A future refactor could extract the key-value arm into `classify_toml_kv_line()`.

**`classify_json` function length: 99 lines** - `serde_fields.rs:49-148`
**Confidence**: 82%
- Problem: At 99 lines, `classify_json` is the longest of the three scanners. The `match` on byte values is inherently flat (cyclomatic complexity from many arms, not deep nesting), so readability is acceptable. The depth-0 key classification was already extracted to `classify_json_key_at_depth0` in this incremental diff, which is a good complexity reduction.
- Fix: No action required. The extraction of `classify_json_key_at_depth0` in this increment was the right move.

## Suggestions (Lower Confidence)

- **Repeated whitespace-skip pattern** - `serde_fields.rs` (multiple locations) (Confidence: 70%) -- The pattern `while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') { i += 1; }` appears in `classify_toml` (line 445), `find_toml_eq_sign` (via manual iteration), and the YAML scanner's `space_skip` logic. A `skip_ws(bytes, pos, len)` helper (like the JSON scanner's `skip_json_whitespace`) could reduce repetition, though each instance is only 3 lines.

- **Test helper duplication across TOML triple-quote tests** - `fields_tests.rs:399-460` (Confidence: 65%) -- The four new tests (F-TOML-07 through F-TOML-10) follow an identical structure: construct source, classify, assert contiguous, assert field lengths, extract StringLiteral texts, assert contains. A parameterized test helper `assert_toml_string_literal(source, expected_content, label)` would collapse 60 lines to ~15, though the current form is perfectly readable.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 3 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED

## Rationale

This incremental diff (commits 13e13e9, 0468ade) actually **reduces** complexity in several ways:

1. **Extracted `classify_json_key_at_depth0`** -- Moved the depth-0 key look-ahead logic out of `classify_json`'s `b'"'` arm into a dedicated 15-line function with a `skip_json_whitespace` helper. This reduces nesting depth in the hot path from 5 to 3 and improves readability.

2. **Extracted `strip_list_prefix`** -- Moved the YAML list-item prefix stripping out of `classify_yaml` into a 16-line standalone function. Clean single-responsibility extraction.

3. **Bounded `in_key_stack` growth** -- Added `MAX_JSON_DEPTH` (1024) cap on the stack to prevent unbounded heap allocation on pathological input. `brace_depth` still counts beyond 1024 so parsing remains correct; only the stack is bounded. This addresses a reliability concern without adding cyclomatic complexity (single `if` guard).

4. **Fixed EOL arithmetic** -- Changed `eol + 1` to `(eol + 1).min(len)` in three places in `classify_toml`, preventing potential off-by-one past the end of input. Minimal complexity impact.

5. **YAML newline trimming** -- Added 4 lines to trim trailing `\n` from quoted string values so BM25F scores are not skewed. Simple bounds-checked subtraction, no new control flow.

6. **TOML escape handling in `find_toml_eq_sign`** -- Converted `for` loop to `while` loop to support `i += 2` escape skipping. Adds one branch but fixes a real bug (backslash before `=` in double-quoted TOML strings).

The new tests (F-TOML-07 through F-TOML-10) are well-structured, each testing a distinct triple-quote edge case with clear assertions and diagnostic messages.

All pre-existing MEDIUM issues predate this increment and are not blocking. The changes in this diff trend toward lower complexity, not higher.
