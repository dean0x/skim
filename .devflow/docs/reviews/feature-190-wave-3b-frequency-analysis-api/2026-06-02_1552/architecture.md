# Architecture Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**Cycle**: 2 (incremental review after 6 fixes from cycle 1)

## Issues in Your Changes (BLOCKING)

No CRITICAL or HIGH blocking issues found.

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Dual encoding implementations across crate boundary** - `crates/rskim-search/src/ast_index/ngram.rs` and `crates/rskim-research/src/ast_types.rs`
**Confidence**: 70% (see Suggestions)

Moved to Suggestions due to sub-threshold confidence -- the intentional crate separation justifies the duplication architecturally.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Encoding duplication across crate boundary** - `ngram.rs` vs `ast_types.rs` (Confidence: 70%) -- `AstBigram::encode/decode` in `rskim-search` duplicates `encode_ast_bigram/decode_ast_bigram` in `rskim-research`. Both use identical bit-shift formulas `(parent << 16) | child`. Since `rskim-search` (runtime) deliberately does not depend on `rskim-research` (offline codegen), this is a principled boundary: the research crate produces the weight tables and the search crate consumes them at runtime via `ast_weights.rs` (auto-generated). The duplication is acceptable given the compile-time firewall. Consider a shared `rskim-ast-encoding` micro-crate only if a third consumer appears.

- **`vocab_lookup` binary-search contract relies on codegen sort order** - `ngram.rs:190-194` (Confidence: 65%) -- `vocab_lookup` calls `NODE_KIND_VOCABULARY.binary_search(&kind)` and uses the returned index directly as a `NodeKindId`. This is correct only because the codegen (`ast-codegen`) emits the vocabulary in sorted order with index == ID. The contract is documented in `ast_weights.rs` ("indexed by NodeKindId") and tested by T6/T7, but there is no compile-time assertion that the array is sorted. A `debug_assert!` in `vocab_lookup` checking sortedness on first call (or a `const`-evaluated check) would make the invariant self-enforcing rather than test-enforced.

- **`Language::name()` string coupling to `ast_bigram_weight` match arms** - `ngram.rs:222-224` (Confidence: 62%) -- `ast_bigram_idf` calls `lang.name()` to get a `&str` and passes it to `ast_bigram_weight` which pattern-matches on exact strings ("Rust", "TypeScript", etc.). Both sides are auto-generated from the same source of truth (the codegen pipeline), so drift is unlikely. But if `Language::name()` ever changes a display string (e.g. "C++" to "Cpp"), the lookup silently falls through to `None` and returns the default weight with no error. The coupling is acceptable given both sides are generated, but worth noting.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | - | - | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR introduces a well-designed production API layer on top of the auto-generated `ast_weights` tables. The architecture is sound for the following reasons:

1. **Clean newtype pattern**: `AstBigram(u32)` and `AstTrigram(u64)` are `#[repr(transparent)]` newtypes with `pub(crate)` inner fields. This prevents external code from constructing invalid encodings while allowing the crate's own modules to access the raw value. The derive set (`Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord`) is exactly right for a numeric key type used in lookups and collections.

2. **Correct layering**: The new `ngram` module sits inside `ast_index` and depends only downward on `ast_weights` (generated data) and `rskim_core::Language` (shared type). No circular dependencies. No upward references. The dependency direction is `lib.rs -> ast_index/mod.rs -> ngram.rs -> ast_weights.rs`, which follows the Clean Architecture dependency rule (applies ADR-001 -- all issues from cycle 1 were fixed).

3. **Single Responsibility**: The `ngram.rs` module has one cohesive purpose: provide typed access to the n-gram encoding and weight lookup. Vocabulary helpers (`vocab_lookup`, `vocab_resolve`, `vocab_len`) are co-located because they share the same backing data (`NODE_KIND_VOCABULARY`). The IDF lookup functions (`ast_bigram_idf`, `ast_trigram_idf`) are thin wrappers that add the `DEFAULT_AST_WEIGHT` fallback policy. Each function does one thing.

4. **Re-export discipline**: The `ast_index/mod.rs` re-exports exactly the public API surface from `ngram`, and `lib.rs` re-exports that same set from the crate root. No leaky abstractions -- consumers can import from either `rskim_search::ast_index::AstBigram` or `rskim_search::AstBigram` without seeing internal module structure.

5. **Encoding consistency with weight tables**: The encoding formula `(parent << 16) | child` matches the key layout in `RUST_AST_BIGRAM_WEIGHTS` (and all per-language tables), verified by test T13. This means `ast_bigram_idf(lang, bigram)` performs a single binary search with no key transformation -- O(log n) with zero allocation.

6. **Principled crate boundary**: The duplication between `rskim-research::ast_types` (offline codegen tool) and `rskim-search::ast_index::ngram` (runtime library) is intentional. The research crate generates weight tables; the search crate consumes them. No runtime dependency between the two. The encoding formulas must match (and are tested), but they serve different lifecycle phases (avoids PF-002 -- not deferring this observation, explicitly noting the design is correct).

7. **Test coverage is behavior-focused**: 45 tests (T1-T14 groups) cover roundtrip encoding, boundary values, display formatting, vocabulary helpers, weight lookup, encoding consistency with the weight table, and ordering semantics. Tests verify behavioral contracts rather than implementation details.

No blocking or should-fix issues identified in this cycle. The three suggestions are all sub-80% confidence and concern defensive hardening rather than architectural violations. The cycle-1 fixes (safe casts, field visibility, doc clarity, ordering tests) have been confirmed addressed in the current diff.
