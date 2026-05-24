# Architecture Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

## Issues in Your Changes (BLOCKING)

### HIGH

**`sweep_parameter` has 9 parameters (exceeds cognitive complexity threshold)** - `crates/rskim-bench/src/tuning.rs:54`
**Confidence**: 82%
- Problem: The extracted `sweep_parameter` function takes 9 arguments (acknowledged by the `#[allow(clippy::too_many_arguments)]` annotation). While extraction from `coordinate_descent` is the correct DRY refactor, 9 parameters is a design smell per Clean Code (Martin, 2008) and ISP. A struct would encapsulate the mutation context (`current`, `current_mrr`, `history`, `pass`) that is threaded identically through every call site.
- Fix: Group the first four parameters into a `SweepState` struct:
```rust
struct SweepState {
    current: BM25FConfig,
    current_mrr: f64,
    history: Vec<ConvergenceStep>,
    pass: usize,
}
```
Then `sweep_parameter` takes `&mut SweepState, param_name, candidates, get_value, make_candidate, evaluate` -- 6 parameters, well within the cognitive limit.

**`field_display_name` duplicates `SearchField` variant-to-string mapping** - `crates/rskim-bench/src/report.rs:32-43`
**Confidence**: 85%
- Problem: This function manually maps every `SearchField` variant to a PascalCase string. `SearchField` already has a `name()` method (returning `snake_case` like `"type_definition"`) and exhaustive match enforcement. Adding a second exhaustive match in a downstream crate means two locations must be updated when a `SearchField` variant is added -- a tight coupling to the upstream enum's representation without leveraging its existing API.
- Fix: Either (a) add a `display_name()` method to `SearchField` in `rskim-search` that returns PascalCase (single source of truth), or (b) derive the PascalCase form programmatically from `SearchField::name()` in the bench crate (convert snake_case to PascalCase via a small utility). Option (a) is preferred for compile-time enforcement.

### MEDIUM

**`run_tune` in `main.rs` is a 120-line orchestration function with mixed responsibilities** - `crates/rskim-bench/src/main.rs:339-457`
**Confidence**: 80%
- Problem: `run_tune` handles: parallel repo loading, sequential FileId reassignment, index building, qrel generation, error tracking setup, coordinate descent invocation, final evaluation, result formatting, and output. This is at least 4 distinct responsibilities (data loading, tuning orchestration, evaluation, reporting), making it the de facto "god function" for tuning. The `build_index` and `make_train_qrels` extractions (good) stop short of completing the decomposition.
- Fix: Extract the parallel-load-and-reassign block (lines 342-372) into a `load_all_repos` function that returns `(Vec<IndexedFile>, HashMap<FileId, String>)`. Extract the final-evaluation-and-format block (lines 421-456) into a `run_final_eval_and_report` function. This brings `run_tune` down to ~40 lines of high-level orchestration.

**`LoadedRepo` struct defined in `main.rs` but used by multiple orchestration functions** - `crates/rskim-bench/src/main.rs:164-168`
**Confidence**: 81%
- Problem: `LoadedRepo` is a shared data transfer type used by `load_repo_files`, `run_bench`, `run_tune`, and `run_qrels`. Defining it in the binary crate (`main.rs`) rather than the library (`types.rs`) means it cannot be used by integration tests or other consumers of `rskim_bench`. This is the same pattern as `IndexedFile` which correctly lives in `types.rs`.
- Fix: Move `LoadedRepo` to `crates/rskim-bench/src/types.rs` alongside `IndexedFile`. This also enables integration tests to use `load_repo_files` directly.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`open_corpus` returns `Box<dyn FileSource>` but always constructs `GitCloneSource`** - `crates/rskim-bench/src/main.rs:147-161`
**Confidence**: 80%
- Problem: The function signature was improved from concrete `GitCloneSource` to `Box<dyn FileSource>` (good for DIP), but the body always constructs a `GitCloneSource`. The trait object allocation has no current benefit since the function has no injection point for alternative sources. For this to follow DIP properly, the source construction should be parameterized (e.g., accept a factory or an already-constructed source) so tests can inject `FixtureSource` without cloning repos.
- Fix: Accept `Option<Box<dyn FileSource>>` or separate the config loading from source construction:
```rust
fn open_corpus(
    corpus_config: Option<PathBuf>,
    corpus_dir: &Path,
    source: Option<Box<dyn FileSource>>,
) -> anyhow::Result<(CorpusConfig, Box<dyn FileSource>)> {
    // ...
    let source = source.unwrap_or_else(|| Box::new(GitCloneSource { ... }));
    Ok((corpus, source))
}
```

## Pre-existing Issues (Not Blocking)

_No critical pre-existing architecture issues identified in files touched by this PR._

## Suggestions (Lower Confidence)

- **`run_bench` parallelism uses `par_iter` with `eprintln!` for progress** - `crates/rskim-bench/src/main.rs:257` (Confidence: 65%) -- Interleaved `eprintln!` from parallel threads may produce garbled output. A progress callback or a collected-then-printed summary would be cleaner, but the current approach is functional for a benchmarking tool.

- **`load_repo_files` accepts `id_offset` but `run_bench` always passes `0`** - `crates/rskim-bench/src/main.rs:258` (Confidence: 70%) -- The `id_offset` parameter is only meaningful in `run_tune` (where IDs are later reassigned anyway). In `run_bench`, each repo gets its own index, so overlapping FileIds from offset=0 are harmless. However, the parameter's existence suggests a globally-unique ID design that is not actually enforced, which could confuse future maintainers.

- **Error swallowing in tune evaluation closure** - `crates/rskim-bench/src/main.rs:393-401` (Confidence: 72%) -- The closure returns `0.0` on error, which is a valid MRR value. This conflates "search returned nothing relevant" with "evaluation failed." The capped-at-5 logging is good but the error could silently steer tuning toward bad configs if errors are systematic for certain parameter combinations.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR introduces a well-structured new crate with good separation between harness, tuning, metrics, qrel generation, extraction, and reporting. The Strategy Pattern for language extractors (via `walk_ast` + closure visitors) is a clean improvement over the duplicated boilerplate. The use of `FIELD_COUNT` constant eliminates magic-number coupling. Key DIP improvements (returning `Box<dyn FileSource>`, injecting evaluator as closure into `coordinate_descent`) demonstrate solid architecture awareness.

Conditions for merge:
1. Address the `sweep_parameter` 9-parameter issue (HIGH) -- extract a `SweepState` struct
2. Address the `field_display_name` duplication (HIGH) -- consolidate the variant-to-string mapping to a single source of truth
