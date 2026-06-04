# Database Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23
**Prior Resolutions**: Cycle 2 resolved 19/23 issues (4 FP). UNION ALL indexed cochange query and top-N limit clamp already fixed.

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Per-file lookup methods use `prepare()` instead of `prepare_cached()`** - `storage_ops.rs:162`, `storage_ops.rs:202`, `storage_ops.rs:236`, `storage_ops.rs:271`
**Confidence**: 82%
- Problem: The four new query methods (`cochanges_for_file`, `top_hotspots`, `top_risks`, `top_coldspots`) all use `self.conn.prepare()` which compiles the SQL statement from scratch on every call. The existing write helpers (`insert_hotspots_in_tx`, `insert_risks_in_tx`, `insert_cochanges_in_tx`, `set_meta`, `sync`) consistently use `prepare_cached()`. The `annotate_hotspots` and `annotate_risks` callers invoke `hotspot_for_file`/`risk_for_file` in a loop over search results, so the statement is compiled N times per enrichment pass. For `resort_partners_by_temporal`, the loop can be up to the resort window (min 100).
- Impact: Unnecessary overhead on repeated calls. For `hotspot_for_file` and `risk_for_file` called inside `annotate_hotspots`/`annotate_risks` loops, the statement is re-prepared per result. `prepare_cached()` would amortize compilation to a single call per statement text.
- Fix: Replace `self.conn.prepare(...)` with `self.conn.prepare_cached(...)` in all six new query methods. The `query_row` calls in `hotspot_for_file` and `risk_for_file` (lines 96 and 123) use `conn.query_row()` which already benefits from SQLite's internal statement cache, so those are acceptable as-is. Focus on the four `prepare()` sites:
  ```rust
  // storage_ops.rs:162 — cochanges_for_file
  let mut stmt = self.conn.prepare_cached(
      "SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_a = ?1 \
       UNION ALL \
       SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_b = ?1 \
       ORDER BY jaccard DESC LIMIT 10000",
  ).map_err(db_err)?;

  // Similarly for top_hotspots (line 202), top_risks (line 236), top_coldspots (line 271)
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Canonical ordering not enforced at INSERT time** - `storage_ops.rs:67-79` (Confidence: 65%) — The `insert_cochanges_in_tx` function trusts the caller to provide `file_a < file_b` lexically. If a caller ever violates this invariant, the `UNION ALL` query in `cochanges_for_file` could return duplicate rows. A debug assertion (`debug_assert!(row.file_a < row.file_b)`) would catch violations in tests without production cost. This is a pre-existing design choice, not introduced by this PR.

- **`cochanges_for_file` hardcoded LIMIT 10000** - `storage_ops.rs:166` (Confidence: 70%) — The 10,000 row cap is reasonable for memory safety but is not configurable. If a highly-coupled file in a large monorepo exceeds this, results are silently truncated with no indication. Consider accepting an optional limit parameter or at minimum documenting the cap in the public API doc.

- **`read_git_head` has no subprocess timeout** - `temporal.rs:152-164` (Confidence: 72%) — The function spawns `git rev-parse HEAD` without a timeout. The doc comment acknowledges this ("NOT safe to use on network-mounted repos") but does not enforce it. Since this is in the staleness-check path (not critical), the risk is low, but a 5-second timeout via `std::process::Command` + child PID kill would make the "hang protection" note actionable.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Database Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The database changes are well-structured. Schema migration v1->v2 is clean and idempotent with proper version gating. The UNION ALL cochange query correctly exploits both the PK index and the new secondary index on `file_b`, avoiding the OR-induced table scan. The `MAX_ROWS_PER_TABLE` clamp before `as i64` prevents integer overflow on top-N methods. All queries use parameterized statements (no SQL injection vectors). The only actionable item is switching `prepare()` to `prepare_cached()` on the four new query methods to avoid repeated statement compilation in hot loops.
