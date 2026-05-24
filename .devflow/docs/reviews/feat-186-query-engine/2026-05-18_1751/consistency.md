# Consistency Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Scope**: Incremental (4 commits since last review)

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Delegation test omits 3 of 7 SearchQuery fields** - `query_tests.rs:207-234`
**Confidence**: 85%
- Problem: `test_search_delegates_to_inner_layer` asserts that the spy received the exact query "unchanged" (comment on line 210 says "same text, same struct fields"), but only checks 4 of 7 `SearchQuery` fields: `text`, `lang`, `limit`, `offset`. The fields `ast_pattern`, `temporal_flags`, and `bm25f_config` are not verified. This is inconsistent with the test's stated contract and with the `config_tests.rs` pattern of exhaustive field coverage.
- Fix: Add assertions for the 3 missing fields:
```rust
assert_eq!(
    received.ast_pattern, original_query.ast_pattern,
    "QueryEngine must forward ast_pattern unchanged"
);
assert_eq!(
    received.temporal_flags, original_query.temporal_flags,
    "QueryEngine must forward temporal_flags unchanged"
);
assert_eq!(
    received.bm25f_config, original_query.bm25f_config,
    "QueryEngine must forward bm25f_config unchanged"
);
```

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **PR description says "64KB default, configurable" but code uses 4096 (4 KiB) and is a const, not configurable** - `query.rs:15` (Confidence: 70%) -- The PR description and the implementation disagree on both the value and the configurability of `MAX_QUERY_BYTES`. The code doc comment says "4 KiB" which matches the `4096` const. The description may be stale or aspirational. Not a code issue, but the mismatch could confuse reviewers or future developers who read the PR description as documentation.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Observations (no issues)

The incremental changes are well-aligned with existing codebase conventions:

1. **Section separators** -- `query_tests.rs` correctly uses `// ----` dashes (test file convention) while `query.rs` uses `// ====` equals signs (production code convention). This matches the established split across all `lexical/` files.
2. **Test file preamble** -- `query_tests.rs` follows the standard pattern: module doc comment, `#![allow(clippy::unwrap_used)]`, `use super::*`, additional imports. Consistent with `scoring_tests.rs`, `config_tests.rs`, `classifier_tests.rs`.
3. **Error assertion pattern** -- The refactored error checks (`let msg = format!("{}", result.unwrap_err()); assert!(msg.contains(...))`) now match the `config_tests.rs` pattern exactly. The previous `match` arms were functional but inconsistent with sibling test files. This is a positive consistency improvement.
4. **`#[must_use]` on `QueryEngine::new`** -- Consistent with the crate convention applied to all pure constructors (`SearchQuery::new`, `NgramIndexReader::new`, etc.).
5. **Test double naming** -- `SpyLayer` and `PanicLayer` follow idiomatic Rust test double naming and are well-documented with doc comments explaining their purpose.
6. **`name()` return values** -- `"query-engine"`, `"spy"`, `"panic"` follow the kebab-case / lowercase convention established by `"empty"` and `"noop"` in `types.rs`.
7. **Glob import `use super::*`** -- Consistent with all other `*_tests.rs` files in the crate.
