# Architecture Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30:00Z
**Prior Resolutions**: Cycle 1 fixed 11/11 issues (module doc style, debug_assert to assert, hot loop allocation, HashMap capacity, NaN guard, Copy/PartialEq/Serialize/Deserialize on FileRiskScores, test coverage). None re-raised.

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **PartialEq on f64 struct inconsistency** - `types.rs:268` (Confidence: 65%) -- `FileRiskScores` derives `PartialEq` with two `f64` fields, while `SearchResult` (line 369) explicitly documents NOT deriving `PartialEq` because "f64 cannot implement it reliably (NaN != NaN)". Both types hold f64 scores. The `FileRiskScores` fields are guaranteed in [0.0, 1.0] by construction (never NaN), so PartialEq is safe in practice, but the divergent pattern could confuse future contributors. Consider adding a doc comment to `FileRiskScores` explaining why PartialEq is safe here (fields are always finite). This was an intentional addition from Cycle 1 -- not a regression.

- **`decay_weight` uses `debug_assert` while `compute_file_risk_scores` uses `assert` for the same precondition** - `scoring.rs:58` vs `scoring.rs:99` (Confidence: 70%) -- Both functions require `half_life_days > 0.0`. The public-facing `compute_file_risk_scores` correctly uses `assert!` (fires in release builds), but `decay_weight` (also `pub`) uses only `debug_assert!`. A caller using `decay_weight` directly in release mode could pass `0.0` and get `NaN` from division by zero. The CLAUDE.md Rust rule says "debug_assert for invariants in hot paths, assert at module boundaries." Since `decay_weight` is public API and a module boundary, it arguably should use `assert!` -- but it is also an `#[inline]` hot-path function, so `debug_assert` follows the hot-path convention. Documenting this design choice in the function's doc comment would resolve the ambiguity.

- **No `Eq` on `FileRiskScores` despite `PartialEq`** - `types.rs:268` (Confidence: 60%) -- `PartialEq` without `Eq` is correct for f64-containing types (since `Eq` requires reflexivity, which NaN violates). The current derive list is technically correct. Noting for completeness only -- no action needed.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR exhibits strong architectural discipline:

1. **Clean I/O separation**: `scoring.rs` contains zero I/O. All functions are pure, deterministic, and take `now_epoch` as an explicit parameter. The `TemporalSource` trait in `types.rs` defines the I/O boundary; `GixSource` in `git_parser.rs` implements it. Scoring never touches git or the filesystem -- it operates solely on `&[CommitInfo]` slices. This matches the feature knowledge description precisely.

2. **Shared types at the right level**: `CommitInfo`, `FileChangeInfo`, and `FileRiskScores` live in `types.rs` (the crate's pure type module), not in the temporal module. This allows other modules (e.g., `cochange`) to share these types without creating circular dependencies.

3. **Single Responsibility**: `scoring.rs` does exactly one thing -- compute risk scores from commit data. Fix-commit classification (`is_fix_commit`) lives in `mod.rs` as a standalone predicate. The parser lives in `git_parser.rs`. Each module has one reason to change.

4. **Dependency Inversion via trait**: The `TemporalSource` trait in `types.rs` (the domain layer) defines the interface. `GixSource` (infrastructure) implements it. Domain does not depend on infrastructure -- the dependency arrow points inward. This is textbook DIP / hexagonal architecture.

5. **Public API surface is minimal and well-documented**: Only `decay_weight`, `compute_file_risk_scores`, `DEFAULT_HALF_LIFE_DAYS`, and `FileRiskScores` are exported. Each has full doc comments with examples, panics documentation, and algorithm descriptions. The `#[must_use]` annotations prevent silent discard of return values.

6. **No coupling introduced**: The scoring module depends only on `crate::types` and `super::is_fix_commit`. It does not pull in gix, regex, or any I/O crate. The dependency graph remains a clean DAG.

7. **Capacity heuristic is bounded**: `HashMap::with_capacity((commits.len() / 4).clamp(64, 50_000))` has explicit lower and upper bounds, preventing both under-allocation and OOM from adversarial input. This follows the reliability principle of explicit bounds on all allocations.

The three suggestions above are all below the 80% confidence threshold -- they represent minor documentation opportunities, not architectural defects. No blocking or should-fix issues were found.
