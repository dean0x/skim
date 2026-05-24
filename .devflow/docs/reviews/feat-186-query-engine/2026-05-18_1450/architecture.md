# Architecture Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Duplicated validation between QueryEngine and NgramIndexReader** - `query.rs:46-63`, `reader.rs:309-327`
**Confidence**: 85%
- Problem: `QueryEngine::search()` validates empty queries (line 47) and `bm25f_config` (line 58-60), but `NgramIndexReader::search()` already performs the exact same checks: empty-query short-circuit at `reader.rs:310-312` and `cfg.validate()` at `reader.rs:322-323`. When `QueryEngine` wraps `NgramIndexReader`, validation runs twice for every query. This is a mild Layering Violation: the decorator and its inner layer both perform identical trust-boundary validation, creating ambiguity about which layer owns validation responsibility.
- Fix: This is acceptable as defense-in-depth for now, since `QueryEngine` is designed to wrap *any* `SearchLayer` (not just `NgramIndexReader`). However, document this as an intentional design choice. Consider adding a doc comment to `QueryEngine` stating that inner layers may also validate, and that `QueryEngine` provides a guaranteed-consistent validation boundary regardless of inner layer implementation. Long-term, inner layers could skip validation when they detect they are wrapped (e.g., via a marker trait or config flag), but that optimization is not needed at this scale.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`QueryEngine` does not implement the builder/decorator lifecycle pattern** - `query.rs:34-43`
**Confidence**: 82%
- Problem: The crate's established lifecycle pattern separates build-time (`LayerBuilder`) from query-time (`SearchLayer`). `QueryEngine::new()` accepts a pre-built `Box<dyn SearchLayer>`, which is correct for a decorator, but there is no builder-side integration: `LayerBuilder::build()` returns a raw `Box<dyn SearchLayer>`, and wrapping it in `QueryEngine` is left to the caller. This means callers must know to wrap the layer manually. For a single decorator this is fine, but as more decorators are added (the PR description mentions Wave 4 features), ad-hoc manual wrapping will become brittle.
- Fix: No immediate code change needed. This is a design note for future waves: consider a pipeline/composition builder (e.g., `SearchPipelineBuilder`) that chains decorators declaratively. The current approach is architecturally sound for a single decorator.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`NgramIndexReader::search()` performs inline validation instead of delegating** - `reader.rs:321-323`
**Confidence**: 80%
- Problem: `NgramIndexReader` validates `bm25f_config` inline within its `search()` method rather than relying on the `QueryEngine` decorator for validation. With `QueryEngine` now available as a first-class validation layer, this creates two validation sites that must be kept in sync. If a new validation rule is added to `QueryEngine` but not to `NgramIndexReader` (or vice versa), queries may behave differently depending on whether they pass through `QueryEngine`.
- Fix: In a future PR, consider extracting validation into a shared function or having `NgramIndexReader` document that it expects pre-validated queries when used behind `QueryEngine`. This keeps SRP intact: `NgramIndexReader` owns scoring, `QueryEngine` owns validation.

## Suggestions (Lower Confidence)

- **Consider generic inner type instead of trait object** - `query.rs:35` (Confidence: 65%) -- `QueryEngine` stores `Box<dyn SearchLayer>`, which requires dynamic dispatch. A generic `QueryEngine<S: SearchLayer>` would enable monomorphization and eliminate vtable overhead. However, the crate already uses `Box<dyn SearchLayer>` as its standard return type from `LayerBuilder::build()`, so this is consistent with existing patterns and the performance difference is negligible for a validation-only decorator.

- **`QueryEngine` is not `Clone`** - `query.rs:34` (Confidence: 70%) -- `SearchQuery` derives `Clone`, `BM25FConfig` derives `Clone`, but `QueryEngine` cannot be `Clone` because `Box<dyn SearchLayer>` is not `Clone`. This could matter if `QueryEngine` needs to be shared across threads via `Arc` rather than cloned. The current design is fine since `SearchLayer` requires `Send + Sync`, making `Arc<QueryEngine>` the natural sharing pattern.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The `QueryEngine` decorator is a textbook application of the Decorator pattern, cleanly implementing the existing `SearchLayer` trait without modifying any existing types. It follows the Dependency Inversion Principle (depends on `SearchLayer` abstraction, not `NgramIndexReader` concretely), and it validates at the trust boundary as the project's CLAUDE.md mandates ("validate at boundaries").

The one condition is documentation: the HIGH finding about duplicated validation between `QueryEngine` and `NgramIndexReader` should be addressed with a doc comment clarifying the intended layering. This is a documentation-level fix, not a code change, and does not block merge.

Strengths:
- Clean Decorator pattern with correct `SearchLayer` trait implementation
- Proper separation of concerns: validation logic is isolated in its own module
- `MAX_QUERY_BYTES` constant is well-documented and appropriately sized
- Defense-in-depth: validation runs at the decorator regardless of inner layer behavior
- No tight coupling: `QueryEngine` depends only on the `SearchLayer` trait
- Re-exports are clean and follow existing crate conventions
