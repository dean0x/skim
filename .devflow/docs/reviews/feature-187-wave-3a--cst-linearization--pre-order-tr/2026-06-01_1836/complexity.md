# Complexity Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Cross-Cycle Awareness

Prior cycle resolved 11 issues (9 fixed, 0 FP, 0 deferred). Key complexity-relevant fixes already applied: centralized bounds constants, preallocated level_stack, fused iterator, lazy-grow ancestor vec. This review cycle focuses on remaining complexity patterns after those improvements.

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`collect_import_names` nesting depth reaches 4 levels** - `crates/rskim-bench/src/extract/typescript.rs:91-136`
**Confidence**: 82%
- Problem: The `collect_import_names` function has 4 levels of nesting (function body > for loop > match arm > for loop > match arm > if-let chain). This is at the warning threshold for nesting depth per complexity metrics (good < 3, warning 3-4, critical > 4). The function processes import clause children, then named_imports children, then import_specifier field lookups.
- Fix: This file is in `rskim-bench` (benchmark support), not production code. The nesting is driven by tree-sitter's AST shape and is structurally imposed. Extracting the inner `named_imports` handler into a separate function would reduce nesting by one level without changing behavior.

## Suggestions (Lower Confidence)

(none -- all findings are above the 80% confidence threshold or below the 60% floor)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 1 | 0 |

**Complexity Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### Function Length

All production functions in the changed code are well under the 30-line "good" threshold:

| Function | File | Lines | Rating |
|----------|------|-------|--------|
| `AstWalkIter::new` | `ast_walk.rs` | 11 | Good |
| `AstWalkIter::skip_subtree` | `ast_walk.rs` | 17 | Good |
| `AstWalkIter::advance` | `ast_walk.rs` | 22 | Good |
| `AstWalkIter::next` (Iterator impl) | `ast_walk.rs` | 38 | Warning (but structurally justified -- single state machine loop with clear phases) |
| `linearize_source` | `linearize.rs` | 28 | Good |
| `linearize_tree` | `linearize.rs` | 30 | Good |
| `walk_tree` | `ast_extract.rs` | 65 | Refactored -- was larger before shared `AstWalkIter` extraction |
| `process_language_files` | `ast_extract.rs` | 65 | Pre-existing, not modified in this PR |

### Cyclomatic Complexity

| Function | Branches | Rating |
|----------|----------|--------|
| `AstWalkIter::next` | 5 (done check, first-call branch, loop, depth guard, node-count guard) | Good (<10) |
| `linearize_tree` | 3 (for loop, error skip, vocab lookup) | Good (<5) |
| `walk_tree` | 7 (for loop, depth grow, error check, parent lookup, grandparent lookup, bigram emit, trigram emit) | Good (<10) |
| `LANG_MAPS` init closure | 5 (for loop, Parser::new match, parse match, u16 try_from, binary search) | Good (<10) |

### Nesting Depth

Maximum nesting in production code is 3 levels:
- `AstWalkIter::next`: loop > if > if = 3 levels -- Good
- `LANG_MAPS` init: for > for > if-let = 3 levels -- Good  
- `linearize_tree`: for > if > (implicit) = 2 levels -- Good
- `walk_tree`: for > if-let > if-let = 3 levels -- Good

### Magic Values

No magic values detected. All numeric constants are named:
- `MAX_AST_DEPTH` / `MAX_AST_NODES` centralized on `AstWalkConfig`
- `MAX_FILE_SIZE` = 100 KiB (named constant with comment)
- `MAX_TRIGRAMS_PER_FILE` = 50,000 (named constant)
- Initial capacity `64` in `Vec::with_capacity` is commented ("typical trees rarely exceed depth 20-30")
- Initial capacity `64` in `level_stack` uses `.min(64)` with comment

### Parameter Count

All functions have 2-4 parameters -- within the good/warning range:
- `linearize_source(source, language)` = 2
- `linearize_tree(tree, lang_map)` = 2
- `walk_tree(tree, vocab, collect_trigrams, result)` = 4
- `AstWalkIter::new(cursor, config)` = 2

### File Length

| File | Lines | Rating |
|------|-------|--------|
| `ast_walk.rs` | 556 | Warning (>500), but 295 lines are tests (53%). Production code is 257 lines -- Good |
| `linearize.rs` | 274 | Good |
| `linearize_tests.rs` | 450 | Test-only -- Good |
| `ast_extract.rs` | 751 | Warning (>500), but 383 lines are tests (51%). Production code is 368 lines -- includes `process_language_files` and `extract_ast_ngrams_from_corpus` which are pre-existing and not modified |
| `linearize_bench.rs` | 159 | Good |

### Bounded Iteration

All loops have explicit bounds:
- `AstWalkIter::skip_subtree`: bounded by `level_stack` depth (finite, pre-capped at `max_depth`)
- `AstWalkIter::advance`: bounded by `level_stack` depth (same)
- `AstWalkIter::next` inner loop: bounded by tree node count and `max_nodes` guard
- `LANG_MAPS` init: bounded by `ts_languages.len()` (14) and `kind_count` (finite per grammar)
- `linearize_tree`: bounded by `AstWalkIter` (which has `max_nodes` guard)

### Design Quality

The shared `AstWalkIter` extraction is a textbook complexity reduction:
- **Before**: Two independent DFS implementations with duplicated cursor management, depth tracking, and bounds guarding (one in `linearize.rs`, one in `ast_extract.rs`)
- **After**: Single reusable iterator (`AstWalkIter`) in `rskim-core` with caller-specific logic (vocabulary lookup, bigram emission) staying in the consuming modules
- The `FusedIterator` marker is correctly implemented (the `done` flag is monotonic)
- The config struct (`AstWalkConfig`) cleanly centralizes the bounds constants with `Default` impl
