# Architecture Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03 18:34

## Scope

Reviewed the Wave 3c AST sparse n-gram extraction changes against `main`:
- `crates/rskim-search/src/ast_index/extract.rs` (new, 270 lines)
- `crates/rskim-search/src/ast_index/extract_tests.rs` (new, 778 lines)
- `crates/rskim-search/src/ast_index/mod.rs` (re-exports)
- `crates/rskim-search/src/lib.rs` (crate-root re-exports)

This is CYCLE 2. Cycle 1 fixed 13 issues (0 false positives). Cycle-1 fixes
(let-chain conversion, debug_assert invariants, HashMap cap, doc accuracy, DI
allocation contract doc, u16 overflow widening) were verified as present and
are NOT re-raised.

## Architectural Assessment

The change is architecturally clean and consistent with the documented
`ast_index` design (`devflow:apply-feature-knowledge` applied against
`.devflow/features/ast-index/KNOWLEDGE.md`). Verified:

- **Layering respected**: `extract.rs` is pure (no I/O, no global state),
  sits correctly above `linearize`/`ngram` and depends inward on
  `rskim_core::Language` and `crate::ast_index` siblings. No dependency-rule
  inversion, no leaky abstraction (no tree-sitter or storage types surface in
  the public output).
- **Dependency injection**: the `extract_ast_ngrams_with_weights` (DI core) /
  `extract_ast_ngrams` (production wrapper) split matches the project's DI
  convention (applies ADR-001 lineage; consistent with `linearize` patterns).
  Weight functions injected as `impl Fn`, keeping the core testable with
  synthetic weights.
- **Single responsibility**: the module does one thing — replay a linearized
  CST through a depth-indexed ancestor stack and emit deduplicated weighted
  n-grams. No god function; the one public function is ~130 lines but linear
  and well-sectioned.
- **PF-004 honored**: gap-fill widens to `u32` before `+ 1`
  (`extract.rs:159-160`), preventing the documented u16-wrap edge.
- **Encapsulation of bounds**: relies on `AstWalkConfig::DEFAULT_MAX_DEPTH/NODES`
  upstream rather than redefining limits — no parallel constant divergence.
- **API surface**: re-exports in `mod.rs` and `lib.rs` are additive and follow
  the existing alphabetized grouping; no breaking changes to prior exports.

No SOLID violations, no circular dependencies, no tight coupling, no god class.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None at >=80% confidence.

## Pre-existing Issues (Not Blocking)

None relevant to architecture.

## Suggestions (Lower Confidence)

- **DI seam stops at the weight layer, not the accumulator/sort** -
  `extract.rs:227-245` (Confidence: 62%) — The HashMap accumulation and
  `sort_unstable_by_key` collection step is inlined in the DI core. If a future
  consumer (#194 on-disk format) needs a different output shape (e.g. streaming
  emit, or pre-sorted insertion), it cannot inject that without forking the
  function. Not a violation today since `AstNgramSet` is the only documented
  output contract; flagging only as a seam to watch when #194/#197 land.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED
