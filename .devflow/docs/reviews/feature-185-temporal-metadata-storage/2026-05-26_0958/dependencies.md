# Dependencies Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**rusqlite pinned at 0.31, latest is 0.40** - `Cargo.toml:46`
**Confidence**: 85%
- Problem: The workspace declares `rusqlite = { version = "0.31" }` while the latest stable release is 0.40.0. This is 9 minor versions behind. rusqlite 0.32+ includes API improvements (better `Transaction` ergonomics, `params_from_iter` enhancements) and libsqlite3-sys upgrades that bundle newer SQLite versions with security patches.
- Note: This is a pre-existing workspace-level dependency choice, not introduced by this PR. The PR correctly reuses the existing workspace version. Upgrading should be evaluated in a separate ticket, weighing API breaking changes across both `rskim` and `rskim-search` crates.
- Fix: Consider a future PR to bump to `rusqlite = { version = "0.40", features = ["bundled"] }` after auditing breaking changes in 0.32-0.40 changelogs.

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

## Rationale

This PR adds `rusqlite = { workspace = true }` to `crates/rskim-search/Cargo.toml`. The dependency change is clean and well-justified:

1. **Workspace consistency**: Uses the existing workspace-level rusqlite 0.31 declaration with `features = ["bundled"]`. No version divergence between crates.
2. **No new transitive dependencies**: rusqlite 0.31.0 was already resolved in `Cargo.lock` via the `rskim` crate. The lockfile diff adds only the `rskim-search` -> `rusqlite` edge; no new crate versions or transitive deps are pulled in.
3. **Lockfile committed**: `Cargo.lock` is properly updated and committed.
4. **License compatible**: rusqlite is MIT-licensed, matching this project's MIT license. libsqlite3-sys bundles SQLite which is public domain.
5. **Publish scope**: `rskim-search` has `publish = false`, so this dependency does not affect downstream consumers on crates.io.
6. **Appropriate use**: The dependency is genuinely used in `storage.rs` (Connection), `storage_ops.rs` (params macro), and `storage_tests.rs`. No phantom dependency.
7. **API encapsulation**: rusqlite types are properly wrapped behind `SearchError::Database` via a private `db_err` helper. No rusqlite types leak into the public API.
8. **Feature flags**: The `bundled` feature compiles SQLite from source, avoiding system library version mismatches across platforms -- appropriate for a CLI tool with cross-platform binary distribution.

The only observation is the pre-existing version gap (0.31 vs 0.40), which is informational and should not block this PR.
