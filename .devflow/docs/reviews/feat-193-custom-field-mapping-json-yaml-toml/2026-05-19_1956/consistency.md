# Consistency Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Scope**: Incremental (commits 13e13e9, 0468ade)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**YAML newline trim does not strip `\r` on CRLF lines** - `crates/rskim-search/src/fields/serde_fields.rs:348-351`
**Confidence**: 82%
- Problem: The new newline trim logic strips a trailing `\n` from the StringLiteral range, but on `\r\n` line endings the `\r` byte remains inside the StringLiteral. This is inconsistent with the stated goal (not boosting non-content bytes with StringLiteral weight) because `\r` is also a non-content whitespace byte. The YAML comment range (line 282) includes the full `\n` (and any `\r`), so the asymmetric trim is intentional between fields, but the `\r` omission within the trim itself is a gap. Elsewhere in this file, `\r` is handled alongside `\n` (e.g., line 274, 277, 383, 406, 461).
- Fix: After trimming `\n`, also trim a preceding `\r`:
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

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`classify_json_key_at_depth0` inlines whitespace skipping instead of using `skip_json_whitespace`** - `crates/rskim-search/src/fields/serde_fields.rs:158-181`
**Confidence**: 85%
- Problem: The newly extracted `classify_json_key_at_depth0` function contains two inline `while` loops that skip JSON whitespace (lines 161-164 and 171-174). Meanwhile, a dedicated `skip_json_whitespace` helper exists at lines 177-185 of the current file, created in the same commit. The function body in the actual file (post-extraction) does use `skip_json_whitespace`, but the diff itself shows the inline loops were extracted without initially using the helper. Reviewing the final file confirms lines 160 and 164 now call `skip_json_whitespace` -- so this was resolved during the extraction. No action needed.
- **Note**: After reviewing the final file state, this finding is **withdrawn**. The diff rendering showed the raw inline code, but the actual file at HEAD uses the `skip_json_whitespace` helper correctly.

## Pre-existing Issues (Not Blocking)

No pre-existing consistency issues found in the reviewed files.

## Suggestions (Lower Confidence)

- **YAML `strip_list_prefix` inner condition differs from original** - `crates/rskim-search/src/fields/serde_fields.rs:381` (Confidence: 65%) -- The old inline code used `rest.len() >= 2 && rest[1] == b' '` to determine offset, while the extracted function uses `rest.starts_with(b"- ")`. These are semantically equivalent given the outer guard but use different idioms; `starts_with` is arguably clearer. No action needed.

- **YAML comment ranges include trailing newlines, StringLiteral does not** - `crates/rskim-search/src/fields/serde_fields.rs:282,348` (Confidence: 62%) -- Asymmetric treatment between Comment and StringLiteral regarding trailing newlines. This is intentional per the commit message (BM25F score skew concern for StringLiteral only), but undocumented as a deliberate design choice. A brief inline comment noting the asymmetry would help future maintainers.

- **No `\r\n` test coverage for YAML StringLiteral trim** - `crates/rskim-search/src/fields/fields_tests.rs` (Confidence: 70%) -- The YAML tests all use `\n` line endings. A CRLF variant would exercise the newline-trim edge case to catch the `\r` leftover described above.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The incremental changes are well-structured: helper functions were cleanly extracted (`classify_json_key_at_depth0`, `strip_list_prefix`, `skip_json_whitespace`), naming conventions are consistent throughout (snake_case, descriptive function names), and the test naming pattern (F-TOML-07 through F-TOML-10) follows the established prefix convention. The `(eol + 1).min(len)` EOL bound pattern is applied consistently across all three TOML branch arms. The one actionable finding is the missing `\r` trim in YAML StringLiteral newline stripping, which creates an inconsistency with how `\r\n` is handled elsewhere in the same scanner.
