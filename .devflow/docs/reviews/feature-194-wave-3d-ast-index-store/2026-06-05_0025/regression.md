# Regression Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05 00:25

## Scope

Reviewed for regression risk against the 4 modified source files in the diff (the
`store/` submodule and benches are net-new and carry low regression risk). Specific
concerns evaluated: `lang_map` visibility widening, re-export collisions in
`lib.rs`/`ast_index/mod.rs`, the `rayon` dependency addition, signature changes to
existing public functions, and the 632-test claim.

## Issues in Your Changes (BLOCKING)

None. All changes to existing files are strictly additive.

## Issues in Code You Touched (Should Fix)

None.

## Pre-existing Issues (Not Blocking)

None observed in the touched lines.

## Verification Detail

**1. `lang_map` widening (`index/mod.rs:32`, `mod` -> `pub(crate) mod`)** — Confidence: 97%
- No regression. The only existing cross-module consumer is
  `index/format.rs:30: pub(crate) use super::lang_map::lang_to_id;` — a sibling within
  the same `index` parent module, which never required `pub(crate)` to compile. The
  `lang_map` identifiers in `ast_index/linearize.rs` are unrelated local variables, not
  the module. Widening only adds an access path (for the new `store/` submodule); it
  removes nothing and narrows nothing. Honors ADR-001 (single source of truth, no
  duplication) without breaking encapsulation any code relied on.

**2. `lib.rs` re-export extension (`lib.rs:35-40`)** — Confidence: 96%
- No regression, no collision. All 13 pre-existing `ast_index` exports are preserved;
  4 names added (`AstFileMetaEntry`, `AstIndexBuilder`, `AstIndexReader`, `AstPosting`).
  The lexical index re-exports `NgramIndexBuilder`/`NgramIndexReader` (lib.rs:42) — the
  new `AstIndex*` names use a distinct prefix and do not shadow them. No name appears
  twice across the `pub use` surface.

**3. `ast_index/mod.rs` re-exports (`+pub mod store` + 4 re-exports)** — Confidence: 96%
- Purely additive. Existing `extract_ast_ngrams` / `extract_ast_ngrams_with_weights`
  public signatures (`extract.rs:117`, `extract.rs:258`) are unchanged and not in the
  diff. `AstNgramSet` API is untouched. The `store/` module does not alter the existing
  extract/linearize APIs — it only consumes `AstNgramSet` as input.

**4. `rayon` dependency add (`rskim-search/Cargo.toml:28`, `Cargo.lock:2386`)** — Confidence: 95%
- Low risk. `rayon` is already a pinned workspace dependency (`Cargo.toml:35`,
  `rayon = "1.10"`) and already consumed by `rskim`, `rskim-bench`, and `rskim-research`.
  Adding `{ workspace = true }` to `rskim-search` introduces no new version and no new
  transitive subtree — Cargo.lock gains a single dependency-list line under the
  `rskim-search` package entry. No dependency-tree risk.

**5. No signature changes / no removed exports / no removed tests** — Confidence: 97%
- `git diff main...HEAD` shows 0 removed `#[test]` attributes and 0 removed test
  functions; 74 added `#[test]` attributes, all in net-new `store/*_tests.rs` files.
  No existing source function changed signature (no `-` lines in existing function defs).

**6. 632-test claim** — Confidence: 95%
- Minor undercount, not a regression. `cargo test -p rskim-search` reports 636 passed,
  0 failed, 4 skipped. The PR description states 632. The delta is the newly added store
  tests plus the A16 ignore adjustment from commit 0ee9de4; all pre-existing tests remain
  present and green. No test was removed, disabled, or weakened.

## Suggestions (Lower Confidence)

- Update the PR description / commit body test count from 632 to the current 636 passing
  for accuracy (Confidence: 70%) — documentation drift only, no functional impact.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 10
**Recommendation**: APPROVED

All existing behavior is preserved. Every change to a pre-existing file is additive
(visibility widening, export extension). No exports removed, no signatures changed, no
defaults altered, no side effects removed, no tests deleted, and the dependency tree is
unchanged beyond a single already-pinned workspace crate. Cross-cycle awareness: no prior
resolutions supplied.
