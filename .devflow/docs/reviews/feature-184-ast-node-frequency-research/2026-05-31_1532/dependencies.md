# Dependencies Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T15:32

## Issues in Your Changes (BLOCKING)

No blocking dependency issues found.

## Issues in Code You Touched (Should Fix)

No should-fix dependency issues found.

## Pre-existing Issues (Not Blocking)

No pre-existing dependency issues found.

## Suggestions (Lower Confidence)

No lower-confidence suggestions.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Dependencies Score**: 10
**Recommendation**: APPROVED

## Analysis Notes

### Changes Reviewed

1. **`crates/rskim-research/Cargo.toml`** -- Two dependency changes:
   - `rskim-core` version bump from `2.9.0` to `2.10.0` (matches actual crate version)
   - Added `tree-sitter = { workspace = true }` (new direct dependency)

2. **`crates/rskim-search/Cargo.toml`** -- One dependency change:
   - `rskim-core` version bump from `2.9.0` to `2.10.0` (matches actual crate version)

3. **`Cargo.lock`** -- Minimal change: added `tree-sitter` to rskim-research's dependency list. No new transitive dependencies introduced (tree-sitter v0.25.10 was already resolved in the workspace via rskim-core).

### Dependency Checklist

- [x] **Justified usage**: `tree-sitter` is used directly at `ast_extract.rs:143` for `tree_sitter::TreeCursor` type annotation. rskim-core exports `Parser` and `Language` but does not re-export `TreeCursor`, making the direct dependency necessary.
- [x] **Version alignment**: Both rskim-core and rskim-research resolve to the same `tree-sitter v0.25.10` via `{ workspace = true }`. No version split -- types are compatible across crate boundaries.
- [x] **Workspace consistency**: All three workspace crates depending on rskim-core (rskim, rskim-research, rskim-search) reference version `2.10.0`, matching the actual crate version.
- [x] **Lockfile committed and minimal**: Cargo.lock diff is 1 line -- only the expected addition.
- [x] **No new transitive dependencies**: tree-sitter was already in the dependency graph via rskim-core.
- [x] **Package is actively maintained**: tree-sitter v0.25.10 is current stable.
- [x] **License compatible**: tree-sitter is MIT-licensed, compatible with this project's MIT license.
- [x] **publish = false**: rskim-research is a developer-only binary (not published to crates.io), so the added dependency has no downstream supply chain impact.
- [x] **No unused dependencies**: `cargo check -p rskim-research` compiles cleanly with no warnings.
- [x] **No known CVEs**: tree-sitter 0.25.x has no known security advisories.
