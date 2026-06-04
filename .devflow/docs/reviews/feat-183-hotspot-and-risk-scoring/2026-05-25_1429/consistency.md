# Consistency Review Report

**Branch**: feat-183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T14:29

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Module doc comment uses `///` instead of `//!`** - `crates/rskim-search/src/temporal/scoring.rs:1`
**Confidence**: 92%
- Problem: The file-level documentation (lines 1-11) uses `///` (outer doc comments) rather than `//!` (inner doc comments). Every other module in this crate (`git_parser.rs`, `types.rs`, `lib.rs`, `mod.rs`, `scoring_tests.rs`) uses `//!` for module-level documentation. With `///`, the doc comment is semantically attached to the next item (`use std::collections::HashMap`) rather than documenting the module itself. This will show up incorrectly in rustdoc output.
- Fix: Change the opening `///` lines to `//!`:
```rust
//! Temporal hotspot and bug-fix density scoring with exponential decay.
//!
//! All functions are pure (no I/O, no side effects). Consumers supply the current
//! epoch timestamp as `now_epoch` so that tests are fully deterministic.
//!
//! # Algorithm overview
//!
//! [`compute_file_risk_scores`] performs a single pass over the commit list,
//! accumulating decay-weighted totals per file. Hotspot scores are then
//! max-normalized so the busiest file always scores 1.0. Fix density is the
//! ratio of fix-weighted touches to total weighted touches per file.
use std::collections::HashMap;
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
| Should Fix | - | 0 | 0 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Rationale

This PR demonstrates excellent consistency across all major dimensions:

1. **Naming conventions**: All function names, constants, and types follow existing crate conventions (`snake_case` functions, `SCREAMING_SNAKE` constants, `PascalCase` types).

2. **Re-export ordering**: The `lib.rs` re-exports maintain alphabetical ordering with uppercase-first convention matching all other `pub use` blocks.

3. **Error handling pattern**: Uses `debug_assert!` for preconditions in hot paths (matching crate CLAUDE.md guidance and `rust.md` rule), no `.unwrap()` in production code.

4. **Test co-location pattern**: `#[cfg(test)] #[allow(clippy::unwrap_used, clippy::expect_used)] #[path = "scoring_tests.rs"] mod tests;` exactly mirrors the `git_parser.rs` pattern.

5. **Derive traits**: `FileRiskScores` derives only `Debug, Clone` without `PartialEq` -- consistent with `SearchResult` (also contains `f64` fields). Feature knowledge confirms this is intentional (never crosses serialization boundary, so no `Serialize/Deserialize`).

6. **Section separators**: Uses the standard `// ====...====` block comment pattern.

7. **Import grouping**: `std` -> `crate::` with blank-line separation between groups.

8. **`#[must_use]`**: Applied to both public functions returning computed values, matching `is_fix_commit` and all `SearchField` methods.

9. **`#[inline]`**: Applied to `decay_weight` (small, hot-path-eligible function), matching `path_str()`, `discriminant()`, and similar small accessors.

10. **`is_fix_commit` reuse**: Calls `super::is_fix_commit` rather than reimplementing the regex, as documented in feature knowledge.

The single finding (wrong doc comment style) is minor and easily fixed. The overall implementation matches crate conventions very closely.
