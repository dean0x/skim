# Consistency Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13T09:30
**PR**: #220

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Missing Cargo.toml metadata in rskim-research** - `crates/rskim-research/Cargo.toml`
**Confidence**: 92%
- Problem: The `rskim-research` crate is missing `authors`, `license`, `description`, and `repository` fields. All three other workspace crates (`rskim-core`, `rskim`, `rskim-search`) include these fields -- even `rskim-search` which is also `publish = false`. This breaks the established metadata pattern.
- Fix: Add the standard metadata fields to match the sibling crates:
```toml
[package]
name = "rskim-research"
version = "0.1.0"
edition = "2024"
authors = ["Skim Contributors"]
license = "MIT"
description = "Empirical bigram IDF weight table generator for rskim-search"
repository = "https://github.com/dean0x/skim"
publish = false
```

**`clap` workspace dependency added but existing consumer not migrated** - `Cargo.toml:41`, `crates/rskim/Cargo.toml:17`
**Confidence**: 85%
- Problem: This PR adds `clap = { version = "4.5", features = ["derive"] }` to workspace dependencies (line 41 of root `Cargo.toml`) so that `rskim-research` can use `clap = { workspace = true }`. However, the existing `rskim` crate still declares `clap = { version = "4.5", features = ["derive"] }` inline (not `{ workspace = true }`). Introducing a workspace dependency without migrating the existing consumer creates a split pattern: one crate uses workspace, the other uses inline. Both compile identically today, but the inconsistency invites future version drift.
- Fix: Update `crates/rskim/Cargo.toml` to use the workspace reference:
```toml
clap = { workspace = true }
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Corpus commit SHAs may be synthetic** - `crates/rskim-research/corpus.toml` (Confidence: 70%) -- Several non-Rust repository commit SHAs in `corpus.toml` appear to follow sequential hexadecimal patterns (e.g., `aed3c07fa9edcc2e38a741a6d1c9e3c2a8d9f4c3`, `b3d3a5c8b2a3d5c8a3d5c8b2a3d5c8a3d5c8b2a3`, `c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0`) rather than being real pinned commits. Since the config validator enforces 40-hex-char format, these will pass validation but fail at clone time. If intentional placeholders, consider adding a comment explaining they need to be replaced before production corpus runs.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `rskim-research` crate demonstrates strong consistency with existing project patterns overall:

- Edition 2024 matches all other crates
- Clippy lint configuration (`unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"`, `todo = "warn"`) matches `rskim-core` and `rskim-search` exactly
- Error handling via `anyhow::Result` in the binary crate is consistent with `rskim` CLI patterns
- `#[must_use]` annotations on pure computation functions match existing conventions in `rskim-search`
- Module documentation (`//!` doc comments) follows the same style as other crates
- Test modules use `#![allow(clippy::unwrap_used)]` consistently, matching `rskim-core` and `rskim-search`
- Workspace dependency references (`{ workspace = true }`) are used for all shared deps
- The `rskim-search/src/weights.rs` generated code includes appropriate `#[must_use]`, doc comments, and inline tests matching the library's conventions

The two MEDIUM items (missing Cargo.toml metadata and split clap dependency pattern) are minor consistency gaps that should be resolved before or shortly after merge.
