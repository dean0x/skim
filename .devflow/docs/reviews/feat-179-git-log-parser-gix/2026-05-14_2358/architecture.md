# Architecture Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicated fix-commit regex between temporal module and heatmap metrics** - `crates/rskim/src/cmd/heatmap/metrics.rs:20-22`, `crates/rskim-search/src/temporal/mod.rs:22-23`
**Confidence**: 90%
- Problem: The PR introduces `is_fix_commit()` with a `Lazy<Regex>` in `temporal/mod.rs` and exports it from `rskim-search`. However, `build_fix_regex()` in `metrics.rs:20-22` contains an identical regex pattern `r"(?i)\b(fix|bug|hotfix|patch|revert)\b"` and is still used by `compute_stability` and `compute_fix_after_touch`. The PR description states it "eliminates the duplication" between heatmap and shared types, and it successfully does so for `CommitRecord`/`FileChange`, but it leaves this regex duplication untouched. Two sources of truth for fix-commit classification will inevitably drift -- if a keyword is added to one, the other will be missed.
- Fix: In `metrics.rs`, remove `build_fix_regex()` and change `compute_stability` and `compute_fix_after_touch` to accept `&dyn Fn(&str) -> bool` or simply call `rskim_search::is_fix_commit()` directly. This eliminates the duplicated regex and aligns heatmap with the canonical predicate. If the `Regex` parameter is needed for backward compatibility, wrap `is_fix_commit` in a local adapter.

**Inconsistent path-to-string conversion strategy across heatmap metrics** - `crates/rskim/src/cmd/heatmap/metrics.rs:99`, `crates/rskim/src/cmd/heatmap/metrics.rs:36`
**Confidence**: 85%
- Problem: The migration from `String` to `PathBuf` for `FileChangeInfo.path` introduced two different conversion strategies within the same module. `compute_coupling` at line 99 uses `f.path.to_str().unwrap_or("")` (returns `Option<&str>`, silently replaces non-UTF-8 paths with empty string), while `compute_churn` (line 36), `compute_stability` (line 213), `compute_authors` (line 268), `compute_fix_after_touch` (line 337), and `compute_encapsulation` (line 450) all use `f.path.to_string_lossy().into_owned()` (allocates, replaces invalid bytes with U+FFFD). The `unwrap_or("")` in coupling is particularly concerning: a non-UTF-8 path would be treated as an empty string, distorting coupling metrics by conflating all non-UTF-8 files into a single "" key.
- Fix: Standardize on `to_string_lossy().into_owned()` in `compute_coupling` to match the other five metric functions. If zero-copy `&str` borrowing is desired for coupling (which borrows from `CommitRecord`), use `to_str().unwrap_or_default()` consistently, but document the choice. The empty-string conflation risk should be addressed either way -- consider filtering out non-UTF-8 paths rather than mapping them to "".

### MEDIUM

**`timestamp` field type mismatch between `CommitInfo` (i64) and heatmap consumers (u64 casts)** - `crates/rskim/src/cmd/heatmap/metrics.rs:215`, `crates/rskim/src/cmd/heatmap/git_source.rs:216`
**Confidence**: 82%
- Problem: `CommitInfo.timestamp` is `i64` (correctly supporting pre-epoch commits as documented in the parser). However, the heatmap migration introduces `as i64` casts in `git_source.rs:216` (parsing `u64` then casting to `i64`) and `as u64` casts in `metrics.rs:215` (casting `commit.timestamp` back to `u64` for `file_commits`). The `as u64` cast on a negative `i64` would produce a very large number, corrupting recency calculations. The `compute_stability` function's signature accepts `now_epoch: u64` while iterating `commit.timestamp as u64`, creating a mismatch when timestamps are negative. This is a latent bug for repositories with pre-epoch timestamps (rare but explicitly documented as supported in the parser).
- Fix: Either (a) make `compute_stability` and all heatmap timestamp handling work with `i64` throughout, or (b) clamp negative timestamps to 0 at the conversion boundary in `git_source.rs` with a documented decision. Option (a) is architecturally cleaner since the shared type chose `i64` deliberately.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Type alias re-export creates misleading names** - `crates/rskim/src/cmd/heatmap/types.rs:9`
**Confidence**: 80%
- Problem: `pub(crate) use rskim_search::{CommitInfo as CommitRecord, FileChangeInfo as FileChange}` renames the canonical types to preserve backward compatibility with heatmap code. While this avoids a large rename, it introduces a permanent naming divergence: all heatmap code refers to `CommitRecord` while the canonical type is `CommitInfo`, and `FileChange` while the canonical type is `FileChangeInfo`. Field names also diverge (`.message` vs the old `.subject`, `.changed_files` vs the old `.files`). New contributors will be confused about whether `CommitRecord` and `CommitInfo` are different types. This also means the old field names (`.subject`, `.files`) had to be changed across all heatmap files anyway, so the aliases only saved a struct name rename.
- Fix: Since all field accesses were already updated (`.subject` -> `.message`, `.files` -> `.changed_files`), the type aliases provide diminishing value. Consider renaming `CommitRecord` -> `CommitInfo` and `FileChange` -> `FileChangeInfo` throughout the heatmap module in a follow-up, then removing the aliases. This completes the consolidation the PR started.

**Heatmap retains its own `GitDataSource` trait alongside the new `TemporalSource` trait** - `crates/rskim/src/cmd/heatmap/git_source.rs:34-50`, `crates/rskim-search/src/types.rs:213-228`
**Confidence**: 80%
- Problem: The PR introduces `TemporalSource` as the canonical trait for git history parsing, with `GixSource` as its implementation. Meanwhile, heatmap retains its `GitDataSource` trait (which shells out to git CLI) and `CliGitSource` implementation. Both traits serve the same fundamental purpose -- fetching commit history -- but use different data flow paths. The PR's stated goal is to introduce shared types and eliminate duplication, but the trait-level duplication remains. This is not blocking because the heatmap's `GitDataSource` has additional methods (`is_git_repo`, `get_repo_root`, `detect_shallow_clone`) that `TemporalSource` does not, but it signals an incomplete architectural migration.
- Fix: This is expected as an incremental migration. Document the intent to eventually replace `GitDataSource` + `CliGitSource` with `TemporalSource` + `GixSource` in the heatmap module, likely in a follow-up PR. The shared types introduced here are the first step toward that convergence.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`build_fix_regex` constructs a new Regex on every call** - `crates/rskim/src/cmd/heatmap/metrics.rs:20-22`
**Confidence**: 85%
- Problem: `build_fix_regex()` compiles a regex from scratch each time it is called. In contrast, the new `temporal/mod.rs` correctly uses `Lazy<Regex>` for the same pattern. Since `build_fix_regex` is called once in `mod.rs` and passed down, this is not a hot-path performance issue, but it is an inconsistency that would be resolved by adopting `is_fix_commit`.

## Suggestions (Lower Confidence)

- **`first_parent_only()` limits diff accuracy for merge-heavy workflows** - `crates/rskim-search/src/temporal/git_parser.rs:119` (Confidence: 65%) -- The rev-walk uses `first_parent_only()` which skips commits from merged branches. For temporal scoring this is a reasonable default (matches `git log --first-parent`), but it means the changed-files set may differ from what `git log` without `--first-parent` would report. Worth documenting as a known limitation.

- **`is_unborn_error` relies on error message substring matching** - `crates/rskim-search/src/temporal/git_parser.rs:252-259` (Confidence: 70%) -- Matching on lowercase substrings of error messages is fragile if gix changes its error wording. However, there is no typed error variant in gix for "unborn HEAD", so string matching is the pragmatic choice.

- **Object cache size is hardcoded at 4MB** - `crates/rskim-search/src/temporal/git_parser.rs:79` (Confidence: 60%) -- The 4MB cache may be suboptimal for very large or very small repositories. Consider making this configurable or using gix's default heuristics.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The PR introduces a well-designed `temporal` module with clean separation of concerns: gix types are correctly contained at the parser boundary, the `TemporalSource` trait enables testability and future alternative implementations, and the shared types in `rskim-search::types` are appropriate as canonical definitions. The `GixSource` struct being stateless, `Copy`, and `Send + Sync` is an excellent design choice.

The two HIGH findings are mechanical issues from the migration: the duplicated fix-commit regex (which the PR partly intended to eliminate) and the inconsistent `PathBuf`-to-string conversion strategy. Both are straightforward to resolve. The MEDIUM timestamp type mismatch is a latent correctness concern worth addressing before the migration is complete.
