# Reliability Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Scope**: Incremental (4 commits since last review)
**Files**: `query.rs`, `query_tests.rs`, `mod.rs`

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

- **Oversized query short-circuit not proven via PanicLayer** - `query_tests.rs:107` (Confidence: 65%) -- `test_oversized_query_returns_invalid_query_error` uses a real inner layer via `build_query_engine` rather than `PanicLayer`. The test verifies the error is returned but does not structurally prove the inner layer is never reached (it relies on ordering, not the panic guard). Consider adding a `PanicLayer` variant analogous to `test_empty_query_short_circuits_inner_layer` to strengthen the short-circuit guarantee for oversized queries.

- **Invalid BM25F short-circuit not proven via PanicLayer** - `query_tests.rs:129` (Confidence: 65%) -- Same pattern as above: `test_invalid_bm25f_config_rejected_before_search` uses a real inner layer. A `PanicLayer`-based test would prove that BM25F validation rejects before delegation, independent of inner layer behavior.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 9/10
**Recommendation**: APPROVED

## Rationale

The incremental changes are clean from a reliability perspective:

1. **Bounded iteration**: No loops in production code. The single test loop (`test_deterministic_results`) is bounded at exactly 10 iterations.

2. **Assertion density**: `QueryEngine::search` validates three preconditions at the trust boundary -- empty text, byte-length overflow, and BM25F config validity -- before delegating to the inner layer. The defense-in-depth comment (line 47-49) explicitly documents that these checks are intentionally independent of the inner layer's own validation. This is strong.

3. **Allocation discipline**: The `format!` macro allocations only occur on error paths (oversized query, invalid BM25F), never on the hot success path. The empty-query short-circuit returns `Vec::new()` which is zero-allocation in Rust (no heap allocation until elements are pushed).

4. **Indirection limits**: Single `Box<dyn SearchLayer>` indirection. No nested boxing or deep reference chains.

5. **Resource bounds**: `MAX_QUERY_BYTES = 4096` is a well-chosen constant that prevents downstream parser strain. The byte-length check (`query.text.len()`) correctly measures UTF-8 byte length, consistent with the constant's name.

6. **Test double quality**: The `SpyLayer` / `PanicLayer` pattern is a significant improvement over the previous approach of building two identical indexes to compare results. `PanicLayer` structurally proves short-circuit paths by panicking if the inner layer is reached -- this is stronger than result comparison.

7. **Mutex safety in tests**: The `Mutex<Option<SearchQuery>>` in `SpyLayer` is used in single-threaded test contexts. The `#![allow(clippy::unwrap_used)]` at file scope is appropriate for test code.

The only gap is that `PanicLayer` is used to prove the empty-query short-circuit but not the oversized-query or invalid-BM25F short-circuits. This is a test coverage suggestion, not a production reliability issue -- the production validation ordering is correct and the error paths are well-tested.
