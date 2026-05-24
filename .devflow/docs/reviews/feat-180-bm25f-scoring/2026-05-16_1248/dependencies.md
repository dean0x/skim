# Dependencies Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

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

- **rskim-core version requirement could be tightened** - `crates/rskim-search/Cargo.toml:17` (Confidence: 65%) -- rskim-search specifies `version = "2.9.0"` but uses `node_kind_priority()` which was added in this PR to rskim-core 2.10.0. Since rskim-search is `publish = false` and uses a path dep, this has no practical impact. If rskim-search were ever published independently, the minimum version should be `"2.10.0"`.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Dependencies Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

**What was added:**
- `tree-sitter = { workspace = true }` added to `crates/rskim-search/Cargo.toml` (1 new direct dependency)
- `rskim-core` dependency (pre-existing, unchanged) exposes `Parser` and new `node_kind_priority()` API

**Why this is clean:**
1. **No new transitive dependencies** -- tree-sitter was already in the lockfile via rskim-core. The Cargo.lock diff is a single line (adding "tree-sitter" to rskim-search's dep list).
2. **Workspace version pinning** -- Uses `{ workspace = true }` which resolves to `"0.25"` (currently 0.25.10), consistent with all other workspace crates.
3. **Justified dependency** -- rskim-search calls `parser.parse()` which returns `tree_sitter::Tree`, then uses `.root_node()` and `.walk()` (tree-sitter types). The direct dependency is necessary because rskim-core does not re-export these types.
4. **Well-commented** -- The Cargo.toml includes a comment explaining why tree-sitter is needed.
5. **License compatible** -- tree-sitter is MIT, same as this project.
6. **Single version resolved** -- `cargo tree -i tree-sitter` confirms no duplicate versions.
7. **Source verified** -- Published on crates.io (registry+https://github.com/rust-lang/crates.io-index), maintained by the tree-sitter organization.
