# Performance Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Commits reviewed**: 13e13e9, 0468ade (incremental)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**YAML scanner: per-line linear scan in `find_yaml_key_colon` re-scans bytes already visited by indent calculation** - `serde_fields.rs:269-305`
**Confidence**: 65%
- Problem: For each YAML line, indent bytes are counted by `take_while` (line 269), then `strip_list_prefix` re-checks prefix bytes (line 301), and `find_yaml_key_colon` linearly scans the remaining content (line 305). All three passes scan overlapping byte regions. On files with very long lines or many short lines, the constant factor is roughly 3x what a single-pass design would need.
- However, this is a line-by-line scanner processing data formats where lines are typically short (< 200 bytes), so the absolute overhead is minimal. This is more of a design observation than a blocking performance concern.

> Moved to Suggestions (below 80% threshold).

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`fill_gaps_and_merge` sorts + allocates a second Vec on every call** - `mod.rs:54-90`
**Confidence**: 82%
- Problem: Each of the three serde scanners (`classify_json`, `classify_yaml`, `classify_toml`) calls `fill_gaps_and_merge` which performs `sort_unstable_by_key` on the ranges Vec, then allocates a new `result` Vec with capacity `ranges.len() * 2 + 1`, copies all ranges into it while filling gaps, then calls `merge_adjacent`. For JSON/YAML/TOML config files of typical size (< 10KB), the overhead is negligible. For very large data files (e.g., a 50MB JSON log dump), this could produce tens of thousands of ranges, and the sort + double-allocation becomes measurable.
- This is pre-existing infrastructure (not introduced in these commits) and only affects the new scanners indirectly. Not blocking.
- Fix: The JSON scanner already emits ranges in order (byte-by-byte forward scan), so the sort is redundant for JSON. YAML's line-by-line scan also produces sorted output. TOML similarly advances forward. A `fill_gaps_and_merge_sorted` variant that skips the sort could save ~10-15% on the post-processing pass for large files. Low priority.

## Suggestions (Lower Confidence)

- **YAML triple-pass per line** - `serde_fields.rs:269-305` (Confidence: 65%) -- The indent calculation, list-prefix stripping, and key-colon detection scan overlapping byte regions per line. A fused single-pass design would reduce constant-factor overhead, but lines in YAML config files are typically short enough that the absolute impact is negligible.

- **`classify_json_key_at_depth0` whitespace skip could use `skip_json_whitespace` helper** - `serde_fields.rs:158-182` (Confidence: 72%) -- The extracted function has its own inline whitespace skip loops. The diff shows this was already refactored to use `skip_json_whitespace` in the final version (lines 160, 164), so this is resolved. No action needed.

- **`scan_triple_quote` lacks early-exit on delimiter mismatch** - `serde_fields.rs:678-697` (Confidence: 62%) -- The triple-quote scanner checks `bytes[i] == delim && bytes[i+1] == delim && bytes[i+2] == delim` on every byte. An optimization would be to use `memchr` for the first delimiter byte and then check the next two, which would be significantly faster on long multi-line strings. However, TOML triple-quoted strings in real config files are typically short (< 100 lines), so the absolute savings are minimal.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | - | - | 0 | - |
| Pre-existing | - | - | 1 | - |

**Performance Score**: 9/10
**Recommendation**: APPROVED

## Rationale

The incremental changes in commits 13e13e9 and 0468ade are performance-positive overall:

1. **`MAX_JSON_DEPTH` cap (line 62)**: Bounds the `in_key_stack` Vec to 1024 entries, preventing unbounded heap growth on pathologically deep JSON input. This is a direct performance/reliability improvement with zero overhead on normal input (the `if` branch is only taken when depth exceeds 1024, which is astronomically rare in real data).

2. **`(eol + 1).min(len)` arithmetic bounds (lines 469, 481, 518)**: Prevents potential overflow on the last line of a file that lacks a trailing newline. Zero performance cost (single `min` instruction).

3. **TOML `find_toml_eq_sign` escape fix (lines 625-634)**: Converted from `for (i, &b) in enumerate()` to `while i < len` to correctly handle `\\` escapes by skipping two bytes. The manual index loop is actually slightly more efficient than the iterator-based version for this use case since it avoids the iterator overhead on the escape skip path.

4. **YAML newline trim (lines 345-351)**: Two conditional checks per quoted string value to trim trailing `\n`. Negligible overhead (two byte comparisons per YAML quoted value).

5. **Markdown `MAX_SOURCE_BYTES` guard (lines 50-55)**: Adds the same size guard that `classify_source` already has, ensuring the Markdown classifier rejects pathologically large input before invoking tree-sitter. This is a performance safety net, not a regression.

6. **Extracted helper functions** (`classify_json_key_at_depth0`, `strip_list_prefix`, `skip_json_whitespace`): Pure refactoring -- the compiler will inline these small functions. No performance impact.

All changes maintain the project's <50ms performance target for 1000-line files. The byte-by-byte and line-by-line scanners are inherently O(n) in source size with small constant factors. No N+1 patterns, no unbounded allocations, no blocking I/O.
