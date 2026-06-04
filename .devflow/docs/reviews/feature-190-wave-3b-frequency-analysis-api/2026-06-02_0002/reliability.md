# Reliability Review Report

**Branch**: feature-190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**PR**: #266

## Issues in Your Changes (BLOCKING)

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 9/10
**Recommendation**: APPROVED

## Analysis

This PR introduces `AstBigram`/`AstTrigram` newtypes, vocabulary helpers, and IDF weight lookup functions. From a reliability perspective, the code is exemplary.

### Bounded Iteration

No loops exist in production code. All functions are pure lookups: binary search on static tables (`vocab_lookup`, `ast_bigram_weight`, `ast_trigram_weight`), direct array indexing with bounds-checked `.get()` (`vocab_resolve`), or simple arithmetic encoding/decoding (`encode`, `decode`). The test file `ngram_tests.rs` contains bounded `for` loops over small fixed arrays (7x6 = 42 iterations for bigrams, 3x3x3 = 27 for trigrams) -- these are test-only and have explicit fixed upper bounds. Consistent with the feature knowledge: "No loops in production code (only in tests)."

### Assertion Density

The production code uses strong compile-time guarantees rather than runtime assertions:
- `#[repr(transparent)]` on both newtypes ensures zero-cost layout.
- `#[must_use]` on all public methods prevents silent discard.
- The `NodeKindId = u16` type alias combined with `u32::from()` / `u64::from()` eliminates truncation risk at compile time -- no narrowing casts in encode paths.
- `vocab_resolve` uses `.get()` for bounds checking, returning `Option` rather than panicking.
- `vocab_lookup` returns `Option<NodeKindId>` -- callers must handle the missing case.

### Allocation Discipline

Zero allocations in all production paths. All types are `Copy`/`Clone`. The vocabulary is `&'static [&'static str]` -- a compile-time constant slice. Weight lookup uses binary search on static sorted arrays with no heap allocation. The `fmt_kind_id` Display helper writes directly to the formatter without intermediate strings. Consistent with feature knowledge: "No allocations after init."

### Indirection Limits

No indirection. Both newtypes wrap a single primitive (`u32`, `u64`) with `#[repr(transparent)]`. All functions take values or `&str` references -- no nested Box, Rc, or multi-level pointer chains.

### Metaprogramming Restraint

No macros, generics, or reflection. All types are concrete. The `derive` attributes (`Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord`) are standard and bounded.

### Vocabulary Cast Safety (vocab_lookup)

`vocab_lookup` casts the binary search index to `NodeKindId` (u16) via `idx as NodeKindId`. The vocabulary currently has ~1,741 entries, well within u16::MAX (65,535). The existing test `vocab_len_nonzero_and_fits_in_u16` in the test file (line 281-288) explicitly guards this invariant. If the vocabulary ever exceeded u16::MAX, this test would fail, preventing a silent truncation bug. This is a well-defended boundary.

### Send + Sync Safety

All types are `Copy` wrappers around primitives (`u32`, `u64`, `u16`). All functions are pure (no mutation, no interior mutability). All static data is `&'static`. These are trivially Send + Sync by construction. Consistent with feature knowledge: "All functions are Send + Sync safe by construction."

### Decision Context

- `applies ADR-001`: No issues found to defer -- all code reviewed meets reliability standards as-is.
- Pitfalls PF-001, PF-002, PF-003 are not relevant to this review focus (release process, review classification, and command rewrite respectively).
