# Database Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`cochanges_for_file` OR query cannot use indexes efficiently** - `crates/rskim-search/src/temporal/storage_ops.rs:156-158`
**Confidence**: 85%
- Problem: The query `WHERE file_a = ?1 OR file_b = ?1` forces SQLite to perform a full table scan or use two separate index lookups combined with an OR merge. The PK covers `(file_a, file_b)` meaning lookups on `file_a` are efficient, but the `idx_cochange_file_b` index on `file_b` alone does not help when combined with an OR condition in a single query. SQLite's query planner typically cannot combine a PK index and a secondary index via OR efficiently; instead it evaluates both indexes independently and unions the results, or falls back to a full scan. For small tables this is negligible, but at the `MAX_ROWS_PER_TABLE` ceiling (500,000 rows) this could produce significant latency.
- Fix: Replace the single `OR` query with a `UNION ALL` of two indexed queries. This lets SQLite use the PK index for the `file_a` leg and `idx_cochange_file_b` for the `file_b` leg independently:
  ```sql
  SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_a = ?1
  UNION ALL
  SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_b = ?1
  ORDER BY jaccard DESC LIMIT 10000
  ```
  Note: If a row has `file_a = file_b = ?1` (self-pair), the UNION ALL would return it twice. If self-pairs are impossible by construction (canonical ordering guarantees `file_a < file_b`), this is safe. If not, use `UNION` instead.

### MEDIUM

**No upper-bound validation on `limit` parameter in top-N query methods** - `crates/rskim-search/src/temporal/storage_ops.rs:187,217,248`
**Confidence**: 82%
- Problem: `top_hotspots`, `top_risks`, and `top_coldspots` accept `limit: usize` and pass it directly as a LIMIT parameter to SQL. While the CLI's `parse_limit_value` enforces `>= 1`, there is no upper-bound validation. A caller passing `usize::MAX` would effectively request the entire table, negating the purpose of LIMIT. The `load_*` methods already have capacity guards (500,001), but the top-N methods do not. Since `limit` is cast to `i64` via `limit as i64`, a value exceeding `i64::MAX` would silently overflow.
- Fix: Add an upper-bound clamp or validation at the method boundary:
  ```rust
  pub fn top_hotspots(&self, limit: usize) -> Result<Vec<HotspotRow>> {
      let limit = limit.min(MAX_ROWS_PER_TABLE);
      // ... rest of method
  }
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**N+1 query pattern in `resort_partners_by_temporal`** - `crates/rskim/src/cmd/search/temporal.rs:247-284`
**Confidence**: 83%
- Problem: `resort_partners_by_temporal` calls `hotspot_for_file` or `risk_for_file` once per partner in a loop. For a file with many co-change partners (up to 10,000 per the LIMIT), this is a classic N+1 pattern -- one query per partner row. Each query is a separate SQLite round-trip. Similarly, `annotate_hotspots` (line 529) and `annotate_risks` (line 550) loop over results calling per-file lookups individually.
- Fix: For the small result sets typical of `--hot`/`--risky` temporal re-sorting (20-50 results), the per-file lookups are acceptable and simpler than a bulk query. However, when used with `cochanges_for_file` returning up to 10,000 partners, the N+1 pattern becomes material. Consider adding a batch lookup method (e.g., `hotspots_for_files(paths: &[&str])`) that uses a single query with `WHERE file_path IN (...)` for large sets. A reasonable threshold: if `partners.len() > 100`, use the batch path.

## Pre-existing Issues (Not Blocking)

(none -- the database layer is new in this branch)

## Suggestions (Lower Confidence)

- **Migration v2 could include `idx_cochange_jaccard`** - `crates/rskim-search/src/temporal/storage.rs:141-143` (Confidence: 65%) -- The `cochanges_for_file` query orders by `jaccard DESC`. Without a composite index on `(file_a, jaccard)` or `(file_b, jaccard)`, the ORDER BY is performed as a filesort after the WHERE filter. At small scale this is fine, but if the table grows to hundreds of thousands of rows with many partners per file, a composite covering index would avoid the sort step entirely.

- **`v1_database_migrates_to_v2_on_reopen` test does not verify indexes actually exist** - `crates/rskim-search/src/temporal/storage_tests.rs:673-711` (Confidence: 72%) -- The migration test only checks that `schema_version()` returns 2 after migration. It does not verify that the three indexes (`idx_hotspot_score`, `idx_risk_score`, `idx_cochange_file_b`) were actually created. A query against `sqlite_master WHERE type='index'` would make this test more robust.

- **`top_coldspots` does not use `idx_hotspot_score` efficiently for ASC ordering** - `crates/rskim-search/src/temporal/storage_ops.rs:248-253` (Confidence: 62%) -- The index `idx_hotspot_score ON hotspot(score)` is a B-tree and supports both ASC and DESC scans. However, `top_coldspots` orders ASC while `top_hotspots` orders DESC. If the index was created with an explicit DESC hint, the ASC scan may require a reverse traversal. In practice, SQLite B-tree indexes support both directions efficiently so this is a minor theoretical concern.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Database Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### What was done well:
- All queries use parameterized bindings (`?1`) -- no SQL injection risk
- Schema migration is properly version-gated and idempotent with `IF NOT EXISTS`
- Forward-compat guard rejects future schema versions
- Capacity bounds (500,000 rows) prevent unbounded memory
- WAL mode with busy timeout for concurrent access
- File permissions restricted to 0o600
- Co-change LIMIT 10,000 addresses the prior review's unbounded query concern
- Comprehensive test coverage: empty tables, roundtrips, sort ordering, migration from v1
- Error handling consistently maps rusqlite errors to domain `SearchError::Database`

### What should be addressed:
- The `OR` query in `cochanges_for_file` should be rewritten as `UNION ALL` for proper index utilization at scale
- Top-N methods should clamp `limit` to prevent silent overflow or unbounded loads
- The N+1 pattern in `resort_partners_by_temporal` is acceptable at typical scale but should be documented or bounded
