# Consistency Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Inconsistent path type usage: `std::path::PathBuf` vs imported `PathBuf`** - `crates/rskim-research/src/clone.rs:82`, `crates/rskim-research/src/config.rs:85`
**Confidence**: 95%
- Problem: The existing `GitCloneSource` struct at line 62 uses the imported `PathBuf` (imported at line 6 via `use std::path::{Path, PathBuf};`). The new `AstGitCloneSource` struct at line 82 uses the fully-qualified `std::path::PathBuf` instead. Similarly, `load_corpus_config` at line 61 takes `path: &Path` (using the import), while the new `load_ast_corpus_config` at line 85 takes `path: &std::path::Path` (fully qualified). Both are semantically identical but stylistically inconsistent within the same file.
- Fix: Use the already-imported `PathBuf` and `Path` types in the new code to match the existing pattern:
```rust
// clone.rs:82 — change from:
pub corpus_dir: std::path::PathBuf,
// to:
pub corpus_dir: PathBuf,

// config.rs:85 — change from:
pub fn load_ast_corpus_config(path: &std::path::Path) -> anyhow::Result<CorpusConfig> {
// to:
pub fn load_ast_corpus_config(path: &Path) -> anyhow::Result<CorpusConfig> {
```
- Applies ADR-001 (fix all noticed issues immediately).

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new AST pipeline modules (ast_types, ast_extract, ast_idf, ast_codegen, ast_validate) demonstrate excellent consistency with the existing lexical pipeline patterns. Specifically:

- **Naming conventions**: All new modules follow the `ast_` prefix pattern consistently. Type names (`AstBigramWeight`, `AstTrigramWeight`, `AstWeightTable`, `AstCorpusStats`, `AstLanguageStats`) mirror the existing naming scheme (`BigramWeight`, `WeightTable`, `CorpusStats`).
- **Error handling**: Consistent use of `anyhow::Result` with `.with_context()` throughout, matching existing modules.
- **`#[must_use]`**: Applied consistently on pure functions, matching the pattern in `extract.rs`, `idf.rs`, and `validate.rs`.
- **Test structure**: All test modules follow the existing pattern: `#![allow(clippy::unwrap_used)]`, `use super::*;`, helper functions like `make_file()` and `sample_table()`, and descriptive test names.
- **CLI pattern**: The new `AstRun`, `AstCodegen`, and `AstValidate` commands perfectly mirror the structure of `Run`, `Codegen`, and `Validate` (same arg patterns, same dispatch style).
- **Function signatures**: `cmd_ast_run`, `cmd_ast_codegen`, `cmd_ast_validate` follow the same signature and implementation patterns as their lexical counterparts.
- **Code generation**: `ast_codegen.rs` follows the same `build_*_rs` / `write_*` decomposition as `codegen.rs`.
- **Validation**: `ast_validate.rs` follows the same report-struct + `run_*_validation` + `print_*_report` pattern as `validate.rs`.
- **Refactoring quality**: The extraction of `fetch_files_parallel` from the old `fetch_all_files` is clean and the old function now delegates properly.

The only consistency issue found is a minor stylistic inconsistency with path type imports (2 occurrences across 2 files), which should be a quick fix.
