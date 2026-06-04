# Reliability Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27

## Issues in Your Changes (BLOCKING)

### HIGH

**Potential panic on short or non-ASCII stored HEAD in `check_temporal_staleness`** - `crates/rskim/src/cmd/search/temporal.rs:136-137`
**Confidence**: 90%
- Problem: The string slicing `&stored_head[..stored_head.len().min(7)]` indexes by byte offset, not character boundary. While git SHA hashes are always hex (ASCII), the `stored_head` value is read from an SQLite `meta` table where any arbitrary string could be stored (e.g., a user could insert non-UTF8-safe data via external tooling, or a future code path could store a different format). If `stored_head` contains multi-byte UTF-8 characters and has length < 7 this is fine (`.min(7)` caps at length), but if a multi-byte character spans the boundary at byte 7, this panics with "byte index 7 is not a char boundary". Similarly for `current_head`, though `git rev-parse HEAD` output is safe in practice. The defensive fix is trivial and eliminates the panic surface.
- Fix: Use `.chars().take(7).collect::<String>()` or `stored_head.get(..7).unwrap_or(&stored_head)` which returns `None` on invalid boundary instead of panicking:
  ```rust
  fn truncate_sha(s: &str, max: usize) -> &str {
      match s.get(..max) {
          Some(prefix) => prefix,
          None => s,
      }
  }
  // Usage:
  truncate_sha(stored_head.trim(), 7),
  truncate_sha(current_head.trim(), 7),
  ```

**`cochanges_for_file` query has no LIMIT -- unbounded result set** - `crates/rskim-search/src/temporal/storage_ops.rs:152-174`
**Confidence**: 85%
- Problem: The `cochanges_for_file` SQL query (`WHERE file_a = ?1 OR file_b = ?1 ORDER BY jaccard DESC`) has no `LIMIT` clause. While the table is capped at 500,000 rows on write via `MAX_ROWS_PER_TABLE`, a single popular file could theoretically be a partner in hundreds of thousands of pairs. The load methods (`load_hotspots`, `load_risks`, `load_cochanges`) all have `LIMIT 500001` as a safety net, and the top-N methods pass `LIMIT ?1`. This query is the only new read method without a bound. The existing `MAX_PAIRS = 2_000_000` capacity bound (from feature knowledge: cochange) limits table size, but a single file could still appear in up to 2M rows. In the CLI path, the partners are then cloned into a `HashSet<String>` (line 417-426 in mod.rs), allocating up to that many strings.
- Fix: Add a `LIMIT` to the SQL query (e.g., 10,000 which is generous) or add a `limit` parameter:
  ```rust
  pub fn cochanges_for_file(&self, path: &str) -> Result<Vec<CochangeRow>> {
      // ... existing code ...
      "SELECT file_a, file_b, count, jaccard FROM cochange \
       WHERE file_a = ?1 OR file_b = ?1 \
       ORDER BY jaccard DESC LIMIT 10000",
      // ...
  }
  ```

### MEDIUM

**`apply_temporal_enrichment` loads entire hotspot/risk table into memory for annotation** - `crates/rskim/src/cmd/search/temporal.rs:466-472`
**Confidence**: 82%
- Problem: When `--hot` or `--risky` is combined with a text query, `apply_temporal_enrichment` calls `db.load_hotspots()` or `db.load_risks()`, which load up to 500,000 rows into a `Vec` and then into a `HashMap`. The caller (`run_query`) typically has only 20 results to annotate. Loading 500K rows to annotate 20 results is disproportionate. The bounded nature of `MAX_ROWS_PER_TABLE` (500K) prevents unbounded allocation, but 500K `HotspotRow` entries (~80 bytes each) is ~40MB which is significant for a CLI tool.
- Fix: Use the per-file lookup methods (`hotspot_for_file`, `risk_for_file`) that this PR also adds, which query a single row per result:
  ```rust
  for result in results.iter_mut() {
      if let Ok(Some(row)) = db.hotspot_for_file(&result.path) {
          result.temporal = Some(TemporalAnnotation {
              hotspot_score: Some(row.score),
              changes_30d: Some(row.changes_30d),
              changes_90d: Some(row.changes_90d),
              ..Default::default()
          });
      }
  }
  ```
  This converts 1 bulk query into N (typically 20) point queries, each served by the primary key index -- faster and constant memory.

**`query_standalone` with blast-radius + sort loads entire hotspot/risk table** - `crates/rskim/src/cmd/search/temporal.rs:222,247`
**Confidence**: 80%
- Problem: Same pattern as above: when `--blast-radius src/auth.rs --hot` is used in standalone mode, `db.load_hotspots()` loads up to 500K rows to sort a typically small partner list (often <50 files). The sort only needs scores for the partner files, not the entire table.
- Fix: Use per-file lookups to build the score map for only the partner files, or add a batch lookup method that accepts a set of paths.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`run_query` silently continues when blast-radius path normalization fails** - `crates/rskim/src/cmd/search/mod.rs:429-431`
**Confidence**: 82%
- Problem: When `normalize_blast_radius_path` returns `Err`, the error is printed to stderr but `blast_radius_paths` remains `None`. The query then proceeds with no file filter -- returning unfiltered results. The user explicitly asked to restrict to co-change partners, but gets full unrestricted results with only a stderr warning. This is graceful degradation for the DB-missing case but arguably incorrect behavior for a path normalization failure (user typo). The PR description says "Failed enrichment -> warning, results unchanged" which matches, but the distinction between "DB missing" and "user provided invalid path" should differ.
- Fix: Consider returning an error (non-zero exit) when `blast_radius` path normalization fails, since the user explicitly requested this filter. Reserve graceful degradation for infrastructure issues (missing DB, failed enrichment), not user input errors.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`read_git_head` spawns a child process on every staleness check** - `crates/rskim/src/cmd/search/temporal.rs:146-152` (Confidence: 65%) -- Spawning `git rev-parse HEAD` adds ~10-50ms latency. Reading `.git/HEAD` directly would be faster and avoid the process overhead, though it would need to handle packed refs.

- **No assertion that `limit > 0` in `top_hotspots`/`top_risks`/`top_coldspots`** - `crates/rskim-search/src/temporal/storage_ops.rs:187,217,248` (Confidence: 60%) -- Passing `limit = 0` produces a valid but vacuous SQL `LIMIT 0` query. A `debug_assert!(limit > 0)` would catch misuse early in development. Low severity since the CLI already rejects `--limit 0`.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The PR demonstrates solid reliability engineering overall: graceful degradation for missing temporal DB (exit 0), proper error propagation via Result types, bounded SQL queries for top-N paths, idempotent schema migration, and mutually-exclusive flag validation at parse time. The main reliability gaps are: (1) a potential panic from byte-indexed string slicing on the staleness warning path, (2) an unbounded `cochanges_for_file` query that is the only new read method without a SQL LIMIT, and (3) unnecessary bulk table loads when per-file lookups would be more efficient and bounded. The enrichment bulk-load pattern is not a correctness issue (bounded by MAX_ROWS_PER_TABLE) but is wasteful for the typical 20-result annotation case.
