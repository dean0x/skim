# Dependencies Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

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

**Dependencies Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

The dependency changes in this PR are minimal, well-structured, and follow workspace conventions:

1. **rayon added to rskim-bench** (`rayon = { workspace = true }`): Already a workspace dependency used by the main `rskim` crate (pinned at `1.10`, resolves to `1.11.0`). Used for `par_iter()` in `main.rs` to parallelize repo processing. No new transitive dependency tree introduced.

2. **tempfile moved from `[dev-dependencies]` to `[dependencies]`**: Correct change -- `tempfile` is used in production code (`main.rs` lines 259, 375, 432) for creating temporary index directories during benchmarking, not just in tests. The previous split listing (`[dependencies]` + `[dev-dependencies]`) was redundant; the consolidated single entry is cleaner.

3. **Lockfile change**: Minimal (1 line added) -- only adds `rayon` to the `rskim-bench` dependency list in `Cargo.lock`. No new external crates introduced, no version bumps, no duplicate dependency versions.

4. **All declared dependencies are verified in use**: Every dependency in `Cargo.toml` (rskim-search, rskim-core, rskim-research, tree-sitter, tree-sitter-rust, tree-sitter-python, tree-sitter-go, anyhow, clap, rayon, serde, serde_json, sha2, tempfile) has corresponding `use` statements in source code.

5. **Workspace alignment**: All dependencies use `{ workspace = true }` for version management. Edition `2024` matches all other workspace crates. `publish = false` correctly prevents accidental crate publication.

6. **License compatibility**: All dependencies (rayon: MIT/Apache-2.0, tempfile: MIT/Apache-2.0, etc.) are compatible with the project's MIT license.

7. **No supply chain concerns**: No new external dependencies introduced. rayon is a well-maintained, widely-used Rust concurrency library (part of the Rust ecosystem's core tooling).
