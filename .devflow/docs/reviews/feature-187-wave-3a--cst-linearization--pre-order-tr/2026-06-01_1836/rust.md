# Rust Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Prior Resolutions**: Cycle 2 resolved 9/11 issues, 0 FP, 0 deferred. This is Cycle 3.

## Issues in Your Changes (BLOCKING)

No blocking issues found.

## Issues in Code You Touched (Should Fix)

No should-fix issues found.

## Pre-existing Issues (Not Blocking)

No pre-existing issues found.

## Suggestions (Lower Confidence)

- **Consider `#[inline]` on `AstWalkIter::skip_subtree` and `AstWalkIter::advance`** - `crates/rskim-core/src/ast_walk.rs:161,184` (Confidence: 65%) -- These small private methods are called in the hot iterator loop. While LLVM likely inlines them anyway due to the single call-site heuristic, explicit `#[inline]` would guarantee it across codegen units if the module structure changes. Low priority since benchmarks already meet targets.

- **`linearize_tree` allocates a new `Parser` on every call via `linearize_source`** - `crates/rskim-search/src/ast_index/linearize.rs:211` (Confidence: 70%) -- Each call to `linearize_source` creates a fresh `Parser::new(language)`. For batch indexing scenarios where thousands of files of the same language are linearized, a caller-owned parser pool would avoid repeated grammar loading. However, Parser::new may be cheap enough that this is immaterial, and the current API is simpler. Would require benchmarking to confirm materiality.

- **Test `linearize_1000_line_file_under_10ms` uses wall-clock timing** - `crates/rskim-search/src/ast_index/linearize_tests.rs:428` (Confidence: 60%) -- Wall-clock assertions in CI can flake under load. The `#[cfg(not(debug_assertions))]` guard helps, and Criterion benchmarks cover the same ground more rigorously, so this is a minor concern. Consider adding a wider margin (e.g., 50ms) if CI flakes are observed.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### Ownership and Borrowing

The code demonstrates excellent ownership discipline throughout:

- `AstWalkIter` holds a `TreeCursor<'a>` with proper lifetime binding to the `Tree` -- the borrow checker enforces the tree outlives the iterator. No cloning or unnecessary ownership transfers.
- `linearize_tree` borrows `&tree_sitter::Tree` and `&[Option<u16>]` -- zero unnecessary copies.
- `linearize_source` takes `&str` for source, following the `C-BORROW` API guideline.
- `AstWalkNode` borrows `Node<'a>` from the tree -- zero-copy node access.

### Error Handling

Consistent, well-structured error handling:

- `linearize_source` returns `Result<LinearizeResult>` using `thiserror`-derived `SearchError`.
- Graceful degradation: oversized files, non-tree-sitter languages, and parse failures return `Ok(default)` rather than errors -- correctly distinguishing file-level issues from configuration-level failures (`SearchError::Ast`).
- The `?` operator is used for grammar load failures. No `.unwrap()` or `.expect()` in production code (clippy `deny(unwrap_used, expect_used)` enforced at crate level).

### Type System Usage

Strong type-driven design:

- `LinearNode` is `Copy` (two `u16` fields) -- optimal for cache-line packing in `Vec<LinearNode>` sequences.
- `AstWalkConfig` uses associated constants (`DEFAULT_MAX_DEPTH`, `DEFAULT_MAX_NODES`) as the canonical source of truth, eliminating magic numbers.
- `AstWalkNode` encodes the error/missing distinction in a boolean field rather than a separate enum, appropriate since callers only need to filter, not match.
- Saturating casts (`u32 -> u16` via `.min(u32::from(u16::MAX)) as u16`) with explicit `#[allow(clippy::cast_possible_truncation)]` -- correct pattern per feature knowledge.

### Concurrency Safety

- `LinearNode` and `LinearizeResult` have `Send + Sync` verified via compile-time assertions in tests.
- `LANG_MAPS` uses `LazyLock<HashMap<Language, Vec<Option<u16>>>>` for thread-safe one-time initialization -- correct pattern for static data shared across threads.
- No `Mutex`, no shared mutable state in production code.

### Iterator Design

- `FusedIterator` impl on `AstWalkIter` is correct: the `done` flag is monotonic (set to `true`, never cleared).
- The iterator's inner loop in `next()` correctly re-checks bounds after skipping subtrees, preventing off-by-one at the boundary.
- `level_stack` pre-allocation with `(max_depth as usize).min(64)` balances memory efficiency against typical depth profiles.

### Bounds Guards (Reliability)

All loops have explicit upper bounds (applies ADR-001 -- all noticed issues fixed in prior cycles):

- `max_depth` and `max_nodes` in `AstWalkIter` prevent unbounded traversal.
- `MAX_FILE_SIZE` (100 KiB) prevents pathological memory allocation.
- `skip_subtree` and `advance` terminate because each iteration either finds a sibling (finite tree width) or pops from `level_stack` (finite stack depth), eventually exhausting the tree.
- The ancestor vector in `ast_extract.rs:walk_tree` grows lazily from 64 to actual depth, avoiding the 501-entry pre-allocation waste (avoids PF-002 -- not deferring known improvements).

### API Surface

- `linearize_source` is the sole public entry point, correctly gating internal complexity.
- Re-exports in `mod.rs` and `lib.rs` are minimal and well-curated: `LinearNode`, `LinearizeResult`, `linearize_source`.
- `#[non_exhaustive]` on `SearchError` allows future variant additions without breaking downstream (confirmed in types.rs).

### Refactoring Quality (ast_extract.rs)

The refactoring of `ast_extract.rs` to use `AstWalkIter` is clean:

- The entire hand-rolled `TreeCursor` loop was replaced by `AstWalkIter::new(tree.walk(), AstWalkConfig::default())` -- eliminating ~80 lines of duplicated cursor management.
- Caller-specific logic (vocabulary lookup, bigram/trigram emission, ancestor tracking) remains in `walk_tree`, correctly separated from traversal mechanics.
- The `#[allow(clippy::collapsible_if)]` on the trigram guard is well-documented with a rationale in the function doc comment.

### Benchmark Quality

- Four benchmark groups cover languages, scaling, nesting depth, and init latency.
- `black_box` correctly prevents compiler elision of results.
- `gen_rust_fns` and `gen_rust_nested` fixtures are deterministic and parameterized.
- The `init_latency` benchmark correctly measures steady-state (post-LazyLock) access cost.
