# Dependencies Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Workspace `clap` dependency added but existing crate not migrated** - `Cargo.toml:41` + `crates/rskim/Cargo.toml:17`
**Confidence**: 90%
- Problem: This PR adds `clap = { version = "4.5", features = ["derive"] }` as a workspace dependency (line 41 of root `Cargo.toml`) and `rskim-research` correctly uses `clap = { workspace = true }`. However, the existing `crates/rskim/Cargo.toml` still uses a direct `clap = { version = "4.5", features = ["derive"] }` declaration (line 17) instead of `workspace = true`. This creates an inconsistency where two crates in the same workspace specify the same dependency differently. The workspace dependency was clearly added to support `rskim-research`, but the pre-existing consumer was not updated.
- Fix: Update `crates/rskim/Cargo.toml` line 17 to use workspace reference:
  ```toml
  clap = { workspace = true }
  ```

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **Large generated files checked into git without LFS** - `crates/rskim-search/data/bigram_weights.json` (556KB) + `crates/rskim-search/src/weights.rs` (343KB) (Confidence: 65%) -- These generated artifacts total ~900KB committed to the repo. For a project of this size this is acceptable, but if the weight table grows significantly with real corpus data, consider git LFS for the JSON source file and adding the generated `.rs` file to `.gitignore` (regenerating at build time via `build.rs` instead).

- **`rskim-research` included in `cargo test --all-features` CI** - `.github/workflows/ci.yml:41` (Confidence: 60%) -- The research crate is now a workspace member, so `cargo test --all-features` in CI will compile and test it. This is likely intentional and adds ~0 extra compile time (zero new transitive deps). If the research crate grows to include slow integration tests (e.g., git cloning), consider `[workspace] exclude` or `#[ignore]` attributes.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Dependencies Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Observations

- **Zero new transitive dependencies**: All 10 dependencies used by `rskim-research` (`anyhow`, `clap`, `ignore`, `indicatif`, `rayon`, `rskim-core`, `serde`, `serde_json`, `sha2`, `tempfile`, `toml`) were already in the workspace. The Cargo.lock diff shows only the new crate entry itself -- no dependency tree expansion.
- **`publish = false` correctly set**: The research crate is a developer tool and correctly marked unpublishable. No risk of accidental crates.io publish.
- **All deps use `workspace = true`**: The research crate consistently uses workspace dependency references, maintaining version consistency across the workspace.
- **No dependency on research crate from published crates**: Neither `rskim` nor `rskim-search` depend on `rskim-research`. The generated `weights.rs` is a static artifact with no runtime coupling.
- **Release workflow is scoped**: `cargo build --release -p rskim` in the release CI explicitly targets only the main binary, so the research crate cannot accidentally bloat release builds.
- **Edition consistency**: `edition = "2024"` matches all other workspace crates.
- **Clippy lints enforced**: The research crate carries the same strict `[lints.clippy]` config (deny `unwrap_used`, `expect_used`, `panic`) as the other crates.
