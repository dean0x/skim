# Consistency Review Report

**Branch**: feature-190 -> main
**Date**: 2026-06-02
**PR**: #266 — Adds AstBigram/AstTrigram newtypes, vocab helpers, IDF weight lookup

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Inner field visibility differs from lexical `Ngram`** - `ngram.rs:61,124` (Confidence: 65%) — `Ngram` uses `pub(crate) u16` while `AstBigram(u32)` and `AstTrigram(u64)` use private inner fields. Both patterns work since all access goes through `key()`/`from_raw()`, and the new code is arguably stricter encapsulation. Not a defect, but a stylistic divergence from the existing newtype in the same crate.

- **`vocab_lookup` cast lacks `clippy::cast_possible_truncation` annotation** - `ngram.rs:193` (Confidence: 62%) — `idx as NodeKindId` casts `usize -> u16` without `#[allow(clippy::cast_possible_truncation)]`, while the sibling `linearize.rs:268` adds that annotation for a similar `u32 -> u16` cast. The vocabulary is 1740 entries (safe in practice), and the decode methods' bit-masked casts are provably within u16 range, so this may not trigger clippy depending on lint configuration. Minor annotation inconsistency.

- **Test section prefix style differs from sibling** - `ngram_tests.rs` (Confidence: 60%) — Uses `T1:`, `T2:` prefix labels in section headers while sibling `linearize_tests.rs` uses `Cycle N:` and the lexical `ngram_tests.rs` uses descriptive names. All three formats use the same `// ──` structural pattern, so this is purely a labeling convention difference, not a structural one.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR demonstrates strong consistency with established codebase patterns. Key observations:

1. **Newtype structure** matches the existing `Ngram` pattern precisely: `#[repr(transparent)]`, `#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]`, `#[must_use] #[inline]` on methods, `pub(crate) fn from_raw()`, and `#[cfg(test)] #[path]` test module declaration.

2. **Section comment headers** use the `// ====` pattern (14 instances) consistent with `linearize.rs` (12) and `ngram.rs` (10).

3. **Re-export chain** follows the established `sub-module -> ast_index/mod.rs -> lib.rs` pattern identical to `LinearNode`/`LinearizeResult`.

4. **Module doc comments** (`//!`) follow the `linearize.rs` convention with encoding description, usage examples, and cross-references to related modules.

5. **Test file structure** follows `linearize_tests.rs`: `//!` doc header, `#![allow(clippy::unwrap_used, clippy::expect_used)]`, `use super::*`, `// ──` section headers — applies FEATURE_KNOWLEDGE patterns correctly.

6. **Error handling** uses `unwrap_or(DEFAULT_AST_WEIGHT)` for weight lookups, matching the `unwrap_or(0)` sentinel pattern in `linearize.rs` and the crate's convention of returning safe defaults rather than errors for missing data.

7. **`#[allow(dead_code)]`** on `from_raw()` is appropriate — these methods are `pub(crate)` anticipating future use, matching the doc comment's stated intent. The existing `Ngram::from_raw` does not need the annotation because it has a production call site (applies ADR-001 — the dead_code annotation is intentional and documented, not a deferred cleanup).

The three suggestions are all sub-80% confidence style observations where the new code either matches its immediate sibling pattern or uses a defensibly stricter alternative. No pattern violations, no feature regressions, no naming inconsistencies in public API.
