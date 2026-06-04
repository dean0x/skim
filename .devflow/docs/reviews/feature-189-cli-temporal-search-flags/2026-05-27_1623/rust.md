# Rust Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23

## Cross-Cycle Awareness

Cycle 2 resolved 19/23 issues (4 FP for top-N structural duplication below refactoring threshold). This cycle 3 review focuses on new patterns and verifies no regressions were introduced by the fixes.

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`resort_partners_by_temporal` accepts `&mut Vec` instead of `&mut [T]`** - `temporal.rs:278` (Confidence: 70%) -- The function signature takes `&mut Vec<CochangeRow>` but only because it replaces the contents via `*partners = temp`. This is an acceptable pattern when the function needs to reorder elements, though an in-place permutation would avoid the intermediate allocation. Below the refactoring threshold given the small window sizes involved (max 500 from `limit * 5` clamped).

- **Redundant post-filter in `NgramIndexReader::search`** - `reader.rs:391-398` (Confidence: 65%) -- The blast-radius `file_filter` is applied both in the first sub-pass (line 362-366, skipping doc_ids during TF accumulation) and again after scoring (line 391-398, filtering `doc_scores`). The second filter is a defense-in-depth measure -- if a doc_id somehow bypasses the first filter (e.g., a future code path modification), the second filter catches it. This is conservative but correct. The comment at line 390 documents the intent.

- **`apply_temporal_enrichment` returns `Result` but is infallible** - `temporal.rs:556-603` (Confidence: 72%) -- The function signature is `-> anyhow::Result<()>` but the body always returns `Ok(())`. The `annotate_hotspots`/`annotate_risks` helpers catch DB errors internally via `eprintln!` and continue. The `Result` return type is reasonable for forward compatibility (future enrichment steps may fail), but currently the `?` is never exercised.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### Ownership and Borrowing

The code follows Rust borrowing conventions well throughout:

- `normalize_blast_radius_path` accepts `&str` and `&Path` (borrows, not owned).
- `cochange_partner` returns `&'a str` with a lifetime tied to the `CochangeRow`, avoiding clones.
- `apply_temporal_enrichment` takes `&mut [ResolvedResult]` (borrow of slice, not Vec).
- The `temporal_annotation_tag` helper borrows `Option<&TemporalAnnotation>` cleanly.
- `cochange_partner_paths` collects into a `HashSet<String>` -- the `.to_string()` allocation is necessary here since the rows may be dropped before the set is consumed.
- The per-file DB lookup pattern (`hotspot_for_file`, `risk_for_file`, `cochanges_for_file`) uses `&str` parameters consistently.

### Error Handling

Error handling follows the project conventions:

- Library layer (`rskim-search`): uses `Result<T, SearchError>` with `db_err` wrapper for rusqlite errors. The `QueryReturnedNoRows` -> `Ok(None)` pattern in per-file lookups is idiomatic.
- CLI layer (`rskim`): uses `anyhow::Result` for application-level errors. Path normalization produces clear contextual errors ("blast-radius file not found: {path}").
- Graceful degradation: `open_temporal_db` returns `Option<TemporalDb>`, allowing callers to degrade without error when the database is missing. `annotate_hotspots`/`annotate_risks` catch per-file errors and emit warnings rather than aborting the entire enrichment.
- The `unwrap_or(Duration::ZERO)` in `storage_ops.rs:567` for `SystemTime::now()` is the established pattern in this codebase.

### Type Safety

- `FileId(u32)` newtype is used consistently for file IDs. The `idx as u32` cast in `query.rs:79` is safe because `DEFAULT_MAX_FILES` (50,000) and `file_count: u32` in the index header both bound the index below `u32::MAX`.
- `TemporalSort` enum with `Copy + Eq` derives enables value comparisons in sort logic without clones.
- `TemporalAnnotation` uses `Option<f64>` per field with `skip_serializing_if = "Option::is_none"` -- clean typed JSON serialization.
- `TemporalQueryOutput` enum uses proper variants (`Hotspots`, `Coldspots`, `Risks`, `Cochanges`) with variant-specific data, making illegal states unrepresentable.
- The typed JSON serialization structs (`HotspotJsonRow`, `RiskJsonRow`, `CochangeJsonRow`) with `#[derive(Serialize)]` prevent field name drift vs. hand-built `serde_json::json!()`.

### Concurrency Safety

- `TemporalDb` is `Send` but not `Sync` (documented in SAFETY comments). The `unchecked_transaction()` calls in `store_*` and `sync` methods are safe because `&self` borrows are exclusive within a single thread.
- No `std::fs` calls in async contexts (all DB operations are synchronous and called from synchronous `run_*` functions).

### Schema Migration

- The v1 -> v2 migration adds performance indexes (`idx_hotspot_score`, `idx_risk_score`, `idx_cochange_file_b`) in an idempotent `CREATE INDEX IF NOT EXISTS` pattern within a transaction.
- The `PRAGMA user_version = 2` is set inside the migration transaction, ensuring atomic version bumps.
- Forward-compatibility: opening a v2 database with older code produces a clear error message ("database schema version {version} is newer than supported").
- The migration test (`v1_database_migrates_to_v2_on_reopen`) manually creates a v1 database and verifies the upgrade path.

### SQL Safety

- All queries use parameterized `?1` placeholders via `rusqlite::params![]` -- no string interpolation.
- The `UNION ALL` optimization in `cochanges_for_file` is well-documented: canonical ordering (`file_a < file_b`) guarantees no self-pairs, making `UNION ALL` safe (no deduplication needed). The `LIMIT 10000` bounds memory.
- Top-N queries clamp `limit` to `MAX_ROWS_PER_TABLE` (500,000) before the `as i64` cast, preventing integer overflow.

### Test Coverage

The PR adds comprehensive test coverage:
- Per-file lookups: empty table, present row, absent row, bidirectional co-change.
- Top-N queries: sort order, limit, empty tables, `usize::MAX` overflow.
- Schema migration: v1 -> v2 upgrade path.
- Path normalization: relative, absolute, outside-repo, nonexistent, dot-slash stripping.
- Flag parsing: all temporal flags, mutual exclusion, composability, missing values.
- Standalone dispatch: all sort modes, blast-radius, limit, empty DB.
- Combined enrichment: hot/cold/risky sort, missing files sort last.
- Output formatting: text and JSON for all variants, empty table branches.
- Staleness check: current HEAD, mismatched HEAD.
