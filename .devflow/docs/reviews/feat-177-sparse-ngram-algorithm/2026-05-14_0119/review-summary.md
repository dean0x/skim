# Code Review Summary

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14_0119
**PR**: #222

## Merge Recommendation: CHANGES_REQUESTED

**Critical Issue**: Two HIGH-confidence test weaknesses in query extraction prevent merge. Both testing findings weaken confidence in the covering-set algorithm's correctness. After addressing the blocking test issues, four MEDIUM-severity findings across architecture, performance, and reliability should be fixed for quality.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking (Your Changes) | 0 | 3 | 6 | 0 |
| Should Fix (Code Touched) | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |
| **Total** | **0** | **3** | **8** | **0** |

**Confidence-Boosted Items**: Three issues flagged by 2+ reviewers with confidence boost:
- O(n) covering-set termination check: 85-90% (4 reviewers) → confidence +40% (accumulated)
- `is_border_bigram` linear scan: 80-85% (3 reviewers) → confidence +20% (accumulated)
- Duplicate `lookup_weight` logic: 80-90% (3 reviewers) → confidence +30% (accumulated)

---

## Blocking Issues (Your Changes)

### HIGH SEVERITY

**1. Query extraction covering-set test has conditional that skips coverage verification** - `ngram_tests.rs:296-319`
**Confidence**: 85% (Testing reviewer)
- **Problem**: `query_extract_covering_set_covers_positions` only verifies coverage for positions where the bigram exists in the synthetic weight table. The conditional check `if w.binary_search_by_key(...).is_ok()` means positions covered by bigrams falling back to `DEFAULT_WEIGHT` are never asserted. The function contract states it covers ALL byte positions, but the test does not fully verify this guarantee for unknown-weight bigrams.
- **Impact**: Cannot confirm the covering-set heuristic's core property (coverage completeness) before merge.
- **Fix**: Remove the conditional and assert coverage of all byte positions unconditionally:
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

**2. Border weight comparison test silently passes on missing bigram** - `ngram_tests.rs:263-281`
**Confidence**: 82% (Testing reviewer)
- **Problem**: `query_extract_border_bigrams_have_higher_weight` uses `if let Some(ai_entry) = ai_entry` to guard the comparison. If `"ai"` is not selected by the covering-set heuristic, the test silently passes without asserting the core property (border bigrams outweigh interior bigrams). Since the covering set is a greedy selection, interior bigrams may be dropped, making this test vacuously true.
- **Impact**: Cannot confirm border-weighted selectivity works correctly; test passes even if the feature breaks.
- **Fix**: Assert both bigrams are selected before comparing:
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
          "'fn' border weight must exceed 'ai' interior weight"
      );
  }
  ```

**3. O(n) covering-set termination check on every loop iteration** - `ngram.rs:277`
**Confidence**: 85-90% (4 reviewers: Performance, Complexity, Reliability, Rust)
- **Problem**: Inside the greedy covering-set loop, `covered.iter().all(|&c| c)` performs a full O(n) scan on every candidate iteration. This creates O(n^2) work where n = query length. While queries are typically short, the function is public API with no documented input-size constraint. The pattern is inefficient and could degrade for unusual inputs.
- **Impact**: Unnecessary O(n) work on every iteration; potential quadratic complexity for pathological queries.
- **Fix**: Track uncovered position count with a counter instead of scanning:
  ```rust
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

---

## Should-Fix Issues (Code You Touched)

### MEDIUM SEVERITY

**4. Duplicated `lookup_weight` binary-search logic between ngram and weights modules** - `ngram.rs:95` and `weights.rs:16142`
**Confidence**: 90% (3 reviewers: Architecture, Consistency)
- **Problem**: `ngram.rs` defines a private `lookup_weight(key, weights)` that performs binary search on a `(u16, f32)` slice with `DEFAULT_WEIGHT` fallback. Meanwhile, `weights.rs` exports `bigram_weight(bigram: u16) -> Option<f32>` doing the same binary search. The private helper is more general (injectable weight table for testing), but duplicating the binary-search logic creates a maintenance risk — if weight encoding changes, two call sites need updating independently.
- **Impact**: DRY violation; maintenance burden if weight strategy changes; code smell of missing abstraction.
- **Fix**: Consolidate by adding a generic `lookup_weight` to `weights.rs` accepting arbitrary slices:
  ```rust
  // In weights.rs — add:
  #[must_use]
  #[inline]
  pub fn lookup_weight(key: u16, weights: &[(u16, f32)]) -> f32 {
      weights
          .binary_search_by_key(&key, |&(k, _)| k)
          .ok()
          .map(|idx| weights[idx].1)
          .unwrap_or(DEFAULT_WEIGHT)
  }

  // Then ngram.rs calls it instead of reimplementing
  ```

**5. Section separator style differs from crate convention** - `ngram.rs:23-25, 34-36, 87-89, 160-162, 210-212`
**Confidence**: 95% (Consistency reviewer)
- **Problem**: `ngram.rs` uses Unicode box-drawing lines (`// ─────...`) while every other file in both `rskim-search` and `rskim-core` uses ASCII equals-sign separators (`// ============...`). The `types.rs` file in the same crate uses the `=` pattern in 18 instances.
- **Impact**: Visual inconsistency; future contributors may not expect the pattern.
- **Fix**: Replace all `// ─────...` with `// ============...` to match crate-wide convention:
  ```rust
  // Before:
  // ─────────────────────────────────────────────────────────────────────────────
  // Constants
  // ─────────────────────────────────────────────────────────────────────────────

  // After:
  // ============================================================================
  // Constants
  // ============================================================================
  ```

**6. Derive trait ordering inconsistent** - `ngram.rs:45`
**Confidence**: 85% (Consistency reviewer)
- **Problem**: `Ngram` uses `#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]` with `Debug` last. Every other struct in `types.rs` puts `Debug` first: `#[derive(Debug, Clone, Copy, ...)]`.
- **Impact**: Minor style inconsistency; breaks crate convention.
- **Fix**: Reorder to match:
  ```rust
  #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
  pub struct Ngram(pub u16);
  ```

**7. O(n*r) linear scan in `is_border_bigram` called per candidate** - `ngram.rs:254`
**Confidence**: 80-85% (3 reviewers: Performance, Architecture, Reliability)
- **Problem**: `is_border_bigram` (line 154-158) does a linear scan of `border_ranges` for every bigram position in `extract_query_ngrams_with_weights`. This is O(n*r) where n = query bytes and r = token count. For typical short queries this is negligible, but the function is public with no input-size guard.
- **Impact**: Acceptable for typical queries but could degrade on unusual input; no documented size constraint.
- **Fix**: Precompute a bitmap marking border positions in O(n+r) and check in O(1):
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

**8. Public field on `Ngram` newtype weakens encapsulation** - `ngram.rs:46`
**Confidence**: 82% (Rust reviewer)
- **Problem**: `Ngram(pub u16)` exposes the raw inner value, allowing callers to construct arbitrary `Ngram` values via `Ngram(raw_u16)` and bypass `from_bytes`. The newtype pattern loses value when the field is public; internal code at line 198 already uses `Ngram(key)` assuming it's accessible.
- **Impact**: Encapsulation weakening; future refactorings blocked; couples consumers to encoding.
- **Fix**: Make the field private and add `from_raw` constructor:
  ```rust
  pub struct Ngram(u16);  // private

  impl Ngram {
      #[must_use]
      #[inline]
      pub(crate) fn from_raw(key: u16) -> Self {
          Self(key)
      }
  }
  // Then line 198: Ngram::from_raw(key) instead of Ngram(key)
  ```

**9. API re-export inconsistency** - `lib.rs:16`
**Confidence**: 85% (Architecture reviewer)
- **Problem**: `lib.rs` re-exports convenience wrappers `extract_ngrams` and `extract_query_ngrams` but not the `_with_weights` variants. The `_with_weights` functions are the core implementation accepting injectable weight tables — the testable, composable API. Downstream crates must reach through `rskim_search::ngram::extract_ngrams_with_weights` rather than `rskim_search::extract_ngrams_with_weights`.
- **Impact**: Inconsistent public surface; discoverability issue for the architecturally correct API.
- **Fix**: Re-export the `_with_weights` variants:
  ```rust
  pub use ngram::{
      BORDER_MULTIPLIER, Ngram,
      extract_ngrams, extract_ngrams_with_weights,
      extract_query_ngrams, extract_query_ngrams_with_weights,
  };
  ```

---

## Strengths & Positive Findings

✅ **Security**: 10/10 - Zero unsafe code, no panics in production, bounded allocations, no I/O, Snyk SAST scan clean.

✅ **Test Coverage**: 382 dedicated tests covering edge cases (empty, single-char, UTF-8, CJK, whitespace-only), behavioral contracts (deduplication, sort order), and performance.

✅ **Rust Practices**: Excellent newtype pattern (except for public field), `#[must_use]` annotations, `#[repr(transparent)]`, `debug_assert!` preconditions, zero-copy `&str` input, `f64` intermediate accumulation for numeric stability, clean test separation.

✅ **Architecture**: Strong separation of concerns (newtype, extraction, query logic), dependency injection via weight table parameters (enabling testability), Strategy Pattern for document vs. query extraction, pure-function design with no I/O.

✅ **Regression**: No lost functionality, no broken behavior, all 3,644 workspace tests passing, intent matches implementation.

✅ **Compatibility**: `cargo check -p rskim` passes; new public API surface is compatible with existing binary crate.

---

## Action Plan

**Before Merge (BLOCKING):**
1. Fix test coverage verification in `query_extract_covering_set_covers_positions` (remove conditional)
2. Fix border-weight test to assert both bigrams are present
3. Replace O(n) covering-set scan with uncovered-count counter

**Before Merge (SHOULD-FIX):**
4. Consolidate `lookup_weight` duplication with weights module
5. Replace Unicode section separators with ASCII `=` convention
6. Reorder `Ngram` derive traits to match crate convention
7. Consider bitmap optimization for border-position detection
8. Make `Ngram.0` field private and add `from_raw` constructor
9. Re-export `_with_weights` variants in `lib.rs`

**After Merge (OPTIONAL):**
- Promote `debug_assert!` on weight sort-order to release-mode check or validate-at-construction pattern
- Add test for unsorted weight table rejection
- Increase performance test threshold to 5000us or add warmup iterations

---

## Summary

This is a well-engineered feature with strong fundamentals: pure Rust, comprehensive test suite, zero unsafe code, clear separation of concerns, and excellent documentation. The three blocking issues are all test-correctness gaps that prevent confident verification of the covering-set algorithm's correctness. After fixing those, six medium-severity findings address architectural duplication, consistency, encapsulation, and performance optimization. None of these represent correctness risks for current usage—they are code quality and maintainability improvements. The implementation is sound and ready for productive fixes.
