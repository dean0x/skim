# Dependencies Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Unnecessary `once_cell` direct dependency -- use `std::sync::LazyLock` instead** - `Cargo.toml:62`, `crates/rskim-search/Cargo.toml:26`, `crates/rskim-search/src/temporal/mod.rs:15`
**Confidence**: 95%
- Problem: The project uses Rust edition 2024 with rustc 1.94.1. `std::sync::LazyLock` has been stable since Rust 1.80.0. Every other lazy static in the codebase (13+ instances across `crates/rskim/src/cmd/log.rs`, `crates/rskim/src/cmd/lint/eslint.rs`, `crates/rskim/src/cmd/lint/mypy.rs`, `crates/rskim/src/cmd/lint/ruff.rs`, etc.) uses `std::sync::LazyLock`. Adding `once_cell` as a direct dependency introduces an inconsistency and an unnecessary explicit dependency. While `once_cell` is already present as a transitive dependency (via gix, rusqlite/hashlink, dashmap), promoting it to a direct dependency is not needed and deviates from the established project pattern.
- Fix: Replace `once_cell::sync::Lazy` with `std::sync::LazyLock` in `crates/rskim-search/src/temporal/mod.rs`, and remove `once_cell` from both `Cargo.toml` (workspace) and `crates/rskim-search/Cargo.toml`:

```rust
// crates/rskim-search/src/temporal/mod.rs
use std::sync::LazyLock;

static FIX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(fix|bug|hotfix|patch|revert)\b").expect("valid regex"));
```

Remove from `Cargo.toml` line 62: `once_cell = "1"`
Remove from `crates/rskim-search/Cargo.toml` line 26: `once_cell = { workspace = true }`

## Issues in Code You Touched (Should Fix)

No issues found.

## Pre-existing Issues (Not Blocking)

No issues found.

## Suggestions (Lower Confidence)

- **`max-performance-safe` feature may be broader than needed** - `Cargo.toml:61` (Confidence: 65%) -- The `max-performance-safe` feature enables `parallel`, `pack-cache-lru-static`, and `pack-cache-lru-dynamic` (via `max-control`). The code only uses commit traversal with tree diffing. The `blob-diff` feature alone (which is already explicitly listed) provides what is needed, plus `parallel` is likely beneficial for pack decoding performance in large repos. However, the feature also pulls in protocol/transport sub-crates as non-optional gix dependencies. If compile time or binary size becomes a concern, narrowing to `features = ["blob-diff", "parallel"]` could reduce the transitive footprint.

- **`rskim-search` promoted from dev-dependency to runtime dependency in rskim** - `crates/rskim/Cargo.toml:17` (Confidence: 70%) -- Previously `rskim-search` was a dev-dependency "compile-time canary" for API surface validation. It is now a runtime dependency to re-export `CommitInfo`/`FileChangeInfo` types. This is architecturally intentional (the PR description confirms type deduplication), but it means the full `rskim-search` dependency tree (including gix with 111 new transitive packages) now ships in the release binary. This is acceptable if heatmap is a core feature, but worth noting the supply chain surface area increase.

- **`gix-protocol` and `gix-transport` compiled but unused** - transitive via gix (Confidence: 60%) -- The gix crate unconditionally depends on `gix-protocol`, which in turn depends on `gix-transport`. These network-related crates are compiled even though this PR only performs local repository operations. This is a gix architecture concern, not something fixable in this PR.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

### Dependency Change Summary

| Change | Details |
|--------|---------|
| New direct deps (workspace) | `gix 0.72` (with `max-performance-safe`, `blob-diff`), `once_cell 1` |
| New direct deps (rskim-search) | `gix`, `regex`, `once_cell` |
| Promoted dep (rskim) | `rskim-search` from dev-dependency to runtime dependency |
| New transitive packages | 111 (predominantly gix-* sub-crates, plus ICU/Unicode, jiff, imara-diff, flate2/zlib-rs) |
| Lockfile delta | +1,383 lines |
| License compatibility | All new deps are MIT or MIT/Apache-2.0 -- compatible with project MIT license |
| Known CVEs | None identified in added versions |
| Package authenticity | `gix` is gitoxide by Byron Bayer (well-known Rust ecosystem maintainer), all sub-crates from same author |

**Dependencies Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The `once_cell` dependency should be replaced with `std::sync::LazyLock` for consistency with the rest of the codebase. The `gix` addition itself is well-justified (pure-Rust git parsing replacing shell-out to `git log`), appropriately configured with `default-features = false`, and from a trusted maintainer. The 111 new transitive packages are expected for a dependency of gix's scope and are all permissively licensed.
