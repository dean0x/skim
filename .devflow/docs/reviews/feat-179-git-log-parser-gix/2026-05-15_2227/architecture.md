# Architecture Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-15T22:27
**Scope**: Incremental (1 commit: bd7b8c1)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Typo in comment: `FileChangeInfoInfo` (double-Info suffix)** - `crates/rskim/src/cmd/heatmap/metrics.rs:78`
**Confidence**: 95%
- Problem: The mechanical rename from `FileChange` to `FileChangeInfo` applied to a comment that already contained `FileChangeInfo`, producing the nonsensical `FileChangeInfoInfo.path`. This misleads any developer reading the borrowing safety rationale in `compute_coupling`.
- Fix:
```rust
// &str keys borrow from PathBuf::to_str() on each FileChangeInfo.path -- valid
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

This commit is a clean mechanical refactor: it removes two type aliases (`CommitRecord` = `CommitInfo`, `FileChange` = `FileChangeInfo`) from `types.rs` and replaces all usages with the canonical names from `rskim_search`. The architectural impact is positive:

1. **Eliminates indirection** -- consumers now use the same names as the source-of-truth crate (`rskim-search`), reducing cognitive overhead. This aligns with the deep-modules principle: no shallow alias layer that hides nothing.

2. **Preserves module boundaries** -- the re-export in `types.rs` (`pub(crate) use rskim_search::{CommitInfo, FileChangeInfo}`) remains the single import point, so the heatmap module's internal organization is unchanged. No layering violations.

3. **Consistent naming across crates** -- `git_source.rs`, `metrics.rs`, and `mod.rs` all now use the same type names as `rskim-search`, eliminating the dual-name confusion that the aliases created.

The only blocking item is a minor comment typo introduced by the find-replace. Fix that and the PR is clean.
