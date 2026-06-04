# Testing Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Hardcoded vocabulary size assertion couples test to data file** - `linearize_tests.rs:82`
**Confidence**: 85%
- Problem: `vocabulary_has_1740_entries()` asserts `NODE_KIND_VOCABULARY.len() == 1740`. This is a snapshot assertion against a generated data file (`ast_weights.rs`, 105K lines). Any future vocabulary regeneration (e.g., adding a new language grammar, re-running `ast_codegen`) will break this test even though the linearization logic is unchanged. This tests an implementation detail (exact vocabulary size) rather than behavior (vocabulary is non-empty and usable).
- Fix: Replace with a behavioral assertion that validates the property the linearizer depends on:
```rust
#[test]
fn vocabulary_is_non_empty_and_within_u16_range() {
    assert!(!NODE_KIND_VOCABULARY.is_empty(), "vocabulary must not be empty");
    assert!(
        NODE_KIND_VOCABULARY.len() <= u16::MAX as usize,
        "vocabulary must fit in u16 index space"
    );
}
```

**Performance test uses wall-clock `Instant` with single sample** - `linearize_tests.rs:428-449`
**Confidence**: 82%
- Problem: `linearize_1000_line_file_under_10ms` uses a single `Instant::now()` measurement. On CI runners with resource contention, a single sample wall-clock measurement is inherently flaky. The `#[cfg(not(debug_assertions))]` guard prevents debug-mode false positives, and the 10ms threshold is generous relative to the Criterion benchmarks, but a single sample remains non-deterministic. The project already has proper Criterion benchmarks (`linearize_bench.rs`) that provide statistically meaningful timing. This inline test duplicates that role less reliably.
- Fix: Either remove this test (the Criterion bench covers the same case) or add a multi-sample loop with a percentile check:
```rust
let mut times = Vec::with_capacity(5);
for _ in 0..5 {
    let start = Instant::now();
    let _ = parse_and_linearize(&source, Language::Rust);
    times.push(start.elapsed());
}
times.sort();
let median = times[2]; // p50
assert!(
    median.as_millis() < 10,
    "median linearize_source took {}ms, expected < 10ms",
    median.as_millis()
);
```

### MEDIUM

**`unknown_kind_emits_sentinel_zero` does not actually verify the sentinel path** - `linearize_tests.rs:384-397`
**Confidence**: 85%
- Problem: The test comment acknowledges "We can't easily inject an unknown kind" and then only checks that if any `kind_id == 0` node exists, vocabulary index 0 is the empty string. This is a tautological assertion on the static vocabulary, not a test of the sentinel emission logic. The test passes even if no node has `kind_id == 0`, making it vacuously true. The behavior under test (unknown kind maps to sentinel 0) is never actually exercised.
- Fix: Restructure as a direct unit test of the vocabulary lookup. The `LANG_MAPS` entries use `None` for unknown kinds, and `unwrap_or(0)` maps that to 0. Test this mapping directly:
```rust
#[test]
fn lang_map_unknown_kind_resolves_to_sentinel() {
    let maps = &*LANG_MAPS;
    let rust_map = maps.get(&Language::Rust).expect("Rust must have a lang map");
    // Find a None entry (if any exist), confirming it would map to sentinel 0.
    // Alternatively, test an out-of-bounds index:
    let out_of_bounds = rust_map.len(); // beyond any valid kind_id
    let result = rust_map.get(out_of_bounds).copied().flatten().unwrap_or(0);
    assert_eq!(result, 0, "out-of-bounds kind_id must resolve to sentinel 0");
    assert_eq!(NODE_KIND_VOCABULARY[0], "", "sentinel ID 0 must map to empty string");
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`error_node_breaks_ancestor_chain_for_descendants` uses indirect proof that is grammar-version-sensitive** - `ast_extract.rs:492-541`
**Confidence**: 80%
- Problem: This test compares bigram counts between clean and broken source to prove the chain-break. The assertion `result_broken.bigrams.len() < result_clean.bigrams.len()` relies on tree-sitter's grammar producing a specific parse tree shape where ERROR nodes suppress at least one bigram. If a grammar update changes how `fn broken(((( { let x = 1; }` is error-recovered, the clean-vs-broken bigram count relationship could change (e.g., the broken version might recover more gracefully). The comment acknowledges this indirection. Consider adding a more direct assertion.
- Fix: The indirect comparison is still valid as a behavioral test (error nodes should suppress bigrams vs. clean code). Keep it, but add a direct assertion on a known property:
```rust
// At minimum: the broken parse must NOT have zero bigrams — it still has
// the source_file -> function_item edge and parts of the body.
assert!(!result_broken.bigrams.is_empty(), "broken source should still produce some bigrams");
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing negative test for `SearchError::Ast` path** - `linearize.rs:212` (Confidence: 65%) -- The `Parser::new` failure after `LANG_MAPS` lookup is a defensive guard that cannot be triggered with normal Language enum values. Testing it would require either a mock or a dedicated test-only language variant. Low priority since the path is essentially unreachable in production.

- **`all_14_ts_languages_produce_output` tests empty source only** - `linearize_tests.rs:277-301` (Confidence: 70%) -- Each of the 14 languages is tested with empty source `""`, which only validates that the root node is produced. Testing with minimal non-trivial source per language (like the benchmark fixtures) would provide stronger coverage of per-language kind mapping. The fixture tests (Rust, TypeScript, Python at lines 334-379) partially cover this, but only 3 of 14 languages get non-trivial source.

- **AstWalkIter tests are single-language (Rust only)** - `ast_walk.rs:264-556` (Confidence: 65%) -- All 15 `AstWalkIter` tests use `parse_rust()`. Since `AstWalkIter` is language-agnostic (it operates on `TreeCursor`), this is acceptable, but a single cross-language smoke test would increase confidence that grammar-specific cursor behaviors (e.g., MISSING node insertion patterns) are handled uniformly.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The test suite is well-structured with clear cycle organization (8 cycles), consistent use of the `assert_node_count_invariant` helper (applies ADR-001 -- all noticed invariant assertions are enforced), and good coverage of error paths, bounds guards, multi-language support, and edge cases. The `AstWalkIter` tests (15 tests) and linearize tests (29 tests) both pass. The new `error_node_breaks_ancestor_chain_for_descendants` test in `ast_extract.rs` validates the chain-break logic introduced by the refactor.

The two HIGH findings are: (1) the hardcoded `1740` vocabulary size assertion that couples tests to generated data, making them brittle to vocabulary regeneration, and (2) the single-sample wall-clock performance test that duplicates the Criterion benchmark less reliably. Both are fixable without architectural changes. The MEDIUM finding about the vacuously-true sentinel test is a gap in coverage that should be addressed.

Cross-cycle note: the prior resolution cycle fixed 9 of 11 issues including centralizing bounds constants, adding `#[must_use]`, fusing the iterator, and adding the chain-break test. This review found no regressions from those fixes (avoids PF-002 -- all findings surfaced for decision, none silently deferred).
