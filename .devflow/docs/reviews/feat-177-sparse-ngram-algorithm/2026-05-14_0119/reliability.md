# Reliability Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14T01:19

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Covering-set loop performs O(n) full scan on every iteration** - `ngram.rs:277`
**Confidence**: 85%
- Problem: Inside the greedy covering-set loop (lines 271-280), the early-exit check `covered.iter().all(|&c| c)` performs a full O(n) linear scan of the `covered` vector on every candidate iteration. For a query of length `L`, this creates O(L^2) work in the worst case. While queries are typically short strings (bounded by user input), this is an implicit bound — there is no explicit upper limit on query length enforced at this function boundary, and the function is public API (`pub fn`).
- Fix: Track remaining uncovered positions with a counter instead of scanning:
```rust
// Greedy covering set.
let mut covered = vec![false; bytes.len()];
let mut uncovered_count = bytes.len();
let mut selected: Vec<(Ngram, f32)> = Vec::new();

for (ngram, w, pos) in candidates {
    let newly_covered_0 = !covered[pos];
    let newly_covered_1 = !covered[pos + 1];
    if newly_covered_0 || newly_covered_1 {
        if newly_covered_0 {
            covered[pos] = true;
            uncovered_count -= 1;
        }
        if newly_covered_1 {
            covered[pos + 1] = true;
            uncovered_count -= 1;
        }
        selected.push((ngram, w));
    }
    if uncovered_count == 0 {
        break;
    }
}
```
This replaces the O(n) per-iteration scan with an O(1) counter check.

---

**`is_border_bigram` performs linear scan over border ranges for every candidate** - `ngram.rs:154-158`
**Confidence**: 82%
- Problem: `is_border_bigram` is called once per bigram position (line 254) and performs a linear scan over all border ranges. For a query with `T` tokens, there are up to `2*T` ranges, and `L-1` bigram positions, yielding O(L*T) work in `extract_query_ngrams_with_weights`. This is bounded by input length (queries are strings) but the bound is implicit. For typical short queries this is negligible, but the function accepts arbitrary `&str` with no documented or enforced length limit.
- Fix: Since border ranges are small for typical queries, this is acceptable for now. For defense-in-depth, consider pre-computing a `Vec<bool>` bitmap of border positions instead:
```rust
let mut is_border = vec![false; bytes.len()];
for &(lo, hi) in &border_ranges {
    for pos in lo..hi {
        is_border[pos] = true;
    }
}
// Then in the candidate builder:
let is_at_border = (pos < is_border.len() && is_border[pos])
    || (pos + 1 < is_border.len() && is_border[pos + 1]);
```

---

**`debug_assert` for weight table sort-order is not checked in release builds** - `ngram.rs:182-185, 235-238`
**Confidence**: 85%
- Problem: The precondition that `weights` must be sorted by key is only checked via `debug_assert!`, which is elided in release builds. Since `extract_ngrams_with_weights` and `extract_query_ngrams_with_weights` are public API functions accepting arbitrary `&[(u16, f32)]` slices, a caller passing an unsorted weight table would silently produce incorrect results (binary search returning wrong indices). The reliability pattern "Assert preconditions and invariants in production code, not just tests" applies here.
- Fix: For the `_with_weights` variants that accept caller-provided data, either:
  - (A) Add a release-mode assertion at the module boundary (since these are boundary functions accepting external data):
    ```rust
    assert!(
        weights.windows(2).all(|w| w[0].0 <= w[1].0),
        "weights must be sorted by key"
    );
    ```
  - (B) Or return `Result` instead of `Vec` so callers handle the invalid-input case. Given the convenience wrappers already pass the known-sorted `BIGRAM_WEIGHTS`, the `_with_weights` variants are the boundary where untrusted data enters — assert there.
  
  Note: The O(n) assert cost on every call may be unacceptable for hot paths. If so, consider a newtype `SortedWeights` that validates on construction (parse-don't-validate pattern), amortizing the check.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No upper bound on HashMap capacity hint** - `ngram.rs:188` (Confidence: 65%) — `bytes.len().min(256)` caps at 256, which is a reasonable heuristic. However, for very large documents the actual unique bigram count is at most 65,536 (all possible u16 values), while the HashMap could still rehash multiple times for inputs between 256 and 65K unique bigrams. Consider `bytes.len().min(65536)` or a tighter estimate, though the current cap is pragmatically fine for most inputs.

- **Single-char token border range extends before position 0 via saturating_sub** - `ngram.rs:132` (Confidence: 62%) — For a single-byte token at the very start of the string (position 0), `start.saturating_sub(1)` produces `lo=0`, meaning the border range is `[0, 1)` which has length 1. This is correct behavior (no negative indices), but the asymmetry with tokens at position 0 vs. later positions could produce subtly different border weighting. The test `border_ranges_single_byte_token` covers this case, so it appears intentional.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. The O(n) full-scan in the covering-set loop (line 277) should be replaced with an O(1) counter — this is a straightforward improvement with no behavioral change.
2. Consider promoting the `debug_assert!` on weight sort-order to a release-mode check or adopting a validated newtype, since the `_with_weights` functions are public API accepting caller-provided data.

### Strengths

- All loops are bounded by input length (no external I/O, no unbounded retries)
- Pre-sized HashMap allocation via `with_capacity` shows allocation discipline
- Early returns for empty/single-char edge cases prevent unnecessary work
- `#[must_use]` on all public functions prevents silently discarding results
- `f64` intermediate accumulation for weight multiplication avoids precision loss
- Comprehensive test suite (382 tests) covers edge cases including UTF-8, CJK, empty input, whitespace-only
- Performance sanity test with explicit threshold guards against regression
- No `unsafe` code, no panicking paths, no unwrap outside tests
