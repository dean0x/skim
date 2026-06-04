# Dependencies Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Changes Analyzed

Two dependencies added to `crates/rskim-search/Cargo.toml`:

| Dependency | Section | Workspace | New to workspace? |
|---|---|---|---|
| `tree-sitter` | `[dependencies]` | Yes (`workspace = true`) | No -- already used by `rskim-core`, `rskim-research` |
| `criterion` | `[dev-dependencies]` | Yes (`workspace = true`) | No -- already used by `rskim-core` |

Cargo.lock diff: 15 lines total, adding only the two resolved entries to `rskim-search`'s package stanza. No new crate versions were introduced to the workspace; no transitive dependency changes.

## Issues in Your Changes (BLOCKING)

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Dependency Review Checklist

- [x] No known CVEs in added packages -- `tree-sitter 0.25` and `criterion 0.5` are already in the workspace lockfile; no new versions introduced
- [x] Version ranges appropriate -- both use `{ workspace = true }` referencing centrally-pinned versions (`tree-sitter = "0.25"`, `criterion = "0.5"`)
- [x] Lockfile updated and committed -- Cargo.lock diff is minimal and correct (2 entries added to `rskim-search` stanza only)
- [x] Packages actively maintained -- `tree-sitter` (GitHub tree-sitter org, very active), `criterion` (widely used Rust benchmarking standard)
- [x] License compatible -- both MIT/Apache-2.0 dual-licensed, compatible with project MIT license
- [x] Package from verified publisher -- both are well-established crates.io packages
- [x] Transitive dependencies reviewed -- no new transitive deps; both crates already resolved in workspace lockfile
- [x] Package name verified (not typosquat) -- canonical crate names
- [x] Bundle size impact considered -- `tree-sitter` was already linked via `rskim-core`; adding direct dependency adds zero binary size. `criterion` is dev-only (not included in release builds)
- [x] Native alternatives considered -- `tree-sitter` is the project's core AST parser (no alternative). `criterion` is the established Rust benchmarking framework matching `rskim-core` usage

## Analysis Notes

**tree-sitter direct dependency is justified**: `rskim-search` calls `tree.walk()` (returns `tree_sitter::TreeCursor`) and accesses `item.node.kind_id()` (from `tree_sitter::Node`). These types are part of `rskim-core`'s public API (`AstWalkIter::new` takes `TreeCursor<'a>`, `AstWalkNode::node` is `tree_sitter::Node<'a>`), so consumers must depend on `tree-sitter` directly. This matches the pattern already established by `rskim-research`, which has the same direct dependency for the same reason.

**criterion dev-dependency is justified**: The PR adds a `linearize_bench` benchmark target (`[[bench]]` section) that uses criterion macros (`criterion_group!`, `criterion_main!`). This follows the identical pattern used by `rskim-core/benches/transform_bench.rs`.

**No new crates introduced**: Both dependencies were already present in `[workspace.dependencies]` and resolved in Cargo.lock. This PR merely allows `rskim-search` to reference them. Zero supply chain surface area expansion.

**Cross-cycle note**: Prior resolution cycles addressed 11 issues (9 fixed). No dependency-related issues were among them; no regressions observed.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Dependencies Score**: 10
**Recommendation**: APPROVED
