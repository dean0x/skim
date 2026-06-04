# Consistency Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45:00Z

## Issues in Your Changes (BLOCKING)

### HIGH

**Deny-list patterns duplicated between `deny_list.rs` constants and `deny_list_pattern_names()` in binary** - `crates/rskim-bench/src/bin/cochange_validate.rs:238-262`, `crates/rskim-bench/src/cochange/deny_list.rs:13-45`
**Confidence**: 90%
- Problem: The `deny_list_pattern_names()` function in the binary manually re-lists all 21 deny-list patterns for report metadata. These are the same constants defined in `deny_list.rs` (`DENIED_FILENAMES`, `DENIED_DIRS`, `DENIED_EXTENSIONS`) but duplicated as strings with different formatting (dirs get trailing `/`, extensions get `*.` prefix). The two sources are already inconsistent: `deny_list.rs` includes `.git` as a denied directory but `deny_list_pattern_names()` does not include `.git/`. Any future change to the deny list must be made in two places, and the current mismatch means the report metadata does not accurately reflect the actual filtering behavior.
- Fix: Export a public function from `deny_list.rs` that returns the authoritative pattern names (e.g., `pub fn pattern_names() -> Vec<String>`) derived from the constants. This follows the existing codebase pattern where `report.rs` in rskim-bench derives display names from authoritative sources (see `field_display_name()` which derives from `SearchField::name()` to avoid duplication). The binary would then call `deny_list::pattern_names()` instead of maintaining its own list. Applies ADR-001 (fix immediately rather than deferring).

### MEDIUM

**`OutputFormat` `Display` impl placed at end of file, inconsistent with main.rs pattern** - `crates/rskim-bench/src/bin/cochange_validate.rs:279-286`
**Confidence**: 82%
- Problem: In the existing `main.rs`, the `Display` impl for `OutputFormat` appears directly after the enum definition (lines 41-48). In the new `cochange_validate.rs`, the identical enum is defined at lines 39-46 but the `Display` impl is placed at the very end of the file (lines 279-286), separated by ~230 lines of unrelated code. This makes the type harder to understand at a glance.
- Fix: Move the `impl std::fmt::Display for OutputFormat` block to immediately follow the `OutputFormat` enum definition (after line 46), matching the established pattern in `main.rs`.

**`check_quality_gates` returns `Result<(), String>` instead of `anyhow::Result`** - `crates/rskim-bench/src/cochange/validate.rs:116`
**Confidence**: 85%
- Problem: Every other public fallible function in `rskim-bench` returns `anyhow::Result<T>` (see `harness::aggregate_results`, `report::to_json`, `qrel::generate_qrels`, `qrel::validate_qrel_coverage`, `tuning::result_to_config`). This function uniquely returns `Result<(), String>`. The doc comment explains the rationale (storing the string directly in `quality_gate_reason`), but this creates a pattern inconsistency. The caller in `validate_repo` already converts it with a `Some(reason)` assignment, which would work identically with `anyhow::Error` via `.to_string()`.
- Fix: Change to `pub fn check_quality_gates(commits: &[CommitInfo]) -> anyhow::Result<()>` using `anyhow::bail!()` for failures. At the call site in `validate_repo`, capture with `if let Err(e) = check_quality_gates(&all_commits) { ... quality_gate_reason: Some(e.to_string()) ... }`. This is minor but maintains the crate-wide convention.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`chrono_now()` produces approximate, non-ISO-8601 timestamps** - `crates/rskim-bench/src/bin/cochange_validate.rs:207-222` (Confidence: 70%) -- The custom timestamp function computes year as `1970 + (secs / 86400) / 365` which ignores leap years, and outputs `YYYY-XX-XXT` format which is not valid ISO-8601. The `RunMetadata::timestamp` doc says "ISO-8601 timestamp". Consider using the `time` crate (already a transitive dependency via `gix`) for correct formatting, or at minimum document the approximation.

- **Aggregate micro metrics averaged by repo count rather than accumulated** - `crates/rskim-bench/src/cochange/validate.rs:556-576` (Confidence: 65%) -- The `aggregate_metrics` function averages micro precision/recall across repos (`mip_sum / count`), but micro averaging conventionally means accumulating TP/predicted/actual counts across all data points and then computing ratios. The current approach is "macro-averaging of micro metrics" which is unusual. The field names (`micro_precision`, `micro_recall`) may be misleading. This could be intentional but is worth a comment clarifying the choice.

- **`make_commit` test helper duplicated across 3 files with slightly different signatures** - `crates/rskim-bench/src/cochange/validate.rs:645`, `crates/rskim-bench/src/cochange/temporal_split.rs:123`, `crates/rskim-bench/tests/cochange_validation.rs:91` (Confidence: 65%) -- Three different `make_commit` helpers with slightly different signatures (`(id, timestamp, paths: &[&str])` vs `(timestamp, id)` with single-file). In the existing bench crate, test helpers like `make_rust_files_with_content` are defined per-module. The current approach is acceptable but consolidating into a shared `#[cfg(test)]` helper module would reduce duplication.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The new cochange module is well-structured and follows most established patterns in the rskim-bench crate: consistent use of `#[must_use]`, `#[cfg(test)]` module organization, section separators (`// ====`), `anyhow` for error handling, serde derives on types, and the `report.rs` / `types.rs` / `validate.rs` module decomposition mirrors the existing `report.rs` / `types.rs` / `harness.rs` structure. The deny-list duplication (HIGH) is the primary consistency concern -- it creates a maintenance burden and already has a `.git` mismatch between the source of truth and the report metadata.
