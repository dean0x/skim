# Rust Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13T09:30

## Issues in Your Changes (BLOCKING)

### HIGH

**Misleading doc comment on `selectivity` function** - `crates/rskim-research/src/idf.rs:43-46`
**Confidence**: 95%
- Problem: The doc comment says "Look up the IDF weight of a bigram in a sorted weight table" and "Returns `None` if the bigram is not in the table", but the function is named `selectivity`, accepts a `query: &str`, and returns `f64` (a total score, not `Option`). The doc was likely copy-pasted from a single-bigram lookup function that was removed.
- Fix:
```rust
/// Compute cumulative IDF selectivity for a query string.
///
/// Sums the IDF weights of all byte-pair bigrams found in `query`.
/// Returns 0.0 if no bigrams match the weight table.
#[must_use]
pub fn selectivity(query: &str, weights: &[(u16, f32)]) -> f64 {
```

### MEDIUM

**`is_border_bigram` has overly broad matching logic** - `crates/rskim-research/src/validate.rs:87-88`
**Confidence**: 85%
- Problem: The condition `window[0] == first2[0] || window[0] == last2[0]` matches any bigram whose first byte equals the first or penultimate byte of *any* token in the query. For a query like `"fn parse"`, byte `b'p'` is `first2[0]` of `"parse"` -- so every bigram starting with `p` anywhere in the query gets the `BORDER_MULTIPLIER`. This is much broader than "overlaps the first or last 2 bytes of a token", which the doc claims. The semantic intent appears to be positional overlap with token boundaries, but the check compares byte values instead of positions.
- Fix: Either (a) remove lines 87-88 entirely and rely on the `window == first2 || window == last2` check, which correctly tests by value for the exact border pairs, or (b) rewrite using position-tracking to compare the actual byte offset of the window against token boundary positions in the original query string.

**`compute_idf` doc says ">= 1.0" but does not enforce it** - `crates/rskim-research/src/idf.rs:7-13`
**Confidence**: 82%
- Problem: The doc states "Returns a value >= 1.0" but when `total_docs = 0`, the formula yields `-inf` (from `ln(0)`). While this edge case is unlikely in practice (the caller always has `total_docs > 0` from a real corpus), the contract is violated. The downstream `codegen` validates `idf > 0.0` but not `>= 1.0`.
- Fix: Either update the doc to state the precondition ("total_docs must be > 0") or add a guard:
```rust
pub fn compute_idf(df: u32, total_docs: u32) -> f32 {
    debug_assert!(total_docs > 0, "total_docs must be positive for IDF computation");
    ((total_docs as f64) / ((df + 1) as f64)).ln() as f32 + 1.0
}
```

## Issues in Code You Touched (Should Fix)

_(none)_

## Pre-existing Issues (Not Blocking)

_(none)_

## Suggestions (Lower Confidence)

- **Large checked-in data file** - `crates/rskim-search/data/bigram_weights.json` (556 KB) and `crates/rskim-search/src/weights.rs` (344 KB, 9659 lines) (Confidence: 65%) -- These are large files checked into version control. The JSON is the source of truth and the `.rs` is generated from it. Consider adding `bigram_weights.json` to `.gitattributes` with `linguist-generated=true` and `weights.rs` with `linguist-generated=true` so they are collapsed in diffs and excluded from language statistics. This is cosmetic, not blocking.

- **`_temp_dir_guard` initialization pattern** - `crates/rskim-research/src/main.rs:104-113` (Confidence: 60%) -- The `let _temp_dir_guard;` declaration without initialization, assigned only in one match arm, relies on Rust's definite-assignment analysis. It compiles correctly but an `Option<TempDir>` pattern would be more readable and idiomatic: `let _temp_dir_guard: Option<tempfile::TempDir>;` then assign `Some(td)` / `None`.

- **`chrono_now` uses `.unwrap_or(0)` on `SystemTime`** - `crates/rskim-research/src/main.rs:283-284` (Confidence: 60%) -- `SystemTime::now().duration_since(UNIX_EPOCH)` only fails if the clock is before Unix epoch. The `unwrap_or(0)` silently maps that to epoch zero. This is fine for a developer tool but inconsistent with the crate's `deny(clippy::unwrap_used)` philosophy -- consider noting this as an intentional silent fallback.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The crate demonstrates strong Rust practices overall: proper use of `anyhow` for error propagation, `#[must_use]` annotations on pure functions, clippy deny lints for `unwrap_used`/`expect_used`/`panic`, well-structured trait abstraction (`FileSource`) with dependency injection for testing, comprehensive test coverage (34 tests), and good use of workspace dependencies. The `thiserror` vs `anyhow` distinction is correctly applied (this is an application/tool crate, not a library). The one HIGH issue is a misleading doc comment that should be fixed before merge. The two MEDIUM issues (overly broad border detection logic producing unreliable validation metrics, and an unenforced contract on `compute_idf`) are worth addressing but not individually merge-blocking given this is a `publish = false` developer tool.
