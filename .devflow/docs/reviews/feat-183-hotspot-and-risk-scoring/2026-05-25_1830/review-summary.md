# Code Review Summary — Cycle 2

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25_1830
**Cycle**: 2 (incremental after Cycle 1: 11/11 fixes applied)

---

## Merge Recommendation: CHANGES_REQUESTED

The code is architecturally sound and well-tested, but **two HIGH-severity reliability and testing issues in Category 1 (blocking) must be resolved before merge**.

Both issues involve asymmetric handling of NaN in the `decay_weight` function:
1. **Reliability (HIGH, 92% confidence)**: `decay_weight` uses `debug_assert!` instead of `assert!` for the `half_life_days > 0.0` precondition on a public boundary, creating an unguarded release-mode path that propagates NaN.
2. **Testing (MEDIUM, 85% confidence)**: No test for NaN `half_life_days` input to `decay_weight`, leaving the edge case undocumented.

Additionally, **two MEDIUM issues in consistency and Rust style should be addressed in the same fix**:
- Assertion level inconsistency between sibling `decay_weight` and `compute_file_risk_scores` functions
- Positional tuple access (`.0`/`.1`) instead of named destructuring

See Action Plan below for the straightforward fixes.

---

## Convergence Status: Cycle Progression

| Metric | Cycle 1 | Cycle 2 | Trend |
|--------|---------|---------|-------|
| **Issues Found** | 11 | 5 | ✅ 55% reduction |
| **Issues Fixed** | 11 | - | (pending) |
| **Blocking Issues** | 4 | 2 | ✅ 50% reduction |
| **Re-raised Issues** | 0 | 0 | ✅ No regressions |
| **Code Quality** | Improved | Stabilizing | ✅ Converging |

**Cycle 1 resolutions verified as correct:**
- ✅ Module doc style (`//!`) applied
- ✅ `debug_assert!` → `assert!` on public boundary (`compute_file_risk_scores`)
- ✅ Hot loop allocation fix (Cow-based deduplication)
- ✅ HashMap capacity heuristic with bounds
- ✅ NaN guard on `elapsed_days`
- ✅ Copy, PartialEq, Serialize, Deserialize added to `FileRiskScores`
- ✅ Test coverage expanded (NaN, Infinity, negative timestamp edge cases)

No Cycle 1 fixes were regressed or re-raised. The two issues in Cycle 2 are **new findings** uncovered by deeper reviewer analysis, not regressions.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| **Blocking** (Your Changes) | 0 | 1 | 2 | 0 | 3 |
| **Should Fix** (Code You Touched) | 0 | 0 | 0 | 0 | 0 |
| **Pre-existing** (Legacy) | 0 | 0 | 1 | 2 | 3 |

**Total Blocking**: 3 issues (1 HIGH, 2 MEDIUM)
**Total Non-Blocking**: 3 issues (informational only)

---

## Blocking Issues (Must Fix Before Merge)

### 1. PUBLIC API BOUNDARY: `decay_weight` uses `debug_assert!` instead of `assert!`
**Location**: `crates/rskim-search/src/temporal/scoring.rs:58`
**Severity**: HIGH (92% confidence across Reliability + Rust reviewers)
**Category**: Issue in Your Changes (blocking)

**Problem**:
- `decay_weight` is a public function exported via `temporal/mod.rs:17`
- Validates `half_life_days > 0.0` with `debug_assert!` (stripped in release builds)
- In release mode, passing `half_life_days = 0.0` → `(-elapsed / 0.0).exp()` → NaN/Inf propagation
- The sister function `compute_file_risk_scores` was already upgraded to `assert!` in Cycle 1 (line 99)
- This creates asymmetric safety guarantees: callers get hardened boundary checks from `compute_file_risk_scores` but unguarded access if calling `decay_weight` directly

**Root Cause**: The public boundary was partially hardened in Cycle 1 but inconsistently — only one of two public functions.

**Fix** (straightforward):
```rust
// Line 58, replace:
debug_assert!(half_life_days > 0.0);

// With:
assert!(half_life_days > 0.0, "half_life_days must be positive");

// Update doc comment (line 41) from "Panics in debug builds" to:
/// Panics when `half_life_days <= 0.0`.

// Update test (lines 126-131) to remove #[cfg(debug_assertions)] gate:
#[test]
#[should_panic(expected = "half_life_days must be positive")]
fn decay_zero_half_life_panics() {
    let _ = decay_weight(1.0, 0.0);
}
```

---

### 2. MISSING TEST: NaN `half_life_days` undocumented in `decay_weight`
**Location**: `crates/rskim-search/src/temporal/scoring_tests.rs` (missing test)
**Severity**: MEDIUM (85% confidence, Testing reviewer)
**Category**: Issue in Your Changes (blocking)

**Problem**:
- PR added NaN sanitization for `elapsed_days` parameter (lines 61-65) with tests
- However, no test for NaN in the `half_life_days` parameter
- The `debug_assert!` on line 58 is silent in release builds
- In release mode, `decay_weight(1.0, f64::NAN)` → `exp(NaN)` → NaN, which `clamp()` does **not** sanitize (NaN comparisons always false)
- Asymmetric testing coverage: `elapsed_days` has 3 edge-case tests (NaN, Inf+, Inf-), `half_life_days` has zero

**Root Cause**: Defensive programming applied to one parameter but not the other; test coverage reflects the asymmetry.

**Fix** (add test after fixing issue #1 above):
```rust
#[test]
#[should_panic(expected = "half_life_days must be positive")]
fn decay_nan_half_life_panics() {
    let _ = decay_weight(1.0, f64::NAN);
}
```

This test documents the expected behavior once the `assert!` is in place (issue #1).

---

### 3. CONSISTENCY: Assertion level divergence between `decay_weight` and `compute_file_risk_scores`
**Location**: `scoring.rs:58` vs `scoring.rs:99`
**Severity**: MEDIUM (85% confidence, Consistency reviewer)
**Category**: Issue in Your Changes (blocking)

**Problem**:
- Both functions validate `half_life_days > 0.0` on the same domain constraint
- `compute_file_risk_scores` uses `assert!` (all builds)
- `decay_weight` uses `debug_assert!` (debug-only)
- Both are public API functions in the same module
- Test at `scoring_tests.rs:174-183` explicitly documents this divergence
- The inconsistency is intentional (performance vs. safety), but not explicitly justified in source

**Root Cause**: Cycle 1 hardened the main entry point (`compute_file_risk_scores`) but left the lower-level function (`decay_weight`) with debug-only checks due to being "a hot-path function."

**Fix**: Resolve via issue #1 above (upgrade to `assert!`), then update the inline doc comment to explain the design:
```rust
/// # Panics
/// Panics when `half_life_days <= 0.0`.
/// 
/// Though this function is inlined and called in a hot loop, we assert rather than debug_assert
/// because it is public API and module boundary. The assertion is cheap (single comparison)
/// relative to the exponential computation that follows.
```

---

### 4. STYLE: Positional tuple access (`.0`/`.1`) instead of named destructuring
**Location**: `scoring.rs:141`, `scoring.rs:143`
**Severity**: MEDIUM (82% confidence, Consistency reviewer)
**Category**: Issue in Your Changes (blocking)

**Problem**:
- Accumulator uses `(weighted_total, weighted_fix_total)` tuples
- Code accesses via `.0` and `.1` positional indexing (lines 141, 143)
- Previous code used named destructuring `let (weighted_total, weighted_fix_total) = ...`
- The refactored Cow-based lookup lost the named bindings, reducing readability

**Root Cause**: Optimization for Cow deduplication (Cycle 1) replaced the entry destructuring pattern with inline `.0`/`.1` access.

**Fix** (straightforward, improves readability):
```rust
// Current (lines 140-144):
let entry = accum.entry(path).or_insert((0.0, 0.0));
entry.0 += w;
if is_fix {
    entry.1 += w;
}

// Improved:
let (weighted_total, weighted_fix_total) = accum.entry(path).or_insert((0.0, 0.0));
*weighted_total += w;
if is_fix {
    *weighted_fix_total += w;
}
```

This restores the named-field pattern while keeping the Cow optimization.

---

## Suggestions (Lower Confidence, 60-72%)

These are informational notes for future polish, not merge blockers:

- **PartialEq on f64 struct inconsistency** (Architecture, 65%) — `FileRiskScores` derives `PartialEq` while `SearchResult` explicitly avoids it due to NaN unreliability. Both hold f64 scores. `FileRiskScores` fields are guaranteed [0.0, 1.0] by construction (never NaN), so PartialEq is safe. Consider adding a doc comment explaining why it's safe here. *(Note: This was an intentional addition from Cycle 1 — not a regression.)*

- **Capacity heuristic uses undocumented magic number** (Consistency, 65%) — The `commits.len() / 4` divisor has no inline justification. Adding a comment explaining the "5-20x fewer unique files than commits" heuristic would clarify the formula. The clamp bounds `(64, 50_000)` are reasonable but also undocumented.

- **Duplicated "Naming note" doc blocks** (Consistency, 70%) — The "Naming note" explaining e-folding vs half-life appears twice (lines 17-22 and lines 34-37). Consider defining it once on the constant and cross-referencing from the function.

- **`decay_always_in_unit_range` test could be parameterized** (Testing, 62%) — The test uses a fixed array of 10 pairs. A parameterized or property-based approach would provide broader coverage.

- **No integration test with realistic commit volume** (Testing, 70%) — All tests use 5-50 commits. A test with hundreds/thousands would validate the HashMap capacity heuristic holds at scale.

---

## Pre-existing Issues (Informational Only — Do Not Block)

| Issue | Location | Confidence | Note |
|-------|----------|------------|------|
| No test for negative `half_life_days` in `compute_file_risk_scores` | `scoring_tests.rs` | 82% | Only zero is tested; negative should also be validated by the assert. Add: `fn compute_scores_negative_half_life_panics()` |
| `make_commit` helper casts `u64` to `i64` without overflow guard | `scoring_tests.rs:24` | 80% | Silent wrap on overflow possible. Use `i64::try_from(ts)` or change param to `i64` to prevent future test bugs. |
| `decay_weight` vs `compute_file_risk_scores` assertion asymmetry | `scoring.rs:58,99` | 72% | Intentional design choice (hot path vs. boundary) but not explicitly documented. Fixing issue #1 above resolves this. |

---

## Architecture & Design Observations

**Strengths verified by all 9 reviewers:**

1. ✅ **Clean I/O separation** — `scoring.rs` is pure computation, zero I/O, all functions pure and deterministic
2. ✅ **Shared types at module boundary** — `CommitInfo`, `FileChangeInfo`, `FileRiskScores` in `types.rs` enable cross-module use without circular deps
3. ✅ **Single Responsibility** — Each module has one reason to change (scoring, parsing, fix classification, types)
4. ✅ **Dependency Inversion** — `TemporalSource` trait in domain layer, `GixSource` infrastructure implements it
5. ✅ **Minimal public API** — Only `decay_weight`, `compute_file_risk_scores`, `DEFAULT_HALF_LIFE_DAYS`, `FileRiskScores` exported
6. ✅ **Performance-critical** — Single-pass hot loop, allocation-efficient (Cow deduplication), HashMap capacity with bounds
7. ✅ **No regressions** — All Cycle 1 fixes verified in place; no degradations from changes
8. ✅ **Comprehensive test suite** — 34 tests, 100% of blocking cases covered, edge cases (NaN, Infinity, timestamps) validated

**Security posture:**
- No unsafe code
- No injection vectors (pure math, no parsing)
- No secrets or credentials
- Numeric boundary enforcement via asserts
- No external deserialization vulnerabilities (Serde is output-only)

---

## Action Plan (in Priority Order)

1. **Upgrade `decay_weight` assertion to `assert!`** (fixes issues #1, #3)
   - Line 58: `assert!(half_life_days > 0.0, "half_life_days must be positive");`
   - Update doc comment (line 41) to reflect release-build guarantee
   - Remove `#[cfg(debug_assertions)]` gate from test at lines 126-131
   - Estimated effort: **2 minutes**

2. **Add test for NaN `half_life_days`** (fixes issue #2)
   - Add `fn decay_nan_half_life_panics()` with `#[should_panic]` in test suite
   - Documents the edge case and validates the assert fires
   - Estimated effort: **1 minute**

3. **Restore named destructuring in accumulator loop** (fixes issue #4, improves readability)
   - Lines 140-144: Replace `.0` / `.1` access with named bindings
   - Estimated effort: **1 minute**

4. **Polish (optional, non-blocking):**
   - Add inline comment explaining `commits.len() / 4` heuristic
   - Consolidate "Naming note" doc duplication
   - Add negative `half_life_days` test to pre-existing suite

**Total effort for blocking fixes: ~4 minutes**

---

## Reviewer Scores Summary

| Reviewer | Score | Recommendation | Key Finding |
|----------|-------|-----------------|-------------|
| Architecture | 9/10 | APPROVED | Clean I/O separation, DIP, no coupling |
| Security | 9/10 | APPROVED | No injection vectors, boundary enforcement sound |
| Performance | 9/10 | APPROVED | Single-pass loop, efficient allocations, inlining |
| Complexity | 9/10 | APPROVED | Algorithm is simple, clear structure |
| Consistency | 8/10 | APPROVED_WITH_CONDITIONS | 2 MEDIUM issues (assertion asymmetry, tuple access) |
| Regression | 9/10 | APPROVED | No breaking changes, backward compatible |
| Testing | 8/10 | APPROVED_WITH_CONDITIONS | 1 MEDIUM missing test (NaN half_life) |
| Reliability | 8/10 | CHANGES_REQUESTED | 1 HIGH (debug_assert in public API) + 1 MEDIUM (NaN guard asymmetry) |
| Rust | 9/10 | APPROVED_WITH_CONDITIONS | 1 MEDIUM (NaN half_life guard) |

**Consensus**: 7 reviewers approve with minor conditions; 2 request changes for HIGH/MEDIUM reliability and testing issues. All issues are straightforward to fix.

