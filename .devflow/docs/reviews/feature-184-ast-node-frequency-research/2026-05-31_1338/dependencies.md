# Dependencies Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Unpinned corpus repos use `commit = "HEAD"` -- non-reproducible research data** - `crates/rskim-research/ast-corpus.toml` (16 occurrences)
**Confidence**: 85%
- Locations: Lines 165, 170, 175, 184, 189, 194, 203, 208, 216, 221, 231, 236, 244, 254, 259 (all "new for AST corpus" repos)
- Problem: 16 of 40 repos use `commit = "HEAD"` instead of pinned SHA hashes. The existing `corpus.toml` pins all 25 repos to specific commits. Using HEAD means the IDF weight tables generated from this corpus are non-reproducible -- running the analysis at different times will produce different results as upstream repos change. For a research tool generating empirical weight tables, reproducibility is a core requirement.
- Fix: Pin each repo to a specific commit SHA. Run the clone step once, capture the resolved HEAD SHAs, and update `ast-corpus.toml`:
```toml
# Instead of:
commit = "HEAD"

# Pin to resolved SHA:
commit = "a1b2c3d4e5f6..."  # actual 40-char SHA from clone
```
- Note: The file header (line 10) documents this as intentional ("New repos use commit = 'HEAD' as a placeholder"), and the config validator explicitly accepts HEAD. However, the weight tables this corpus generates will be embedded in rskim-core as static lookup data. Non-reproducible inputs to static data are a dependency-management concern. (applies ADR-001 -- fix now rather than defer)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none -- all findings met the 80% confidence threshold or were below 60%)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

### Dependency Change Summary

| Change | Detail | Assessment |
|--------|--------|------------|
| `tree-sitter = { workspace = true }` added to rskim-research | Direct use of `tree_sitter::TreeCursor` in `ast_extract.rs:133` justifies this | Correct -- not re-exported from rskim-core |
| `rskim-core` version bump 2.9.0 -> 2.10.0 in rskim-research | Matches current rskim-core package version | Correct |
| `rskim-core` version bump 2.9.0 -> 2.10.0 in rskim-search | Matches current rskim-core package version | Correct |
| `Cargo.lock` updated (+1 line) | Adds tree-sitter to rskim-research dependency list | Correct -- single version (0.25.10) in lockfile |
| `ast-corpus.toml` added (40 repos) | New corpus config for AST n-gram research | See HIGH finding above |
| No new transitive dependencies | tree-sitter already in the workspace dependency graph | Clean |
| No duplicate crate versions introduced | `cargo tree --duplicates` shows only pre-existing console version split | Clean |
| All workspace dependencies used | grep confirms each dep in Cargo.toml is imported in source | Clean |

**Dependencies Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The sole blocking finding is the 16 unpinned corpus repos using `commit = "HEAD"`. The actual Cargo dependency changes (tree-sitter addition, rskim-core version bumps, lockfile update) are all clean, correctly versioned, and well-justified. Pin the HEAD commits to specific SHAs for reproducible research data and this is ready to merge from a dependencies perspective.
