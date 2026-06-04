# Complexity Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`cmd_ast_run` is 108 lines with inline summary-printing logic that duplicates `ast_validate`** - `main.rs:374-481`
**Confidence**: 85%
- Problem: At 108 lines, `cmd_ast_run` exceeds the 50-line warning threshold and approaches the "hard to understand in 5 minutes" zone. Lines 450-478 recompute error-node-rate with the same formula used in `ast_validate.rs:65-77`, creating logic duplication. The function mixes orchestration (clone, extract, stabilize, rekey, IDF) with reporting (summary printing), violating single-responsibility. Applies ADR-001 -- fixing this now avoids accruing debt.
- Fix: Extract the summary-printing block (lines 450-478) into a dedicated `log_ast_summary(table: &AstWeightTable)` helper function, similar to the existing `log_validation_summary` for the lexical pipeline. Reuse the error-rate computation from `ast_validate::run_ast_validation` or a shared helper to avoid the duplication.

**`extract_ast_ngrams_from_corpus` is 105 lines with mixed concerns** - `ast_extract.rs:205-309`
**Confidence**: 82%
- Problem: At 105 lines, this function handles progress bar setup, file grouping by language, SHA-256 deduplication, per-file extraction with error handling, DF map accumulation, and stats collection. This is at the upper boundary of the complexity guideline (>50 lines = HIGH). The function is still readable due to clear sequential structure, but the mix of I/O (progress bar), data processing (dedup + extraction), and aggregation (stats collection) makes it harder to unit test each concern independently.
- Fix: Extract the inner per-language processing loop (lines 241-298) into a helper function like `extract_language_ngrams(lang_files, vocab, collect_trigrams) -> (HashMap<AstBigram,u32>, HashMap<AstTrigram,u32>, AstLanguageStats)`. This would reduce the corpus function to ~40 lines of orchestration and make per-language extraction independently testable.

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_tree` has 5 parameters** - `ast_extract.rs:132-137`
**Confidence**: 82%
- Problem: The function takes 5 parameters (`cursor`, `ctx`, `depth`, `parent_id`, `grandparent_id`). While `WalkContext` already consolidates several related fields (a good pattern), `depth`, `parent_id`, and `grandparent_id` are traversal state that could be folded into the context struct to bring the parameter count to 2.
- Fix: Add `depth`, `parent_id`, and `grandparent_id` fields to `WalkContext`. This reduces the call site at line 183 from `walk_tree(cursor, ctx, depth + 1, current_id, parent_id)` to `walk_tree(cursor, ctx)` with ctx updated before each recursive call. However, since the recursive nature requires these to be stack-local (not heap-stored), the current approach is defensible for a recursive walker. Consider this a "should-fix" rather than blocking.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`main.rs` is 570 lines total across two parallel pipelines** - `main.rs`
**Confidence**: 80%
- Problem: The file now contains two full command pipelines (lexical: `cmd_run`/`cmd_codegen`/`cmd_validate` and AST: `cmd_ast_run`/`cmd_ast_codegen`/`cmd_ast_validate`) plus shared utilities. While each function is well-factored, the file-level complexity is approaching the 500-line warning threshold (currently 570 lines). This is informational since the pre-existing lexical pipeline contributes ~50% of the file.
- Fix: Consider extracting AST subcommand handlers into a separate `cmd_ast.rs` module in a future refactoring.

## Suggestions (Lower Confidence)

- **Error-rate calculation duplicated between `main.rs:465-473` and `ast_validate.rs:65-77`** - `main.rs:465`, `ast_validate.rs:65` (Confidence: 75%) -- The same `error_node_count as f32 / total_node_count as f32` pattern is computed in two places. A shared `fn error_node_rate(stats: &AstLanguageStats) -> f32` would centralize this.

- **`write_bigram_lookup_fn` and `write_trigram_lookup_fn` are structurally identical** - `ast_codegen.rs:263-328` (Confidence: 70%) -- These two functions differ only in type names (`u32` vs `u64`, "bigram" vs "trigram"). A generic approach or macro could reduce the 66 lines to a single parameterized function, but the duplication is acceptable for generated-code writers where explicitness aids debugging.

- **`validate_ast_table` repeats the same IDF validation loop for bigrams and trigrams** - `ast_codegen.rs:55-90` (Confidence: 65%) -- The two loops at lines 63-74 and 76-87 are identical except for the key size format specifier. Could be unified with a helper, but the current explicitness is reasonable for a 36-line function.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 1 | 0 |

**Complexity Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The codebase is well-structured overall -- the module decomposition across 5 new files (`ast_types`, `ast_extract`, `ast_idf`, `ast_codegen`, `ast_validate`) follows single-responsibility principles effectively. The `WalkContext` pattern for bundling recursive traversal state is a good complexity-reduction technique. Constants for bounds (`MAX_AST_DEPTH`, `MAX_AST_NODES`, `MAX_FILE_SIZE`, `MAX_TRIGRAMS_PER_FILE`) are well-named and documented. Nesting depth stays at 4 across all functions, which is within acceptable range.

The two HIGH findings center on `cmd_ast_run` (108 lines with duplicated error-rate logic) and `extract_ast_ngrams_from_corpus` (105 lines mixing concerns). Both are addressable by extracting focused helper functions without changing behavior. Avoids PF-002 -- surfacing all findings for resolution rather than deferring.
