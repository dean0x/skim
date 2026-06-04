# Rust Review Report

**Branch**: feature-190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**PR**: #266

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Truncating `as` cast in `vocab_lookup` without compile-time guard** - `crates/rskim-search/src/ast_index/ngram.rs:193`
**Confidence**: 82%
- Problem: `idx as NodeKindId` (i.e. `usize as u16`) is a truncating cast. Today the vocabulary has 1740 entries so it is safe, and `vocab_len_nonzero_and_fits_in_u16` validates this at test time. However, the `as` cast would silently produce wrong IDs if the vocabulary grew past 65535 entries. A `u16::try_from(idx).ok()` chain would make the contract self-enforcing and remove the dependency on a runtime test for correctness.
- Fix:
  ```rust
  NODE_KIND_VOCABULARY
      .binary_search(&kind)
      .ok()
      .and_then(|idx| u16::try_from(idx).ok())
  ```
  This replaces the `as` cast with a fallible conversion. If the vocabulary exceeds u16::MAX, the function returns `None` (correct behavior) instead of silently wrapping.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Doc-comment references stale module path** - `crates/rskim-search/src/ast_index/ngram.rs:44` (Confidence: 65%) -- The doc on `DEFAULT_AST_WEIGHT` references `crate::weights::DEFAULT_WEIGHT` but the re-export in `lib.rs` is `weights::DEFAULT_WEIGHT`. If the weights module is ever reorganized, the intra-doc link will break. Consider using a proper `[`...`]` intra-doc link so rustdoc validates it.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

This is a well-crafted PR that adds `AstBigram`/`AstTrigram` newtypes following the project's established patterns precisely. Key observations:

**Strengths (aligns with FEATURE_KNOWLEDGE):**
- Newtypes use `#[repr(transparent)]` for zero-cost abstraction -- correct.
- `encode`/`decode` use bit shifting on `u32`/`u64` -- matches the encoding in `rskim-research/src/ast_types.rs`.
- `from_raw()` is `pub(crate)` -- prevents external construction from raw values.
- All public methods have `#[must_use]` and `#[inline]` -- consistent with project conventions.
- `vocab_lookup` uses binary search on sorted static array -- correct pattern.
- `fmt_kind_id` Display helper uses `match` on `Option` with three-way fallback (`Some("")`, `Some(s)`, `None`) -- matches documented pattern exactly.
- No `unsafe`, no panics in non-test code.
- Comprehensive test coverage (37 tests) with boundary values, roundtrip properties, and consistency checks against the actual weight tables.

**Decision citations:**
- `applies ADR-001`: The single HIGH finding (truncating `as` cast) should be fixed now rather than deferred, per the project's "fix all noticed issues immediately" policy.

**The one condition for approval** is replacing the `as NodeKindId` cast in `vocab_lookup` with `u16::try_from(idx).ok()` to make the u16 bound self-enforcing rather than relying solely on a runtime test assertion.

The remaining changes (formatting adjustments in `ast_walk.rs`, `ast_extract.rs`, `linearize.rs`, `linearize_tests.rs`, `linearize_bench.rs`) are purely `cargo fmt` reformats with no behavioral change.
