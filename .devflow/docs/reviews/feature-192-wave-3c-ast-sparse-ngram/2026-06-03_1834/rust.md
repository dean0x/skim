# Rust Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**Date**: 2026-06-03 18:34
**Scope**: PR #269 — `crates/rskim-search/src/ast_index/extract.rs` (new) + `extract_tests.rs` (new)
**Cycle**: 2 (cycle-1 fixes verified, only NEW issues raised)

## Cycle-1 Verification (all hold)

- Let-chains present at lines 159–161, 191–194, 206–210 — no stale `#[allow(clippy::collapsible_if)]` remain.
- PF-004 u16 widening applied: gap-fill uses `u32::from(node.depth) > u32::from(p) + 1` (line 160). Regression-locked by tests B1 (`u16_max_depth_no_panic_no_spurious_null`, `two_nodes_at_u16_max_depth_no_panic`).
- parent/grandparent resolved from `node.depth` directly via `checked_sub(1)/(2)` (lines 179–187) — no round-trip through a recomputed `d`.
- `debug_assert!` lines wrapped within the 100-char rustfmt limit.
- `cargo clippy -p rskim-search --all-targets -- -D warnings` → 0 warnings, 0 errors.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None at >=80% confidence.

## Pre-existing Issues (Not Blocking)

None.

## Suggestions (Lower Confidence)

- **Inconsistent integer cast style** - `crates/rskim-search/src/ast_index/extract.rs:152` (Confidence: 70%) — `let d = node.depth as usize;` is the only `as` cast in the file; every other u16→usize conversion uses the infallible `usize::from(...)` (lines 130, 137, 162, 182, 187). `as` for widening is harmless here but breaks the file's own convention and is the idiom clippy's `cast_lossless`/style lints discourage. Replace with `usize::from(node.depth)` for consistency.

- **`d` is only consumed by `debug_assert!`** - `crates/rskim-search/src/ast_index/extract.rs:152,164,176` (Confidence: 65%) — `d` is referenced solely inside two `debug_assert!` macros. In release builds the asserts compile out, so the binding exists only to feed debug-only checks. This is intentional (the asserts document depth-bounds invariants per the reliability rule) and does not warn, but a reader may wonder why `d` exists when parent/gp resolution uses `node.depth` directly. Optional: inline `usize::from(node.depth)` into the assert sites, or add a one-line comment that `d` is the debug-assert capture, to make the intent explicit.

## Assessment Notes (positive — no action)

Reviewed against the rust skill checklist and the focus areas requested:

- **Ownership/borrowing**: `nodes: &[LinearNode]` borrowed (not owned), `LinearNode` is `Copy` so `.copied().flatten()` is zero-cost. No `.clone()`-to-satisfy-borrow-checker. Input immutability is test-locked (C3 `input_slice_unmodified`).
- **DI closure bounds**: `impl Fn(AstBigram) -> f32` / `impl Fn(AstTrigram) -> f32` are the right bound — `Fn` (not `FnMut`/`FnOnce`) correctly signals the closures are pure and called repeatedly without mutation. Matches the documented contract and the project's DI convention.
- **Result/Option handling**: `checked_sub(1)/(2)` + `.and_then(|pd| ancestors.get(usize::from(pd)).copied().flatten())` is the idiomatic underflow-safe parent/gp resolution — no manual bounds math, no panic path. Depth-0 underflow guarded and test-locked (B5).
- **Newtype pattern**: keys flow through `AstBigram`/`AstTrigram` newtypes (`#[repr(transparent)]`, `Ord`); maps are keyed by the newtype, not raw integers — illegal-state-unrepresentable applied.
- **Integer cast safety**: the only widening cast (line 152) is u16→usize which is infallible on all supported targets; no truncating/lossy casts in the diff. The depth-saturation truncating cast lives in `linearize.rs`, outside this PR.
- **Collection/sort idioms**: `HashMap` accumulation → `into_iter().map(...).collect()` → `sort_unstable_by_key(|e| e.ngram.key())` is idiomatic and gives deterministic output despite HashMap iteration order (locked by C1 sorted/unique and C2 determinism tests). Capacity cap `nodes.len().min(1024)` is a reasonable allocation bound.
- **`#[must_use]`** present on both public entry points.
- **Error handling**: pure function, no fallible I/O — `Result` correctly not used; no `.unwrap()`/`.expect()` in library code (test-only, gated by `#![allow(clippy::unwrap_used, clippy::expect_used)]`).
- **Reliability rule**: the single `for node in nodes` loop is bounded by the slice; gap-fill `for slot in &mut ancestors[fill_start..d]` is bounded by the table; production-code invariants asserted via `debug_assert!` (depth-bounds, gap-fill non-empty range). `applies ADR-001` — the P1 flaky wall-clock assertion was removed and replaced with a correctness smoke test rather than silently capping coverage.
- **Test quality**: behavior-focused (assert emitted edges, counts, sort order, determinism), no implementation coupling, no try/catch-around-expectation patterns. `bigram_keys` helper removes boilerplate. B2 characterizes the documented residual gap-fill divergence so any silent change fails loudly.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED

The two suggestions are sub-80% style/cleanliness items, not defects. The module is idiomatic, clippy-clean, well-bounded, and thoroughly tested. Cycle-1 fixes are intact.
