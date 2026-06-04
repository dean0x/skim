# Testing Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269 (Cycle 2)
**Date**: 2026-06-03 18:34

## Scope & Verification

- `extract_tests.rs` (new, 778 lines): 26 cases ran green 4× across thread configs
  (`--test-threads=1` and default). No flakiness, fully deterministic.
- Modifications to 8 existing `*_tests.rs` files: all are mechanical clippy-driven
  refactors (`field_reassign_with_default` → struct-update syntax, `&[row.clone()]`
  → `std::slice::from_ref`, `match` → `if let`, `>= && <=` → `RangeInclusive::contains`,
  added `clippy::panic` to allow lists). Verified each preserves assertion semantics —
  no test lost coverage.
- Flaky-test replacement (P1, lines 489-521) verified sound: the prior `< 5ms`
  wall-clock assertion was replaced with a correctness-only non-empty smoke test.
- Cross-cycle: prior cycle-1 fixes (B1-B5 regression tests, perf-gate replacement)
  are present and verified; not re-raised. The "flaky test tracked in a GitHub issue"
  note refers to `storage_perf_tests.rs` / `linearize_tests.rs` wall-clock tests —
  confirmed NOT in this PR's diff, so out of scope for this cycle.

## Issues in Your Changes (BLOCKING)

None. The new test file is behavior-focused (asserts on observable output via public
`key()`/`decode()`, never on internal `HashMap` state), deterministic, and covers the
documented invariants and edge cases.

## Issues in Code You Touched (Should Fix)

None at or above the 80% reporting threshold.

## Pre-existing Issues (Not Blocking)

**Wall-clock timing assertions in sibling perf tests** — `storage_perf_tests.rs:224-314`,
`linearize_tests.rs:506`
**Confidence**: 85%
- Problem: These tests assert `elapsed.as_millis() < ceiling`, the exact flaky pattern
  the P1 note in `extract_tests.rs` calls out. On a shared CI runner a single un-warmed
  call does not reliably bound latency.
- Impact: Informational only — NOT modified by this PR, and the prior-resolution note
  states a tracking issue already exists. Listed for completeness; do not block.
- Fix (separate PR): move latency gates to `cargo bench` (Criterion), keep only
  correctness smoke tests in the unit suite — mirroring the P1 replacement done here.

## Suggestions (Lower Confidence)

- **No independent multi-count assertion** — `extract_tests.rs` (Confidence: 72%) —
  F7/B3 verify `count == 3` for a *single* repeated edge; F9 verifies `count == 1`.
  No test builds a set with two *distinct* edges at *different* repetition counts and
  asserts each independently. A `[10@0,20@1, 10@0,20@1, 10@0,30@1]` fixture asserting
  `(10→20).count == 2` AND `(10→30).count == 1` in one set would close this gap and
  guard against a future cross-key count-attribution bug.

- **Weight-constancy on repeated edges is untested** — `extract.rs:197` (Confidence: 68%) —
  The accumulation map captures weight only on first insert (`or_insert((w, 0))`).
  No test pins that the first-seen weight is the one retained when an edge repeats.
  Low risk given weight functions are pure by contract, but a 1-line assertion in F7
  (`entry.weight == 1.0` is implicitly there via unit weights — an injected-weight
  variant of B3/F7 would make the contract explicit).

- **B4 `gap_fill_at_max_depth_boundary` comment vs. fixture mismatch** —
  `extract_tests.rs:696-702` (Confidence: 65%) — The comment says "max_depth will be 10
  ... ancestor table [None; 11]" but the fixture's max observed depth is 10, so the
  table is correctly sized 11. The narrative is accurate; the only nit is the comment
  describes `fill_start = 2` while prev_depth is 1 (so `fill_start = 2` is correct).
  No behavior issue — comment is fine on re-read; flagging only because the inline
  walkthrough is dense enough to mislead a future maintainer.

## Strengths Observed

- Synthetic DI weights (`unit_bigram_weight`/`unit_trigram_weight`) keep structural
  tests deterministic and decoupled from the production IDF tables — exactly the
  dependency-injection split documented in KNOWLEDGE.md. End-to-end tests (F8, P1,
  `unknown_ngram_default_weight`) exercise the real tables separately.
- Determinism is genuinely tested (C2 `deterministic_two_runs_equal`) and structurally
  guaranteed: HashMap iteration order is non-deterministic but both output vecs are
  `sort_unstable_by_key` on unique keys, making order total. C1 independently asserts
  strict-ascending uniqueness.
- Documented edge cases all have locking tests: B1 (u16::MAX overflow, ×2), B2 (spurious
  same-depth-sibling edge characterization), B3 (trigram count), B4 (max-depth boundary
  ×2), B5 (depth-0 underflow ×2), F6/F6b (sentinel suppression at parent and grandparent).
- Input immutability (C3) and crate-root re-export resolution (C5) covered.
- Assertions are behavior-level — they read `result.bigrams`, decode via public `key()`/
  `decode()`, and never reach into the ancestor table or accumulation maps.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 1 | 0 |

**Testing Score**: 9/10
**Recommendation**: APPROVED
