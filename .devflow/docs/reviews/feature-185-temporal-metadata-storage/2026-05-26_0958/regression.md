# Regression Review Report

**Branch**: feature-185-temporal-metadata-storage -> main
**Date**: 2026-05-26T09:58

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**META_LAST_UPDATED doc says ISO-8601 but implementation stores Unix epoch seconds** - `crates/rskim-search/src/temporal/storage.rs:52`
**Confidence**: 90%
- Problem: The doc comment on `META_LAST_UPDATED` reads "Key storing the ISO-8601 UTC timestamp" but `sync()` in `storage_ops.rs:308-312` stores `SystemTime::now().duration_since(UNIX_EPOCH).as_secs().to_string()` -- a plain integer string like `"1748200000"`, not ISO-8601 (`"2026-05-25T21:44:49Z"`). The test at `storage_perf_tests.rs:131` confirms the value is parsed as `u64`, corroborating the mismatch. Any downstream consumer relying on the doc comment to parse the value as ISO-8601 will fail.
- Fix: Either change the doc comment to say "Unix epoch seconds" or change the implementation to actually store ISO-8601. Since the test expects `u64`, updating the doc is the least disruptive option:
  ```rust
  /// Key storing the Unix epoch timestamp (seconds) of the last successful [`TemporalDb::sync`].
  pub const META_LAST_UPDATED: &str = "last_updated";
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SearchError` enum lacks `#[non_exhaustive]`; adding `Database` variant is technically semver-breaking** - `crates/rskim-search/src/types.rs:561`
**Confidence**: 82%
- Problem: The `SearchError` enum is `pub` and does not carry `#[non_exhaustive]`. Adding the `Database(String)` variant is a semver-breaking change for any external crate that has an exhaustive `match` on `SearchError`. Within this workspace there are no exhaustive matches (confirmed by grep), and the PR description explicitly notes this. However, this is the second new variant added in recent PRs (`CapacityExceeded` in #251, `Database` in #253). Each addition is a theoretical breaking change for downstream consumers of the `rskim-search` crate on crates.io.
- Fix: Add `#[non_exhaustive]` to `SearchError` to make future variant additions non-breaking. This is a one-time semver break that prevents all future ones:
  ```rust
  #[derive(Debug, Error)]
  #[non_exhaustive]
  pub enum SearchError { ... }
  ```
  Since the crate is pre-1.0 and the CLAUDE.md confirms rapid evolution, this is a should-fix, not a blocker.

**Behavioral difference: `compute_file_temporal_stats` deduplicates files per commit, `compute_file_risk_scores` does not** - `crates/rskim-search/src/temporal/scoring.rs:243-247` vs `scoring.rs:137-147`
**Confidence**: 80%
- Problem: The new `compute_file_temporal_stats` uses a `HashSet<String>` per-commit to deduplicate file paths (a file listed twice in one commit counts once). The existing `compute_file_risk_scores` does NOT deduplicate -- a file appearing twice in one commit gets double the decay weight. This is documented in the new function's doc comment and tested, so it appears intentional. However, when the two functions are called on the same `CommitInfo` slice, they will produce different counts for files that appear multiple times in a single commit's `changed_files`. This asymmetry could confuse callers who expect both functions to agree on "how many times file X was touched."
- Fix: If intentional (likely -- raw counts should be accurate, while weighted scores are more tolerant of duplicates), add a brief doc comment on `compute_file_risk_scores` noting that it does NOT deduplicate, so the design asymmetry is explicitly documented in both directions.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`unchecked_transaction` vs `transaction` in store methods** - `crates/rskim-search/src/temporal/storage_ops.rs:30,54,84,257` (Confidence: 65%) -- `unchecked_transaction()` bypasses Rust's borrow checker for the connection, allowing `&self` methods to start transactions. This works correctly when only one transaction is active at a time, but the safety invariant is not compiler-enforced. If a future caller nests store calls or adds concurrent access, the unchecked variant would silently allow undefined behavior. The `TemporalDb` is documented as not `Sync`, which mitigates multi-thread risk.

- **`compute_file_temporal_stats` allocates a `String` per file per commit** - `crates/rskim-search/src/temporal/scoring.rs:245` (Confidence: 62%) -- `file.path_str().into_owned()` allocates a new `String` for every file in every commit, even if the path was already seen in a previous commit. `compute_file_risk_scores` uses a borrow-first-then-own pattern (`path_cow`/`accum.get_mut`) that avoids this. For large histories this could be measurably slower, though for the current use case (persistence, not hot-loop ranking) it is likely acceptable.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR adds new functionality (new `SearchError::Database` variant, `FileTemporalStats` type, `compute_file_temporal_stats` function, and the full `TemporalDb` SQLite persistence layer) without removing or modifying any existing exports, return types, or default behaviors. All 437 tests pass including the full downstream integration suite. The new `Database` variant on `SearchError` is non-breaking in practice (no exhaustive matches exist). The doc/impl mismatch on `META_LAST_UPDATED` should be corrected before merge to prevent downstream confusion. The deduplication asymmetry and `#[non_exhaustive]` suggestion are lower priority but worth addressing.
