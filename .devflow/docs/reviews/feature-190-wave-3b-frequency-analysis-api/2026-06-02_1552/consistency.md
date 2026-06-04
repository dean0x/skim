# Consistency Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02T15:52

## Cross-Cycle Awareness

Cycle 1 resolved 6 issues (including field visibility to pub(crate)). 3 false positives dismissed. This cycle focuses on residual consistency patterns not covered in cycle 1.

## Issues in Your Changes (BLOCKING)

### MEDIUM

**LinearNode.kind_id uses raw `u16` while ngram.rs defines `NodeKindId = u16` type alias** - `crates/rskim-search/src/ast_index/linearize.rs:78`, `crates/rskim-search/src/ast_index/ngram.rs:35`
**Confidence**: 85%
- Problem: The new `ngram.rs` module introduces `pub type NodeKindId = u16` as a semantic type alias and uses it consistently throughout AstBigram/AstTrigram APIs. However, `LinearNode` in `linearize.rs` (line 78) still declares `pub kind_id: u16` using the raw primitive. Since `LinearNode.kind_id` and `NodeKindId` represent the exact same domain concept (an index into `NODE_KIND_VOCABULARY`), one module uses a typed alias while its sibling uses a bare primitive. This creates an inconsistency in the public API surface: a caller working with `LinearNode.kind_id` and `AstBigram::encode(parent, child)` sees `u16` in one place and `NodeKindId` in the other for the same thing.
- Fix: Update `LinearNode` to use the `NodeKindId` alias. This requires importing `NodeKindId` from the sibling module or moving the type alias to `mod.rs`/a shared location. Since both sub-modules are siblings under `ast_index`, the cleanest approach would be to define `NodeKindId` in `mod.rs` and have both `linearize.rs` and `ngram.rs` import it:

```rust
// mod.rs
pub type NodeKindId = u16;

// linearize.rs
use super::NodeKindId;
pub struct LinearNode {
    pub kind_id: NodeKindId,
    pub depth: u16,
}
```

**Test section header numbering gap: T14 jumps to T16 (skips T15)** - `crates/rskim-search/src/ast_index/ngram_tests.rs:446`
**Confidence**: 90%
- Problem: The test section headers follow a consecutive numbering convention (T1 through T14), but the final section is labeled `T15` on disk (line 446) while the diff intermediate state showed `T16`. Looking at the on-disk file, the actual header reads `// -- T15: DEFAULT_AST_WEIGHT constant value` which is correct sequential numbering. However, there is no T15 section header in the file -- the sequence goes T1, T2, T3, T4, T5, T6, T7, T8, T9, T9b, T10, T11, T12, T13, T14, T15. The `T9b` breaks the numeric consistency pattern. All other test files in the codebase (linearize_tests.rs) use purely sequential `AC-*` naming without letter suffixes.
- Fix: Rename `T9b` to `T10` and shift subsequent sections (T10 -> T11, ... T15 -> T16) to maintain a clean numeric sequence. Alternatively, if T9b was intentionally a sub-test of T9 (both test `ast_bigram_idf` for known entries), consolidate T9 and T9b into one test or rename T9b to something like `T9_typescript` to match the naming style of other modules. The linearize_tests.rs pattern uses descriptive cycle headers like `// -- AC-T4:` which does not use letter suffixes.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`Ngram` (lexical) has no `#[allow(dead_code)]` on `from_raw` but `AstBigram`/`AstTrigram` do** - `crates/rskim-search/src/ast_index/ngram.rs:98,161` vs `crates/rskim-search/src/ngram.rs:70` (Confidence: 65%) -- The lexical `Ngram::from_raw` is used by production code so does not need the annotation, while `AstBigram::from_raw` and `AstTrigram::from_raw` are marked `#[allow(dead_code)]` because they are pub(crate) but only used in tests currently. This is technically correct but worth noting: when production code starts using `from_raw`, the `#[allow(dead_code)]` should be removed for consistency with the `Ngram` sibling.

- **`LinearNode` derives `Default` but `AstBigram`/`AstTrigram` do not** - `crates/rskim-search/src/ast_index/linearize.rs:74` vs `crates/rskim-search/src/ast_index/ngram.rs:60,123` (Confidence: 60%) -- `LinearNode` derives `Default` (zeroed fields), which makes sense for `LinearizeResult::default()`. `AstBigram` and `AstTrigram` intentionally omit `Default` since a zero-valued bigram/trigram encodes `(0,0)` which is the sentinel pair, and implicitly constructing one could mask bugs. This is a reasonable design choice, not a consistency issue.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `ngram.rs` module is highly consistent with the existing `Ngram` newtype in `src/ngram.rs` -- it follows the same `#[repr(transparent)]`, same derive trait set (`Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord`), same `pub(crate)` inner field, same `from_raw`/`encode`/`decode`/`key` method pattern, same `#[must_use]` + `#[inline]` annotations, same section-separator style, and same `Display` impl pattern. The module doc comments, `#[cfg(test)]` path directive for test files, and re-export chain through `mod.rs` -> `lib.rs` all match established patterns. The `pub(crate)` visibility on `from_raw` and inner fields follows the feature knowledge guidance (avoids PF-002 by surfacing all findings; applies ADR-001 by flagging the `NodeKindId` inconsistency for immediate resolution rather than deferring).

The two MEDIUM findings are both about internal naming consistency. Neither blocks correctness or introduces regressions. The `NodeKindId` type alias inconsistency is the most actionable: it would improve API coherence across the `ast_index` public surface.
