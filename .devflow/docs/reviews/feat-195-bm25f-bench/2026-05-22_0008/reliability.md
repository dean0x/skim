# Reliability Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

## Issues in Your Changes (BLOCKING)

### HIGH

**Recursive `walk_nodes` has no depth bound** - `crates/rskim-bench/src/extract/mod.rs:90-111`
**Confidence**: 85%
- Problem: The `walk_nodes` function recurses on every child node of the tree-sitter AST with no depth limit. While tree-sitter ASTs are typically bounded by the source file's nesting depth, pathological or maliciously crafted source files (deeply nested expressions, chains of binary operators, heavily nested modules) could blow the stack. The corpus is fetched from external git repos, so the input is not fully trusted.
- Fix: Add a `max_depth` parameter with a reasonable upper bound (e.g., 256). This matches the NASA/JPL "Power of Ten" rule that every recursion must have a fixed upper bound.
```rust
fn walk_nodes<F>(
    node: tree_sitter::Node<'_>,
    cursor: &mut tree_sitter::TreeCursor<'_>,
    bytes: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    visit: &mut F,
    depth: usize,
) where
    F: FnMut(tree_sitter::Node<'_>, &[u8], &mut Vec<ExtractedSymbol>),
{
    const MAX_DEPTH: usize = 256;
    if depth >= MAX_DEPTH {
        return;
    }
    visit(node, bytes, symbols);

    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            walk_nodes(child, cursor, bytes, symbols, visit, depth + 1);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
```

**Tuning error counter silent saturation with no hard failure** - `crates/rskim-bench/src/main.rs:386-410`
**Confidence**: 82%
- Problem: The evaluation error counter uses `AtomicU32` and caps error logging at 5 messages. If every evaluation fails (e.g., the index is corrupt), `coordinate_descent` will run the full sweep (3 passes * ~30+ evaluations = ~90+ calls) with all evaluations returning 0.0 MRR, and only produce a warning at the end. The tuning result would be meaningless but no error is returned -- the function continues to the "final evaluation" step with a bogus config.
- Fix: After tuning completes, fail hard if all evaluations errored (total_errors exceeds a reasonable fraction of expected evaluations):
```rust
let total_errors = eval_error_count.load(Ordering::Relaxed);
if total_errors > 0 {
    eprintln!(
        "[tune] {total_errors} evaluation(s) failed and returned 0.0 MRR — results may be unreliable."
    );
}
// Fail hard if ALL evaluations appear broken (heuristic: if initial eval itself failed)
if tuning_result.best_train_mrr == 0.0 && total_errors > 0 {
    anyhow::bail!(
        "Tuning produced 0.0 MRR with {total_errors} evaluation error(s) — likely an index or reader problem"
    );
}
```

### MEDIUM

**`run_tune` ID reassignment silently drops files when `contents.remove` returns `None`** - `crates/rskim-bench/src/main.rs:360-372`
**Confidence**: 83%
- Problem: In the ID reassignment loop, if `lr.contents.remove(&old_id)` returns `None`, the `IndexedFile` is still pushed to `all_indexed` but with no corresponding content entry in `all_contents`. Downstream code (e.g., `build_index`, `make_train_qrels`) will treat missing content as `""` via `unwrap_or("")`, which means the file exists in the index with empty content -- a silent data integrity violation that would skew benchmark results.
- Fix: Either assert that removal always succeeds (it should, since both vecs were built from the same source) or log a warning:
```rust
if let Some(content) = lr.contents.remove(&old_id) {
    all_contents.insert(FileId(global_id), content);
} else {
    debug_assert!(false, "FileId {old_id:?} has no content -- data consistency violation");
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`top_two_boost_fields` assumes FIELD_COUNT >= 2 without assertion** - `crates/rskim-bench/src/tuning.rs:200-205`
**Confidence**: 80%
- Problem: The function unconditionally indexes `indexed[0].1` and `indexed[1].1` without verifying that `FIELD_COUNT >= 2`. While `FIELD_COUNT` is currently 8 (from `rskim_search`), this is an implicit assumption from an external crate. If `FIELD_COUNT` ever changed to 1 or 0, this would panic at runtime with an out-of-bounds index.
- Fix: Add a compile-time or runtime assertion:
```rust
fn top_two_boost_fields(boosts: &[f32; FIELD_COUNT]) -> [usize; 2] {
    const _: () = assert!(FIELD_COUNT >= 2, "FIELD_COUNT must be >= 2 for top_two_boost_fields");
    let mut indexed: [(f32, usize); FIELD_COUNT] = std::array::from_fn(|i| (boosts[i], i));
    indexed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    [indexed[0].1, indexed[1].1]
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`report.rs:field_display_name` not exhaustive-by-construction** - `crates/rskim-bench/src/report.rs:32-43` (Confidence: 70%) -- The `match` lists all 8 SearchField variants explicitly. If a new variant is added to `SearchField`, this function will fail to compile only if `SearchField` is `#[non_exhaustive]` or the match is truly exhaustive. Consider relying on a `Display` impl from the source crate or `strum` derive instead of a manual match.

- **`report.rs:tuning_section` indexes `best_field_boosts[i]` without bounds check** - `crates/rskim-bench/src/report.rs:70-74` (Confidence: 65%) -- The loop `for (i, field) in SearchField::ALL.iter().enumerate()` indexes into `t.best_field_boosts[i]` and `t.best_field_b[i]` which are `[f32; FIELD_COUNT]`. If `SearchField::ALL.len() != FIELD_COUNT` this would panic. Currently safe because both are derived from the same constant, but a `debug_assert_eq!` would make the invariant explicit.

- **`evaluate_split` swallows NaN from search results** - `crates/rskim-bench/src/harness.rs:156-158` (Confidence: 62%) -- If `reciprocal_rank` or `precision_at_k` returned NaN (e.g., due to a scoring bug in rskim-search), the NaN would silently propagate through the MRR average. The `debug_assert!` added to `mrr()` in `metrics.rs` partially mitigates this but only catches it at aggregation time, not at the per-query level.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability fundamentals: bounded iteration in `coordinate_descent` (MAX_PASSES=3), `checked_add` for FileId overflow prevention, proper `anyhow::Result` propagation throughout, validation guards on config names, and the debug_assert for MRR finiteness. The main concerns are the unbounded recursion in `walk_nodes` (which processes untrusted external repo content) and the silent degradation path in tuning where all evaluations can fail without halting the pipeline.
