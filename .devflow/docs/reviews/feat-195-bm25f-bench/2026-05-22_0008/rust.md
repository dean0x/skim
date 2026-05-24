# Rust Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`sweep_parameter` `from_value` records stale value across multi-step sweeps** - `crates/rskim-bench/src/tuning.rs:67`
**Confidence**: 82%
- Problem: `from_value` is captured once at the start of `sweep_parameter` (line 67: `let from_value = get_value(current) as f64`). If a candidate improves MRR and mutates `current`, subsequent candidates in the same sweep record the original `from_value` rather than the intermediate value. This means the convergence trace may show `from_value` that does not match the actual pre-improvement value for the 2nd, 3rd, etc. improvements within a single sweep call. For a diagnostic trace this is a minor data accuracy issue, but could be misleading when analyzing tuning behavior.
- Fix: Move `from_value` capture inside the improvement branch so it always reflects the actual prior state:
```rust
fn sweep_parameter<G>(
    current: &mut BM25FConfig,
    current_mrr: &mut f64,
    history: &mut Vec<ConvergenceStep>,
    pass: usize,
    param_name: &str,
    candidates: &[f32],
    get_value: impl Fn(&BM25FConfig) -> f32,
    make_candidate: impl Fn(&BM25FConfig, f32) -> BM25FConfig,
    evaluate: &mut G,
) where
    G: FnMut(BM25FConfig) -> f64,
{
    for &val in candidates {
        let candidate = make_candidate(current, val);
        if candidate.validate().is_err() {
            continue;
        }
        let candidate_mrr = evaluate(candidate);
        if candidate_mrr > *current_mrr {
            let from_value = get_value(current) as f64; // capture just before mutation
            history.push(ConvergenceStep {
                pass,
                parameter: param_name.to_string(),
                from_value,
                to_value: val as f64,
                mrr_improvement: candidate_mrr - *current_mrr,
            });
            *current = candidate;
            *current_mrr = candidate_mrr;
        }
    }
}
```

### MEDIUM

**`walk_ast_with_parser` is dead code** - `crates/rskim-bench/src/extract/mod.rs:45`
**Confidence**: 90%
- Problem: `walk_ast_with_parser` is declared `pub(crate)` but is only called by `walk_ast` on line 86 of the same module. No other module calls it directly. While the doc comment describes parser reuse for processing many files, no caller currently exercises that pattern. This adds API surface that is tested only indirectly and may mislead future maintainers into thinking it is actively used.
- Fix: Either (a) make it private (`fn walk_ast_with_parser`) since `walk_ast` is the sole caller in the same module, or (b) if parser reuse is planned, add a `#[cfg(test)]` test that exercises it directly to validate the contract.

**`path.clone()` on every node visit in extractors** - `crates/rskim-bench/src/extract/rust_lang.rs:31`, `go.rs:29`, `python.rs:28`
**Confidence**: 85%
- Problem: Each extractor clones the `PathBuf` on every symbol push (`path.clone()`). For files with many symbols this creates unnecessary allocations. The `path` variable is a `PathBuf` captured by `move` into the closure; every clone allocates a new heap buffer.
- Fix: Use `Arc<PathBuf>` or `Rc<PathBuf>` for zero-cost clones within the closure, or restructure `ExtractedSymbol` to take a `&Path` lifetime reference. Example with `Arc`:
```rust
pub fn extract(path: &Path, content: &str) -> Vec<ExtractedSymbol> {
    let path = std::sync::Arc::new(path.to_path_buf());
    super::walk_ast(
        content,
        tree_sitter_rust::LANGUAGE.into(),
        move |node, bytes, symbols| {
            // ... use (*path).clone() which is still PathBuf::clone,
            // or change ExtractedSymbol.file_path to Arc<PathBuf>
        },
    )
}
```
Note: For the benchmark crate (not a hot path), this is a "should consider" rather than a hard requirement.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_nodes` recursion has no depth bound** - `crates/rskim-bench/src/extract/mod.rs:90-111`
**Confidence**: 80%
- Problem: The recursive `walk_nodes` function has no explicit depth limit. For deeply nested ASTs (e.g., deeply nested expressions, macro-generated code), this could overflow the stack. The project's reliability guidelines state: "Every loop, retry, and resource has an explicit bound." Tree-sitter ASTs for typical source files are shallow enough that this is unlikely in practice, but the function processes arbitrary user-provided code corpora.
- Fix: Add a depth parameter with a reasonable ceiling (e.g., 256):
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
    if depth > 256 { return; }
    visit(node, bytes, symbols);
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            walk_nodes(child, cursor, bytes, symbols, visit, depth + 1);
            if !cursor.goto_next_sibling() { break; }
        }
        cursor.goto_parent();
    }
}
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`find_last_identifier` is also unbounded recursive** - `crates/rskim-bench/src/extract/rust_lang.rs:86-115`
**Confidence**: 80%
- Problem: Same unbounded recursion pattern as `walk_nodes`. `find_last_identifier` recurses into child nodes to find the deepest identifier. Deeply nested `use` paths (unlikely but possible with macros) could overflow. This was not changed in this PR but exists in the same module.

## Suggestions (Lower Confidence)

- **`LoadedRepo` could use `#[must_use]` on construction helpers** - `crates/rskim-bench/src/main.rs:179` (Confidence: 65%) -- `load_repo_files` returns a `Result<(LoadedRepo, u32)>` which already forces handling via `?`, but the `LoadedRepo` struct itself could benefit from `#[must_use]` to catch accidental discards.

- **Phase numbering gap in qrel.rs comments** - `crates/rskim-bench/src/qrel.rs:81` (Confidence: 70%) -- Comments jump from "Phase 1" (line 63) to "Phase 3" (line 81), skipping "Phase 2". The old Phase 2 (filter) was merged into Phase 1, but the comment numbering was not updated. Minor readability issue.

- **`field_display_name` could use `SearchField::name()` method** - `crates/rskim-bench/src/report.rs:32-43` (Confidence: 72%) -- `SearchField` already has a `name()` method returning snake_case strings. The manually maintained PascalCase mapping in `field_display_name` is a second source of truth that could drift if variants are added to `SearchField`. Consider deriving from `SearchField::name()` or using `Display` impl if one exists.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong Rust patterns throughout: proper `Result` propagation via `?` and `anyhow::Context`, borrowing over cloning (e.g., `QrelInput<'a>` using `&'a str` instead of `String`), `FIELD_COUNT` constant replacing magic `8` literals, `#[deny(clippy::unwrap_used)]` in non-test code, and clean trait-based abstraction (`FileSource`, `SearchLayer`). The `sweep_parameter` refactoring is a good extraction that eliminates three copies of the same sweep logic. The parallel processing with rayon is correctly implemented given `FileSource: Send + Sync`.

The one HIGH-severity issue is the `from_value` recording inaccuracy in the convergence trace, which affects diagnostic output quality. The MEDIUM items are about defensive depth bounds and a small dead-code surface. None are merge-blocking in isolation, but fixing the `from_value` bug before merge would improve the trustworthiness of tuning diagnostics.
