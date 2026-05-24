# Complexity Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19T17:26

## Issues in Your Changes (BLOCKING)

### HIGH

**`classify_json` function length and nesting depth** - `crates/rskim-search/src/fields/serde_fields.rs:49-174`
**Confidence**: 85%
- Problem: `classify_json` is 125 lines with 5 levels of nesting in the key classification branch (lines 117-158: `while` > `match` > `if in_object && in_key` > `if brace_depth == 1 && !inside_array_at_root` > `if j < len && ...`). The look-ahead logic for determining whether a key's value is an object/array (lines 123-155) is a 32-line block embedded inside a match arm inside a conditional. Cyclomatic complexity is approximately 14 (match with 7 arms, nested conditionals for key classification, two while loops for whitespace skipping).
- Fix: Extract the look-ahead logic into a helper function. The block at lines 123-155 is a self-contained unit that determines whether a depth-0 key maps to `TypeDefinition` or `SymbolName`:
```rust
/// Look ahead past the key string to determine if its value is an object or array.
/// Returns TypeDefinition if the value starts with `{` or `[`, otherwise SymbolName.
fn classify_json_key_at_depth0(bytes: &[u8], after_key: usize, len: usize) -> SearchField {
    let mut j = after_key;
    // Skip whitespace
    while j < len && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
        j += 1;
    }
    // Skip colon
    if j < len && bytes[j] == b':' {
        j += 1;
    }
    // Skip whitespace
    while j < len && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
        j += 1;
    }
    if j < len && (bytes[j] == b'{' || bytes[j] == b'[') {
        SearchField::TypeDefinition
    } else {
        SearchField::SymbolName
    }
}
```

**`classify_yaml` function length and cognitive load** - `crates/rskim-search/src/fields/serde_fields.rs:239-358`
**Confidence**: 82%
- Problem: `classify_yaml` is 119 lines with high cognitive load. The function handles blank lines, comments, document markers, list item stripping (with effective indent recalculation), key detection, key trimming, value scanning, and quoted value classification -- all in a single function body. The list item destructuring block (lines 289-302) produces a 3-element tuple that requires the reader to track `eff_indent`, `eff_rest_start`, and `eff_rest` through the remainder of the function. Cyclomatic complexity is approximately 12.
- Fix: Extract the list-item prefix stripping into a helper:
```rust
struct EffectiveLine<'a> {
    indent: usize,
    rest_start: usize,
    rest: &'a [u8],
}

fn strip_list_prefix(rest: &[u8], rest_start: usize, indent: usize, bytes: &[u8], line_end: usize) -> EffectiveLine<'_> {
    if rest.starts_with(b"- ") || rest == b"-\n" || rest == b"-\r\n" || rest == b"-" {
        let offset = if rest.len() >= 2 && rest[1] == b' ' { 2 } else { 1 };
        let new_start = rest_start + offset;
        EffectiveLine { indent: indent + 1, rest_start: new_start, rest: &bytes[new_start..line_end] }
    } else {
        EffectiveLine { indent, rest_start, rest }
    }
}
```
This reduces the main function body by ~15 lines and eliminates the unnamed tuple.

### MEDIUM

**`classify_toml` function length** - `crates/rskim-search/src/fields/serde_fields.rs:398-496`
**Confidence**: 82%
- Problem: `classify_toml` is 98 lines. While the nesting depth is manageable (max 4), the function body handles whitespace skipping, EOL finding, blank lines, comments, section headers, and key-value parsing in a single function. The key-value branch (lines 453-491) contains inline whitespace trimming logic that is similar to the pattern in `classify_yaml`. Cyclomatic complexity is approximately 10.
- Fix: The function is at the warning threshold rather than critical. The most impactful extraction would be the key-value branch (lines 456-486) into a `classify_toml_key_value` helper, which would bring the main function under 60 lines and reduce its cyclomatic complexity to approximately 6.

**Duplicated whitespace-skipping pattern (3 occurrences)** - Confidence: 80%
- `serde_fields.rs:129-136` (JSON look-ahead, first whitespace skip)
- `serde_fields.rs:142-148` (JSON look-ahead, second whitespace skip)
- `serde_fields.rs:334-337` (YAML value skip)
- `serde_fields.rs:476-479` (TOML value skip)
- Problem: The pattern `while j < len && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') { j += 1; }` appears 4 times across the file. A minor readability issue, but contributes to line count in the already-long functions.
- Fix: Extract a `skip_whitespace(bytes: &[u8], start: usize, len: usize) -> usize` helper. This is a LOW priority change -- the duplication is localized and the pattern is simple enough to be self-documenting.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`serde_fields.rs` file length** - `crates/rskim-search/src/fields/serde_fields.rs` (Confidence: 65%) -- At 658 lines, this file is above the 500-line warning threshold. The three scanners are well-separated by section markers, and splitting into `json.rs` / `yaml.rs` / `toml.rs` would be a natural next step if more format-specific logic is added. Not a problem today given the clear internal structure.

- **`fields_tests.rs` file length** - `crates/rskim-search/src/fields/fields_tests.rs` (Confidence: 60%) -- At 702 lines, the test file is long but well-organized with section headers and systematic naming (F-JSON-01, F-YAML-01, etc.). Test files typically warrant higher thresholds. No action needed.

- **`classify_toml_value` parameter count** - `crates/rskim-search/src/fields/serde_fields.rs:505` (Confidence: 65%) -- 6 parameters (`bytes`, `val_start`, `eol`, `len`, `ranges`, `i`) is at the warning threshold. The `i: &mut usize` out-parameter for communicating multi-line string advancement back to the caller is a pragmatic choice but adds cognitive load.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-structured overall. Each scanner follows a clear pattern (empty check, byte-level scan, fill-gaps-and-merge post-processing) and the module decomposition is logical (serde scanners vs. tree-sitter markdown vs. shared helper). The helper functions (`scan_json_string`, `find_yaml_key_colon`, `find_toml_eq_sign`, etc.) are small, focused, and well-documented.

The two HIGH findings are about function length and nesting depth in `classify_json` and `classify_yaml`. These are addressable by extracting 1-2 helpers each without architectural changes. The existing decomposition (e.g., `classify_toml_value`, `classify_toml_inline_comment`) shows the pattern is already understood -- it just needs to be applied more thoroughly to the JSON key classification and YAML list-item handling.

Conditions: Consider extracting the JSON look-ahead logic and YAML list-prefix stripping into helpers before or shortly after merge to keep these functions under the 100-line threshold.
