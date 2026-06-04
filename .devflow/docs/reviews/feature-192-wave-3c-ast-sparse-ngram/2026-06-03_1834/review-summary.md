# Code Review Summary

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269 (Wave 3c — AST sparse n-gram extraction)
**Date**: 2026-06-03_1834

## Merge Recommendation: CHANGES_REQUESTED

The PR is architecturally sound, extensively tested, and secure. However, performance has identified a HIGH blocking issue in the hot path: weight lookups run on every emitted n-gram instead of only on unique keys. This redundant computation (binary searches over a ~24K-entry table) dominates the avoidable cost in the extraction core. The fix is straightforward (Entry API) with zero behavior change. Consistency has identified one MEDIUM should-fix issue in test files (incomplete struct-update migration). Resolve both and the PR is approvable.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 1 | 0 |

---

## Blocking Issues

### HIGH: Weight Lookup Runs Per-Emit Instead of Per-Unique-Key

**File**: `crates/rskim-search/src/ast_index/extract.rs:195-198, 211-214`
**Confidence**: 90% (flagged by performance reviewer)
**Category**: Blocking (in your changes)

**Problem**: The weight closure is invoked unconditionally on every emitted edge, before the HashMap dedup decides whether the key is new:

```rust
let key = AstBigram::encode(p, node.kind_id);
let w = bigram_weight(key);                       // ← runs every emit
let entry = bigram_map.entry(key).or_insert((w, 0));
entry.1 += 1;
```

`bigram_weight` / `trigram_weight` resolve to `ast_bigram_idf` / `ast_trigram_idf`, which do a `match lang.name()` (string compare) plus a `binary_search_by_key` over the per-language weight table. The Rust trigram table alone is ~24K entries, so each lookup is ~15 comparisons. The weight is a pure function of the key (module doc states this explicitly), so recomputing it for every repeated occurrence is pure waste.

Most edges repeat multiple times within a file. The emit count is typically an order of magnitude larger than the unique-key count, meaning binary search runs ~N times when it only needs to run ~unique_keys times.

**Impact**: For a 1000-function fixture (already in the bench at `extract_ngrams/rust_fns/1000`), the node count is in the tens of thousands with the vast majority of emits being repeated edges (`function_item > block`, `block > expression_statement`, etc.). The wasted work is `(total_emits − unique_keys) × log2(table_len)` comparisons per file — directly on the `<50ms / 1000-line` performance budget mandated in CLAUDE.md. This is the single largest avoidable cost in the function.

**Fix**: Only compute the weight when the entry is vacant, using the Entry API so the lookup happens once per unique key:

```rust
use std::collections::hash_map::Entry;

let key = AstBigram::encode(p, node.kind_id);
match bigram_map.entry(key) {
    Entry::Occupied(mut e) => { e.get_mut().1 += 1; }
    Entry::Vacant(e) => { let w = bigram_weight(key); e.insert((w, 1)); }
}
```

Apply the same pattern to the trigram path. This drops the weight lookups from O(emits) to O(unique keys) with no behavior change (the value is identical). Validate with the existing `extract_ngrams` Criterion group before/after.

---

## Should-Fix Issues

### MEDIUM: Incomplete Struct-Update Migration in `lexical/scoring_tests.rs`

**File**: `crates/rskim-search/src/lexical/scoring_tests.rs:3, ~121-126, ~143-145, ~184-186`
**Confidence**: 82% (flagged by consistency reviewer)
**Category**: Should Fix (in code you touched)

**Problem**: This PR establishes a consistent test-style migration — replace `let mut x = T::default(); x.field = v;` with struct-update syntax `T { field: v, ..T::default() }`. It was applied across `config_tests.rs`, `query_tests.rs`, `index/reader_tests.rs`. Those files consequently no longer need the `clippy::field_reassign_with_default` allow.

`lexical/scoring_tests.rs` is the lone holdout: it **adds** `clippy::field_reassign_with_default` to its allow header (line 3) and keeps the old `cfg.k1 = …; cfg.field_boosts = …;` pattern that the other files converted. These are full-field assignments (not array-index mutation), so they are convertible.

**Impact**: One file diverges from the convention the rest of the PR enforces. Future readers won't know whether the holdout was intentional. The added lint allow is the inverse direction from the rest of the PR (which removes the need for it). Minor — no behavior change — but it muddies the otherwise clean migration.

**Fix**: Convert the full-field reassignment blocks in `scoring_tests.rs` to struct-update syntax and drop `clippy::field_reassign_with_default` from the allow header, matching `config_tests.rs`. Where a test sets multiple fields at once, struct-update still applies:

```rust
let cfg = BM25FConfig {
    k1: 0.0,
    field_boosts: [0.0; FIELD_COUNT],
    field_b: [0.0; FIELD_COUNT], // no length normalisation
    ..BM25FConfig::default()
};
```

Leave any genuine `cfg.field_boosts[i] = …` index mutations untouched (they correctly need neither change nor the allow). Applies ADR-001 (fix noticed inconsistencies immediately rather than deferring).

---

## Suggestions (Lower Confidence)

The following items were flagged by multiple reviewers at 60-79% confidence. They are optional polish, not blocking.

- **Per-emit count saturation (Performance + Reliability, 65%)** — `extract.rs:198, 214` — `entry.1 += 1` on a `u32`. Release profile doesn't set `overflow-checks = true`, so this would wrap silently if overflowed. In practice unreachable: production input is bounded at 100K nodes, so term frequency cannot approach `u32::MAX`. A `saturating_add(1)` would make the safety self-evident with zero cost, but is optional.

- **Sort comparator recomputes key on every comparison (Performance, 82%)** — `extract.rs:235, 245` — `sort_unstable_by_key(|e| e.ngram.key())` calls `.key()` once per comparison element access. `sort_by_cached_key` would cache, but for trivially-copyable u32/u64 keys the current form is acceptable. Prefer leaving it unless profiling shows sort dominating.

- **Inconsistent integer cast style (Rust + Consistency, 70%)** — `extract.rs:152` — `let d = node.depth as usize;` is the only `as` cast in the file; every other u16→usize conversion uses `usize::from(...)` (lines 130, 137, 162, 182, 187). Both are sound for widening, but `usize::from(node.depth)` matches surrounding style.

- **`extract_tests.rs` allow header coverage (Consistency, 68%)** — `extract_tests.rs:6` — `ngram_tests.rs`, `classifier_tests.rs`, `query_tests.rs` added `clippy::panic` to their allows this PR; `extract_tests.rs` keeps only `unwrap_used, expect_used`. Verify the test file does not contain `panic!` or `unreachable!`; if it does, align the header.

- **Multi-count assertion gap (Testing, 72%)** — `extract_tests.rs` — F7/B3 verify count=3 for a single repeated edge; F9 verifies count=1. No test builds a set with two distinct edges at different repetition counts and asserts each independently. A fixture with `[10@0,20@1, 10@0,20@1, 10@0,30@1]` asserting `(10→20).count == 2` AND `(10→30).count == 1` in one set would guard against cross-key count-attribution bugs.

---

## Pre-existing Issues (Not Blocking)

- **Wall-clock timing assertions in sibling perf tests** — `storage_perf_tests.rs:224-314`, `linearize_tests.rs:506` (Testing, 85%) — These tests assert `elapsed.as_millis() < ceiling`, the exact flaky pattern the P1 note in `extract_tests.rs` calls out. Informational only — NOT modified by this PR, and a tracking issue already exists per the prior-resolution note.

---

## Strengths

- **Architecture**: Clean layering, correct DI split (`extract_ast_ngrams_with_weights` core + `extract_ast_ngrams` wrapper), all SOLID rules followed. Dependency injection of weight functions enables deterministic synthetic testing.
- **Security**: Pure function, no I/O/SQL/network/secrets. Allocation bounds verified (ancestor table ≤256 KiB, HashMap capped at 1024). Integer overflow paths hardened with explicit widening (PF-004). No panic surface on adversarial input.
- **Reliability**: All bounds verified by construction (loops, allocation). Depth underflow guarded via `checked_sub`. Cycle-1 fixes (u16 widening, assertions, capacity caps) verified intact and locked by regression tests B1–B5.
- **Testing**: 26 deterministic test cases covering all documented edge cases (u16::MAX overflow ×2, depth-0 underflow ×2, max-depth boundary ×2, sentinel suppression ×2, same-depth-sibling characterization, repeated-edge counting, determinism, immutability). Synthetic DI weights keep structural tests decoupled from production IDF tables.
- **Consistency**: New `extract.rs` exemplary in matching sibling-module conventions (naming, DI split, derives, banners, doc style, `#[must_use]`, re-export ordering). Test refactors (struct-update migration) uniformly applied except for one file.
- **Regression**: Purely additive change. All 12 pre-existing exports from KNOWLEDGE.md remain unchanged. No function bodies modified. Test coverage preserved (every modified test refactors prior assertions without loosening them).

---

## Convergence Status

**Cycle**: 2 (of 2 so far)
**Prior Resolution**: Cycle 1 (2026-06-03_1229) fixed 13 issues, 0 false positives, 0 deferred
**Prior FP Ratio**: 0% (0 of 13 findings were false positives)
**Assessment**: **Converging toward resolution** — Cycle 1 fixed all blocking issues (u16 overflow, let-chains, debug asserts, HashMap cap). Cycle 2 found only 1 new HIGH blocking issue (performance: per-emit weight lookup) and 1 MEDIUM should-fix (test consistency). The 0% FP ratio from cycle 1 indicates high-quality analysis with no hallucination loop. The new findings are both legitimate, low-risk, and straightforward to resolve. No pattern of recurring/overlapping issues that would suggest analysis drift.

---

## Action Plan

1. **HIGH blocking** — Move weight lookups behind Entry::Vacant arm (bigram + trigram paths). Validate with existing Criterion bench before/after.
2. **MEDIUM should-fix** — Convert full-field reassignments in `scoring_tests.rs` to struct-update syntax, drop the lint allow.
3. (Optional) Address suggestions: integer cast consistency, saturation semantics, test coverage gaps.

**Expected timeline**: Both fixes are mechanical and low-risk. Estimated 30–60 minutes.

---

## Review Scores

| Focus | Score | Recommendation |
|-------|-------|-----------------|
| Security | 10/10 | APPROVED |
| Architecture | 9/10 | APPROVED |
| Performance | 8/10 | CHANGES_REQUESTED |
| Complexity | 9/10 | APPROVED |
| Consistency | 9/10 | APPROVED_WITH_CONDITIONS |
| Regression | 10/10 | APPROVED |
| Testing | 9/10 | APPROVED |
| Reliability | 10/10 | APPROVED |
| Rust | 9/10 | APPROVED |

**Aggregate**: 7 of 9 reviewers APPROVED or APPROVED_WITH_CONDITIONS; Performance CHANGES_REQUESTED (HIGH blocking); Consistency APPROVED_WITH_CONDITIONS (MEDIUM should-fix). Both blocking/should-fix issues are resolved by this synthesis and are straightforward to fix.
