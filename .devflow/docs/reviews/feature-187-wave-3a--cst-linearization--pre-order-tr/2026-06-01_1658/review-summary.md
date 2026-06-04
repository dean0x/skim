# Code Review Summary

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Cycle**: 2 (Incremental Review)
**Date**: 2026-06-01_1658
**Prior Cycle**: Resolved 12/15 issues (Cycle 1), FP ratio 13%

## Merge Recommendation: CHANGES REQUESTED

**Reasoning**: One HIGH issue in blocking category (misleading test name), multiple MEDIUM blocking/should-fix issues across three crates. The refactoring is architecturally sound (complexity reduction, improved encapsulation) but requires resolution of: (1) test naming alignment with assertion, (2) duplicated constants centralization, (3) pre-allocation improvements in two hot paths, (4) test assertion strengthening.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| **Blocking** | 0 | 1 | 5 | 0 | **6** |
| **Should Fix** | 0 | 0 | 3 | 0 | **3** |
| **Pre-existing** | 0 | 0 | 2 | 0 | **2** |
| **Total** | 0 | 1 | 10 | 0 | **11** |

---

## Blocking Issues (Fix Before Merge)

### HIGH: 1 Issue

**Test function name does not match assertion threshold** - `linearize_tests.rs:428`
**Confidence**: 95% (3 reviewers flagged: testing-HIGH, consistency-MEDIUM)
- **Problem**: Function named `linearize_1000_line_file_under_5ms` but assertion checks `< 10ms`. The threshold was changed from 5ms to 10ms, input changed from 100 to 1000 functions, but function name was not updated. This violates the principle that test names describe expected behavior.
- **Impact**: HIGH - False documentation of performance contract; test readers get incorrect expectations; CI output confusing.
- **Fix**: Rename function to `linearize_1000_line_file_under_10ms` to match the new assertion (single-line change).

### MEDIUM: 5 Issues

**1. Duplicated bounds constants across three modules** - `linearize.rs:40-45`, `ast_extract.rs:21-24`, `ast_walk.rs:69-71`
**Confidence**: 85% (architecture review)
- **Problem**: `MAX_AST_DEPTH` (500) and `MAX_AST_NODES` (100,000) defined identically in three locations. The extraction of `AstWalkIter` into `rskim-core` was the right moment to centralize. If any caller changes their local constant without updating others, bounds behavior silently diverges.
- **Impact**: MEDIUM - Silent divergence risk as codebase evolves; violates DRY principle on critical safety constants.
- **Fix**: Export canonical defaults from `AstWalkConfig` and have callers reference them:
```rust
// rskim-core/src/ast_walk.rs
impl AstWalkConfig {
    pub const DEFAULT_MAX_DEPTH: u32 = 500;
    pub const DEFAULT_MAX_NODES: u32 = 100_000;
}

// In linearize.rs and ast_extract.rs:
const MAX_AST_DEPTH: u32 = AstWalkConfig::DEFAULT_MAX_DEPTH;
const MAX_AST_NODES: u32 = AstWalkConfig::DEFAULT_MAX_NODES;
```

**2. `level_stack` in `AstWalkIter` has no pre-allocation** - `ast_walk.rs:118`
**Confidence**: 82% (reliability-HIGH, performance-should-fix, rust-suggestion)
- **Problem**: `level_stack: Vec::new()` starts empty and grows on each descent. While `max_depth=500` bounds the maximum size (2 KiB), the stack grows without a capacity hint, causing 4-5 reallocations per deep traversal (0 -> 1 -> 2 -> 4 -> 8 -> 16 -> 32). `AstWalkIter` is now shared across two hot paths (linearize and ast_extract), multiplying the reallocation cost.
- **Impact**: MEDIUM - Allocator pressure in hot path; each call pays reallocation cost unnecessarily.
- **Fix**: Pre-allocate with capacity:
```rust
pub fn new(cursor: tree_sitter::TreeCursor<'a>, config: AstWalkConfig) -> Self {
    let initial_cap = (config.max_depth as usize).min(64);
    Self {
        cursor,
        level_stack: Vec::with_capacity(initial_cap),
        // ...
    }
}
```

**3. `level_stack` briefly exceeds bounds before guard runs** - `ast_walk.rs:118,170,214`
**Confidence**: 82% (reliability-HIGH)
- **Problem**: The `advance()` method (line 170-173) pushes onto `level_stack` *before* the bounds guard in `next()` runs on the next iteration. While not a memory safety issue (2 KiB is safe), this violates the reliability principle that every collection should have an explicit capacity bound when max size is known. `level_stack.capacity()` is never explicitly capped.
- **Impact**: MEDIUM - Violates allocation discipline; implicit rather than explicit bounds.
- **Fix**: Same as above -- `Vec::with_capacity(config.max_depth as usize)` makes the bound explicit.

**4. Unconditional 501-element Vec allocation in ast_extract::walk_tree** - `ast_extract.rs:137`
**Confidence**: 85% (performance-MEDIUM)
- **Problem**: `vec![None; ancestor_cap]` allocates 501 entries (each `Option<NodeKindId>` is 8 bytes = ~4 KiB) on every call to `walk_tree`, regardless of actual tree depth. In the corpus extraction path, this function is called once per file. For thousands of files, this creates thousands of short-lived 4 KiB allocations hitting the allocator repeatedly. Typical Rust/TS files rarely exceed depth 20-30, so 95%+ of the allocation is never written.
- **Impact**: MEDIUM - Allocator churn in batch corpus processing; fixed 4 KiB allocation per file when typically only ~160 bytes needed.
- **Fix**: Option A (preferred) - Reuse across files by threading through `walk_tree` or resetting via `fill(None)`. Option B - Start small (64) and grow on demand:
```rust
let mut ancestors: Vec<Option<NodeKindId>> = vec![None; 64];
// ...in loop:
if depth >= ancestors.len() {
    ancestors.resize(depth + 1, None);
}
```

**5. Weak test assertion for ERROR children traversal** - `ast_walk.rs:335-348`
**Confidence**: 82% (testing-MEDIUM)
- **Problem**: Test `error_children_still_yielded` claims to verify that children of ERROR nodes are yielded (AC-F5), but assertion `items.len() > 1` only proves more than root was yielded. Sibling non-error nodes could satisfy this without proving ERROR children appear. The assertion does not isolate whether nodes that are *children* of an ERROR node specifically appear in output.
- **Impact**: MEDIUM - Test passes trivially for any non-empty parse; does not actually verify claimed behavior; avoids PF-002 (surfacing deferred test coverage).
- **Fix**: Strengthen assertion to prove deep traversal under ERROR nodes:
```rust
let error_items: Vec<_> = items.iter().filter(|n| n.is_error).collect();
assert!(!error_items.is_empty(), "should have error nodes");
let min_error_depth = error_items.iter().map(|n| n.depth).min().unwrap();
let has_deeper_non_error = items.iter().any(|n| !n.is_error && n.depth > min_error_depth);
assert!(
    has_deeper_non_error,
    "expected non-error nodes deeper than shallowest error (depth {min_error_depth})"
);
```

---

## Should-Fix Issues (Recommended Fixes)

### MEDIUM: 3 Issues

**1. No test for `AstWalkIter` `FusedIterator` contract** - `ast_walk.rs:240-520`
**Confidence**: 80% (testing-medium)
- **Problem**: `AstWalkIter` behaves as a fused iterator (returns `None` forever after exhaustion), verified manually by `exhausted_returns_none` test, but does not implement the `FusedIterator` marker trait. This misses an opportunity to document the guarantee and enable standard library optimizations.
- **Fix**: Add trait implementation and compile-time assertion:
```rust
impl<'a> std::iter::FusedIterator for AstWalkIter<'a> {}

#[test]
fn ast_walk_iter_is_fused() {
    fn assert_fused<T: std::iter::FusedIterator>() {}
    assert_fused::<AstWalkIter<'_>>();
}
```

**2. No test for ancestor chain break on ERROR nodes** - `ast_extract.rs:153-159`
**Confidence**: 82% (testing-medium)
- **Problem**: The new `walk_tree` implementation sets `ancestors[depth] = None` when encountering ERROR nodes, breaking bigram/trigram chains for descendants. Existing tests verify ERROR nodes don't appear in bigrams but don't verify the chain-break behavior. This ancestor-chain logic is the most complex part of the refactor and has no dedicated test.
- **Fix**: Add test that verifies valid nodes below ERROR nodes don't produce bigrams to the ERROR's parent:
```rust
#[test]
fn error_ancestor_breaks_bigram_chain() {
    let mut vocab = NodeKindVocabulary::new();
    let source = "fn broken(((( { let x = 1; }";
    let result = extract_ast_ngrams_from_file(source, Language::Rust, &mut vocab, false).unwrap();
    assert!(result.error_node_count > 0);
    // Verify chain-break: ERROR node's parent should not appear as a bigram parent
    // of ERROR node's children.
}
```

**3. Ancestor table allocation should be optional per-caller** - `ast_extract.rs:136-137`
**Confidence**: 80% (performance-medium)
- **Problem**: `walk_tree` always allocates 501 `Option<NodeKindId>` entries even though `linearize.rs` doesn't need ancestor tracking at all. The allocation is hardcoded for the specific need of `ast_extract.rs`.
- **Fix**: Consider threading an optional `ancestors` buffer through `walk_tree` signature, or accept 4 KiB allocation as acceptable for this use case.

---

## Pre-existing Issues (Informational Only)

### MEDIUM: 2 Issues

**1. `#[must_use]` attribute removed from `linearize_source`** - `linearize.rs:195`
**Confidence**: 82% (regression-MEDIUM)
- **Problem**: The base branch had `#[must_use = "..."]` annotation on public `linearize_source`. The PR removes it. While `Result` is inherently `#[must_use]` in Rust's standard library (compiler still warns), the custom message provided extra context for callers. The codebase's other functions (`ngram.rs`, `ast_weights.rs`, `types.rs`) consistently apply explicit `#[must_use]` on public functions.
- **Impact**: MEDIUM - Breaks local convention; loses custom documentation; compiler warning still fires.
- **Note**: Not blocking since compiler warning still fires. Recommend re-adding for consistency.

**2. `LANG_MAPS` LazyLock initializes all 14 grammars eagerly** - `linearize.rs:109-174`
**Confidence**: 80% (performance-pre-existing)
- **Problem**: The first call to `linearize_source` for any language triggers `LazyLock` initialization of `LANG_MAPS`, which loads all 14 tree-sitter grammars and parses empty strings for each. This is a one-time cost but front-loads all initialization at once rather than amortizing across first use of each language.
- **Impact**: MEDIUM - Not a regression (pre-existing); may be surprising in single-file latency-sensitive contexts.
- **Note**: Informational only. Current approach is correct for batch indexing where all languages are used.

---

## Convergence Status

**Cycle Progression**: 
- Cycle 1: 15 issues → 12 fixed, 2 false positives, 1 deferred (duplication → AstWalkIter extraction resolved)
- Cycle 2: 11 unique issues (some convergence from Cycle 1) → Metrics below

**Cross-Reviewer Convergence**:
- `level_stack` pre-allocation: 3 reviewers (reliability, performance, rust) → confidence boosted to 82%
- Duplicated constants: 2 reviewers (architecture, rust) → confidence 85%
- Test name mismatch: 3 reviewers (testing, consistency, regression) → confidence 95%
- Weak test assertions: 2 reviewers (testing) → confidence 82%
- Ancestor allocation: 1 reviewer (performance) → confidence 85%

**False Positive Rate**: 2/15 = 13% (Cycle 1) → healthy convergence ratio

---

## Per-Reviewer Scores

| Reviewer | Focus | Score | Recommendation |
|----------|-------|-------|-----------------|
| Security | Security patterns | 9/10 | APPROVED_WITH_CONDITIONS |
| Architecture | SOLID, layering, patterns | 8/10 | APPROVED_WITH_CONDITIONS |
| Performance | Allocations, N+1, throughput | 8/10 | APPROVED_WITH_CONDITIONS |
| Complexity | Cognitive load, nesting | 9/10 | APPROVED |
| Consistency | Naming, conventions, uniformity | 9/10 | APPROVED_WITH_CONDITIONS |
| Regression | API stability, behavior preservation | 9/10 | APPROVED_WITH_CONDITIONS |
| Testing | Coverage, assertions, names | 8/10 | CHANGES_REQUESTED |
| Reliability | Bounds, allocation discipline, loops | 8/10 | APPROVED_WITH_CONDITIONS |
| Rust | Language idioms, trait usage | 9/10 | APPROVED |

**Aggregate Score**: 8.4/10

---

## What This PR Does Well

1. **Complexity reduction** - Extracted duplicated DFS traversal (80+90 lines, complexity 10+12) into shared `AstWalkIter` (complexity ~6). Net reduction of ~150 lines of duplicated logic.
2. **Clean layering** - `AstWalkIter` in lowest layer (`rskim-core`), both consumers depend downward. No circular dependencies.
3. **Consistent error handling** - `linearize_tree` returns `LinearizeResult` directly (infallible). `SearchError::AstError` -> `SearchError::Ast` rename follows conventions.
4. **Comprehensive bounds guarding** - `max_depth=500`, `max_nodes=100K`, `saturating_add` for counters, bounds checks in `next()`.
5. **14 new unit tests** for `AstWalkIter` covering pre-order order, depth, error/missing nodes, bounds, zero limits, invariants, fused behavior.
6. **Unified constants** - Both old DFS loops had same limits; extraction allows truth-sourcing from single location.

---

## Action Plan

1. **Rename test** (HIGH) - `linearize_1000_line_file_under_5ms` -> `linearize_1000_line_file_under_10ms` (1 line)
2. **Centralize bounds constants** (MEDIUM) - Export `DEFAULT_MAX_DEPTH` and `DEFAULT_MAX_NODES` from `AstWalkConfig`; update two call sites (5 lines)
3. **Pre-allocate `level_stack`** (MEDIUM) - Add `Vec::with_capacity()` to `AstWalkIter::new()` (2 lines)
4. **Optimize ancestor allocation** (MEDIUM) - Either thread buffer through `walk_tree` or accept 4 KiB as acceptable tradeoff
5. **Strengthen test assertions** (MEDIUM) - Add depth-based check to prove ERROR children are yielded (10 lines)
6. **Add `FusedIterator` impl** (MEDIUM) - Trait impl + compile-time assertion test (4 lines)
7. **Add ancestor chain-break test** (MEDIUM) - New test for ERROR chain-break behavior (15 lines)
8. **Optional: Re-add `#[must_use]` attribute** - Restore convention consistency on `linearize_source` (1 line)

---

## Summary

The refactoring is **architecturally sound** (complexity reduction, improved encapsulation, correct layering) with **9.5/10 code quality**. The core issue is **test/documentation alignment**: the performance test name doesn't match its assertion (HIGH), and test assertions for critical behaviors (ERROR traversal, ancestor chain-break) are too weak to prove claimed invariants (MEDIUM, applies ADR-001). The allocation micro-optimizations (`level_stack` pre-allocation, constant centralization) are MEDIUM severity because they affect hot paths but not correctness.

**Merge blockers**: HIGH test naming issue. **Should-fix before merge**: MEDIUM constant duplication, pre-allocation, test strengthening. **Not blocking**: Pre-existing `#[must_use]` removal.

Estimated fix effort: **30-45 minutes** across 7 targeted changes (mostly 1-10 line edits).
