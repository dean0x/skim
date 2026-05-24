# Consistency Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14T01:19

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Section separator style differs from crate convention** - `ngram.rs:23-25, 34-36, 87-89, 160-162, 210-212`
**Confidence**: 95%
- Problem: `ngram.rs` uses Unicode box-drawing line separators (`// ─────...`) while every other file in both `rskim-search` and `rskim-core` uses ASCII equals-sign separators (`// ============...`). The `types.rs` file in the same crate uses the `=` pattern in 18 instances, and `rskim-core/src/lib.rs` and `rskim-core/src/types.rs` use it consistently throughout. This is a visual inconsistency introduced by the new module.
- Fix: Replace all `// ─────...` lines with the `// ============...` pattern used elsewhere:
```rust
// Before (ngram.rs):
// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

// After (matching types.rs / rskim-core):
// ============================================================================
// Constants
// ============================================================================
```

**Derive trait ordering inconsistent with crate convention** - `ngram.rs:45`
**Confidence**: 85%
- Problem: `Ngram` uses `#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]` with `Debug` last. Every other struct in `rskim-search/src/types.rs` puts `Debug` first: `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, ...)]`. The analogous newtype `FileId` derives `Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize` with `Debug` leading.
- Fix: Reorder to match the crate convention:
```rust
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ngram(pub u16);
```

**Duplicate weight lookup logic between `ngram::lookup_weight` and `weights::bigram_weight`** - `ngram.rs:95-101`
**Confidence**: 82%
- Problem: `ngram.rs` defines a private `lookup_weight(key, weights)` that performs binary search on a `(u16, f32)` slice and falls back to `DEFAULT_WEIGHT`. Meanwhile, `weights.rs` already exports a public `bigram_weight(bigram: u16) -> Option<f32>` that does the same binary search on `BIGRAM_WEIGHTS`. The private helper is more general (accepts any weight slice), which is needed for test injection, but the duplication of the binary-search logic introduces a maintenance risk — if the weight table format changes, two call sites need updating.
- Fix: This is acceptable as-is because `lookup_weight` serves a different purpose (injectable weight table for testing). However, consider documenting the relationship explicitly:
```rust
/// Look up the IDF weight for a bigram key in a sorted `(key, weight)` slice.
///
/// This is the injectable variant of [`crate::weights::bigram_weight`], accepting
/// an arbitrary weight table for testing. Falls back to [`DEFAULT_WEIGHT`] when
/// the key is absent.
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Test file uses crate-level `#![allow(clippy::unwrap_used)]` instead of module-level attribute** - `ngram_tests.rs:1`
**Confidence**: 85%
- Problem: `ngram_tests.rs` uses `#![allow(clippy::unwrap_used)]` (crate-level inner attribute) at line 1. In `types.rs`, the same allow is applied as an outer attribute on the `mod tests` block: `#[allow(clippy::unwrap_used)] mod tests {`. The `weights.rs` test module has no unwraps and does not need the attribute. While both forms work correctly under the `#[path = ...]` include mechanism, the inner attribute form (`#!`) is the less common pattern in this crate.
- Fix: Since the file is included via `#[path = "ngram_tests.rs"]`, the inner attribute is technically correct and scoped to just this module. However, for visual consistency with `types.rs`, consider moving the allow to the `#[cfg(test)]` block in `ngram.rs`:
```rust
// ngram.rs:298-300
#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "ngram_tests.rs"]
mod tests;
```
And remove `#![allow(clippy::unwrap_used)]` from `ngram_tests.rs` line 1.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`FileId` lacks `#[repr(transparent)]` while `Ngram` has it** - `types.rs:30-31` vs `ngram.rs:44-46`
**Confidence**: 65% (below threshold, moved to Suggestions)

## Suggestions (Lower Confidence)

- **`FileId` and `Ngram` have inconsistent `#[repr(transparent)]` usage** - `types.rs:31` vs `ngram.rs:44` (Confidence: 65%) -- `Ngram` annotates `#[repr(transparent)]` but the analogous newtype `FileId(pub u32)` in `types.rs` does not. Both are single-field wrappers. If `repr(transparent)` is important for FFI or transmute safety of `Ngram`, document why; if not, the attribute is noise that differs from the sibling type.

- **`Ngram` lacks `Serialize`/`Deserialize` derives unlike other public types** - `ngram.rs:45` (Confidence: 62%) -- Every other public struct in `rskim-search` (`FileId`, `SearchQuery`, `SearchResult`, `IndexStats`) derives `Serialize` and `Deserialize`. `Ngram` does not. This is likely intentional (bigrams are internal index primitives not serialized to JSON), but it breaks the pattern. If future search results need to expose matched ngrams, this will need adding.

- **`#[path = "ngram_tests.rs"]` test file pattern is unique in codebase** - `ngram.rs:299` (Confidence: 60%) -- No other module in `rskim-search` or `rskim-core` uses a separate `#[path = ...]` test file. All other test modules are inline `mod tests {}` blocks within the source file. The separate file is reasonable given the test count (382 lines), but it introduces a pattern that future contributors may not expect.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `ngram` module is well-structured, thoroughly documented, and follows most crate conventions. The four medium-severity findings are all stylistic consistency gaps (section separators, derive ordering, clippy allow placement, and a minor documentation suggestion for the duplicate lookup helper). None represent correctness or regression risks. The section separator mismatch is the most visible inconsistency since it affects the entire file and diverges from a universal pattern across both crates.
