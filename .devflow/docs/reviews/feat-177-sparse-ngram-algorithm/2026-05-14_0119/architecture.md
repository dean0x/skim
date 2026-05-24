# Architecture Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicated `lookup_weight` reimplements existing `bigram_weight`** - `crates/rskim-search/src/ngram.rs:95`
**Confidence**: 90%
- Problem: `ngram.rs` defines a private `lookup_weight(key, weights)` function that performs binary search over a `(u16, f32)` slice and falls back to `DEFAULT_WEIGHT`. The existing public `bigram_weight(bigram)` in `weights.rs:16142` performs the same binary search over `BIGRAM_WEIGHTS` but returns `Option<f32>` instead of defaulting. This is a near-duplicate — the logic is the same binary search pattern on the same data shape. When the weight table encoding or lookup strategy changes, two call sites must be updated independently.
- Fix: Consolidate by either (a) adding a `pub fn lookup_weight(key: u16, weights: &[(u16, f32)]) -> f32` to `weights.rs` that accepts an arbitrary slice and uses `DEFAULT_WEIGHT` as fallback, then call it from `ngram.rs`, or (b) refactoring `bigram_weight` to accept an optional fallback. The design choice of accepting an arbitrary `weights` slice (for testability with synthetic tables) is sound — but the binary search logic should live in one place.

```rust
// In weights.rs — add generic lookup:
#[must_use]
#[inline]
pub fn lookup_weight(key: u16, weights: &[(u16, f32)]) -> f32 {
    weights
        .binary_search_by_key(&key, |&(k, _)| k)
        .ok()
        .map(|idx| weights[idx].1)
        .unwrap_or(DEFAULT_WEIGHT)
}

// Then bigram_weight becomes a thin wrapper:
pub fn bigram_weight(bigram: u16) -> Option<f32> {
    BIGRAM_WEIGHTS
        .binary_search_by_key(&bigram, |&(k, _)| k)
        .ok()
        .map(|idx| BIGRAM_WEIGHTS[idx].1)
}
```

### MEDIUM

**Duplicated `BORDER_MULTIPLIER` constant across crates** - `crates/rskim-search/src/ngram.rs:32` and `crates/rskim-research/src/validate.rs:20`
**Confidence**: 82%
- Problem: `BORDER_MULTIPLIER` is defined as `f32 = 3.5` in `ngram.rs` (production code) and as `f64 = 3.5` in `validate.rs` (research code). The value is identical but the types differ. If the multiplier is tuned in one crate but not the other, the research validation tool will no longer validate against the actual production behavior. This is a DRY violation across a trust boundary.
- Fix: The research crate already depends on workspace-level crates. If `rskim-research` can depend on `rskim-search` (or a shared constants crate), import `BORDER_MULTIPLIER` from a single source of truth. If the dependency direction forbids this (research should not be a dependency of search), document the coupling explicitly with a comment in both locations, e.g. `// SYNC: must match rskim-search::ngram::BORDER_MULTIPLIER`.

**`is_border_bigram` uses linear scan over border ranges** - `crates/rskim-search/src/ngram.rs:154`
**Confidence**: 80%
- Problem: `is_border_bigram` is called once per bigram position in `extract_query_ngrams_with_weights` (line 254) and iterates all border ranges with `.any()`. For queries, this is O(positions x ranges). Query strings are typically short (< 100 bytes), so the practical impact is negligible, but this creates a latent O(n*m) pattern. As queries grow or if this function is reused for document-side extraction, it could become a bottleneck.
- Fix: Since border ranges are sorted by `lo` ascending (they are produced in token order), a binary search or sorted-range approach could replace the linear scan. However, given the typical query size (< 100 bytes, < 20 ranges), this is a reasonable design trade-off for now. Consider adding a comment documenting this assumption: `// O(n*m) acceptable for typical query lengths < 100 bytes`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Public API re-exports `extract_ngrams_with_weights` indirectly but not directly** - `crates/rskim-search/src/lib.rs:16`
**Confidence**: 85%
- Problem: `lib.rs` re-exports `extract_ngrams` and `extract_query_ngrams` (the convenience wrappers) but not the `_with_weights` variants. The `_with_weights` functions are the core implementation that accepts injectable weight tables — the testable, composable API surface. Downstream crates that want to unit-test against synthetic weights must reach through `rskim_search::ngram::extract_ngrams_with_weights` rather than `rskim_search::extract_ngrams_with_weights`. This creates an inconsistent public surface: the module is `pub mod ngram` so the functions are technically accessible, but the re-export pattern signals that the convenience wrappers are the intended API.
- Fix: Either (a) re-export the `_with_weights` variants in `lib.rs` to signal they are first-class API, or (b) add a doc comment on `pub mod ngram` explaining that the `_with_weights` variants are intentionally module-scoped for advanced use. Option (a) is preferred since the weight-injectable design is the architecturally correct pattern (DIP compliance).

```rust
pub use ngram::{
    BORDER_MULTIPLIER, Ngram,
    extract_ngrams, extract_ngrams_with_weights,
    extract_query_ngrams, extract_query_ngrams_with_weights,
};
```

## Pre-existing Issues (Not Blocking)

No pre-existing architectural issues identified in the reviewed files.

## Suggestions (Lower Confidence)

- **Consider a `WeightTable` newtype wrapping `&[(u16, f32)]`** - `crates/rskim-search/src/ngram.rs:181` (Confidence: 65%) -- Both extraction functions accept `weights: &[(u16, f32)]` with a debug_assert that the slice is sorted. A newtype `WeightTable` with a constructor that validates sort order would encode this invariant in the type system rather than relying on runtime assertions.

- **Covering-set early-exit check is O(n) per iteration** - `crates/rskim-search/src/ngram.rs:277` (Confidence: 70%) -- The `covered.iter().all(|&c| c)` check inside the loop scans the entire covered array on each iteration. A counter tracking the number of uncovered positions (decremented when positions are newly covered) would make this O(1). For typical query lengths this is academic, but it is a clean optimization if query sizes grow.

- **`extract_ngrams_with_weights` returns `Vec<(Ngram, f32)>` instead of a dedicated result type** - `crates/rskim-search/src/ngram.rs:181` (Confidence: 62%) -- A `WeightedNgram { ngram: Ngram, weight: f32 }` struct would be more self-documenting than a tuple, especially as downstream consumers pattern-match on the results. This is minor and a matter of API ergonomics.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `ngram` module demonstrates strong architectural fundamentals: clean separation of concerns (newtype, extraction, query logic), dependency injection via weight table parameters (enabling testability without mocks), the Strategy Pattern for document vs. query extraction, and a pure-function design with no I/O — fully consistent with the library's stated architecture ("IMPORTANT: This is a LIBRARY with NO I/O"). The `Ngram` newtype, `#[must_use]` annotations, and `debug_assert!` on preconditions follow Rust idioms well.

The primary architectural concern is the duplicated binary-search logic between `ngram::lookup_weight` and `weights::bigram_weight`. This should be consolidated before the module accumulates more consumers. The `BORDER_MULTIPLIER` duplication across crates is a secondary concern that should be addressed with either a shared import or explicit sync documentation. The public API re-export inconsistency is a minor polish item that improves discoverability.

Conditions for approval:
1. Consolidate `lookup_weight` with `weights.rs` to eliminate the duplicated binary search pattern (HIGH).
