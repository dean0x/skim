# Consistency Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03 18:34
**Cycle**: 2 (cycle-1 fixes excluded)

## Scope

`extract.rs` + `extract_tests.rs` (new), plus test-style migration across
`lexical/*`, `temporal/*`, `index/reader_tests.rs`, `ast_index/ngram_tests.rs`.
Compared against sibling modules `linearize.rs` and `ngram.rs`, and the
`ast-index` KNOWLEDGE.md conventions.

## Issues in Your Changes (BLOCKING)

None. `extract.rs` is strongly consistent with its siblings:

- Section banner style (`// ===…=== / // Public types`) matches `linearize.rs`
  and `ngram.rs` exactly.
- `#[must_use]` on both public extraction fns, matching the `#[must_use]`
  convention on `ngram.rs` encode/key/idf functions.
- Derive ordering `#[derive(Debug, Clone, Copy, PartialEq)]` on `*Entry`
  structs matches `LinearNode`'s derive style; `AstNgramSet` adds `Default`
  consistent with `LinearizeResult`.
- Import grouping (std, external crate `rskim_core`, `super::`, `crate::`)
  matches `linearize.rs` ordering.
- `Result<T>` alias: not applicable here — both new fns are infallible and
  return `AstNgramSet` directly, which is correct (no fallible path to wrap).
  This matches the KNOWLEDGE note that extraction is pure with no error path.
- Naming (`AstNgramSet` / `AstBigramEntry` / `AstTrigramEntry`, fields
  `ngram`/`weight`/`count`) matches the documented entry-type convention and
  the PR description verbatim. DI split (`extract_ast_ngrams_with_weights` core
  + `extract_ast_ngrams` production wrapper) follows the sibling convention.
- Re-export ordering in `lib.rs` and `mod.rs` keeps the alphabetized,
  type-then-fn grouping already in place.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Incomplete struct-update migration in `temporal/scoring_tests.rs`** —
`crates/rskim-search/src/lexical/scoring_tests.rs:3` (allow header) and the
`let mut cfg = BM25FConfig::default(); cfg.k1 = …; cfg.field_boosts = …;`
blocks (e.g. lines ~121-126, ~143-145, ~184-186)
**Confidence**: 82%
- Problem: This PR establishes a consistent test-style migration — replace
  `let mut x = T::default(); x.field = v;` with struct-update syntax
  `T { field: v, ..T::default() }`. It was applied across
  `config_tests.rs`, `query_tests.rs`, `index/reader_tests.rs`. Those files
  consequently no longer need the `clippy::field_reassign_with_default` allow.
  `lexical/scoring_tests.rs` is the lone holdout: it *adds*
  `clippy::field_reassign_with_default` to its allow header (line 3) and keeps
  the old `cfg.k1 = …; cfg.field_boosts = …; cfg.field_b = …` whole-field
  reassignment pattern that the other files converted. These are full-field
  assignments (not array-index mutation), so they are convertible — unlike the
  array-element cases (`cfg.field_b[2] = …`) that were correctly left as-is in
  `config_tests.rs`/`reader_tests.rs` and even annotated with the explanatory
  comment `// array element mutation — struct-update syntax doesn't apply here`.
- Impact: One file diverges from the convention the rest of the PR enforces;
  future readers won't know whether the holdout was intentional. The added
  lint allow is the inverse direction from the rest of the PR (which removes
  the need for it). Minor — no behavior change — but it muddies the otherwise
  clean migration.
- Fix: Convert the full-field reassignment blocks in `scoring_tests.rs` to
  struct-update syntax and drop `clippy::field_reassign_with_default` from the
  allow header, matching `config_tests.rs`. Where a test sets multiple fields
  at once, struct-update still applies:
  ```rust
  let cfg = BM25FConfig {
      k1: 0.0,
      field_boosts: [0.0; FIELD_COUNT],
      field_b: [0.0; FIELD_COUNT], // no length normalisation
      ..BM25FConfig::default()
  };
  ```
  Leave any genuine `cfg.field_boosts[i] = …` index mutations untouched (they
  correctly need neither change nor the allow). Applies ADR-001 (fix noticed
  inconsistencies immediately rather than deferring).

## Pre-existing Issues (Not Blocking)

None observed in the reviewed scope.

## Suggestions (Lower Confidence)

- **`extract_tests.rs` allow header omits `clippy::panic`** -
  `crates/rskim-search/src/ast_index/extract_tests.rs:6` (Confidence: 68%) —
  `ngram_tests.rs`, `classifier_tests.rs`, and `query_tests.rs` added
  `clippy::panic` to their allow headers this PR; `extract_tests.rs` and
  `linearize_tests.rs` keep only `unwrap_used, expect_used`. This is fine *if*
  `extract_tests.rs` contains no `panic!`/`unreachable!` (the B2 characterization
  test described in KNOWLEDGE may or may not use one). Verify the test file does
  not rely on a bare `panic!`; if it does, align the header with its siblings.

- **`node.depth as usize` cast in `extract.rs:152`** -
  `crates/rskim-search/src/ast_index/extract.rs:152` (Confidence: 62%) —
  Elsewhere the codebase prefers `usize::from(node.depth)` for widening
  conversions (used three lines later at :162, and throughout `linearize.rs`).
  Line 152 uses the `as usize` cast form instead. Both are sound for `u16→usize`,
  but the `usize::from` form is the established idiom in these modules. Minor
  stylistic drift; a linter would not flag a widening `as` cast.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 9
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `extract.rs` code is exemplary in matching sibling-module conventions
(naming, DI split, derives, banners, doc style, `#[must_use]`, re-export
ordering). The only genuine consistency gap is the incomplete test-style
migration in `lexical/scoring_tests.rs`, which still uses the
field-reassign-after-default pattern (plus its lint allow) that the rest of the
PR converts to struct-update syntax. Resolving that one file makes the
migration uniform.
