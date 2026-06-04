# Regression Review Report

**Branch**: feat-183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T14:29
**PR**: #252

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**`FileRiskScores` missing `PartialEq` and `Serialize`/`Deserialize` derives** - `crates/rskim-search/src/types.rs:268`
**Confidence**: 82%
- Problem: Every other public struct in `types.rs` derives `PartialEq` and `Serialize`/`Deserialize` (see `CochangeStats`, `TemporalFlags`, `CommitInfo`, `FileChangeInfo`, `TemporalMetadata`, `HistoryResult`, `IndexStats`). `FileRiskScores` derives only `Debug, Clone`. The module-level doc comment explicitly states "All types are derived with appropriate traits." The absence of `PartialEq` prevents consumers from writing equality assertions in tests without manual float comparison. The absence of `Serialize`/`Deserialize` prevents round-tripping through JSON, which is the established pattern for all other public types in this crate (the `SearchResult` type also has `f64` fields and does derive `Serialize`/`Deserialize`). While `PartialEq` on `f64` fields has known caveats (NaN != NaN), the existing `SearchResult` type handles this identically -- its doc comment notes the NaN caveat but still derives `Serialize`/`Deserialize`. Future consumers wanting to serialize risk scores (e.g., a `--json` flag on heatmap output) would be blocked.
- Fix: Add derives consistent with the crate's convention:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct FileRiskScores {
  ```
  Omitting `PartialEq` is defensible (matching `SearchResult`), but `Serialize`/`Deserialize` should be added to match every other public type in the module.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`debug_assert!` for `half_life_days > 0` silently passes zero in release** - `crates/rskim-search/src/temporal/scoring.rs:47,81` (Confidence: 65%) -- In release builds, `decay_weight(x, 0.0)` produces `-inf.exp() = 0` or `inf.exp() = inf` depending on sign, then clamped. No panic, no error. A consumer accidentally passing `0.0` in production would get silently wrong results. Consider a runtime guard or `assert!` at the `compute_file_risk_scores` entry point. However, the doc comment does document this, and `debug_assert!` is the project's stated convention for hot-path invariants (CLAUDE.md: "debug_assert! for invariants in hot paths").

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Regression Analysis Summary

**Lost Functionality**: None. No exports removed. All prior re-exports from `lib.rs` (`GixSource`, `is_fix_commit`) remain intact. The change is purely additive -- three new symbols (`DEFAULT_HALF_LIFE_DAYS`, `compute_file_risk_scores`, `decay_weight`) and one new type (`FileRiskScores`) are added to the public API.

**Broken Behavior**: None. The `is_fix_commit` function, `GixSource`, and all existing types are untouched. The heatmap module (`crates/rskim/src/cmd/heatmap/metrics.rs`) continues to use `rskim_search::is_fix_commit` without any interface change. No return types widened, no defaults changed, no side effects removed.

**Intent vs Reality**: Matches. The commit message states "Add per-file hotspot and bug-fix density metrics to the temporal module. Pure computation, no I/O, no new dependencies." The implementation delivers exactly this: two pure functions, one new struct, 47 tests, no I/O, no new crate dependencies.

**Incomplete Migrations**: N/A. This is net-new functionality with no old API to migrate from. The new symbols are not yet consumed by the CLI crate, which is expected -- the PR description positions this as a foundational computation layer for future integration.

**Test Coverage**: 47 new tests across 6 groups (decay_weight unit tests, basic cases, acceptance criteria, fix density specifics, edge cases, determinism). All 388 rskim-search tests pass. The tests cover boundary conditions (negative timestamps, future commits, very old commits, zero elapsed) and verify mathematical properties (monotonicity, unit range, determinism across 50 runs).

**Condition for approval**: Add `Serialize, Deserialize` derives to `FileRiskScores` to match the crate's convention for public types.
