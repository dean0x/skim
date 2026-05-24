# Architecture Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicated source of truth for FIELD_COUNT (magic number 8 vs constant)** - `builder.rs:51`, `builder.rs:74`, `builder.rs:187-188`, `builder.rs:259`, `builder.rs:263`, `format.rs:93`, `format.rs:156`
**Confidence**: 85%
- Problem: The `lexical/config.rs` defines `pub const FIELD_COUNT: usize = 8` and `SearchField::count()` returns `8`, yet the builder and format modules use the raw literal `8` for array sizes (`[u64; 8]`, `[u32; 8]`, `[f32; 8]`). The reader correctly imports and uses `FIELD_COUNT` from `lexical`, but the builder and format modules do not. This creates multiple sources of truth for the same invariant — if `SearchField` ever gains a 9th variant, the builder/format arrays will silently remain at 8 without a compile error.
- Fix: Import and use `FIELD_COUNT` (or `SearchField::count()`) in builder.rs and format.rs array declarations instead of the literal `8`:
  ```rust
  use crate::lexical::FIELD_COUNT;
  total_field_lengths: [u64; FIELD_COUNT],
  // and similarly for all [u32; 8] / [f32; 8] in format.rs
  ```

**`classify_source` bypasses the `FieldClassifier` trait — parallel classification architectures** - `lexical/classifier.rs:93`
**Confidence**: 82%
- Problem: The crate already defines a `FieldClassifier` trait (types.rs:436) and `NodeInfo` struct designed to decouple classification from tree-sitter. The new `classify_source` function directly instantiates `rskim_core::Parser`, walks the tree-sitter AST, and hardcodes classification logic via `map_priority_to_field` — completely bypassing the abstraction layer that was purpose-built for this exact use case. This creates two parallel classification architectures: one trait-based (injectable, testable) and one concrete (tightly coupled to rskim_core internals). The trait is now unused dead API surface.
- Fix: Either (a) implement `FieldClassifier` as the backing logic for `classify_source`, accepting a `&dyn FieldClassifier` parameter so behaviour can be injected/mocked, or (b) if the trait is premature, remove `FieldClassifier` + `NodeInfo` to avoid maintaining dead abstractions. The current state where both exist but don't interact violates ISP (unused interface) and creates confusion about which is canonical.

### MEDIUM

**`pub(crate)` visibility leak on `NgramIndexBuilder::file_count`** - `builder.rs:47`
**Confidence**: 88%
- Problem: The field was changed from private to `pub(crate)` visibility. There is no apparent consumer of this field outside the struct's own impl block — only tests access `builder.file_count` and tests already have access through the module system. Widening visibility without a consumer creates unnecessary coupling surface.
- Fix: Keep it private and add a `pub(crate) fn file_count(&self) -> u32` getter if external crate-internal access is genuinely needed. If only tests need it, `#[cfg(test)]` accessor or asserting via `stats()` after build is cleaner.

**Tight coupling: `classify_source` directly instantiates `rskim_core::Parser`** - `lexical/classifier.rs:104`
**Confidence**: 83%
- Problem: `classify_source` calls `rskim_core::Parser::new(lang)` directly (content coupling). This means the function cannot be tested without the full tree-sitter grammar loaded, cannot be mocked for languages that don't exist yet, and binds the lexical module to rskim_core's parser implementation details. DIP violation: a high-level scoring module depends on a concrete low-level parser.
- Fix: Accept a pre-parsed tree (or a parser factory trait) as a parameter. The tree is already being computed in the builder context — passing it in avoids re-parsing. Alternatively, accept a generic `impl Fn(&str, Language) -> Result<Tree>` to allow test doubles.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SearchField::count()` and `FIELD_COUNT` are independent constants that can drift** - `types.rs:98-100`, `lexical/config.rs:26`
**Confidence**: 85%
- Problem: `SearchField::count()` returns the hardcoded literal `8`. `lexical::FIELD_COUNT` is a separate `const` also set to `8`. Neither is derived from the other, and neither will cause a compile error if one changes without the other. The comment on `count()` says "Must equal `FIELD_COUNT`" but this is a documentation invariant, not a type-system invariant.
- Fix: Define one as the canonical source and derive the other:
  ```rust
  // In types.rs:
  pub const fn count() -> usize { Self::ALL.len() }
  
  // In lexical/config.rs:
  pub const FIELD_COUNT: usize = SearchField::count();
  ```
  This way adding a variant to `ALL` propagates everywhere automatically.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`FieldClassifier` trait + `NodeInfo` are dead code** - `types.rs:414-438`
**Confidence**: 80%
- Problem: The trait was added in anticipation of the BM25F classifier, but the actual classifier does not use it. It is exported in the public API (`lib.rs:28`) but has zero production consumers. Dead public API surface is a maintenance burden and confuses users about the intended extension point.
- Fix: Either implement it (see BLOCKING issue above) or remove it in a follow-up cleanup PR. If kept for future use, add `#[doc(hidden)]` or move to an internal module with a tracking issue.

## Suggestions (Lower Confidence)

- **Potential O(n) memory overhead from per-byte field_at array** - `classifier.rs:116` (Confidence: 70%) — For very large files, allocating a `Vec<SearchField>` with one byte per source byte could be expensive. The run-length-encoded output (ranges) is far smaller. Consider computing ranges directly via the tree walk rather than allocating the full array and compressing. This is an architectural choice (simplicity vs memory) worth documenting.

- **HashMap allocations in search hot path** - `reader.rs:273-279` (Confidence: 65%) — Each search creates 4 HashMaps (`doc_scores`, `doc_field_tfs`, `doc_positions`, `doc_meta_cache`). For high-QPS scenarios, pre-allocated or pooled structures would reduce allocation pressure. May be premature for current use case.

- **Sort stability change** - `reader.rs:336` (Confidence: 72%) — Switching from `sort_unstable_by` to `sort_by` for tie-breaking is correct for determinism but trades performance. Since BM25F scores are floats and ties should be rare, `sort_unstable_by` with the tie-breaking comparator would preserve both determinism and performance.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new `lexical/` module is well-structured with clear separation of concerns (config, classifier, scoring). The format v2 design is clean and the builder/reader integration is solid. However, the duplication between the existing `FieldClassifier` trait and the new concrete `classify_source` function creates architectural confusion about the canonical classification path. The hardcoded `8` literals scattered across builder/format should use the `FIELD_COUNT` constant to maintain a single source of truth for the field count invariant.
