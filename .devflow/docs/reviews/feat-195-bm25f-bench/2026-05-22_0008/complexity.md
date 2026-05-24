# Complexity Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

## Issues in Your Changes (BLOCKING)

### HIGH

**`sweep_parameter` has 9 parameters (exceeds 5-parameter threshold)** - `crates/rskim-bench/src/tuning.rs:54`
**Confidence**: 85%
- Problem: The `sweep_parameter` function accepts 9 parameters (`current`, `current_mrr`, `history`, `pass`, `param_name`, `candidates`, `get_value`, `make_candidate`, `evaluate`). This exceeds the 5-parameter guideline and was already flagged by clippy (suppressed with `#[allow(clippy::too_many_arguments)]`). While the function consolidates duplicated sweep logic (a net positive), the parameter count creates a wide call signature that's harder to read and maintain.
- Fix: Group the mutable state into a struct:
  ```rust
  struct SweepState<'a> {
      current: &'a mut BM25FConfig,
      current_mrr: &'a mut f64,
      history: &'a mut Vec<ConvergenceStep>,
      pass: usize,
  }
  ```
  This reduces the parameter count from 9 to 6 (`state`, `param_name`, `candidates`, `get_value`, `make_candidate`, `evaluate`) and eliminates the clippy suppression.

### MEDIUM

**`run_tune` is 118 lines (lines 339-457), approaching maintainability threshold** - `crates/rskim-bench/src/main.rs:339`
**Confidence**: 82%
- Problem: `run_tune` orchestrates parallel loading, ID reassignment, index building, qrel generation, coordinate descent, final evaluation, and output formatting. At 118 lines it is not critical (under 200), but the function handles 7 distinct responsibilities, making it harder to understand at a glance. The PR already extracted `build_index` and `make_train_qrels` (good decomposition), but the ID reassignment loop (lines 360-372) and the evaluation closure setup (lines 386-403) could also benefit from extraction.
- Fix: Extract the ID reassignment loop into a `merge_loaded_repos` helper:
  ```rust
  fn merge_loaded_repos(
      loaded_repos: Vec<LoadedRepo>,
  ) -> anyhow::Result<(Vec<IndexedFile>, HashMap<FileId, String>)> {
      // ... ID reassignment logic from lines 356-372 ...
  }
  ```
  This would bring `run_tune` below 100 lines and give each responsibility a clear name.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`main.rs` is 520 lines (exceeds 500-line warning threshold)** - `crates/rskim-bench/src/main.rs`
**Confidence**: 82%
- Problem: The file contains CLI argument structs, 4 subcommand handlers, and 3 helper functions. At 520 lines it crosses the 500-line warning threshold. This is a pre-existing structural issue that the PR's changes made slightly larger (adding `LoadedRepo`, `load_repo_files`, `build_index`, `make_train_qrels`). The helpers are well-decomposed individually, but the file itself is accumulating breadth.
- Fix: Consider splitting into `cli.rs` (argument structs + Command enum) and `main.rs` (dispatch + handlers) in a future PR.

**`integration.rs` is 561 lines (exceeds 500-line warning threshold)** - `crates/rskim-bench/tests/integration.rs`
**Confidence**: 80%
- Problem: The integration test file grew to 561 lines with the addition of 3 new tests (items 11, 12, 13). Each test is individually clear and well-commented, but the file is accumulating many concerns. This is not blocking since test files are naturally longer.
- Fix: No immediate action needed. If the file continues growing, consider splitting by test category (pipeline tests, extraction tests, validation tests).

## Suggestions (Lower Confidence)

- **`field_display_name` could use `SearchField::ALL` or `Display` trait** - `crates/rskim-bench/src/report.rs:32` (Confidence: 65%) -- The match covers all 8 variants manually; if `SearchField` already implements `Display` or has a `name()` method, this function duplicates that. If not, consider implementing `Display` on `SearchField` upstream to avoid drift when variants are added.

- **`walk_nodes` uses unbounded recursion** - `crates/rskim-bench/src/extract/mod.rs:90` (Confidence: 70%) -- The recursive tree walk has no depth limit. For typical source files this is safe (AST depth < 100), but pathologically nested files could overflow the stack. tree-sitter itself handles this for most grammars, so the practical risk is low.

- **Repeated `repo_name` extraction pattern across functions** - `crates/rskim-bench/src/main.rs:184,243,255,347,481` (Confidence: 62%) -- The pattern `repo_entry.url.rsplit('/').next().unwrap_or("unknown")` appears 5 times. A small helper `fn repo_name(entry: &RepoEntry) -> &str` would reduce duplication and make the intent clearer.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 2 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The codebase exhibits strong decomposition practices overall. The PR actively reduces complexity by extracting the `walk_ast` helper (eliminating triplicated parser boilerplate across Go/Python/Rust extractors), extracting `sweep_parameter` (removing duplicated sweep logic from `coordinate_descent`), and decomposing `build_index` and `make_train_qrels` from `run_tune`. The main blocking item is the 9-parameter `sweep_parameter` signature which, while an improvement over the inlined duplication it replaced, could be further refined with a state struct. All functions have clear single-level control flow, nesting stays within 3 levels, and cyclomatic complexity is low throughout. The changes represent a net complexity reduction.
