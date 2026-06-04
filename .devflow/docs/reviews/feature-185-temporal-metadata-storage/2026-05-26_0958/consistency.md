# Consistency Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58
**PR**: #253

## Issues in Your Changes (BLOCKING)

### HIGH

**META_LAST_UPDATED doc comment says ISO-8601 but code writes Unix epoch seconds** - `crates/rskim-search/src/temporal/storage.rs:52`
**Confidence**: 95%
- Problem: The doc comment for `META_LAST_UPDATED` reads: "Key storing the ISO-8601 UTC timestamp of the last successful `TemporalDb::sync`." However, in `storage_ops.rs:308-312`, the `sync` method writes a raw Unix epoch integer (`SystemTime::now().duration_since(UNIX_EPOCH).as_secs().to_string()`), not an ISO-8601 string like `"2026-05-25T21:44:49Z"`. This is a documentation-code mismatch that will mislead consumers of the `meta` table.
- Fix: Either change the doc comment to match the implementation:
  ```rust
  /// Key storing the Unix epoch timestamp (seconds) of the last successful [`TemporalDb::sync`].
  pub const META_LAST_UPDATED: &str = "last_updated";
  ```
  Or change the implementation to produce ISO-8601 (as documented):
  ```rust
  use chrono::Utc;
  let now_iso = Utc::now().to_rfc3339();
  meta_stmt.execute(params![META_LAST_UPDATED, now_iso]).map_err(db_err)?;
  ```
  The first option is simpler since the test in `storage_perf_tests.rs:131` already parses it as a `u64`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`db_err` signature deviates from `gix_err` pattern** - `crates/rskim-search/src/temporal/storage.rs:65`
**Confidence**: 82%
- Problem: The established error-conversion pattern in the same temporal module is `gix_err` in `git_parser.rs:49`:
  ```rust
  #[inline]
  fn gix_err(e: impl std::fmt::Display) -> SearchError {
      SearchError::Git(e.to_string())
  }
  ```
  The new `db_err` takes a concrete `rusqlite::Error` instead of `impl std::fmt::Display`, and omits `#[inline]`:
  ```rust
  pub(super) fn db_err(e: rusqlite::Error) -> SearchError {
      SearchError::Database(e.to_string())
  }
  ```
  While both work correctly, the inconsistency means the two error helpers in the same module family follow different generic conventions. The visibility difference (`fn` vs `pub(super)`) is justified by the cross-file split, but the signature and `#[inline]` difference are gratuitous.
- Fix: Align `db_err` with the `gix_err` pattern:
  ```rust
  #[inline]
  pub(super) fn db_err(e: impl std::fmt::Display) -> SearchError {
      SearchError::Database(e.to_string())
  }
  ```

**Redundant `#![allow(clippy::unwrap_used)]` in storage test files** - `crates/rskim-search/src/temporal/storage_tests.rs:6`, `crates/rskim-search/src/temporal/storage_perf_tests.rs:8`
**Confidence**: 85%
- Problem: The `#[allow(clippy::unwrap_used)]` is applied on the outer `mod` declaration in `storage.rs:215,220`, AND as inner attributes `#![allow(clippy::unwrap_used)]` at the top of each test file. This is redundant -- the outer attribute already suppresses the lint for the entire module. The existing pattern in `scoring.rs:272` applies the allow only on the outer `mod` declaration, and `scoring_tests.rs` has no inner `#![allow]`. The storage test files deviate from this established pattern.
- Fix: Remove the inner `#![allow(clippy::unwrap_used)]` from `storage_tests.rs:6` and `storage_perf_tests.rs:8` to match the `scoring_tests.rs` pattern.

**`#[must_use]` uses custom messages, deviating from crate convention** - `crates/rskim-search/src/temporal/storage_ops.rs:126,154,185,214`, `crates/rskim-search/src/temporal/storage.rs:202`
**Confidence**: 80%
- Problem: Every other `#[must_use]` in the `rskim-search` crate uses bare `#[must_use]` without a custom message string (checked across `types.rs`, `ngram.rs`, `weights.rs`, `scoring.rs`). The new storage code uses `#[must_use = "load_hotspots returns a Result; use or propagate the rows"]` and similar verbose messages. While Rust supports custom messages, this is a style inconsistency within the crate.
- Fix: Use bare `#[must_use]` to match existing crate convention:
  ```rust
  #[must_use]
  pub fn load_hotspots(&self) -> Result<Vec<HotspotRow>> {
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`sync` duplicates SQL from individual `store_*` methods** - `crates/rskim-search/src/temporal/storage_ops.rs:250-325` (Confidence: 70%) -- The `sync` method contains copy-pasted DELETE+INSERT SQL that is identical to `store_hotspots`, `store_risks`, and `store_cochanges`. If the SQL changes in one place but not the other, they will drift. Consider factoring the inner loop bodies into private helper methods called by both paths. However, the current approach has the advantage of being explicit within a single transaction scope, so this may be an intentional trade-off.

- **`temp_db()` helper duplicated across two test files** - `crates/rskim-search/src/temporal/storage_tests.rs:19-24`, `crates/rskim-search/src/temporal/storage_perf_tests.rs:24-29` (Confidence: 65%) -- Both test files define an identical `temp_db()` helper. In the scoring test file, shared test helpers live in one place. A shared test-utilities module would reduce duplication, but since these are test-only helpers in two files within the same module, the duplication is modest.

- **`scoring.rs` module doc comment not updated to mention new `compute_file_temporal_stats`** - `crates/rskim-search/src/temporal/scoring.rs:1-11` (Confidence: 62%) -- The top-of-file module doc says the module provides `decay_weight` and `compute_file_risk_scores` but does not mention `compute_file_temporal_stats`. The new function is documented individually but the module-level overview is stale.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 3 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The new `TemporalDb` persistence layer demonstrates strong consistency with existing patterns overall: the `#[path]` co-located test convention matches `scoring.rs`, the error-conversion helper follows the `gix_err` pattern (with minor signature differences), the module split (`storage.rs` / `storage_ops.rs` / `storage_types.rs`) mirrors the three-module pattern from `cochange/`, and the re-export chain through `mod.rs` -> `lib.rs` follows established conventions. The `SearchError::Database` variant follows the same `String`-wrapping pattern as `SearchError::Git`.

The one blocking issue is the doc-code mismatch on `META_LAST_UPDATED` (claims ISO-8601, stores Unix epoch). The three should-fix items are minor style inconsistencies that would benefit from alignment with existing crate patterns before merge.
