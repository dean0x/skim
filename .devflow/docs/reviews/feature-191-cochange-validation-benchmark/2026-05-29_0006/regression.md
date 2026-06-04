# Regression Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T00:06Z

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

## Regression Analysis

### 1. Lost Functionality — NONE DETECTED

This PR is purely additive (3,257 insertions, 2 deletions). No files were deleted, no exports were removed, and no CLI options were altered. The only removals are the two `fn` visibility changes in `clone.rs` (private -> public), which are strictly expansions.

Checklist:
- No exports removed
- No CLI options removed
- No API endpoints removed
- No event handlers removed

### 2. Broken Behavior — NONE DETECTED

The modified existing files were analyzed:

- **`crates/rskim-research/src/clone.rs`**: Two functions (`extract_repo_name`, `git_run_with_timeout`) changed from private to `pub`. Their signatures, parameters, and return types are unchanged. A new function `clone_with_history` was added (purely additive). No existing callers are affected because no external crate depended on `rskim-research` internals.

- **`crates/rskim-research/src/config.rs`**: A new field `deep_clone: bool` with `#[serde(default)]` was added to `RepoEntry`. The `serde(default)` attribute ensures backward compatibility — existing TOML configs (e.g., `corpus.toml`) that lack this field will deserialize to `false`. The existing test (`dummy_repo()`) was updated to include `deep_clone: false`, matching the default. Verified: `cargo test --package rskim-research` passes (47 tests).

- **`crates/rskim-bench/Cargo.toml`**: Added a new `[[bin]]` target (`cochange-validate`) and a `libc` dependency (cfg(unix) only). The existing `rskim-bench` binary and library are unaffected. A new `pub mod cochange` in `lib.rs` is purely additive.

- **`Cargo.lock`**: Updated to reflect the new dependency, no version changes to existing deps.

### 3. Intent vs Reality Mismatch — NONE DETECTED

The PR description states: "cochange-validate benchmark for blast-radius precision/recall." The implementation delivers exactly that:
- A new binary (`cochange-validate`) that clones repos, builds co-change matrices, and evaluates precision/recall at multiple thresholds
- Parallel processing with 3-thread cap as described
- Temporal train/test split with quality gates
- Reports precision/recall at macro and micro averaging across 6 thresholds
- Integration tests validate end-to-end behavior with a synthetic git repo

All stated features are fully implemented with tests.

### 4. Incomplete Migrations — NOT APPLICABLE

No migration or API change was performed. The `deep_clone` addition is a new optional field, not a migration of an existing API.

### 5. Backward Compatibility Verification

- `cargo check --package rskim-bench` — compiles cleanly
- `cargo test --package rskim-bench --lib` — 150 tests pass
- `cargo test --package rskim-research` — 47 tests pass
- No external workspace crate depends on `rskim-bench`
- The existing `corpus.toml` (without `deep_clone`) still parses correctly via `#[serde(default)]`

### 6. Notes on Design Safety

The `test_utils` module in `cochange/mod.rs` is compiled unconditionally (not behind `#[cfg(test)]`) so that integration tests in `tests/` can import from it. This is a deliberate and common Rust pattern for test utility sharing across crate boundaries. Since `rskim-bench` is a leaf crate with no downstream consumers in the workspace, this has zero regression risk.

The `pub` visibility changes in `clone.rs` expand the API surface but do not break any existing code. They expose internal helpers needed by the new benchmark binary. applies ADR-001
