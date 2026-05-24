# Regression Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental (459d0af...HEAD, 3 commits)

## Issues in Your Changes (BLOCKING)

### HIGH

**`-j` short flag for `--json` removed** - `crates/rskim/src/cmd/search/mod.rs:136`
**Confidence**: 95%
- Problem: The previous `parse_flags` matched `"--json" | "-j"` on the JSON flag arm. The refactored version only matches `"--json"`. Any existing scripts or user habits relying on `-j` will now hit the new unrecognised-flag error path (`anyhow::bail!`), turning a working invocation into a hard error. This is a breaking behavioral change introduced by the refactoring commit.
- Fix: Restore the short alias:
  ```rust
  "--json" | "-j" => json = true,
  ```
  Also add `-j` to the "Valid flags" list in the unrecognised-flag error message at line 171.

### MEDIUM

**`--stats --json` output format changed from Debug to Display** - `crates/rskim/src/cmd/search/mod.rs:287`
**Confidence**: 82%
- Problem: The staleness field in JSON output previously used Rust `Debug` formatting (`format!("{staleness_status:?}")`) which produced `"Current"`, `"HeadChanged { stored: \"...\", current: \"...\" }"`, etc. It now uses `Display` (`staleness_status.to_string()`) which produces `"current"`, `"stale (HEAD changed: abcdef12...->12345678...)"`, etc. Any downstream consumers parsing the JSON `staleness` field (scripts, agent hooks, CI pipelines) will break on the changed string values.
- Fix: This may be intentional (Display is more human-friendly). If so, document as a breaking change. If backward compatibility matters, consider keeping the Debug-style enum variant names in JSON output while using Display for the text output. For example:
  ```rust
  // JSON: machine-friendly variant name
  "staleness": format!("{staleness_status:?}"),
  // Text: human-friendly Display
  writeln!(out, "  staleness     : {staleness_status}")?;
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Action flag last-one-wins silently** - `crates/rskim/src/cmd/search/mod.rs:130-135` (Confidence: 65%) -- When multiple action flags are passed (e.g. `--build --rebuild`), the last one silently wins because `action_flag` is overwritten. The old code had the same implicit behavior through the `if` cascade, but the new enum-based approach makes it easy to detect and reject conflicting flags. Consider erroring on duplicate action flags.

- **`query_parts` collected but unused when action flag is set** - `crates/rskim/src/cmd/search/mod.rs:176,181` (Confidence: 62%) -- When an action flag like `--build` is present, positional arguments still accumulate into `query_parts` but are discarded by `action_flag.unwrap_or_else(...)`. This is harmless but slightly misleading -- `skim search --build some_text` silently ignores `some_text`. Consider warning or erroring when positional args accompany a non-query action.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The `-j` short flag removal is a clear regression -- it breaks backward compatibility for any user or script relying on the short form. The `--stats --json` format change is a behavioral change that may or may not be intentional but should be acknowledged. Otherwise the refactoring is well-executed: the `SearchAction` enum, `Result`-returning `parse_flags`, `Display` for `StalenessCheck`, the infinite-rebuild-loop fix, and the manifest-return optimization are all clean improvements with good test coverage (56 new tests). All callers of the changed function signatures (`check_staleness`, `auto_refresh_if_stale`, `parse_flags`) have been correctly updated.
