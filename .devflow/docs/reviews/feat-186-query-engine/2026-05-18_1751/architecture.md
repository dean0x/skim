# Architecture Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Scope**: Incremental (4 commits since last review)
**Files**: query.rs, query_tests.rs, mod.rs

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

- **`Arc<SpyLayer>` orphan-rule workaround may not scale** - `query_tests.rs:46-54` (Confidence: 65%) -- Implementing `SearchLayer for Arc<SpyLayer>` directly is a pragmatic test-only workaround, but if future test doubles need the same pattern you may want a generic blanket impl (`impl<T: SearchLayer> SearchLayer for Arc<T>`) on the trait itself, or wrap in a newtype. Fine for a single test double; worth revisiting if the pattern repeats.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This increment is architecturally clean. The 4 commits refine an already well-structured decorator without changing the public contract. Specific observations:

**Decorator pattern (correct application of OCP and DIP)**
`QueryEngine` wraps `Box<dyn SearchLayer>`, depends on the `SearchLayer` abstraction rather than any concrete index, and can be composed around any layer without modifying it. This is textbook Decorator and satisfies the Open/Closed Principle -- new validation rules can be added to the decorator without touching the inner layer.

**Single Responsibility**
`QueryEngine` has exactly one concern: trust-boundary validation. It validates empty queries, oversized queries, and BM25F config, then delegates. The validation logic is 17 lines with no branching beyond the three guard clauses. No god-class risk.

**Dependency direction**
All dependencies point inward: `query.rs` depends on the trait (`SearchLayer`) and the error type (`SearchError`), both defined in `types.rs`. The decorator never imports concrete index types. Clean Architecture dependency rule holds.

**Test doubles (SpyLayer, PanicLayer)**
The refactored tests properly use test doubles instead of comparing two real index instances. `SpyLayer` proves delegation; `PanicLayer` proves short-circuiting. Both implement the `SearchLayer` trait, validating the interface segregation.

**Defense-in-depth comment (line 47-49)**
Explicitly documents that the decorator intentionally duplicates validation that the inner layer may also perform. This makes the design intent clear and prevents future contributors from removing "redundant" checks.

**`#[must_use]` on `new` (line 40)**
Correct Rust convention for a constructor whose return value must not be discarded.

**PR description mismatch (not blocking)**
The PR description states "64KB default" but `MAX_QUERY_BYTES` is 4096 (4 KiB). The code and its doc comment ("4 KiB is well beyond any reasonable human or tool-generated query") are internally consistent, so this is a PR description inaccuracy, not a code defect.
