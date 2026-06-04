# Regression Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03 12:29
**Focus**: Regression (lost functionality, broken behavior, intent-vs-reality, incomplete migration)

## Verdict Summary

This PR is genuinely additive at the API surface and the clippy-driven edits to
pre-existing test files are **semantically equivalent** — no assertion was weakened,
no test semantics changed. Build compiles cleanly and the full `rskim-search` suite
passes (548 lib + 3 integration, 0 failed). No regressions found.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None.

## Pre-existing Issues (Not Blocking)

None observed within the touched files.

## Verification Performed

### 1. Re-exports — no shadowing or symbol conflict (verified)
`mod.rs` and `lib.rs` only **add** new symbols to existing `pub use` lists:
`AstBigramEntry`, `AstNgramSet`, `AstTrigramEntry`, `extract_ast_ngrams`,
`extract_ast_ngrams_with_weights`. No existing export was removed or renamed
(no `^-` removals of prior symbols in the `pub use` diff — only reflow + additions).
- Confirmed the struct names `AstNgramSet`/`AstBigramEntry`/`AstTrigramEntry` are
  defined only in `crates/rskim-search/src/ast_index/extract.rs`. The sibling crate
  `rskim-research/src/ast_extract.rs` exposes differently-named functions
  (`extract_ast_ngrams_from_file`, `extract_ast_ngrams_from_corpus`) in a separate
  crate — no collision, no shadowing.
- Lost-functionality check (`git diff | grep "^-export"` equivalent for Rust `pub`):
  zero public items removed.

### 2. Gap-fill slice panic-safety (recent commit 30f6838) — safe (verified)
`ancestors[usize::from(p + 1)..d]` at extract.rs:142:
- `ancestors` is sized `max_depth + 1`, where `max_depth = max(n.depth)` over all
  input nodes, so upper bound `d = node.depth <= max_depth < ancestors.len()` — in range.
- The slice is only reached under `node.depth > p + 1`, so `p + 1 < d` — lower <= upper,
  non-panicking range.
- `p + 1` overflow: `p` is `u16`; depth is bounded at `DEFAULT_MAX_DEPTH = 500` by the
  upstream `AstWalkConfig`, so `p + 1` cannot wrap.
- Parent/grandparent resolution uses `checked_sub(1)`/`checked_sub(2)` + `ancestors.get(..)`,
  so depth-0 and depth-1 nodes cannot underflow or index out of bounds.
  No panic path for empty input, depth-0 nodes, max depth, or large depth jumps.

### 3. Clippy test-file fixes — behavior preserved (verified, all pass)
Each modified pre-existing test file was checked for assertion weakening:

| File | Lint fixed | Change | Semantics |
|------|-----------|--------|-----------|
| temporal/scoring_tests.rs | manual_range_contains | `w >= 0.0 && w <= 1.0` → `(0.0..=1.0_f64).contains(&w)` | Equivalent; NaN handled by preceding `is_finite()` guard, so range-on-NaN behavior is unreachable |
| temporal/storage_tests.rs | cloned_ref_to_slice_refs | `&[row.clone()]` → `std::slice::from_ref(&row)` | Same single-element slice contents; `row` stays available for later `assert_eq!` |
| index/reader_tests.rs | field_reassign_with_default | `let mut c = Default(); c.k1 = X;` → struct-update `BM25FConfig { k1: X, ..default() }` | Identical field values; array-element mutations correctly left as-is (struct-update can't target array indices) |
| lexical/config_tests.rs | field_reassign_with_default | struct-update refactor | Identical field values |
| lexical/query_tests.rs | field_reassign_with_default + panic allow | struct-update refactor (4 tests) | Identical; all still assert `InvalidQuery` + "k1" message |
| lexical/scoring_tests.rs | field_reassign_with_default | `#![allow(...)]` added | Lint suppression only, no logic change |
| lexical/classifier_tests.rs | single_match + panic | `match { Err(FileTooLarge) => panic!; _ => {} }` → `if let Err(FileTooLarge) = result { panic! }` | Exactly equivalent: panics only on FileTooLarge, accepts Ok/parser-error |
| ast_index/ngram_tests.rs | panic | added `clippy::panic` to allow list | Lint suppression only |

All assertions remain at the same strength (no `assert!` downgraded to a print, no
expected variant loosened, no case dropped from coverage). `avoids PF-002` — none of
these were dismissed; each was individually traced.

### 4. Intent vs reality — matches (verified)
PR claims "pure additive change", DI core + production wrapper, single-allocation
ancestor table, `(ngram, weight, count)` contract. The code at extract.rs matches:
`extract_ast_ngrams_with_weights` (DI) + `extract_ast_ngrams` (wrapper), one
`vec![None; max_depth+1]` allocation, entries carry `weight` and `count`. No partial
implementation or missing-edge-case gap relative to the stated intent. The documented
residual gap-fill divergence (same-depth-sibling spurious edge) is acknowledged in
KNOWLEDGE.md and confined to malformed regions — not a regression.

### 5. Build + test gate (verified — avoids PF-003)
- `cargo test -p rskim-search --no-run` → `build-finished success:true`, zero errors.
- `cargo test -p rskim-search` → **548 passed; 0 failed; 1 ignored** (lib) and
  **3 passed; 0 failed; 3 ignored** (integration). Clippy-modified tests and the 18 new
  extract tests all green.

## Suggestions (Lower Confidence)

None.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 10
**Recommendation**: APPROVED

The change is regression-safe: additive public API with no removed/renamed exports,
no signature changes to existing functions, panic-safe gap-fill slicing, and clippy
test refactors that preserve assertion semantics. Full suite passes.
