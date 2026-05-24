# Regression Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13T09:30
**PR**: #220

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Workspace `clap` dependency promotion is incomplete** - `Cargo.toml:41`
**Confidence**: 82%
- Problem: The PR adds `clap = { version = "4.5", features = ["derive"] }` as a workspace dependency (line 41 of root `Cargo.toml`), but `crates/rskim/Cargo.toml` line 17 still declares the same dependency inline rather than using `{ workspace = true }`. This creates a maintenance inconsistency -- two declarations of the same dependency at the same version. If someone later updates the workspace version, the rskim binary will silently lag behind. This is not a functional regression today (both resolve to 4.5.48), but it creates a latent regression vector.
- Fix: Either update `crates/rskim/Cargo.toml` to `clap = { workspace = true }`, or remove `clap` from the workspace `[dependencies]` and keep it local to each crate that needs it. The former is cleaner.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **CI compile time increase from 9.6k-line weights.rs** - `crates/rskim-search/src/weights.rs` (Confidence: 65%) -- The generated `weights.rs` is ~9,600 lines with a `const` array of 9,596 `(u16, f32)` tuples. While Rust handles large const arrays well, this may add noticeable compile time to `rskim-search` and downstream crates. Worth monitoring CI times after merge. Not a regression in functionality, but could regress build performance.

- **rskim-research tests added to CI without network isolation guard** - `crates/rskim-research/` (Confidence: 60%) -- CI runs `cargo test --all-features` which now includes `rskim-research`. The unit tests are properly fixture-based and do not require network, but the `GitCloneSource` code path (which calls `git clone`) has no `#[ignore]` integration tests that could accidentally be added later without a CI guard. The current test suite is safe, but there is no explicit barrier preventing a future test from calling network code in the default test run.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Regression Checklist

- [x] No exports removed without deprecation -- purely additive changes
- [x] Return types backward compatible -- no existing signatures changed
- [x] Default values unchanged -- no existing defaults modified
- [x] Side effects preserved -- no existing behavior altered
- [x] All consumers of changed code updated -- `rskim-search::lib.rs` new exports are additive, existing `search_api.rs` integration tests pass (11/11)
- [x] Migration complete across codebase -- no migration needed (additive only)
- [x] CLI options preserved -- no existing CLI changes
- [x] API endpoints preserved -- library API only expanded
- [x] Commit message matches implementation -- 4 commits accurately describe scaffold + weights + fixes
- [x] Breaking changes documented -- PR description correctly states "Breaking Changes: None"

### Analysis

This PR is very low regression risk. The change is entirely additive:

1. **New crate `rskim-research`** (`publish = false`): A developer-only tool that generates bigram IDF weight tables. It is not published, not linked by the release binary, and will not affect the release pipeline. The `cargo build --release -p rskim` in CI and release workflows explicitly targets only the rskim package.

2. **New module `rskim-search::weights`**: Adds `BIGRAM_WEIGHTS`, `DEFAULT_WEIGHT`, and `bigram_weight()` to the public API of `rskim-search`. This is purely additive -- no existing exports were removed or changed. The existing 11 integration tests in `crates/rskim/tests/search_api.rs` pass without modification, confirming no API breakage.

3. **Workspace `Cargo.toml` changes**: Added `rskim-research` to workspace members and promoted `clap` to a workspace dependency. The clap version/features are identical to what `rskim` already used inline, so there is no version conflict. The only concern is the incomplete migration (rskim still uses inline clap).

4. **No changes to the release pipeline**: `release.yml` builds only `-p rskim`, publishes only `rskim-core` and `rskim` to crates.io, and generates npm packages from the `rskim` binary. The new `rskim-research` crate (with `publish = false`) is invisible to the release process.

The one MEDIUM finding (clap dependency inconsistency) is a maintenance hygiene issue, not a functional regression. It creates a latent risk that is easy to address before or after merge.
