# Rust Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Scope**: Incremental (4 commits: c4c3cef, 21b07d2, 5312a63, 2a563b4)

## Issues in Your Changes (BLOCKING)

No blocking issues found.

## Issues in Code You Touched (Should Fix)

No should-fix issues found.

## Pre-existing Issues (Not Blocking)

No pre-existing issues found.

## Suggestions (Lower Confidence)

- **`SearchLayer` impl on `Arc<SpyLayer>` is an orphan-rule workaround that duplicates delegation** - `query_tests.rs:46-54` (Confidence: 65%) -- The `impl SearchLayer for Arc<SpyLayer>` block manually delegates to `(**self).search()` and `(**self).name()`. A newtype wrapper (e.g., `struct SharedSpy(Arc<SpyLayer>)`) implementing `SearchLayer` would be more idiomatic and avoids a blanket-like impl on a foreign type (`Arc`) for a local trait. This is test-only code so the impact is low, but the pattern could set a precedent if copied into production code.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This incremental diff is clean, well-structured Rust code. Specific observations:

**Ownership and borrowing (good):**
- `QueryEngine` correctly holds `Box<dyn SearchLayer>` (owned, heap-allocated trait object) satisfying the `Send + Sync` requirement from the trait.
- The `search` method borrows `&SearchQuery` rather than taking ownership, which is correct for a decorator that forwards to inner layers.

**Error handling (good):**
- All fallible paths return `Result` via the `?` operator or explicit `Err()` construction.
- Error messages include both the limit and the actual value, making diagnostics actionable.
- `BM25FConfig::validate()` is called via `?` propagation -- clean and idiomatic.

**Type-driven design (good):**
- `#[must_use]` on `QueryEngine::new` ensures the constructed engine is not silently discarded.
- The decorator pattern with `Box<dyn SearchLayer>` enables composable validation without modifying existing layers.
- `MAX_QUERY_BYTES` is a named constant, not a magic number.

**Test quality (good):**
- `SpyLayer` verifies exact query forwarding (text, lang, limit, offset) -- tests behavior, not implementation.
- `PanicLayer` proves short-circuit paths never reach the inner layer -- a strong assertion.
- Edge cases (NaN, Infinity, NEG_INFINITY, boundary-exact length, unicode, whitespace-only, single-char) are well-covered.
- `#![allow(clippy::unwrap_used)]` is appropriately scoped to the test module only.
- The shift from `match result.unwrap_err()` pattern-matching to `format!("{}", result.unwrap_err())` with `assert!(msg.contains(...))` is a reasonable simplification -- it tests the Display output which is what end users see, though it is slightly less precise than matching the enum variant directly.

**Concurrency (good):**
- `Mutex<Option<SearchQuery>>` in `SpyLayer` is correct for interior mutability in a `Send + Sync` context, even though tests are single-threaded. The code is future-proof for concurrent test harnesses.

**No anti-patterns detected:**
- No `.unwrap()` in production code (only in tests behind `#![allow(clippy::unwrap_used)]`).
- No unnecessary `.clone()` calls to satisfy the borrow checker.
- No `unsafe` blocks.
- No blocking I/O in async context (no async code in this diff).
