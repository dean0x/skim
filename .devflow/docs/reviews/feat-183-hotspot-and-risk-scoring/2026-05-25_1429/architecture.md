# Architecture Review Report

**Branch**: feat-183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25
**PR**: #252

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`FileRiskScores` missing `Copy` derive — two-f64 struct is trivially copyable** - `crates/rskim-search/src/types.rs:268`
**Confidence**: 85%
- Problem: `FileRiskScores` contains only two `f64` fields (16 bytes total) but only derives `Debug, Clone`. This forces consumers to call `.clone()` where the compiler could simply copy the value. Other similarly-shaped types in the same file (`FileId`, `SearchField`) derive `Copy`. While `f64` cannot derive `Eq`/`Hash`, it can derive `Copy` and `PartialEq`. The current derive set is inconsistent with codebase conventions for small value types.
- Fix: Add `Copy` and `PartialEq` (f64 implements `PartialEq`, just not `Eq`):
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq)]
  pub struct FileRiskScores {
  ```
  `PartialEq` is useful for assertions in downstream consumers without needing the `approx_eq` helper that tests currently use for simple exact-equality cases.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`path_str().into_owned()` allocates a String per file per commit in the hot loop** - `crates/rskim-search/src/temporal/scoring.rs:112`
**Confidence**: 82%
- Problem: Inside the inner loop (`for file in &commit.changed_files`), `file.path_str().into_owned()` creates a fresh `String` allocation for every file-commit pair. When the same file appears in many commits (the common case for hotspot analysis), this creates N duplicate allocations that all hash to the same bucket. The `Cow<str>` from `path_str()` could be used directly, or paths could be interned. This is a `Should-Fix` because it is in the same module as your changes and affects performance of the function you authored.
- Fix: Use `Cow<str>` as the key type to avoid allocation when the path is valid UTF-8 (which it almost always is per the `FileChangeInfo::path_str` doc comment), or pre-intern paths into a `HashMap<&Path, usize>` index in a first pass:
  ```rust
  // Option 1: Use Cow directly (requires lifetime changes to accum)
  // Option 2: Convert path to string once per unique path using entry API
  let path_key = file.path.to_string_lossy().into_owned(); // current approach is acceptable
  ```
  Note: The current approach is functionally correct and `HashMap` entry API deduplicates efficiently. This is a performance polish item, not a correctness issue. Acceptable to defer if profiling shows this is not a bottleneck.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`debug_assert!` for `half_life_days > 0.0` silently misbehaves in release builds** - `crates/rskim-search/src/temporal/scoring.rs:47,81` (Confidence: 70%) — In release builds, a zero or negative `half_life_days` would produce `NaN`/`Inf` instead of panicking. Consider whether this should be a proper validation (return `Result` or use `assert!`) at the public API boundary, per the project's "validate at boundaries" principle. The `debug_assert!` is documented in the `# Panics` section, and the doc comment says "Panics in debug builds," so this is an intentional design choice — but callers in release mode get no guard.

- **`HashMap<String, FileRiskScores>` return type exposes an unordered collection** - `crates/rskim-search/src/temporal/scoring.rs:80` (Confidence: 65%) — Returning `HashMap` is fine for lookup-oriented consumers, but if downstream code ever needs to iterate in a deterministic order (e.g., for display or serialization), callers must sort. A `BTreeMap` or a `Vec<(String, FileRiskScores)>` sorted by path would provide deterministic iteration. Current usage may not need this — depends on future consumers.

- **`50_000` capacity cap is a magic number** - `crates/rskim-search/src/temporal/scoring.rs:96` (Confidence: 62%) — The `.min(50_000)` pre-allocation cap is undocumented. Consider extracting to a named constant with a comment explaining the reasoning (e.g., memory limit, typical repo size assumption).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Architectural Assessment

This PR demonstrates excellent adherence to the established temporal module architecture:

1. **I/O vs Pure split respected**: `scoring.rs` is entirely pure computation with no I/O, matching the `git_parser.rs` (I/O) / `scoring.rs` (pure) boundary documented in the feature knowledge. This follows the same pattern as the cochange module's builder/reader split.

2. **Type placement correct**: `FileRiskScores` lives in `types.rs` alongside `CommitInfo` and `FileChangeInfo`, exactly where the feature knowledge specifies shared types belong.

3. **Re-export chain correct**: `scoring.rs` -> `temporal/mod.rs` -> `lib.rs`, matching the documented re-export flow pattern.

4. **Single Responsibility**: `decay_weight` is a standalone pure function, `compute_file_risk_scores` is a single-pass aggregator, and `is_fix_commit` (pre-existing) handles classification. Each has one reason to change.

5. **Dependency direction clean**: `scoring.rs` depends only on `crate::types` and `super::is_fix_commit`. No infrastructure imports, no I/O, no gix types leak into the scoring layer.

6. **Testability by design**: Injecting `now_epoch` as a parameter rather than calling `SystemTime::now()` makes tests fully deterministic — a textbook dependency injection pattern applied at the function level.

7. **`#[must_use]` annotations present** on both public functions, consistent with the project's Rust conventions.

The one blocking condition is the missing `Copy` derive on `FileRiskScores`, which is a minor consistency fix. The should-fix allocation note is performance polish that can be deferred.
