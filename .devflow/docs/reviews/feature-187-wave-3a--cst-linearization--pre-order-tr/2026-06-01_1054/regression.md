# Regression Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

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

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Regression Analysis Detail

### 1. Lost Functionality

**No regressions detected.** (Confidence: 95%)

- No exports were removed. The diff shows only additions: `pub mod ast_index` and `pub use ast_index::{LinearNode, LinearizeResult, linearize_source}` in `lib.rs`.
- No CLI options were removed or modified.
- No existing API signatures were changed.
- The only line removed in the entire diff is `#![allow(clippy::unwrap_used)]` in `ast_extract.rs` tests, replaced with the wider `#![allow(clippy::unwrap_used, clippy::expect_used)]`. This is strictly additive -- existing allows are preserved.

### 2. Broken Behavior

**No regressions detected.** (Confidence: 95%)

- The `SearchError` enum is `#[non_exhaustive]`, so adding `AstError(String)` is backward-compatible for downstream crates (they must already have wildcard match arms). Verified: the only match on `SearchError` in `validate.rs:578-581` uses `other => ...` wildcard. All other usage constructs specific variants rather than matching exhaustively.
- No return types were changed on existing functions.
- No default values were modified.
- No existing side effects were removed.

### 3. Intent vs Reality Mismatch

**No mismatch detected.** (Confidence: 90%)

- PR description states: "New ast_index module. Only modified files: types.rs (error variant), lib.rs (module + exports), Cargo.toml (deps). No breaking changes." This matches the diff exactly:
  - `types.rs`: new `AstError(String)` variant added (additive, non-breaking due to `#[non_exhaustive]`)
  - `lib.rs`: new `pub mod ast_index` + `pub use` re-exports (additive)
  - `Cargo.toml`: new `tree-sitter` dep + `criterion` dev-dep + bench target (additive)
  - `ast_extract.rs`: widened clippy allow list (minor, test-only)
  - All other files (`linearize.rs`, `linearize_tests.rs`, `linearize_bench.rs`, `mod.rs`) are new

### 4. Incomplete Migrations

**No migration issues.** (Confidence: 95%)

- No APIs were deprecated or replaced. This is purely additive new functionality.
- The `tree-sitter` dependency added to `rskim-search/Cargo.toml` uses `{ workspace = true }`, consistent with the workspace-level `tree-sitter = "0.25"` definition. No version mismatch.

### 5. Dependency Regression Risk

**Low risk.** (Confidence: 90%)

- `tree-sitter = { workspace = true }` was added to `rskim-search`. This dependency already exists in the workspace (used by `rskim-core`). Adding it to `rskim-search` as a direct dependency is correct since `linearize.rs` uses `tree_sitter::Tree` and `tree_sitter::TreeCursor` directly.
- `criterion = { workspace = true }` in dev-deps is standard for benchmarks, no runtime impact.

### 6. Test Coverage for Regression Prevention

The new module includes 30+ tests across 8 test cycles covering types, vocabulary lookup, core linearization, ERROR/MISSING handling, bounds guards, multi-language, edge cases, and performance. The tests include:
- Invariant assertions (`node_count == nodes.len() + error_count`)
- All 14 tree-sitter languages verified
- Serde-only languages (JSON, YAML, TOML) return empty defaults
- Oversized file guard
- UTF-8 multibyte handling
- Binary-like input safety

### Regression Checklist

- [x] No exports removed without deprecation
- [x] Return types backward compatible
- [x] Default values unchanged
- [x] Side effects preserved
- [x] All consumers of changed code updated (no consumers were affected)
- [x] Migration complete across codebase (no migration needed)
- [x] CLI options preserved
- [x] API endpoints preserved
- [x] Commit message matches implementation
- [x] `#[non_exhaustive]` on SearchError ensures additive variant is non-breaking (applies ADR-001 -- no issues left unresolved)
