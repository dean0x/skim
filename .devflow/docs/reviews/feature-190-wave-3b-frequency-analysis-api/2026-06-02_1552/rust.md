# Rust Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02T15:52
**Prior Resolutions**: Cycle 1 resolved 6 of 9 issues (3 false positives). This cycle focuses on net-new findings only.

## Issues in Your Changes (BLOCKING)

No blocking issues found.

## Issues in Code You Touched (Should Fix)

No should-fix issues found.

## Pre-existing Issues (Not Blocking)

No critical pre-existing issues found.

## Suggestions (Lower Confidence)

- **Asymmetric masking in `AstBigram::decode`** - `ngram.rs:77` (Confidence: 65%) -- `AstBigram::decode` uses `(self.0 >> 16) as NodeKindId` without an explicit `& 0xFFFF` mask, while `AstTrigram::decode` consistently masks all three components. The bigram cast is provably safe (u32 >> 16 leaves at most 16 bits), so this is a readability/consistency observation rather than a correctness issue. Adding `& 0xFFFF` would make the decode pattern uniform across both types.

- **`vocab_len` comparison uses `<` instead of `<=`** - `ngram_tests.rs:285` (Confidence: 60%) -- The test asserts `len < u16::MAX as usize` but the vocabulary could theoretically have exactly `u16::MAX` entries (65535) and still be valid since `NodeKindId` is `u16`. The current check is conservative and safe (it rejects one valid state), and the vocabulary currently has 1740 entries so this is academic.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

**Newtype design (applies ADR-001, feature knowledge patterns)**: The `AstBigram` and `AstTrigram` newtypes follow the Rust newtype pattern correctly -- `#[repr(transparent)]` wrapping, `Copy`/`Clone`/`Eq`/`Ord` derives, `pub(crate)` inner field, and `from_raw` as `pub(crate)` for internal iteration. The encoding formulas match the weight tables exactly (verified by T13), and the `#[derive(Ord)]` on `u32`/`u64` produces the correct parent-major and grandparent-major ordering (verified by T14).

**Error handling**: All public functions return `Option` or concrete values -- no `unwrap`/`expect` in production code. The `#[must_use]` attribute is consistently applied to all 13 public methods and functions. Weight lookup gracefully falls back to `DEFAULT_AST_WEIGHT` for unknown entries and non-tree-sitter languages.

**Type safety**: `NodeKindId` type alias clarifies intent. `vocab_lookup` uses `u16::try_from(idx).ok()` for safe narrowing (avoids PF-002 -- no silent truncation). `encode` uses `u32::from`/`u64::from` for widening casts (infallible). `decode` casts are safe: bigram masks via shift semantics, trigram masks explicitly via `& 0xFFFF`.

**Test coverage**: 45 tests across 15 groups (T1-T15) covering roundtrip encoding, boundary values, Display formatting, vocabulary helpers, IDF weight lookup for multiple languages, encoding consistency with weight tables, and ordering semantics. The `bigram_encoding_consistent_with_weight_table` test (T13) is particularly valuable -- it verifies that the encoding matches the stored weight table layout, preventing the kind of key mismatch that caused the remap bug in the research crate.

**Clippy**: Zero warnings. The `#[allow(dead_code)]` on `from_raw` methods is documented with intent (internal use for weight table iteration).

**Documentation**: Module-level doc comments with encoding formulas and usage examples. All public items have doc comments. The `mod.rs` usage example shows a complete workflow from linearization through bigram encoding to weight lookup.

**Cycle 1 fixes verified**: `u16::try_from` in T13 (safe narrowing), doc comment clarity, `pub(crate)` field visibility, ordering tests, TypeScript IDF test -- all confirmed present in HEAD.
