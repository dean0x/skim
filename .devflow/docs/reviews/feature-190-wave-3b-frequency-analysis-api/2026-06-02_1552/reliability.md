# Reliability Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02T15:52
**PR**: #266
**Prior Resolutions**: Cycle 1 resolved 6 issues (including truncating `as` cast -> `try_from`), 3 false positives, 0 deferred.

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

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

### Bounded Iteration

All iteration in this PR is bounded. The new `ngram.rs` module contains no loops
at all -- all functions are pure arithmetic (encode/decode), single-index lookups
(binary search on a static table), or single-element accesses (vocabulary array
index). The only loop-containing code is in `linearize_tree` (pre-existing), which
delegates to `AstWalkIter` with its `max_depth=500` and `max_nodes=100_000` bounds.
No unbounded operations introduced.

### Assertion Density

The test suite (450 lines, 45 tests in T1-T14 groups) provides thorough assertion
coverage across encode/decode roundtrips, boundary values (0, u16::MAX), key formula
verification, Display formatting, vocabulary helpers, IDF weight lookup, encoding
consistency with the weight table, and ordering semantics. The production code
itself uses `#[must_use]` on all public functions and `#[repr(transparent)]` on
newtypes for ABI correctness. The `from_raw` constructors are correctly scoped
to `pub(crate)` to prevent external callers from bypassing the encoding contract
(avoids anti-pattern documented in FEATURE_KNOWLEDGE).

### Allocation Discipline

The new `ngram.rs` module allocates nothing. All types are `Copy` (`AstBigram` is
`u32`, `AstTrigram` is `u64`). Vocabulary helpers index into static `&[&str]` arrays.
Weight lookup uses binary search on static `&[(u32, f32)]` / `&[(u64, f32)]` slices.
Zero allocation in the hot path.

### Indirection Limits

No indirection beyond a single `#[repr(transparent)]` newtype wrapper. The types
directly wrap primitives (`u32`, `u64`) with zero-cost abstraction.

### Metaprogramming Restraint

No macros, no recursive generics, no reflection. The type alias `NodeKindId = u16`
is a simple alias. All derive macros are standard (`Debug, Copy, Clone, PartialEq,
Eq, Hash, PartialOrd, Ord`).

### Cast Safety (Cross-Cycle Awareness)

Cycle 1 flagged truncating `as` casts and the fix replaced them with `u16::try_from`.
In the new code:

- `AstBigram::decode` lines 77-78: `(self.0 >> 16) as NodeKindId` and
  `(self.0 & 0xFFFF) as NodeKindId`. These are safe: `>> 16` on a `u32` produces
  at most 0xFFFF, and `& 0xFFFF` is always within u16 range. The mask makes these
  equivalent to `u16::try_from(...).unwrap()` but without the branch. Confidence
  that these are safe: 100%.
- `AstTrigram::decode` lines 140-142: all three components are masked with
  `& 0xFFFF` before the `as NodeKindId` cast, guaranteeing u16 range.
- `linearize.rs` line 264: `item.node.kind_id() as usize` is a widening cast
  from u16 to usize (always safe). Pre-existing, not introduced in this PR.
- `linearize.rs` line 253: `AstWalkConfig::DEFAULT_MAX_NODES as usize` is a
  widening cast from u32 to usize (always safe on 32-bit+ targets). Pre-existing.
- `linearize.rs` line 269: `item.depth.min(u32::from(u16::MAX)) as u16` uses
  saturating min before cast. Pre-existing, already marked with
  `#[allow(clippy::cast_possible_truncation)]` as documentation of intent.

All `as` casts in the new code are either widening (safe) or masked to the target
range (safe). No truncation risk. Applies ADR-001 (all noticed issues fixed
immediately -- the cycle 1 `try_from` fix covered the one real truncation risk,
and no new truncation risks were introduced).

### Fallback Behavior

`ast_bigram_idf` and `ast_trigram_idf` correctly fall back to `DEFAULT_AST_WEIGHT`
(1.0) for both unknown entries and non-tree-sitter languages. The `.unwrap_or()`
pattern is deterministic and never panics. The gotcha about distinguishing "not
found" from "found with weight 1.0" is documented in FEATURE_KNOWLEDGE and in the
doc comments.

### `vocab_lookup` Conversion Safety

Line 193: `u16::try_from(idx).ok()` correctly uses `try_from` (not `as u16`) when
converting the binary search result index to `NodeKindId`. This is the correct
pattern from cycle 1. The vocabulary has 1740 entries, well within u16 range, but
the `try_from` guards against future growth.
