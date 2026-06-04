# Performance Review Report

**Branch**: feature-190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**PR**: #266

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

(none)

## Analysis Notes

### Positive Performance Characteristics

The new code is exceptionally well-optimized for its use case:

1. **Zero-cost newtypes** -- `AstBigram(u32)` and `AstTrigram(u64)` use `#[repr(transparent)]` with `Copy`/`Clone` derives. The newtypes compile down to plain integers with no runtime overhead. Bit-shift encode/decode operations (`ngram.rs:69-71`, `ngram.rs:132-133`) are single-instruction on all modern CPUs.

2. **All functions are `#[inline]`** -- `encode`, `decode`, `key`, `from_raw` on both types are marked `#[inline]`, allowing the compiler to eliminate function call overhead at call sites.

3. **Binary search on sorted static data** -- `vocab_lookup` (`ngram.rs:189-194`) performs binary search on the 1740-entry `NODE_KIND_VOCABULARY` static array. O(log 1740) = ~11 comparisons, well under 1us.

4. **IDF lookup is two O(log n) searches with no allocation** -- `ast_bigram_idf` (`ngram.rs:222-224`) chains `lang.name()` (returns `&'static str`, zero-cost) through `ast_bigram_weight` (match dispatch O(1) + binary search on ~1740-entry weight table). No heap allocation, no I/O, no intermediate data structures.

5. **No `Display` on hot paths** -- The `fmt::Display` implementations (`ngram.rs:104-111`, `ngram.rs:167-176`) perform vocab resolution and string formatting, but are not called in any production code path -- only in test assertions and debug output.

6. **No I/O in any function** -- All functions operate on `&'static` data or stack values. The entire module is pure computation.

7. **`vocab_lookup` cast safety** -- The `idx as NodeKindId` cast (`ngram.rs:193`) is safe because vocabulary size (1740) is well within `u16::MAX` (65535), validated by existing tests.

### Weight Table Scale

The `ast_weights.rs` file (~105K lines) contains static sorted arrays totaling ~103K entries across all 14 languages. These live in the binary's `.rodata` segment and are paged in by the OS on demand -- no startup cost for unused language tables.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 10/10
**Recommendation**: APPROVED

The new ngram API is pure, zero-allocation, and sub-microsecond per call. The `#[repr(transparent)]` newtypes, `#[inline]` annotations, binary-search lookups on sorted static data, and absence of any I/O make this code performance-optimal for its purpose. No performance issues found.
