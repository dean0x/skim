# Consistency Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### HIGH

**Integer type mismatch between `FileTemporalStats` (u32) and `HotspotRow`/`RiskRow` (i64)** - `crates/rskim-search/src/temporal/storage_types.rs:18-20`, `crates/rskim-search/src/types.rs:284-290`
**Confidence**: 90%
- Problem: `FileTemporalStats` uses `u32` for `changes_30d`, `changes_90d`, `total_commits`, and `fix_commits`, but the SQLite row types `HotspotRow` and `RiskRow` use `i64` for the semantically identical fields (`changes_30d`, `changes_90d`, `total_commits`, `fix_commits`). These two structs represent the same data at different layers (in-memory computation vs. persistence), yet use incompatible integer widths. When converting from `FileTemporalStats` to `HotspotRow`/`RiskRow` at the sync boundary, callers must perform lossy `u32` -> `i64` widening (safe) or `i64` -> `u32` narrowing (potentially lossy). The type mismatch makes the relationship between the two layers non-obvious and requires manual conversion at every use site.
- Fix: Either standardize both on `u32` (with `i64::from(u32)` at the SQLite boundary where rusqlite expects `i64`) or standardize both on `i64`. Standardizing on `u32` in the domain types and widening only at the SQLite insertion point is the more idiomatic Rust approach since commit counts cannot be negative:

```rust
// storage_types.rs - use u32 to match FileTemporalStats
pub struct HotspotRow {
    pub file_path: String,
    pub score: f64,
    pub changes_30d: u32,  // was i64
    pub changes_90d: u32,  // was i64
}

pub struct RiskRow {
    pub file_path: String,
    pub risk_score: f64,
    pub total_commits: u32,  // was i64
    pub fix_commits: u32,    // was i64
    pub fix_density: f64,
}

pub struct CochangeRow {
    pub file_a: String,
    pub file_b: String,
    pub count: u32,  // was i64
    pub jaccard: f64,
}

// In storage_ops.rs insert helpers, widen at the boundary:
stmt.execute(params![row.file_path, row.score, i64::from(row.changes_30d), i64::from(row.changes_90d)])
```

### MEDIUM

**`db_err` visibility is `pub(super)` while analogous `gix_err` is private** - `crates/rskim-search/src/temporal/storage.rs:66`
**Confidence**: 82%
- Problem: The existing `gix_err` helper in `git_parser.rs` (line 49) uses bare `fn` (private to the module). The new `db_err` helper uses `pub(super)`, leaking it to the parent `temporal` module. The doc comment explicitly says "Private to this module -- rusqlite types must not leak into the public API," yet the visibility is wider than necessary. The `storage_ops` sub-module accesses it via `super::db_err`, which works because `storage_ops` is a `#[path]` child of `storage.rs`. Since `storage_ops` is a child module within `storage`, plain `pub(crate)` or even a re-export would be needed if it were a sibling -- but with the current `#[path]` nesting, `pub(super)` is technically correct. However, it deviates from the established `gix_err` pattern and the stated intent in the doc comment.
- Fix: Since `storage_ops.rs` is declared as a submodule of `storage.rs` via `#[path]`, `pub(in crate::temporal::storage)` would be the most precise visibility. If that is too verbose, keeping `pub(super)` with an updated doc comment that acknowledges the `storage_ops` sub-module access is acceptable. The key point is the doc comment should match the actual visibility.

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **Duplicate `temp_db()` helper across two test files** - `crates/rskim-search/src/temporal/storage_tests.rs:17-22`, `crates/rskim-search/src/temporal/storage_perf_tests.rs:22-27` (Confidence: 65%) -- The identical `temp_db()` helper function is defined in both `storage_tests.rs` and `storage_perf_tests.rs`. The crate's `cochange` module uses a shared `test_helpers` sub-module for this pattern. A shared `storage_test_helpers.rs` would reduce duplication, though the current approach works fine for two files.

- **`#[path]` attribute for non-test submodules is unique to this module** - `crates/rskim-search/src/temporal/storage.rs:24,31` (Confidence: 62%) -- Other modules in the crate use `#[path]` only for co-located test files. The `storage` module uses it for production submodules (`storage_types.rs`, `storage_ops.rs`) as well. This is a valid Rust pattern but structurally distinct from the rest of the crate where production submodules use standard `mod` declarations with directory-based file layout. Not a defect, but worth noting as a minor structural inconsistency.

- **Test module allows only `clippy::unwrap_used` but `scoring_tests.rs` allows both `unwrap_used` and `expect_used`** - `crates/rskim-search/src/temporal/storage.rs:220,225` (Confidence: 60%) -- The storage test modules only allow `clippy::unwrap_used`, while the scoring test module allows both `clippy::unwrap_used` and `clippy::expect_used`. Currently the storage tests do not use `.expect()`, so this is not a bug, but it is a minor inconsistency in the allow-list pattern. If `.expect()` is added later, it would require updating the allow attribute.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The new storage layer is well-structured overall. Naming conventions match the feature knowledge expectations (`CochangeRow`, `HotspotRow`, `RiskRow`; `store_`/`load_` naming; `SearchError::Database` wrapping). Error handling consistently uses the `db_err` helper, mirroring the `gix_err` pattern. Doc comments follow the established `# Errors` / `# Parameters` / `# Returns` convention. The primary consistency issue is the `u32` vs `i64` type mismatch for commit count fields between the in-memory computation types and the persistence row types, which will create friction at the conversion boundary when these layers are wired together.
