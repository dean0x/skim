# Testing Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03
**Focus**: testing (extract_tests.rs coverage of AST sparse n-gram extraction)

## Scope

Reviewed `crates/rskim-search/src/ast_index/extract_tests.rs` (509 lines, 18 tests) against the
production module `extract.rs` and the documented contract in
`.devflow/features/ast-index/KNOWLEDGE.md`. The 18-test count matches the documented inventory
(F1–F9, C1–C5, P1, plus F6b sentinel-grandparent and `two_dropped_nodes_wide_gap`,
`unknown_ngram_default_weight`).

Overall the suite is strong: tests are behavior-focused (assert on the observable `AstNgramSet`
output — keys, counts, weights — never on internal ancestor-table state), use dependency-injected
synthetic weights for determinism, and cover the happy path plus several edge cases well. The gaps
below are about *missing* boundary/negative cases, not brittleness in existing tests.

## Issues in Your Changes (BLOCKING)

None. No test in this PR is implementation-coupled, flaky, or asserts on internals. No blocking
defect found in the test code itself.

## Issues in Code You Touched (Should Fix)

### HIGH

**The documented "residual gap-fill edge case" has no test (spurious-edge divergence)** — Confidence: 90%
- `extract.rs:135-146` (gap-fill), `extract_tests.rs` (no covering test)
- Problem: KNOWLEDGE.md lines 198-204 document a known divergence: a dropped ERROR node that had a
  *same-depth preceding sibling* leaves no gap in depth values, so the orphaned child binds to that
  sibling as a spurious parent. The gap-fill tests (`depth_jump_breaks_chain` F5,
  `two_dropped_nodes_wide_gap`) only exercise the case where the drop DOES leave a depth jump
  (`>+1`), which is the path gap-fill actually handles. The documented residual behavior — where
  gap-fill does NOT fire and a spurious edge IS emitted — is asserted nowhere. This is the single
  most important behavioral characteristic of the algorithm that is currently unverified.
- Impact: The divergence is a deliberate, accepted limitation. Without a characterization test it
  can silently change (e.g. a future "fix" that over-corrects, or a regression that turns the benign
  spurious edge into a panic or a corrupt key) and no test will catch it. A test that *documents and
  locks* the current accepted behavior (sequence like `node(10,0), node(20,1), node(30,1), node(40,2)`
  where the depth-1 sibling pattern produces the spurious `20→40` or `30→40` edge — author should
  confirm exact expected edge against the algorithm) converts an undocumented assumption into an
  enforced contract. applies ADR-001 (lock the noticed gap rather than hand-wave it), avoids PF-002
  (do not treat a documented divergence as out-of-scope to test).
- Fix: Add a characterization test that builds the same-depth-sibling sequence and asserts the
  *exact* current output (the spurious edge present, weight 1.0). Name it to flag intent, e.g.
  `dropped_error_with_same_depth_sibling_emits_documented_spurious_edge`.

**Trigram `count` accumulation is never asserted** — Confidence: 85%
- `extract.rs:178-179`, `extract_tests.rs:242-300`
- Problem: `repeated_edge_dedup_counts_occurrences` (F7) and `suppressed_occurrences_not_counted`
  (F9) verify the `count` field for *bigrams* (count == 3, count == 1). There is no equivalent test
  asserting that a repeated *trigram* edge accumulates `count` correctly (the `entry.1 += 1` on
  line 179). Trigram emission has a strictly stricter guard than bigram emission (requires both
  grandparent and parent non-sentinel), so its accumulation path is genuinely distinct code and
  deserves its own coverage.
- Impact: A regression in trigram counting (e.g. an `or_insert((w, 1))` typo, or a guard that
  drops repeats) would pass the entire suite. Trigram `count` feeds the documented TF-IDF/BM25
  future-proofing contract (KNOWLEDGE.md 159-161), so an undercounted trigram silently degrades
  downstream scoring.
- Fix: Add a test with a repeated grandparent→parent→child chain (e.g. the F7 pattern extended one
  level deeper, repeated 3×) asserting `trigram.count == 3` and a single deduplicated entry.

### MEDIUM

**No max-depth boundary test for the ancestor-table sizing invariant** — Confidence: 82%
- `extract.rs:116-121` (table sized `max_depth + 1`), `extract.rs:142` (slice `..d`)
- Problem: The ancestor table is sized to `max(depth) + 1` and the gap-fill slice `ancestors[p+1..d]`
  relies on `d <= max_depth` (the table-sizing invariant) to be panic-safe. No test drives a node at
  the maximum observed depth combined with a depth jump landing exactly at that maximum — i.e. the
  exact slice-end-equals-len boundary. All current depth-jump tests use small depths (3, 4) well
  inside the table. The panic-prone slice introduced by the recent simplification (commit removed the
  explicit `end <= ancestors.len()` guard) is therefore never exercised at its boundary.
- Impact: The slicing is provably panic-safe under the current invariant (verified by inspection:
  `d <= max_depth` so `d <= len-1 < len`; `p+1 < d` so `start < end`), but that proof is load-bearing
  and untested. A future change to the sizing line (line 121) that breaks the `+ 1` or the max scan
  would reintroduce an out-of-bounds panic with no failing test. A boundary test pins the invariant.
- Fix: Add a test such as `node(10, 0), node(20, 1), node(30, 5)` AND a separate node establishing
  `max_depth == 5` reached via a jump, asserting it returns without panic and suppresses the orphan.
  Better: a property-style test asserting "no input of arbitrary `(kind_id, depth)` pairs panics"
  (see Suggestions).

**Depth-0-only and single-node inputs untested** — Confidence: 80%
- `extract_tests.rs` (no covering test); `extract.rs:130, 149-157`
- Problem: The only inputs smaller than 3 nodes are the empty case (F1) and the 2-node
  `unknown_ngram_default_weight`. There is no test for a single depth-0 node (`[node(10, 0)]`) which
  exercises the `checked_sub(1)` → `None` parent path with no prior node, nor for an all-depth-0
  sibling list (`[node(10,0), node(20,0)]`) which should emit *zero* bigrams (no parent at any
  depth). These are the natural boundary of the parent-resolution `checked_sub` underflow guard
  (KNOWLEDGE.md 375-378 explicitly calls out this underflow concern at depth 0).
- Impact: The `checked_sub(1)`/`checked_sub(2)` underflow guards at depth 0 and depth 1 are the
  module's protection against panic on shallow nodes; they have no direct negative test. A regression
  to plain subtraction would panic only on real depth-0-first input, which the current suite never
  isolates (F2/F3 etc. all start at depth 0 but immediately descend, masking the boundary).
- Fix: Add `single_root_node_emits_nothing` (`[node(10,0)]` → empty set) and
  `all_siblings_at_depth_zero_emit_nothing` (`[node(10,0), node(20,0), node(30,0)]` → empty set).

**Performance-gate test is environment-fragile and only weakly meaningful** — Confidence: 80%
- `extract_tests.rs:486-509` (`extract_3000_line_file_under_budget`)
- Problem: The gate asserts wall-clock `elapsed.as_millis() < 5` for a single un-warmed run. This is
  a hard absolute threshold with no warm-up, no iteration averaging, and millisecond-granularity
  timing (`as_millis()` truncates — a 4.9ms run and a 0ms run both pass, but a 5.0ms run fails). On a
  loaded CI runner or shared machine this is a textbook flaky-test pattern (timing dependency,
  Testing skill HIGH severity). It is gated to release builds only (`cfg(not(debug_assertions))`),
  which limits blast radius, but when it runs it can fail for reasons unrelated to the code.
- Impact: Intermittent red builds that are not regressions; the 5ms number is also not anchored to a
  documented budget (CLAUDE.md's <50ms target is for 1000-line *parse+transform*, not extraction
  alone). A single un-iterated timing assertion provides weak signal — it mostly proves "did not
  catastrophically regress," not "meets a meaningful budget."
- Fix: Either (a) loosen the threshold with headroom and document its provenance, (b) average over
  N iterations after a warm-up run to reduce variance, or (c) move the real performance guard to the
  existing Criterion bench (`benches/linearize_bench.rs` `extract_ngrams` group) and downgrade this
  to a generous smoke-test ceiling (e.g. < 50ms) whose only job is catching O(n²) blowups. Criterion
  is the right home for meaningful perf gating; an inline `Instant` assertion is not.

## Pre-existing Issues (Not Blocking)

None identified in test code outside this PR's additions. The pre-existing `*_tests.rs` clippy
fixes in the diff (manual_range_contains, cloned_ref_to_slice_refs, field_reassign_with_default,
single_match) are mechanical lint corrections and do not alter test behavior or assertions.

## Suggestions (Lower Confidence)

- **Property-based "never panics" test for the extractor** - `extract_tests.rs` (Confidence: 70%) —
  The module is a pure parser-shaped function over `(kind_id, depth)` pairs; this is the canonical
  use case for property testing (Testing skill: parsers/state machines). A `proptest`/`quickcheck`
  property generating arbitrary `Vec<LinearNode>` and asserting "returns without panic AND output is
  sorted+unique" would subsume the max-depth-boundary and depth-0 manual cases and harden the
  panic-prone slice against any input. Only 70% because adding a new dev-dependency may exceed PR
  scope.
- **Assert ordering of `count` independence from emission order** - `extract_tests.rs:361` (Confidence: 65%)
  — `deterministic_two_runs_equal` (C2) covers determinism via full-struct equality, but does not
  isolate that HashMap iteration order cannot leak into the sorted output. The existing C1 sort test
  plus C2 largely cover this; a dedicated assertion is marginal.
- **Trigram-side sentinel-parent suppression at depth ≥ 2** - `extract_tests.rs:198` (Confidence: 65%)
  — F6b covers sentinel *grandparent*; a sentinel *parent* (middle slot) for a trigram at depth 2 is
  covered transitively by F6 but not asserted at the trigram level specifically.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 2 | 3 | - |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 7/10

The suite is well-structured, behavior-focused, and deterministic — no brittleness or
implementation-coupling. Points deducted for: the documented residual gap-fill divergence being
unverified (the algorithm's most important characterization gap), missing trigram `count` coverage,
absent depth-0/single-node and max-depth-boundary cases (the exact slice the recent simplification
made panic-prone is never exercised at its boundary), and an environment-fragile single-shot timing
gate.

**Recommendation**: APPROVED_WITH_CONDITIONS

No test is wrong, and the production code is panic-safe by inspection — but the coverage gaps mean
the suite would not catch a regression in the most subtle parts of the algorithm. Address the two
HIGH items (residual gap-fill characterization test; trigram count test) before merge per ADR-001;
the MEDIUM boundary/perf items are strongly recommended and small.
