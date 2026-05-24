---
focus: complexity
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Complexity Review

## Summary

The rskim-bench crate is well-decomposed into focused modules with clean separation of concerns. Two functions exceed the 50-line threshold significantly (`run_tune` at 134 lines and `coordinate_descent` at 109 lines), the three extractor modules share near-identical boilerplate that could be consolidated, and the `run_tune` function orchestrates too many distinct responsibilities. No critical complexity issues.

## Issues in Your Changes (BLOCKING)

### HIGH

**`run_tune` is a 134-line god function with 7+ distinct responsibilities** - `main.rs:220-353`
**Confidence**: 92%
- Problem: `run_tune` handles corpus loading, file collection with ID assignment, qrel input construction, qrel generation, index building, train-split filtering, coordinate descent invocation with an inline closure, tuning result conversion, final benchmark evaluation, and output formatting. This is at least 7 distinct phases in a single function. At 134 lines, it far exceeds the 50-line threshold and has a cyclomatic complexity around 8 (for-loops, match, conditionals in closure). The inline closure at line 291-306 captures state and contains its own error handling logic, adding cognitive load.
- Suggestion: Extract phases into named helpers. For example:
  - `load_all_repo_files(corpus, source) -> (Vec<IndexedFile>, HashMap<FileId, String>)` (lines 224-248) — this pattern is also duplicated in `run_bench` and `run_qrels`
  - `build_index(files, contents, dir) -> NgramIndexBuilder` (lines 265-278)
  - `run_tuning_evaluation(idx_path, train_qrels) -> TuningResult` (lines 289-306)
  - `run_final_comparison(indexed, contents, tuned_cfg) -> BenchResult` (lines 314-338)
  - `render_output(output_format, bench_result, tuning_result)` (lines 340-350) — also duplicated in `run_bench`

**`coordinate_descent` is a 109-line function with 4-level nesting and repetitive sweep blocks** - `tuning.rs:46-154`
**Confidence**: 88%
- Problem: The function contains three structurally identical "sweep" blocks (k1 sweep at lines 59-79, boost sweep at lines 82-107, b sweep at lines 111-136). Each follows the same pattern: iterate candidates, build config, validate, evaluate, compare, update if better. The nesting depth reaches 4 levels (pass loop > field loop > candidate loop > improvement check) in the boost and b sweeps. At 109 lines, it exceeds the 50-line threshold by more than 2x.
- Suggestion: Extract a generic `sweep_parameter` helper that takes a list of candidates, a config-builder closure, and the current best, returning the updated best. This would reduce the three sweep blocks to three one-line calls:
  ```rust
  fn sweep<F>(
      candidates: &[f32],
      current: &mut BM25FConfig,
      current_mrr: &mut f64,
      history: &mut Vec<ConvergenceStep>,
      pass: usize,
      param_name: &str,
      get_current: impl Fn(&BM25FConfig) -> f32,
      build_candidate: F,
      evaluate: &mut impl FnMut(BM25FConfig) -> f64,
  ) where F: Fn(&BM25FConfig, f32) -> BM25FConfig { ... }
  ```

### MEDIUM

**Duplicated file-loading and QrelInput construction across 3 CLI subcommands** - `main.rs:161-200`, `main.rs:228-259`, `main.rs:376-404`
**Confidence**: 85%
- Problem: `run_bench`, `run_tune`, and `run_qrels` each independently implement the pattern: iterate repos, call `fetch_files`, sort by path, construct `IndexedFile`/`QrelInput` with enumerated `FileId`s, and build a contents map. The three variants differ slightly in detail (per-repo vs. all-repos aggregation, different ID assignment strategies) but the core structure is duplicated. This is a maintainability risk — a change to file loading logic (e.g., adding a language filter) would need to be applied in three places.
- Suggestion: Extract a `load_corpus_files(corpus, source, repo_filter)` helper that returns `(Vec<IndexedFile>, HashMap<FileId, String>)` and optionally groups by repo. This would reduce each subcommand to a single call plus its specific evaluation logic.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Extractor boilerplate duplicated across 3 modules (go.rs, python.rs, rust_lang.rs)** - `extract/go.rs:16-35`, `extract/python.rs:15-34`, `extract/rust_lang.rs:18-37`
**Confidence**: 82%
- Problem: All three extractors share an identical `extract()` function body (19 lines): create parser, set language, parse, get bytes/root/cursor, call `walk_node`, return symbols. They also share identical `walk_node` recursion boilerplate (lines 76-86 in go.rs, 76-86 in python.rs, 100-109 in rust_lang.rs). That is approximately 30 lines duplicated verbatim across 3 files (90 lines total of identical code). When a fourth language extractor is added, this pattern will be copied again.
- Suggestion: Extract a shared `parse_and_walk` function in `extract/mod.rs` that takes the tree-sitter `Language` and a node-visitor closure. Each language module would then only supply the language constant and the match arms for its specific node kinds:
  ```rust
  pub fn parse_and_walk<F>(
      lang: tree_sitter::Language,
      path: &Path,
      content: &str,
      visit: F,
  ) -> Vec<ExtractedSymbol>
  where F: FnMut(Node, &[u8], &Path, &mut Vec<ExtractedSymbol>)
  ```

**`to_markdown` is 69 lines with string concatenation and hardcoded field names** - `report.rs:33-101`
**Confidence**: 80%
- Problem: The function builds a markdown string through 20+ `push_str` calls with inline formatting. The `field_names` array at line 82-91 hardcodes 8 field names that should probably come from the type system or a shared constant to stay in sync with `SearchField` variants. At 69 lines, the function is above the 50-line warning threshold. Each section (aggregate, per-repo, tuning, field boosts) could be a separate helper.
- Suggestion: Extract `render_tuning_section(t: &TuningResult) -> String` and `render_repo_section(repo: &RepoBenchResult) -> String` to bring each rendering function under 30 lines. Move the field name array to a `const` or derive it from the enum.

## Pre-existing Issues (Not Blocking)

(none - this is an entirely new crate)

## Suggestions (Lower Confidence)

- **`run_bench` at 78 lines straddles the 50-line threshold** - `main.rs:141-218` (Confidence: 65%) -- The function is linear and readable but does combine repo iteration, file loading, indexing, evaluation, and output in one place. If `run_tune` gets refactored to extract shared helpers, `run_bench` would naturally shrink as well.

- **`walk_node` functions use recursive tree traversal with cursor** - `extract/go.rs:37-86`, `extract/python.rs:36-86`, `extract/rust_lang.rs:39-109` (Confidence: 60%) -- While tree-sitter cursors are bounded by the AST depth (typically < 50 for source files), the recursive `walk_node` pattern has no explicit depth bound. For this benchmark context with known inputs this is acceptable, but it deviates from the project's reliability principle of explicit bounds on recursion.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The crate has a sound modular structure -- types, metrics, split, configs, qrel, harness, tuning, report, and extractors are each well-scoped single-responsibility modules. The `metrics.rs`, `split.rs`, `configs.rs`, and `types.rs` modules are exemplary in simplicity. However, two functions (`run_tune` at 134 lines, `coordinate_descent` at 109 lines) need decomposition, and the triple-duplicated extractor boilerplate and file-loading patterns present maintainability risks. These are not blocking for correctness but will make the crate harder to extend when adding new languages or tuning strategies.
