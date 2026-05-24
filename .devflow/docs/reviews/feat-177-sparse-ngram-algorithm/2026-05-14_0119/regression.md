# Regression Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14
**PR**: #222

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Duplicate `BORDER_MULTIPLIER` and `token_border_ranges` logic across crates** - `crates/rskim-research/src/validate.rs:20` and `crates/rskim-search/src/ngram.rs:32` (Confidence: 65%) -- Both crates define `BORDER_MULTIPLIER` (f64 3.5 in research, f32 3.5 in search) and independent `token_border_ranges` implementations. If the constant or algorithm drifts in one crate, the other silently diverges. Consider importing from the canonical `rskim-search` crate in `rskim-research` once search is stabilized. Not blocking because research is a dev/offline crate, not a production dependency, and the values match today.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### 1. Lost Functionality -- PASS

No exports were removed. The branch is purely additive:
- Added: `pub mod ngram` declaration in `lib.rs`
- Added: `pub use ngram::{BORDER_MULTIPLIER, Ngram, extract_ngrams, extract_query_ngrams}` re-export
- All prior exports (`types::*`, `weights::*`) remain unchanged

### 2. Broken Behavior -- PASS

- **No existing signatures changed.** No function parameters added, removed, or reordered.
- **No return types changed.** All existing public functions retain their original signatures.
- **No default values changed.** `DEFAULT_WEIGHT` in `weights.rs` is untouched.
- **`weights.rs` changes are formatting-only.** The diff shows ~9,600 insertions and ~8,960 deletions, but word-level diff confirms the only semantic change is `cargo fmt` reformatting of the `assert_eq!` macro in the test at the top of the file. All 10,000+ weight tuples retain identical keys and values -- trailing whitespace was added before inline comments.

### 3. Intent vs Reality -- PASS

PR description states: "Wave 1 bigram extraction with border-weighted selectivity for the sparse n-gram search index." The implementation matches:
- `Ngram` newtype with encode/decode roundtrip
- `extract_ngrams` / `extract_ngrams_with_weights` for document extraction with max-weight dedup
- `extract_query_ngrams` / `extract_query_ngrams_with_weights` for query extraction with border-weighted covering-set heuristic
- f64 intermediate accumulation for border weight multiplication (line 259)
- 59 dedicated tests (391 lines in `ngram_tests.rs`)

No TODO/FIXME/HACK/XXX markers found in the new code.

### 4. Incomplete Migrations -- PASS (N/A)

This is net-new code, not a migration. No old API was deprecated or replaced.

### 5. Compile-Time Canary -- PASS

The `rskim` binary crate lists `rskim-search` as a compile-time canary dependency. `cargo check -p rskim` passes, confirming the new public API surface is compatible.

### 6. Full Test Suite -- PASS

All 3,644 workspace tests pass (59 in rskim-search, remainder in rskim-core and rskim). No test regressions detected.
