# Dependencies Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-11

## Issues in Your Changes (BLOCKING)

_No blocking issues found._

## Issues in Code You Touched (Should Fix)

_No should-fix issues found._

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Heavy transitive dependency via rskim-core** - `crates/rskim-search/Cargo.toml:12`
**Confidence**: 82%
- Problem: `rskim-search` depends on `rskim-core` but only uses `Language` (an enum) and `SkimError` (error type). This pulls in 14 tree-sitter grammar crates transitively (C compilation, ~15 native libraries). As search layers grow, this coupling means every search consumer compiles all grammars.
- Fix: In a future PR, consider extracting `Language` and `SkimError` into a lightweight `rskim-types` crate that both `rskim-core` and `rskim-search` depend on. Not blocking because `publish = false` limits blast radius and this is explicitly a foundation crate awaiting Waves 1-6.

## Suggestions (Lower Confidence)

- **No MSRV declared after edition 2024 upgrade** - `Cargo.toml` (Confidence: 65%) — Edition 2024 requires Rust 1.85+. Without a `rust-version` field in the workspace or package Cargo.toml files, downstream users or CI runners on older toolchains will get confusing compilation errors. Consider adding `rust-version = "1.85"` to the workspace root.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Dependencies Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

### Changes Reviewed

1. **New crate: `rskim-search` v0.1.0** — 3 direct deps (rskim-core, serde, thiserror), 1 dev-dep (serde_json). All via workspace declarations. Minimal and appropriate for a types/traits foundation crate.

2. **thiserror 1.0 -> 2.0 upgrade** — Clean migration. thiserror 2.0 is a drop-in replacement (same `#[error]`, `#[from]`, `#[source]` attributes). Lockfile confirms single version (no duplicate thiserror 1.x remaining). No known CVEs in either version.

3. **Edition 2021 -> 2024** — Applied to all 3 workspace crates. CI uses `dtolnay/rust-toolchain@stable` (currently 1.94.1), which fully supports edition 2024. All 3333 tests pass.

4. **rskim-search as dev-dependency of rskim** — Used as a compile-time API canary (documented in comment). Does not add to the binary's runtime dependency graph. Appropriate pattern for validating a library's public API surface before it becomes a runtime dep.

5. **Lockfile changes** — Removed thiserror 1.0.69 + thiserror-impl 1.0.69. Added rskim-search entry. Net reduction in unique crate count.

6. **License audit** — All dependencies MIT or MIT/Apache-2.0. Compatible with project MIT license.

7. **No known vulnerabilities** — thiserror 2.0.17, serde 1.0.228, serde_json 1.0.145 have no advisory entries.

8. **Supply chain** — All dependencies are well-known, high-download-count crates from verified publishers (dtolnay for thiserror/serde, serde-rs org for serde_json).
