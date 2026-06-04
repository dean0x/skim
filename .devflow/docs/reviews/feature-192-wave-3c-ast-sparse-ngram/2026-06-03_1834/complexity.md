# Complexity Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**Date**: 2026-06-03 18:34
**Scope**: PR #269, Cycle 2 — `crates/rskim-search/src/ast_index/extract.rs` (new)
**Diff**: `git diff main...HEAD` — extract.rs (+270), extract_tests.rs (+778), mod.rs (+5), ngram_tests.rs (1)

## Summary of Assessment

The single non-trivial function under review, `extract_ast_ngrams_with_weights`
(extract.rs:117-248), implements the 5-step ancestor-stack algorithm documented
in `ast-index/KNOWLEDGE.md` (gap-fill → resolve → emit bigram → emit trigram →
record). The complexity present is **essential**, not accidental: it mirrors the
documented design step-for-step, the control flow is linear (one loop, no nested
decision trees), max nesting is 3, and each phase is delimited by a section
comment. Cycle 1 already collapsed nested `if`s into let-chains and simplified
gap-fill bounds. No new deep nesting, long-function, or unclear-control-flow
issues were introduced.

No findings reach the >=80% confidence threshold. The items below are reported in
Suggestions only (60-79%).

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None.

## Pre-existing Issues (Not Blocking)

None within scope.

## Suggestions (Lower Confidence)

- **Two parallel ancestor-access idioms in one function** - `extract.rs:167,179-187,221`
  (Confidence: 65%) — The function reads the ancestor table three ways: direct
  slicing `ancestors[fill_start..d]` (gap-fill, line 167), direct index assign
  `ancestors[d] = ...` (record, line 221), and defensive `ancestors.get(usize::from(pd))`
  via `checked_sub` (resolve, lines 179-187). Each is individually justified —
  direct indexing is guarded by the preceding `debug_assert!(d < table_len)`, and
  `.get()` cleanly absorbs the depth<1 / depth<2 underflow at the root. But the
  mixed idiom forces a reader to confirm two different safety arguments in the same
  body. Minor readability cost; not worth changing if it risks the verified
  bounds reasoning. avoids PF-004 (the `checked_sub`/u32-widen discipline is
  load-bearing and should stay).

- **Borderline cyclomatic complexity in the core loop** - `extract.rs:117-248`
  (Confidence: 62%) — Counting the early return, the loop, and the three let-chain
  predicates (gap-fill 2 terms, bigram 3 terms, trigram 4 terms) plus the
  `checked_sub().and_then()` resolve chains puts cyclomatic complexity in the
  ~12-14 range (HIGH band per the 10-20 guideline). However the branches are flat
  and sequential rather than nested, and the emit predicates are inherent to the
  sentinel-suppression contract. Extracting the bigram/trigram emit blocks into
  two small `fn`s taking `(parent, gp, node, &mut map, &weight_fn)` would drop the
  visible branch count, but would also add parameter-passing noise and obscure the
  shared `entry().or_insert()` accumulation pattern. Net-neutral; flagged only so
  the threshold is documented, not as a recommended change.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Complexity Score**: 9
**Recommendation**: APPROVED

### Notes
- Cross-cycle: cycle-1 fixes (let-chain collapse, gap-fill bounds simplification,
  depth-from-node resolution) verified present in current code; not re-raised.
- Function length: body ~97 lines incl. comments / docstring; logical statement
  count is well under the 50-line warning threshold once comments and the
  collect/sort tail are discounted. Not flagged.
- Both Suggestions are net-neutral refactors; per the Iron Law of the complexity
  skill, the code is explainable in under 5 minutes thanks to the KNOWLEDGE.md
  step mapping and inline section headers.
