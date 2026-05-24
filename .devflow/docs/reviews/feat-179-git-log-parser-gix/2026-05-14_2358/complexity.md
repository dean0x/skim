# Complexity Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Repetitive `to_string_lossy().into_owned()` pattern across 6 metrics functions** - `crates/rskim/src/cmd/heatmap/metrics.rs:36`, `metrics.rs:99`, `metrics.rs:211`, `metrics.rs:268`, `metrics.rs:337`, `metrics.rs:450`, `metrics.rs:462`
**Confidence**: 85%
- Problem: The migration from `String` paths to `PathBuf` introduced `file.path.to_string_lossy().into_owned()` at 7+ call sites across `compute_churn`, `compute_coupling`, `compute_stability`, `compute_authors`, `compute_fix_after_touch`, and `compute_encapsulation`. Each function independently performs the same lossy conversion, allocating a new `String` each time a file path is accessed. This is a readability and maintainability issue -- any change to how paths are stringified requires updating every call site.
- Fix: Add a helper method or free function in the `types` module (or on `FileChangeInfo` itself) to centralize the conversion:
  ```rust
  impl FileChangeInfo {
      /// Repo-root-relative path as a borrowed `&str` (or owned String for non-UTF8 paths).
      pub fn path_str(&self) -> Cow<'_, str> {
          self.path.to_string_lossy()
      }
  }
  ```
  Then replace all `file.path.to_string_lossy().into_owned()` with `file.path_str().into_owned()` (or use `Cow` directly to avoid allocation when the path is valid UTF-8).

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`parse_history_impl` function length at 108 lines (lines 73-181)** - `crates/rskim-search/src/temporal/git_parser.rs:73`
**Confidence**: 82%
- Problem: `parse_history_impl` is 108 lines with 3 levels of nesting in the main loop body (for-match-match and for-match-if patterns). While each section is documented with clear comments, the function handles repository opening, shallow detection, HEAD resolution, cutoff computation, sorting configuration, the commit walk loop (with error handling, timestamp extraction, cutoff checking, field extraction, and tree-diff delegation), and result assembly. Cyclomatic complexity is moderate (~8-9 paths through error handling and conditional branches).
- Fix: Consider extracting the commit walk loop body (lines 125-171) into a helper function like `process_commit_info(info, cutoff_secs, is_shallow, repo) -> Option<CommitInfo>` that returns `None` for skipped commits and `Err` for fatal errors. This would reduce `parse_history_impl` to ~50 lines and isolate the per-commit logic.

**`compute_coupling` function length at 100 lines** - `crates/rskim/src/cmd/heatmap/metrics.rs:83`
**Confidence**: 80%
- Problem: `compute_coupling` spans 100 lines (83-183) with nested loops for pair enumeration (3 levels deep at lines 115-125). This is pre-existing complexity, but the migration touched it (changing `commit.files` to `commit.changed_files` and adjusting the `to_str` calls). The function handles four distinct phases: accumulation, pair enumeration, output construction, and sorting. The 3-deep nesting in the pair enumeration loop is at the threshold for readability.
- Fix: This is a should-fix-while-here opportunity. The pair enumeration (lines 114-126) could be extracted into a helper, though the function's current structure with clear section comments is adequate. No immediate action required unless the function grows further.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`compute_fix_after_touch` function at 82 lines with moderate nesting** - `crates/rskim/src/cmd/heatmap/metrics.rs:321-404`
**Confidence**: 80%
- Problem: This function has a nested filter closure (lines 372-378) that scans a range for fix commits within a window, creating 3 levels of nesting. The overall logic is correct and well-commented, but the combination of HashSet construction, multiple Vec allocations, and the range-scan filter makes it the most complex single function in the metrics module. Not touched by this PR beyond field renames.

## Suggestions (Lower Confidence)

- **`is_unborn_error` string matching fragility** - `crates/rskim-search/src/temporal/git_parser.rs:252-259` (Confidence: 70%) -- Checking 6 different substring patterns in a lowercased error message is brittle; if gix changes its error wording, a valid unborn repo could surface as a hard error. Consider matching on the gix error type/variant directly if the API exposes one.

- **Magic number `86_400` used in two files** - `crates/rskim-search/src/temporal/git_parser.rs:102`, `crates/rskim/src/cmd/heatmap/metrics.rs:233` (Confidence: 65%) -- The seconds-per-day constant `86_400` appears inline in both the new `git_parser.rs` and the existing `metrics.rs`. A named constant (`SECONDS_PER_DAY`) would improve readability, though this is a minor stylistic point.

- **`changed_files_for_commit` closure return type verbosity** - `crates/rskim-search/src/temporal/git_parser.rs:218` (Confidence: 62%) -- The closure signature `|change| -> std::result::Result<_, std::convert::Infallible>` is verbose. A type alias like `type InfallibleResult<T> = std::result::Result<T, std::convert::Infallible>` or using `Ok::<_, Infallible>(...)` at the return site would reduce noise.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new code is well-structured with clear function decomposition, good separation of concerns (GixSource delegates to `parse_history_impl` which delegates to `changed_files_for_commit`), and small focused helper functions (`gix_err`, `is_unborn_error`, `empty_result`, `first_line_of`). The main complexity concern is the repetitive `to_string_lossy().into_owned()` pattern introduced by the `String` to `PathBuf` migration in the heatmap metrics, which should be centralized before it proliferates further. The `parse_history_impl` function is at the upper boundary of comfortable length but remains readable thanks to clear section comments. Overall, the PR demonstrates good complexity management for a feature of this scope.
