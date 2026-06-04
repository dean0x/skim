# Performance Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### HIGH

**Parser re-created per call in `linearize_source` -- redundant grammar initialization in batch scenarios** - `crates/rskim-search/src/ast_index/linearize.rs:211`
**Confidence**: 82%
- Problem: `linearize_source` creates a new `Parser::new(language)` on every invocation (line 211). While `LANG_MAPS` is lazily initialized once, the parser itself is allocated and the grammar is loaded fresh each call. When linearizing a corpus of thousands of files (the intended use case for AST indexing), this repeats grammar loading per file. `Parser::new` sets the tree-sitter language on each construction, which involves internal FFI setup.
- Impact: In batch indexing of N files, N parser allocations occur instead of one per language. For the 14 supported languages across thousands of files, this adds measurable overhead. The existing `ast_extract.rs` has the same pattern, so this is consistent -- but the PR description states this is for indexing pipelines where batch performance matters.
- Fix: Accept an optional `&mut Parser` parameter, or provide a `linearize_source_with_parser` variant for batch callers. Alternatively, consider caching parsers per language in a thread-local. This is a design-level concern for when the batch indexing layer is built on top -- acceptable to defer if batch API is a separate PR.

### MEDIUM

**`linearize_source` benchmarks include parsing time, not just linearization** - `crates/rskim-search/benches/linearize_bench.rs:94-101`
**Confidence**: 85%
- Problem: The benchmark comment at line 94 says "Parsing happens in setup (outside b.iter()) -- benchmark linearization only", but this is incorrect. `linearize_source` calls `parser.parse(source)` internally (linearize.rs:216), so the `b.iter()` closure includes both parse time AND linearization time. The comment is misleading and the benchmark does not isolate linearization from parsing.
- Impact: Benchmark results will conflate parsing latency with the linearization traversal, making it impossible to attribute regressions. If tree-sitter parsing regresses, linearization benchmarks will appear to regress too, making root cause analysis harder.
- Fix: Either (a) update the comment to say "benchmarks end-to-end linearize_source including parsing" or (b) expose `linearize_tree` (or a test helper) to benchmark traversal separately from parsing. Option (a) is the minimal fix. Option (b) would provide more actionable performance data.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`ancestors` Vec in `walk_tree` resizes one element at a time** - `crates/rskim-research/src/ast_extract.rs:148-149`
**Confidence**: 83%
- Problem: When a node's depth exceeds the current `ancestors` Vec length, the code resizes to exactly `depth + 1` (line 149: `ancestors.resize(depth + 1, None)`). In a tree that gradually deepens (depth 64, then 65, then 66...), this triggers a reallocation on every new depth level until the Vec's capacity catches up via the allocator's doubling strategy. The initial capacity of 64 is well-chosen for typical files (depth 20-30), but pathological inputs that deepen gradually will hit repeated small reallocations.
- Impact: Minor in practice. The Vec's allocator doubles capacity on growth, so the total number of reallocations is O(log(max_depth)). With max_depth=500, that is at most ~3 reallocations beyond the initial 64. The comment at lines 133-136 already explains the design rationale. This is a low-impact concern given the bounds guards.
- Fix: No code change needed -- the current approach is the correct trade-off. The comment already documents the rationale. Raising this for completeness per ADR-001 (applies ADR-001).

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`LANG_MAPS` uses `HashMap<Language, ...>` instead of a flat array** - `crates/rskim-search/src/ast_index/linearize.rs:108` (Confidence: 65%) -- With only 14 languages and `Language` likely being a small enum, an array indexed by language discriminant would avoid HashMap hashing overhead on every `linearize_source` call. The HashMap lookup is O(1) amortized but involves hashing and comparison, whereas array indexing is a single bounds-checked load. This is a micro-optimization that matters only in very high-throughput batch scenarios.

- **`init_latency` benchmark measures parse + linearize, not LazyLock access** - `crates/rskim-search/benches/linearize_bench.rs:137-143` (Confidence: 72%) -- The "init_latency" benchmark claims to measure "steady-state cost of accessing [LANG_MAPS] (one atomic load)" but actually measures a full `linearize_source("")` call including Parser::new, parser.parse, and tree traversal. The atomic load for LazyLock access is nanoseconds; the benchmark is dominated by parser setup. To truly measure LazyLock access cost, directly access `LANG_MAPS.get(&Language::Rust)` without going through `linearize_source`.

- **Benchmark scaling group re-generates source strings on each benchmark parameter** - `crates/rskim-search/benches/linearize_bench.rs:92-102` (Confidence: 62%) -- The `gen_rust_fns(n_fns)` call at line 93 runs inside the parameter loop but outside `b.iter()`, which is correct for Criterion. No issue here, just noting that the 1000-function fixture generates a ~55 KB string that is under the 100 KiB `MAX_FILE_SIZE` limit, so the benchmark correctly exercises the full path.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The performance design of this PR is strong:

1. **Data layout**: `LinearNode` is 4 bytes (two u16 fields), `Copy`, and `Send+Sync`. This is cache-line-friendly and minimizes allocation pressure for the output Vec.

2. **O(1) vocabulary lookup**: `LANG_MAPS` builds per-language `Vec<Option<u16>>` indexed by tree-sitter `kind_id`, giving array-indexed O(1) lookup during traversal instead of string hashing. The one-time binary search cost (1740 entries) at `LazyLock` init is amortized across all subsequent calls.

3. **Bounded allocations**: `Vec::with_capacity(descendant_count.min(MAX_AST_NODES))` pre-sizes the output Vec using tree-sitter's node count, capped to prevent pathological over-allocation. The `ancestors` Vec in `ast_extract` uses lazy growth from 64 entries.

4. **Bounds guards**: `AstWalkConfig::DEFAULT_MAX_DEPTH` (500) and `DEFAULT_MAX_NODES` (100,000) prevent runaway traversal. The `MAX_FILE_SIZE` (100 KiB) guard rejects oversized files before parsing.

5. **Iterator design**: `AstWalkIter` uses an explicit `level_stack` with `Vec::with_capacity(min(max_depth, 64))`, avoiding the previous approach of re-pushing `(depth, parent_id, grandparent_id)` tuples. The `FusedIterator` implementation enables stdlib optimizations.

6. **Comprehensive benchmarks**: Four Criterion groups cover per-language, scaling, nesting depth, and init latency -- matching the project's benchmark culture.

The one blocking HIGH finding (parser re-creation per call) is a design consideration for batch performance that may be acceptable to defer to the indexing pipeline PR. The misleading benchmark comment should be fixed. Overall, performance discipline is excellent -- well-documented trade-offs, appropriate bounds, and cache-friendly data structures.
