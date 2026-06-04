# Testing Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### HIGH

**Performance test name misleading after threshold relaxation** - `linearize_tests.rs:428`
**Confidence**: 85%
- Problem: The test function is named `linearize_1000_line_file_under_5ms` but the assertion threshold was changed from `< 5` to `< 10` ms and the input changed from 100 to 1000 functions. The function name no longer describes the actual assertion, violating the principle that test names describe expected behavior.
- Fix: Rename the test to `linearize_1000_line_file_under_10ms` to match the new assertion:
```rust
#[test]
#[cfg(not(debug_assertions))]
fn linearize_1000_line_file_under_10ms() {
```

### MEDIUM

**`error_children_still_yielded` (ast_walk.rs:335) uses weak assertion that does not prove ERROR children are yielded** - `ast_walk.rs:335-348`
**Confidence**: 82%
- Problem: The test claims to verify that children of ERROR nodes are still yielded (AC-F5). However, its assertion -- `items.len() > 1` -- only proves that more than the root was yielded. Sibling non-error nodes (not children of the ERROR node) could satisfy this assertion. The test does not isolate whether nodes that are children of an ERROR node specifically appear in the output. This is a weak assertion that passes trivially for any non-empty parse.
- Fix: Strengthen the assertion by checking that at least one node has a depth greater than the depth of the shallowest ERROR node, or that the total yield count exceeds the number of non-error shallow nodes:
```rust
#[test]
fn error_children_still_yielded() {
    let tree = parse_rust("fn broken(((( {}");
    let config = AstWalkConfig::default();
    let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

    let error_items: Vec<_> = items.iter().filter(|n| n.is_error).collect();
    assert!(!error_items.is_empty(), "broken syntax should produce is_error nodes");

    // At least one non-error node must appear at a depth deeper than
    // the shallowest error node, proving error children are traversed.
    let min_error_depth = error_items.iter().map(|n| n.depth).min().unwrap();
    let has_deeper_non_error = items.iter().any(|n| !n.is_error && n.depth > min_error_depth);
    assert!(
        has_deeper_non_error,
        "expected non-error nodes deeper than the shallowest error node (depth {min_error_depth})"
    );
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No test for `AstWalkIter` `FusedIterator` contract** - `ast_walk.rs:240-520`
**Confidence**: 80%
- Problem: `AstWalkIter` behaves as a fused iterator (returns `None` forever after exhaustion), and the `exhausted_returns_none` test verifies this manually by calling `next()` twice after exhaustion. However, the iterator does not implement the `FusedIterator` marker trait. Both consumers (`linearize.rs` and `ast_extract.rs`) use `iter.by_ref()` then access counters post-exhaustion, which is correct regardless, but adding the trait would document the guarantee and allow the standard library to optimize `.fuse()` chains.
- Fix: Add `impl FusedIterator for AstWalkIter<'_> {}` and a compile-time assertion:
```rust
impl<'a> std::iter::FusedIterator for AstWalkIter<'a> {}

// In tests:
#[test]
fn ast_walk_iter_is_fused() {
    fn assert_fused<T: std::iter::FusedIterator>() {}
    assert_fused::<AstWalkIter<'_>>();
}
```

**No test coverage for `ast_extract::walk_tree` ancestor chain break on ERROR nodes** - `ast_extract.rs:153-159`
**Confidence**: 82%
- Problem: The new `walk_tree` implementation in `ast_extract.rs` contains critical logic where encountering an ERROR node sets `ancestors[depth] = None`, breaking the bigram/trigram chain for descendants. The existing test `error_nodes_counted_but_not_in_bigrams` verifies that ERROR nodes do not appear in bigrams, but it does not verify the chain-break behavior -- that a valid grandchild of an ERROR node does not produce a bigram with the ERROR node's parent as its own parent. This ancestor-chain logic is the most complex part of the refactor and has no dedicated test.
- Fix: Add a test that verifies the chain break explicitly. Parse source where a valid node appears below an ERROR node, and confirm that the valid node does not produce a bigram to the ERROR node's parent:
```rust
#[test]
fn error_ancestor_breaks_bigram_chain() {
    let mut vocab = NodeKindVocabulary::new();
    // Source where `let x = 1;` appears inside a broken fn context.
    // The ERROR node between source_file and the let_declaration should
    // break the bigram chain: no bigram should connect source_file -> let_declaration.
    let source = "fn broken(((( { let x = 1; }";
    let result = extract_ast_ngrams_from_file(source, Language::Rust, &mut vocab, false).unwrap();
    assert!(result.error_node_count > 0, "should have error nodes");
    // Verify no bigram links a node outside the error subtree to a node inside it
    // without the error itself appearing as an intermediary.
    // (The exact assertion depends on vocabulary IDs, but the key invariant is:
    // ERROR node's parent should not appear as a bigram parent of ERROR node's children.)
}
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`resolve_kinds` helper in linearize_tests.rs performs unchecked index access** - `linearize_tests.rs:31-37`
**Confidence**: 85%
- Problem: The `resolve_kinds` helper function performs `NODE_KIND_VOCABULARY[n.kind_id as usize]` without bounds checking. If a `kind_id` were ever out of range (e.g., due to a regression in vocabulary mapping), this would panic with an unhelpful index-out-of-bounds message rather than a descriptive test failure.
- Fix: Use `.get()` with an expect message:
```rust
fn resolve_kinds(result: &LinearizeResult) -> Vec<&'static str> {
    result
        .nodes
        .iter()
        .map(|n| {
            NODE_KIND_VOCABULARY
                .get(n.kind_id as usize)
                .copied()
                .expect("kind_id out of vocabulary range")
        })
        .collect()
}
```

## Suggestions (Lower Confidence)

- **Missing integration test for `AstWalkIter` consumed from both `linearize.rs` and `ast_extract.rs` on the same input** - (Confidence: 70%) -- Since both consumers now delegate to `AstWalkIter`, a single integration test parsing the same source through both paths and comparing `node_count` / `error_count` would catch any divergence in how they interpret the shared iterator's output. Currently they are only tested independently.

- **Performance test uses `Instant::now()` wall-clock timing without statistical rigor** - `linearize_tests.rs:440-448` (Confidence: 65%) -- The performance test runs a single iteration and asserts `< 10ms`. On CI with variable load, a single-sample wall-clock measurement can flake. The `#[cfg(not(debug_assertions))]` guard helps (release-only), and 10ms is generous, so flake risk is low -- but the pattern is fragile.

- **`error_children_still_yielded` in linearize_tests.rs (line 205) duplicates the same weak assertion pattern as ast_walk.rs** - `linearize_tests.rs:204-219` (Confidence: 72%) -- The linearize-level test compares `with_error.node_count > 0` and `clean.node_count > 0`, but does not verify that children of error nodes specifically contributed to the count.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Testing Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The test suite is well-structured: 8 test cycles covering types, vocabulary, traversal, error handling, bounds, multi-language, edge cases, and performance. The `assert_node_count_invariant` helper is applied consistently (applies ADR-001 -- issues are surfaced rather than deferred). The `AstWalkIter` unit tests (14 tests) provide good coverage of the iterator contract including zero-limit edge cases and fused-iterator behavior.

The primary concerns are: (1) the performance test name is stale after the threshold change (HIGH), (2) the "error children still yielded" test in both `ast_walk.rs` and `linearize_tests.rs` uses assertions too weak to verify the claimed behavior (MEDIUM), and (3) the ancestor chain-break logic in `ast_extract.rs` -- the most complex new logic -- lacks a dedicated test (MEDIUM, avoids PF-002 by surfacing rather than deferring).
