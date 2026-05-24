# Performance Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**O(n * r) linear scan in `is_border_bigram` called per candidate in query extraction** - `ngram.rs:254`
**Confidence**: 85%
- Problem: `is_border_bigram` (line 154-158) does a linear scan of `border_ranges` for every bigram position. In `extract_query_ngrams_with_weights`, this is called once per byte position in the query (line 254), making the border-detection phase O(n * r) where n = query length and r = number of border ranges. For typical short queries this is negligible, but the function has no input length guard and the API is public -- a caller could pass a long string (e.g., full file contents used as a "query"), causing quadratic-like behavior as r grows proportionally with n.
- Fix: For short queries (typical search use), this is acceptable. For robustness, either (a) sort `border_ranges` and use binary search to find overlapping ranges in O(log r), or (b) precompute a `Vec<bool>` bitmap marking border positions in O(n + r) and check the bitmap in O(1) per candidate. The bitmap approach is simplest:
  ```rust
  // Replace is_border_bigram linear scan with a precomputed bitmap
  let mut is_border = vec![false; bytes.len()];
  for &(lo, hi) in &border_ranges {
      for pos in lo..hi {
          is_border[pos] = true;
      }
  }
  // Then in the map closure:
  let at_border = is_border[pos] || is_border[pos + 1];
  let multiplier = if at_border { BORDER_MULTIPLIER } else { 1.0_f32 };
  ```

**O(n) full-coverage check on every iteration of greedy covering set** - `ngram.rs:277`
**Confidence**: 90%
- Problem: Inside the greedy covering-set loop (line 271-280), `covered.iter().all(|&c| c)` scans the entire `covered` vector on every iteration to check if all positions are covered. This makes the covering-set phase O(n * c) where n = query length and c = number of candidates selected. For a query of length L, there are L-1 candidates and up to ~L/2 selections, so worst case is O(L^2). Again, short queries mask this, but the public API has no length guard.
- Fix: Maintain a counter of uncovered positions and decrement it when marking new positions as covered:
  ```rust
  let mut covered = vec![false; bytes.len()];
  let mut uncovered_count = bytes.len();
  let mut selected: Vec<(Ngram, f32)> = Vec::new();

  for (ngram, w, pos) in candidates {
      if !covered[pos] || !covered[pos + 1] {
          if !covered[pos] {
              covered[pos] = true;
              uncovered_count -= 1;
          }
          if !covered[pos + 1] {
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
  This reduces the coverage check from O(n) to O(1) per iteration.

### MEDIUM

**`HashMap` capacity heuristic uses `min(len, 256)` which may under-size for large inputs** - `ngram.rs:188`
**Confidence**: 80%
- Problem: `HashMap::with_capacity(bytes.len().min(256))` caps the pre-allocation at 256 entries. For a 60KB file (the benchmark case), there are ~16K unique bigrams possible, and the commit message mentions 16,325 unique bigrams in the production corpus. With a cap of 256, the HashMap will need to rehash and resize multiple times as it grows from 256 to its final size. The pre-sizing commit (4f4ddb9) intended to avoid allocation overhead but the cap limits its effectiveness.
- Fix: Use a higher cap or remove it. Since the maximum possible unique bigrams is 65,536 (all u16 values) and the typical corpus has ~16K, a cap of 4096 or even `bytes.len().min(16384)` would be more appropriate:
  ```rust
  let capacity = if bytes.len() < 512 { bytes.len() } else { 4096 };
  ```
  Alternatively, since bigrams are u16 keys, you could use a flat array `[f32; 65536]` instead of a HashMap for zero-overhead lookup and dedup -- at a cost of 256KB stack/heap. This would eliminate hashing entirely.

**Performance sanity test has a flaky threshold boundary** - `ngram_tests.rs:379-384`
**Confidence**: 82%
- Problem: The release-mode assertion `elapsed.as_micros() < 2000` failed at 2070us during this review's test run (passed on subsequent warm-cache run). The threshold is tight enough that CPU frequency scaling, background load, or cold instruction caches cause intermittent failures. The commit 4f4ddb9 already adjusted this threshold once (from `as_millis() < 1` to `as_micros() < 2000`), suggesting ongoing flakiness.
- Fix: Either (a) increase the threshold to 5000us (5ms) which still validates sub-50ms performance per the project's requirements, or (b) run the timing loop multiple times and take the minimum to reduce noise:
  ```rust
  let mut best = std::time::Duration::from_secs(u64::MAX);
  for _ in 0..5 {
      let start = std::time::Instant::now();
      let _ = std::hint::black_box(extract_ngrams(&text));
      best = best.min(start.elapsed());
  }
  assert!(best.as_micros() < 3000, ...);
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`token_border_ranges` allocates a Vec on every call without capacity hint** - `ngram.rs:115`
**Confidence**: 80%
- Problem: `Vec::new()` starts with zero capacity. For a query with N tokens, there are up to 2*N ranges pushed. A small `with_capacity` based on a rough estimate (e.g., 8 or 16 for typical queries) would avoid reallocations for common cases.
- Fix: `let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(16);`

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Binary search on 16K-entry weight table for every bigram** - `ngram.rs:95-101` (Confidence: 65%) -- Each `lookup_weight` call does a binary search on the 16,117-entry `BIGRAM_WEIGHTS` table (~14 comparisons per lookup). For document extraction scanning all byte pairs in a file, this means millions of binary searches on large files. A perfect hash table or flat `[f32; 65536]` lookup array (256KB, initialized at startup) would reduce per-lookup cost from O(log n) to O(1). Only worth pursuing if profiling shows `lookup_weight` as a hot path.

- **Query extraction builds full candidate Vec before selecting** - `ngram.rs:248-262` (Confidence: 60%) -- `extract_query_ngrams_with_weights` materializes all (n-1) candidates into a Vec, sorts them, then selects a covering subset. For very long queries this is wasteful since the covering set is typically much smaller. A partial-sort or selection algorithm could avoid the full sort, but queries are typically short so the practical impact is minimal.

- **`covered.iter().all()` after `covered[pos]`/`covered[pos+1]` re-checks already-known positions** - `ngram.rs:277` (Confidence: 75%) -- Already addressed in the uncovered_count fix above; this is the same issue viewed from a redundancy angle.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The core algorithms are sound for the intended use case (short search queries and document indexing). The two HIGH-severity items -- O(n*r) border scan and O(n) coverage check per iteration -- are technically correct but contain quadratic complexity hidden behind the assumption that queries are short. Since the API is public with no documented input-size constraint, these should be hardened. The fixes are straightforward (bitmap + counter) and would make the code robust for any input size without changing the algorithm's semantics. The HashMap capacity and test flakiness are lower priority but worth addressing for reliability.
