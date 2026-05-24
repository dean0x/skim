# Regression Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-15T22:27
**Scope**: Incremental (1 commit: bd7b8c1)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Typo in comment: `FileChangeInfoInfo` (doubled suffix)** - `crates/rskim/src/cmd/heatmap/metrics.rs:78`
**Confidence**: 95%
- Problem: The find-and-replace that converted `FileChange` to `FileChangeInfo` in comments also hit `FileChangeInfo.path`, producing `FileChangeInfoInfo.path`. This is a documentation accuracy regression — the comment now names a type that does not exist.
- Fix:
  ```rust
  // line 78: change
  // &str keys borrow from PathBuf::to_str() on each FileChangeInfoInfo.path — valid
  // to
  // &str keys borrow from PathBuf::to_str() on each FileChangeInfo.path — valid
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Regression Checklist

- [x] No exports removed without deprecation — type aliases replaced with direct re-exports; old names (`CommitRecord`, `FileChange`) were `pub(crate)` only and all internal consumers migrated
- [x] Return types backward compatible — `fetch_commits` returns `Vec<CommitInfo>` (same underlying type, just canonical name)
- [x] Default values unchanged
- [x] Side effects preserved
- [x] All consumers of changed code updated — zero remaining references to `CommitRecord` or bare `FileChange` in the codebase
- [x] Migration complete across codebase — grep confirms no leftover old-name usage
- [x] Commit message matches implementation — states "remove type aliases, use canonical names" which is exactly what the diff does
- [x] Tests pass — 135 heatmap unit tests + 26 integration tests all green

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The refactor is clean and complete: all four files consistently replace type aliases with canonical re-exports, the migration leaves no orphaned references, and all 135+ tests pass. The single blocking item is a trivial comment typo (`FileChangeInfoInfo` on line 78 of metrics.rs) introduced by an overzealous find-and-replace. Fix the typo and this is ready to merge.
