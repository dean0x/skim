# Regression Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**Date**: 2026-06-03 18:34
**PR**: #269 (Wave 3c — AST sparse n-gram extraction)
**Cycle**: 2 (cycle-1 fixes excluded; only NEW regression risks raised)

## Summary of Analysis

The change is **purely additive**, matching the PR description. The diff adds a new
`extract` submodule (`extract.rs`, `extract_tests.rs`), re-exports five new symbols, and
adds one Criterion bench group. Every other modified file is a clippy-conformance refactor
of existing tests. No regression risks at >=80% confidence were found.

### Export surface (verified — no regressions)

`mod.rs` and `lib.rs` only ADD symbols; nothing removed, nothing renamed, no signature
change to any pre-existing export:

- Added: `AstBigramEntry`, `AstNgramSet`, `AstTrigramEntry`, `extract_ast_ngrams`,
  `extract_ast_ngrams_with_weights`.
- All 12 pre-existing exports from the KNOWLEDGE.md Public API Surface table
  (`linearize_source`, `LinearNode`, `LinearizeResult`, `NodeKindId`, `AstBigram`,
  `AstTrigram`, `DEFAULT_AST_WEIGHT`, `vocab_lookup`, `vocab_resolve`, `vocab_len`,
  `ast_bigram_idf`, `ast_trigram_idf`) remain present and unchanged.
- The new public surface matches the table's documented Wave-3c rows exactly.

### Behavior alterations (verified — none)

No existing function bodies were modified. `extract.rs` is new code. The Node Count
Invariant and vocabulary-sorted invariant live in `linearize.rs`/`ngram.rs`, which were
not touched by this PR — those invariants are unaffected.

### Test modifications (verified — coverage preserved, NOT weakened)

Every modified `*_tests.rs` file outside `extract_tests.rs` is a clippy-driven refactor
that is semantically identical to the prior assertions:

| File | Change | Coverage impact |
|---|---|---|
| `reader_tests.rs` | `let mut cfg; cfg.k1 = x` → `BM25FConfig { k1: x, ..default() }` (3 sites); array-element mutations left as-is with explanatory comment | None — same configs, same `validate()`/score assertions retained |
| `config_tests.rs` | struct-update syntax (2 sites) | None — same assertions |
| `query_tests.rs` | struct-update syntax for bad configs (4 sites) + `clippy::panic` allow | None — all `InvalidQuery` + `k1` message assertions retained |
| `classifier_tests.rs` | `match { Err(..) => panic! , _ => {} }` → `if let Err(..) = .. { panic! }` + `clippy::panic` allow | None — identical panic-on-`FileTooLarge` semantics |
| `scoring_tests.rs` | added `field_reassign_with_default` allow only | None — no assertion change |
| `temporal/scoring_tests.rs` | `w >= 0.0 && w <= 1.0` → `(0.0..=1.0_f64).contains(&w)` (4 sites) | None — semantically identical range check; `is_finite` and `approx_eq` checks retained |
| `temporal/storage_tests.rs` | `&[row.clone()]` → `std::slice::from_ref(&row)` (5 sites) | None — identical single-element slice; all `assert_eq!(found, row)` retained |
| `ngram_tests.rs` | added `clippy::panic` allow only | None |

No assertions were deleted, loosened, or commented out. No `#[ignore]` added.

### Cycle-1 perf-gate replacement (verified — intent preserved)

`extract_tests.rs:489-521` — the flaky wall-clock `< 5ms` assertion was replaced with
`large_input_smoke_completes_nonempty`, which linearizes a ~3000-node Rust fixture, runs
`extract_ast_ngrams`, and asserts non-empty bigrams AND trigrams. The real perf gate moved
to the new `extract_ngrams` Criterion group in `linearize_bench.rs`. The replacement
preserves correctness intent (call completes, produces output) without the non-deterministic
latency bound. This aligns with the cycle-1 note and `applies ADR-001` (no silent coverage cap).

### Bench additions (verified — additive)

`linearize_bench.rs` adds `bench_extract_ngrams` to `criterion_group!`; the pre-existing
`bench_init_latency` and the three other groups are retained. The new import
`extract_ast_ngrams` is additive. `gen_rust_fns` helper already exists (line 23) and is
reused. No bench was removed.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None.

## Pre-existing Issues (Not Blocking)

None.

## Suggestions (Lower Confidence)

None at >=60% confidence. The change is a clean additive feature with full test coverage
and clippy-conformant test refactors.

## Cross-Cycle Notes

- PRIOR_RESOLUTIONS parsed successfully: cycle 1 fixed 13 issues, 0 false positives. No
  cycle-1 fix was reverted; the u16-overflow widening (PF-004) is present at `extract.rs:160`
  and locked by test B1. The flaky perf-gate replacement is confirmed intact (see above).
- No cycle-1 finding is re-raised.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 10
**Recommendation**: APPROVED
