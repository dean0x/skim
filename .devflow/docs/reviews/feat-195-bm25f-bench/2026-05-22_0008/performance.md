# Performance Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**PathBuf::clone() inside per-node visitor closures (3 occurrences)** -- Confidence: 85%
- `crates/rskim-bench/src/extract/go.rs:29`, `crates/rskim-bench/src/extract/python.rs:28`, `crates/rskim-bench/src/extract/rust_lang.rs:31`
- Problem: Each language extractor clones a `PathBuf` inside the `move` closure on every matching node (function, type, import). For files with many symbols, this produces one heap allocation per extracted symbol. The previous code did `path.to_path_buf()` at the call site but passed `&Path` into the recursive walker, avoiding clones in the hot loop. The refactored version calls `path.clone()` inside the visitor because the closure captured the owned `PathBuf`.
- Fix: Change `ExtractedSymbol.file_path` to borrow from the caller (`&'a Path`) or use an `Arc<PathBuf>` so the clone is a ref-count bump rather than a heap allocation. Alternatively, clone once before the closure and move an `Rc<PathBuf>` in:

```rust
let path = Rc::new(path.to_path_buf());
super::walk_ast(
    content,
    tree_sitter_rust::LANGUAGE.into(),
    move |node, bytes, symbols| {
        // path.clone() is now an Rc bump, not a heap alloc
        ...
    },
)
```

This matters because `extract_symbols` is called for every file in the corpus, and each file may have dozens of symbols.

### MEDIUM

**New tree-sitter Parser allocation per file in `walk_ast`** -- Confidence: 82%
- `crates/rskim-bench/src/extract/mod.rs:82`
- Problem: The `walk_ast` convenience function creates a new `tree_sitter::Parser` for every call. In `generate_qrels`, this means one parser allocation per file in the corpus. The crate already provides `walk_ast_with_parser` which accepts a pre-existing parser, but the language-specific extractors (`go::extract`, `python::extract`, `rust_lang::extract`) all use `walk_ast` instead, and `generate_qrels` loops over files calling `extract_symbols` one at a time.
- Fix: This is acceptable for the current bench harness where file counts are in the low hundreds and parser creation is fast (~microseconds). However, `walk_ast_with_parser` exists but is unused -- consider using it in `extract_symbols` by accepting an optional parser, or create the parser once per language group within `generate_qrels`. No immediate action required since the bottleneck is index building and searching, not parsing.

**`format!("boost[{field_idx}]")` called in inner loop of coordinate descent** -- Confidence: 80%
- `crates/rskim-bench/src/tuning.rs:129`
- Problem: Inside the coordinate descent loop (up to MAX_PASSES=3 iterations x FIELD_COUNT=8 fields), `sweep_parameter` is called with `&format!("boost[{field_idx}]")` and `&format!("b[{field_idx}]")`. Each call allocates a `String` on the heap. The format string is only used when an improvement is found (to record in `history`), but the allocation happens unconditionally for every field sweep.
- Fix: Pre-compute the parameter names before entering the sweep loop:

```rust
let boost_names: Vec<String> = (0..FIELD_COUNT)
    .map(|i| format!("boost[{i}]"))
    .collect();
// Then pass &boost_names[field_idx] to sweep_parameter
```

This is a micro-optimization (24 small allocations per pass, 72 total) and LOW impact in practice since the evaluate closure dominates runtime. Noting for completeness.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`sym.name.clone()` in `generate_qrels` Phase 1 hot loop** -- Confidence: 82%
- `crates/rskim-bench/src/qrel.rs:73-76`
- Problem: For every extracted symbol that passes the filter, `sym.name.clone()` is called to insert into `df_map`, and the `(file_id, sym)` pair is pushed to `raw_symbols`. This is two clones of the name string per symbol (one for df_map key, one implicit in the sym struct). For large corpora with thousands of symbols, this accumulates.
- Fix: This is the standard trade-off for using owned strings in a HashMap. The previous code had the same pattern (it filtered after collecting, which was actually worse -- it cloned even symbols that would be filtered out). The current code is an improvement because it filters before pushing. No action needed; noting for context.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Sequential ID reassignment in `run_tune`** - `crates/rskim-bench/src/main.rs:360-372` (Confidence: 65%) -- The parallel file loading followed by sequential ID reassignment is correct, but the drain/remove pattern on `LoadedRepo` forces a linear scan of the HashMap for each file. For very large corpora (thousands of files per repo), using `drain()` on `lr.contents` after the indexed loop would be more cache-friendly. Current corpus sizes make this negligible.

- **`qrel_inputs` Vec allocation in `run_on_files` and `make_train_qrels`** - `crates/rskim-bench/src/harness.rs:46-54`, `crates/rskim-bench/src/main.rs:320-328` (Confidence: 60%) -- Both functions create an intermediate `Vec<QrelInput>` that borrows content from the `contents` HashMap. This is fine for the current scale but could be replaced with an iterator adapter if allocation pressure becomes visible.

- **Coordinate descent evaluations per pass could be reduced** - `crates/rskim-bench/src/tuning.rs:106-170` (Confidence: 70%) -- Each pass evaluates 6 (k1) + 8*9 (boosts) + 2*5 (b) = 88 candidate configs. With 3 max passes, that is up to 264 full corpus evaluations. The current design is intentional (simple greedy search) and the `evaluate` closure dominates runtime. A line search or golden section approach could halve evaluations, but the simplicity trade-off is reasonable for a tuning harness.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Performance Changes

1. **Single-reader pattern** (`harness.rs:83`, `main.rs:383`): Opening the `NgramIndexReader` once and overriding BM25F config per-query via `SearchQuery::bm25f_config` eliminates redundant index open/mmap operations per config. This is a significant improvement over the previous `open_with_config` per-config approach.

2. **Parallel repo processing** (`main.rs:252-270`): `run_bench` now uses `par_iter()` on filtered repos, which parallelizes the fetch-index-evaluate pipeline per repo. Each repo gets its own `tempdir` so there is no contention on index files. Well done.

3. **Parallel file loading in `run_tune`** (`main.rs:343-352`): File I/O for all repos is parallelized with rayon, with sequential ID reassignment afterward. This correctly avoids ID collisions while still benefiting from parallel I/O.

4. **`QrelInput` now borrows content** (`qrel.rs:50`): Changed from `content: String` to `content: &'a str`, eliminating one clone per file in `run_on_files` and `make_train_qrels`. Good zero-copy improvement.

5. **Merged filter-then-push in `generate_qrels`** (`qrel.rs:71-77`): Symbols are now filtered in the same loop as extraction and pushed only if they pass, eliminating the previous two-pass pattern (collect all, then filter). Reduces allocations for discarded symbols.

6. **Fixed-size arrays for TuningResult** (`types.rs:85-86`): Changed `Vec<f32>` to `[f32; FIELD_COUNT]` for `best_field_boosts` and `best_field_b`. This eliminates heap allocation for these small arrays and enables `Copy` semantics.

### Conditions

The HIGH-severity PathBuf clone issue (3 occurrences across extractors) should be addressed before merge if the corpus will scale significantly. For the current corpus size (a handful of repos), it is tolerable but introduces unnecessary allocation in the hottest extraction loop.
