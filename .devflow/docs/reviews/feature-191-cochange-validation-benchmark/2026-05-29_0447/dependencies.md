# Dependencies Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Stale Cargo.lock: phantom `libc` entry for rskim-bench** - `Cargo.lock:2316`
**Confidence**: 92%
- Problem: The committed `Cargo.lock` lists `libc` as a direct dependency of `rskim-bench`, but `crates/rskim-bench/Cargo.toml` does not declare it. A prior resolution (Cycle 2) correctly removed `libc` from Cargo.toml but the lockfile was not regenerated to match. `cargo check --locked` happens to pass because Cargo treats the extra entry as inert, but the lockfile no longer accurately reflects the dependency graph. CI or contributor workflows that rely on `--locked` to detect drift will not catch real lockfile issues while this phantom entry exists. An unstaged local change already fixes this, but it is not committed. Applies ADR-001 (fix noticed issues immediately).
- Fix: Run `cargo generate-lockfile` or `cargo update -w` to regenerate the lockfile, then commit the updated `Cargo.lock` alongside the other changes.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **rskim-research pins rskim-core at version 2.9.0 while actual version is 2.10.0** - `crates/rskim-research/Cargo.toml:24` (Confidence: 65%) -- The path dependency override means this has zero runtime impact within the workspace, but the version string `2.9.0` is stale. If rskim-research were ever published or consumed outside the workspace, the version constraint would reject the actual rskim-core version. Low practical risk since `publish = false`.

- **Self-referencing dev-dependency pattern could confuse tooling** - `crates/rskim-bench/Cargo.toml:46` (Confidence: 62%) -- `rskim-bench = { path = ".", features = ["test-utils"] }` in `[dev-dependencies]` is a valid Cargo pattern for enabling features only during testing, and Cargo resolves it correctly (confirmed by `cargo tree`). However, some third-party tools (cargo-deny, cargo-audit, IDE indexers) may misinterpret self-referencing dependencies. The pattern itself is sound and widely used in the Rust ecosystem; this is informational only.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Dependencies Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The dependency footprint of this PR is minimal and well-managed:

- **No new external crates** introduced. Total package count (355) is unchanged from main.
- **No workspace dependency version changes**. All new code uses existing workspace dependencies via `{ workspace = true }`.
- **All imports map to declared dependencies**. No undeclared or phantom usage in source code.
- **Feature gating** for `test-utils` is clean: `#[cfg(any(test, feature = "test-utils"))]` ensures test helpers never compile into production builds.
- **`publish = false`** on rskim-bench means dependency hygiene issues have no downstream impact on crates.io consumers.
- **`libc` is correctly gated** behind `#[cfg(unix)]` in `rskim-research` (the actual user), with a Windows fallback path via `taskkill`.

The single blocking issue is the stale `Cargo.lock` entry -- a lockfile hygiene fix that should be committed before merge to keep the dependency graph accurate. The unstaged local fix already exists; it just needs to be committed.
