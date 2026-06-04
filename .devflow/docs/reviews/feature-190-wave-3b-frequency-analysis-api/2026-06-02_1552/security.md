# Security Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**Cycle**: 2 (incremental, post-resolution)

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

### What was reviewed

The diff introduces 815 new lines across 9 files. The core security-relevant change is a new module (`ngram.rs`, 256 lines) that provides `AstBigram` and `AstTrigram` newtypes for packing tree-sitter node-kind IDs into compact integer keys, plus vocabulary helpers and IDF weight lookup functions. The remaining changes are formatting adjustments, doc-comment updates, and 450 lines of tests.

### Security assessment

This change has an exceptionally small security surface for the following reasons:

1. **No I/O, no network, no filesystem access**: All new code is pure computation over static, compile-time arrays (`NODE_KIND_VOCABULARY`, per-language weight tables). No user-supplied data reaches these functions at runtime in the current architecture.

2. **No trust boundary crossing**: The new public API (`AstBigram::encode`, `AstTrigram::encode`, `vocab_lookup`, `ast_bigram_idf`, etc.) is consumed only by `rskim-research` (build-time tooling), not by the CLI binary that processes user input. Even if external callers provide arbitrary `NodeKindId` values, the worst outcome is a `DEFAULT_AST_WEIGHT` fallback or a `None` return -- no panics, no undefined behavior.

3. **Safe integer casts**: All `as NodeKindId` casts in `decode()` are preceded by `& 0xFFFF` bitmask operations, making truncation impossible. The `vocab_lookup` function correctly uses `u16::try_from(idx).ok()` instead of `as u16` (this was a prior-cycle fix that is now verified in place).

4. **Visibility controls enforced**: `from_raw()` on both `AstBigram` and `AstTrigram` is `pub(crate)`, preventing external callers from bypassing the encoding contract. The inner fields (`pub(crate) u32` / `pub(crate) u64`) are similarly restricted.

5. **No allocation in hot paths**: All public functions operate on stack values or perform lookups into static slices. No denial-of-service vector through unbounded allocation.

6. **No hardcoded secrets or credentials**: The module contains only structural constants (`DEFAULT_AST_WEIGHT = 1.0`).

### Prior resolution verification

The Cycle 1 resolution fixed a truncating `as` cast to use `try_from`. Verified at `ngram.rs:193`: `u16::try_from(idx).ok()` is correctly in place (applies ADR-001).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 10/10
**Recommendation**: APPROVED
