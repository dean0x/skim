# Dependencies Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Outdated workspace dependency: rusqlite 0.31 vs latest 0.40** - `Cargo.toml:46`
**Confidence**: 82%
- Problem: The workspace pins `rusqlite = "0.31"` with `libsqlite3-sys 0.28.0`. The latest stable release is `rusqlite 0.40.0`. This is a 9-minor-version gap. While this PR only adds the existing workspace dependency to `rskim-search/Cargo.toml` (i.e., it did not introduce the version pin), the version choice is now load-bearing across two crates. The `0.31 -> 0.40` upgrade path includes API changes (e.g., `params!` macro changes, `Connection::open` signature updates) and newer bundled SQLite with security patches and performance improvements. The bundled `libsqlite3-sys 0.28.0` ships an older SQLite version that may lack recent CVE fixes in upstream SQLite (though no RUSTSEC advisories exist for rusqlite 0.31 itself).
- Fix: Consider upgrading the workspace `rusqlite` version to `0.40` in a follow-up PR. This is not blocking for this PR since the version was already established in the workspace before this branch, but should be tracked as tech debt.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider feature-gating rusqlite in rskim-search** - `crates/rskim-search/Cargo.toml:26` (Confidence: 65%) -- The `rskim-search` crate is a library (`publish = false`). If temporal storage is an optional feature, gating `rusqlite` behind a cargo feature flag (e.g., `temporal-storage`) would allow consumers to avoid the SQLite compile-time cost when they don't need persistence. However, this depends on whether the crate always requires temporal storage.

## Dependency Review Checklist

| Check | Status | Notes |
|-------|--------|-------|
| No known CVEs | PASS | No RUSTSEC advisories for rusqlite 0.31 |
| Version ranges appropriate | PASS | Workspace-level pin at 0.31, not overly wide |
| Lockfile updated and committed | PASS | Cargo.lock adds only the `rusqlite` entry to `rskim-search` deps |
| Package actively maintained | PASS | rusqlite is actively maintained (latest 0.40.0) |
| License compatible | PASS | rusqlite is MIT-licensed, matches project MIT license |
| Package from verified publisher | PASS | rusqlite is a well-established crate (18M+ downloads) |
| Transitive dependencies reviewed | PASS | No new transitive deps -- rusqlite and libsqlite3-sys were already in Cargo.lock via `rskim` crate |
| Package name verified | PASS | Not a typosquat |
| Bundle size impact | PASS | No binary size increase -- `libsqlite3-sys` (bundled) was already linked via `rskim` |
| Native alternatives considered | N/A | SQLite is the right tool for local persistent storage |

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Dependencies Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. Track the rusqlite 0.31 -> 0.40 upgrade as tech debt (can be a separate PR since the version pin predates this branch).

### Positive Observations

- **Correct reuse of existing workspace dependency**: Rather than adding a new dependency, this PR reuses the `rusqlite` already declared in the workspace `Cargo.toml` and already present in `Cargo.lock`. Zero new transitive dependencies were introduced.
- **Bundled feature is appropriate**: Using `features = ["bundled"]` ensures deterministic builds across platforms without requiring a system SQLite installation -- consistent with the existing `rskim` crate's usage.
- **Clean API boundary**: rusqlite types are kept private behind a `db_err` helper and `SearchError::Database` variant. No rusqlite types leak into the public API, which means upgrading rusqlite later is a localized change.
- **Minimal lockfile churn**: Only 1 line added to `Cargo.lock` (the dependency edge from `rskim-search` to `rusqlite`), confirming no version conflicts or duplicate dependency trees.
