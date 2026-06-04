# Dependencies Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

No blocking dependency issues found.

## Issues in Code You Touched (Should Fix)

No should-fix dependency issues found.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**rskim-core version pin stale across internal crates** - `crates/rskim-research/Cargo.toml:24`, `crates/rskim-search/Cargo.toml`
**Confidence**: 85%
- Problem: `rskim-research` and `rskim-search` both pin `rskim-core = { version = "2.9.0", path = "../rskim-core" }` but `rskim-core` is at version `2.10.0`. This works today because Cargo path overrides take precedence over version constraints in a workspace, but it means the declared version requirement is a lie -- if a crate were ever published or the path removed, resolution would fail. Both crates have `publish = false`, reducing the practical impact.
- Fix: Update the version field to `"2.10.0"` in both `crates/rskim-research/Cargo.toml` and `crates/rskim-search/Cargo.toml` to match the actual `rskim-core` version. This should be part of the release-prep script (applies ADR-001).

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

## Analysis Notes

**Dependency change summary:**
- **Added**: `tree-sitter = { workspace = true }` to `crates/rskim-research/Cargo.toml`
- **Lockfile impact**: 1 line added (adding existing `tree-sitter` to the research crate's dependency list)
- **New transitive dependencies**: Zero -- `tree-sitter 0.25.10` was already resolved in `Cargo.lock` via `rskim-core`

**Checklist:**
- [x] No known CVEs in added packages (tree-sitter 0.25.10, already in use)
- [x] Version ranges appropriate (workspace = true delegates to workspace-level `"0.25"` pin)
- [x] Lockfile updated and committed
- [x] Package actively maintained (tree-sitter is actively maintained by the tree-sitter org)
- [x] License compatible (MIT, matches project license)
- [x] Package from verified publisher (tree-sitter GitHub organization)
- [x] Transitive dependencies reviewed (none new)
- [x] Package name verified (not typosquat -- already a workspace dependency)
- [x] No unnecessary new dependencies -- tree-sitter is the core parsing infrastructure reused from rskim-core
- [x] No new external crates introduced (aligns with FEATURE_KNOWLEDGE constraint)

**Why this is clean:** The research crate needs direct access to the tree-sitter `Parser` and `Tree` types for AST node-kind extraction. Rather than re-exporting from `rskim-core`, it declares a direct dependency on the same workspace-pinned version. This is the correct pattern -- it avoids coupling the research crate's AST traversal needs to whatever subset `rskim-core` happens to re-export, while `{ workspace = true }` guarantees version consistency across the workspace.
