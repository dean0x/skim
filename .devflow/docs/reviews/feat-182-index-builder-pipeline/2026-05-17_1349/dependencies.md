# Dependencies Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**rskim-core version mismatch in rskim-search and rskim-research** - `crates/rskim-search/Cargo.toml:17`, `crates/rskim-research/Cargo.toml:24`
**Confidence**: 82%
- Problem: Both crates declare `rskim-core = { version = "2.9.0", path = "../rskim-core" }` while `rskim-core` is at version `2.10.0`. The `version` field is only enforced during crates.io publishing (and both crates have `publish = false`), so this is functionally harmless for local development. However, it creates drift that could cause confusion or break if publish status changes.
- Fix: Update both to `version = "2.10.0"` to match the actual crate version. Consider adding a CI check or release-prep script step that verifies all workspace-internal version references stay in sync.

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 1 | 0 |

**Dependencies Score**: 9/10
**Recommendation**: APPROVED

### Detailed Analysis

**Dependency change reviewed:** `tempfile` moved from `[dev-dependencies]` to `[dependencies]` in `crates/rskim/Cargo.toml`.

This is a well-justified change. The new `manifest.rs` module uses `tempfile::NamedTempFile` in production code (line 35, 221) for atomic file writes -- writing to a temp file in the same directory, then renaming into place so readers never observe a partial write. This is the correct pattern for crash-safe file persistence and mirrors the exact same approach already used in `rskim-search/src/index/builder.rs:13`.

**Key observations:**
- No new dependencies were added to the workspace -- `tempfile = "3.0"` was already declared in workspace dependencies and resolved to `3.23.0` in `Cargo.lock`.
- `Cargo.lock` is unchanged by this PR, confirming no new transitive dependencies were introduced.
- The `tempfile` crate is well-maintained (Rust ecosystem standard for safe temp files), MIT-licensed, and has no known vulnerabilities.
- All other dependencies used in the new search module (`anyhow`, `clap`, `rayon`, `sha2`, `serde`, `serde_json`, `ignore`, `rskim_core`, `rskim_search`) were already production dependencies of the `rskim` crate.
- `cargo check` passes cleanly with the dependency change.
