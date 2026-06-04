# Regression Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**PR**: #266
**Review Cycle**: 2 (incremental, post-resolution of cycle 1)

## Cross-Cycle Awareness

Cycle 1 identified 9 issues (6 fixed, 3 false positives, 0 deferred). All 6 fixes verified present in current HEAD:
- `u16::try_from` safe cast in `ngram.rs:193` -- confirmed present
- `pub(crate)` field visibility on `AstBigram(u32)` and `AstTrigram(u64)` -- confirmed at lines 61 and 124
- Ordering semantics tests (T14) added -- confirmed at lines 409-443
- TypeScript known-entry IDF test (T9b) added -- confirmed at lines 305-319
- Misleading weight comments cleaned up in T9/T12 -- confirmed
- Doc-comment stale module path fixed -- confirmed

No new regression vectors introduced by the resolution fixes. The `u16::try_from` change is strictly safer than the previous `as` cast. The ordering tests and TypeScript IDF test are additive test coverage.

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

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 10/10
**Recommendation**: APPROVED

## Regression Checklist

- [x] No exports removed without deprecation -- all 3 pre-existing crate-level exports (`LinearNode`, `LinearizeResult`, `linearize_source`) preserved in both `ast_index/mod.rs:35` and `lib.rs:35-38`; 9 new exports added additively
- [x] Return types backward compatible -- `linearize_source` signature unchanged (single-line format-only change)
- [x] Default values unchanged -- no constants or defaults modified in existing code; new `DEFAULT_AST_WEIGHT = 1.0` is additive
- [x] Side effects preserved -- no behavioral changes to existing functions; `linearize_tree`, `linearize_source`, `AstWalkIter` production logic untouched
- [x] All consumers of changed code updated -- verified: `rskim-research/ast_extract.rs` and `rskim-search/ast_index/linearize.rs` both still compile and pass tests (16/16 and 70/70 respectively)
- [x] Migration complete across codebase -- no migration needed (additive-only change)
- [x] CLI options preserved -- no CLI changes
- [x] API endpoints preserved -- no endpoint changes
- [x] Commit message matches implementation -- 3 commits: (1) feat: AstBigram/AstTrigram newtypes with vocab helpers and 45 tests, (2) fix: consolidate ngram tests for file limit, (3) fix: safe cast, doc clarity, field visibility, ordering tests. All verified present.
- [x] Breaking changes documented -- N/A (no breaking changes)

## Analysis

### Scope Classification

This PR modifies 9 files (815 insertions, 43 deletions). The changes break into three categories:

**1. New production code (additive, zero regression risk):**
- `ngram.rs` (256 lines): New module with `AstBigram`/`AstTrigram` newtypes, vocabulary helpers (`vocab_lookup`, `vocab_resolve`, `vocab_len`), and IDF weight lookup (`ast_bigram_idf`, `ast_trigram_idf`). All functions are pure (no I/O, no mutable state).
- `mod.rs` lines 33 and 36-39: New `mod ngram` declaration and `pub use` re-exports.
- `lib.rs` lines 35-38: Expanded `pub use` that preserves all 3 existing exports and adds 9 new ones.

**2. New test code (additive, zero regression risk):**
- `ngram_tests.rs` (450 lines): 45 tests across 15 groups covering encode/decode roundtrips, boundary values, key formula verification, Display formatting, vocabulary helpers, IDF weight lookup, encoding consistency with weight tables, ordering semantics, and default constant value.

**3. Format-only changes (cargo fmt, zero behavior change):**
- `ast_walk.rs`: Single assert reformatted (test code only, line 401).
- `ast_extract.rs`: Function parameter wrapping and assert formatting (test code only).
- `linearize_bench.rs`: Const string and closure formatting.
- `linearize.rs`: Function signature formatting (line 203).
- `linearize_tests.rs`: Assert and struct literal formatting.

### Regression Risk Assessment

**Shared primitive (`AstWalkIter`) not modified:** The only change to `ast_walk.rs` is a cosmetic assert reformatting in test code. The production `AstWalkIter` implementation (lines 1-260) is untouched. Both consumers (`linearize.rs` and `ast_extract.rs`) remain stable. Applies feature knowledge: "changes to ast_walk.rs affect both linearize and rskim-research/ast_extract" -- verified no production changes exist.

**Encoding consistency verified:** `AstBigram::encode()` and `AstTrigram::encode()` use the exact same bit-packing formulas as `rskim-research/src/ast_types.rs` (`encode_ast_bigram` / `encode_ast_trigram`). Test T13 (`bigram_encoding_consistent_with_weight_table`) directly validates that `AstBigram::encode()` produces the same u32 key as stored in `RUST_AST_BIGRAM_WEIGHTS[0]`. This guards against encoding drift.

**Vocabulary dependency is safe:** `vocab_lookup` uses `binary_search` on `NODE_KIND_VOCABULARY`, which is a sorted `&[&str]` array. The sortedness invariant is guarded by existing test `vocabulary_is_sorted` in `linearize_tests.rs:137`. The vocabulary itself is auto-generated and not modified in this PR.

**`Language::name()` alignment verified:** `ast_bigram_idf` and `ast_trigram_idf` delegate to `ast_bigram_weight(lang.name(), ...)`. The `Language::name()` return values ("Rust", "TypeScript", etc.) match the match arms in `ast_weights.rs:105466-105481` exactly. Non-tree-sitter languages ("JSON", "YAML", "TOML") correctly fall through to the default arm.

**Test suite healthy:** All 100 tests across the 3 affected test modules pass (70 ast_index, 14 ast_walk, 16 ast_extract).

### Intent vs. Reality

PR description claims: AstBigram (u32 newtype) and AstTrigram (u64 newtype), modified ast_index/mod.rs, lib.rs, ast_walk.rs, ast_extract.rs, 45 new tests. All verified present and matching. The commit messages accurately describe the implementation. Applies ADR-001 -- no issues found to defer.
