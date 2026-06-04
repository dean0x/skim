# Consistency Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Corpus config comment style inconsistency between corpus.toml and ast-corpus.toml** - `crates/rskim-research/ast-corpus.toml:15-17`
**Confidence**: 85%
- Problem: The existing `corpus.toml` uses `# ---- Rust ----` section header style (4 dashes, no box-drawing chars). The new `ast-corpus.toml` uses a much longer box-drawing style: `# ---...---` (65-char wide lines). While purely cosmetic, the two config files serve the same purpose (corpus repo lists) and sit in the same directory.
- Fix: Adopt the same section header style as `corpus.toml` for visual consistency:
```toml
# Before (ast-corpus.toml):
# -----------------------------------------------------------------
# Rust (5 repos -- reused from corpus.toml)
# -----------------------------------------------------------------

# After (matching corpus.toml):
# ---- Rust (5 repos -- reused from corpus.toml) ----
```

**`version` field type inconsistency: `u8` in AST vs `u8` in lexical (correct) but different validation behavior** - `crates/rskim-research/src/ast_codegen.rs:56`
**Confidence**: 82%
- Problem: Both `WeightTable.version` and `AstWeightTable.version` are `u8`, and both codegen paths validate `version > 0`. However, the lexical `codegen::generate_weights_rs` also validates `table.weights.is_empty()` returning a specific error ("weight table is empty"). The AST `validate_ast_table` validates `table.vocabulary.is_empty()` but does not check whether `bigram_weights` is empty. An AST table with a vocabulary but zero bigram weights would silently generate a Rust file with empty arrays. The lexical path rejects this.
- Fix: Add an empty-bigrams check in `validate_ast_table`:
```rust
if table.bigram_weights.is_empty() {
    anyhow::bail!("AST weight table has no bigram weights");
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`#[must_use]` annotation pattern gap in AST types vs lexical types** - `crates/rskim-research/src/ast_types.rs:146`, `crates/rskim-research/src/ast_types.rs:186-194`
**Confidence**: 82%
- Problem: The `NodeKindVocabulary` methods `new()`, `len()`, `is_empty()`, and `kinds()` correctly have `#[must_use]`. However, `get_or_insert()` (line 151) lacks `#[must_use]` despite returning a `NodeKindId` that callers always need. The lexical pipeline's `encode_bigram` and `decode_bigram` in `extract.rs` consistently apply `#[must_use]`. The AST encode/decode functions correctly follow this pattern, but `get_or_insert` breaks it.
- Fix: Add `#[must_use]` to `get_or_insert`:
```rust
/// Return the ID for `kind`, inserting a new entry if not yet present.
#[must_use]
pub fn get_or_insert(&mut self, kind: &str) -> NodeKindId {
```
- Note: Actually `get_or_insert` has a side effect (insertion), so `#[must_use]` may be debatable. The return value IS always used in practice, but the function is also legitimately called for its side effect. This is borderline -- the caller should decide.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Lexical `CorpusStats` uses `unique_bigrams: usize` but AST `AstLanguageStats` uses `unique_bigrams: usize` at the per-language level** - `crates/rskim-research/src/ast_types.rs:292` (Confidence: 65%) -- The stats nesting differs: lexical has a single flat `CorpusStats` with `unique_bigrams` at corpus level, while AST has `AstLanguageStats` with `unique_bigrams` per-language. This is intentional (the AST pipeline groups by language) but the asymmetry means aggregated corpus-level unique counts are absent from the AST output. Minor data model divergence.

- **`cmd_ast_validate` default path lookup inconsistency with `cmd_validate`** - `crates/rskim-research/src/main.rs:536-539` vs `crates/rskim-research/src/main.rs:349` (Confidence: 70%) -- `cmd_validate` uses `default_json_path()` directly, while `cmd_ast_validate` inlines an equivalent fallback pattern: `ast_codegen::default_ast_weights_json_path().unwrap_or_else(...)`. Both ultimately do the same thing (workspace root lookup with current-dir fallback), but the mechanism differs. Extracting a `default_ast_json_path()` helper to match the lexical pattern would improve structural symmetry.

- **Progress bar message pattern difference between lexical and AST** - `crates/rskim-research/src/main.rs:249` vs `crates/rskim-research/src/ast_extract.rs:342` (Confidence: 62%) -- In `fetch_files_parallel`, the progress message shows the repo URL. In `extract_ast_ngrams_from_corpus`, it shows the language name. Both are reasonable for their context (repo-level vs language-level iteration), but the inconsistency may confuse users running both pipelines.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The AST pipeline demonstrates strong consistency with the existing lexical pipeline. It follows the same module naming pattern (`ast_extract` mirrors `extract`, `ast_idf` mirrors `idf`, etc.), reuses the same IDF formula via `idf::compute_idf`, shares the `FileSource` trait, and follows the same error handling conventions (`anyhow::Result`, `.with_context()`). The `#[must_use]` annotations on encode/decode functions, `#[cfg(test)]` module conventions (`#![allow(clippy::unwrap_used)]`), and doc comment style are all consistent. The feature knowledge note that "AST pipeline mirrors the existing lexical bigram pipeline" is well-validated by the code.

The two blocking items are minor: a cosmetic config file style difference and a missing empty-table validation check that the lexical pipeline has. Neither represents a pattern violation that would cause runtime issues, but fixing them aligns the pipelines fully. Applies ADR-001 (fix noticed issues immediately). Avoids PF-002 (all findings surfaced, none classified as skippable).
