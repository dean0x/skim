# Testing Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Covering-set verification test weakened by conditional assertion** - `ngram_tests.rs:296-319`
**Confidence**: 85%
- Problem: `query_extract_covering_set_covers_positions` verifies coverage only for positions where the bigram exists in the synthetic weight table. This conditional check (`if w.binary_search_by_key(...).is_ok()`) means positions covered by bigrams that fall back to `DEFAULT_WEIGHT` are never asserted. Since the implementation covers ALL byte positions (not just those with known weights), the test does not fully verify the covering-set guarantee documented in the function contract. Specifically, for `"fn main()"`, the space character between `"n"` and `"m"` generates a bigram `(n, space)` that IS in the synthetic table, but a query with unknown-weight bigrams would skip verification of those positions.
- Fix: Remove the conditional and assert coverage of all byte positions unconditionally, since the covering-set heuristic is supposed to cover every position regardless of weight source:
```rust
#[test]
fn query_extract_covering_set_covers_all_positions() {
    let query = "fn main()";
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights(query, &w);
    let bytes = query.as_bytes();

    let mut covered = vec![false; bytes.len()];
    for (ngram, _) in &result {
        for (pos, window) in bytes.windows(2).enumerate() {
            if Ngram::from_bytes(window[0], window[1]) == *ngram {
                covered[pos] = true;
                covered[pos + 1] = true;
            }
        }
    }

    for (pos, &c) in covered.iter().enumerate() {
        assert!(c, "position {pos} must be covered by the covering set");
    }
}
```

**Border weight comparison test has silent pass on missing bigram** - `ngram_tests.rs:263-281`
**Confidence**: 82%
- Problem: `query_extract_border_bigrams_have_higher_weight` uses `if let Some(ai) = ai_entry` to guard the comparison, meaning if `"ai"` is not selected by the covering-set heuristic, the test silently passes without ever asserting the core property (border bigrams outweigh interior bigrams). Since the covering set is a greedy selection, interior bigrams may be dropped, making this test vacuously true in some configurations.
- Fix: Either assert that `ai_entry.is_some()` before comparing, or restructure the test to use a minimal query where both border and interior bigrams are guaranteed to appear in the covering set:
```rust
#[test]
fn query_extract_border_bigrams_have_higher_weight() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("fn main()", &w);

    let fn_entry = result
        .iter()
        .find(|(n, _)| *n == Ngram::from_bytes(b'f', b'n'));
    let ai_entry = result
        .iter()
        .find(|(n, _)| *n == Ngram::from_bytes(b'a', b'i'));

    assert!(fn_entry.is_some(), "'fn' must appear in query result");
    assert!(ai_entry.is_some(), "'ai' must appear in query result for comparison");
    assert!(
        fn_entry.unwrap().1 > ai_entry.unwrap().1,
        "'fn' border weight ({}) must exceed 'ai' interior weight ({})",
        fn_entry.unwrap().1, ai_entry.unwrap().1,
    );
}
```

### MEDIUM

**No test for duplicate bigram handling in query extraction** - `ngram_tests.rs`
**Confidence**: 85%
- Problem: Document extraction has a dedicated `extract_deduplicates_repeated_bigrams` test, but query extraction lacks an analogous test. The covering-set heuristic selects bigrams by position, so a repeated bigram could appear multiple times in the output (at different positions). There is no test verifying whether this is the intended behavior or a bug. The function documentation does not specify deduplication semantics for query output.
- Fix: Add a test with a query containing repeated bigrams (e.g., `"aa bb aa"`) and assert the expected behavior -- either deduplication or documented allowance of duplicates:
```rust
#[test]
fn query_extract_handles_repeated_bigrams() {
    let mut w: Vec<(u16, f32)> = vec![
        (Ngram::from_bytes(b'a', b'a').key(), 5.0),
        (Ngram::from_bytes(b'b', b'b').key(), 5.0),
    ];
    w.sort_by_key(|&(k, _)| k);
    let result = extract_query_ngrams_with_weights("aa bb aa", &w);
    // Assert the expected behavior for repeated bigrams
    assert!(!result.is_empty());
}
```

**Performance test uses wall-clock timing without warmup** - `ngram_tests.rs:368-391`
**Confidence**: 80%
- Problem: `extract_ngrams_1000_line_file_under_1ms` takes a single timing measurement without a warmup iteration. The first call may include one-time costs (HashMap allocator warmup, binary search cache misses on the 16K-entry weight table). This makes the test potentially flaky on cold CI runners. The generous thresholds (2000us release, 500ms debug) partially mitigate this, but the test name promises "under 1ms" while the actual assertion allows 2ms.
- Fix: Add at least one warmup call before the timed measurement, and align the test name with the actual assertion:
```rust
#[test]
fn extract_ngrams_1000_line_file_under_2ms_release() {
    let line = "fn process_item(item: &Item) -> Result<Output, Error> { todo!() }\n";
    let text: String = line.repeat(1000);

    // Warmup — prime allocator and CPU caches
    let _ = extract_ngrams(&text);

    let start = std::time::Instant::now();
    let result = extract_ngrams(&text);
    let elapsed = start.elapsed();

    assert!(!result.is_empty());
    // ... assertions unchanged
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`extract_max_weight_dedup` test does not verify max-wins over lower weight** - `ngram_tests.rs:131-141`
**Confidence**: 80%
- Problem: The test creates a single-entry weight table `(aa -> 9.0)` and feeds `"aaaa"`. Since the same bigram `"aa"` always looks up the same weight (9.0), the test verifies deduplication but not that the *maximum* weight wins when a bigram could have different weights at different positions. In the current implementation, `lookup_weight` returns the same value for the same key regardless of position, so max-weight dedup is effectively a no-op for document extraction. The test name suggests it validates max-weight semantics but it only validates count-dedup.
- Fix: This is inherent to the design (same key always maps to the same weight), so this is informational. Consider renaming or adding a doc comment clarifying that max-weight dedup is meaningful when weights are position-dependent (future extension) or simply documenting that dedup is by-key and weight is deterministic.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing test for unsorted weight table rejection** - `ngram_tests.rs` (Confidence: 70%) -- The `debug_assert!` in `extract_ngrams_with_weights` validates that the weight table is sorted, but there is no test verifying this assertion fires for unsorted input. A `#[test] #[should_panic]` test would catch regressions if the assertion is accidentally removed. Only matters in debug builds since `debug_assert!` is stripped in release.

- **No test for query extraction with all-whitespace input** - `ngram_tests.rs` (Confidence: 65%) -- Document extraction has `extract_whitespace_only` but query extraction lacks an equivalent. `extract_query_ngrams_with_weights("   ", &w)` exercises a code path where `token_border_ranges` returns empty ranges but there are still byte-level bigrams. The behavior may differ from empty string.

- **Covering-set test uses string matching which may over-count coverage** - `ngram_tests.rs:303-310` (Confidence: 65%) -- The verification loop matches ngrams by value across all positions. If the same ngram value appears at multiple positions in the query, coverage is attributed to ALL positions where that bigram occurs, not just the position the covering-set selected. This could mask a bug where the covering set selects an ngram at position X but the test credits positions Y and Z.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite is well-structured with clear cycle organization (Ngram type -> Document extraction -> Border detection -> Query extraction -> API wiring -> Performance). The 35 tests cover boundary conditions (empty, single char, UTF-8, CJK), behavioral contracts (deduplication, sort order, covering set), and performance. However, two HIGH findings weaken confidence in the query extraction tests: the covering-set verification has a conditional that can skip positions, and the border-weight comparison can silently pass without asserting its core property. Both should be tightened before merge.
