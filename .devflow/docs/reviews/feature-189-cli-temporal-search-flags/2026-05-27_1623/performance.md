# Performance Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23
**Cycle**: 3 (incremental — 23 prior issues, 19 fixed, 4 FP)

## Cross-Cycle Awareness

All performance-critical items from Cycles 1-2 have been addressed:
- BM25F pre-filter in first sub-pass (fixed)
- Hoisted `sorted_paths()` to avoid duplicate manifest load (fixed)
- UNION ALL for bidirectional cochange lookup (fixed)
- Pre-truncate window bounding N+1 per-file lookups (fixed)
- In-place index sort for permutation (fixed)
- Per-file annotate N+1 at default limit=20 (confirmed FP at ~0.2ms/query)
- Top-N structural duplication (confirmed FP, below threshold)

No regressions detected on previously resolved items.

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

- **Subprocess spawn for staleness check lacks timeout** - `temporal.rs:152-164` (Confidence: 65%) — `read_git_head` spawns `git rev-parse HEAD` synchronously without a timeout. The doc comment acknowledges this risk for network-mounted repos. On local disk this is sub-10ms, and it only runs in the standalone temporal path (not the hot text-query path), so the practical risk is low. A future hardening pass could add a bounded wait.

- **Clone in permutation apply** - `temporal.rs:328` (Confidence: 60%) — `partners[i].clone()` allocates a new `CochangeRow` per element during permutation. Since the window is pre-truncated to at most `limit*5` (clamped >= 100), this is bounded and the per-row cost is small (two Strings + two primitives). A swap-based in-place permutation would avoid the allocation entirely, but the improvement is marginal at these sizes.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR demonstrates strong performance awareness throughout:

1. **BM25F pre-filtering**: The `file_filter` on `SearchQuery` is checked in the innermost posting-list loop (`reader.rs:362-366`), avoiding TF accumulation and scoring for documents outside the blast-radius allowlist. This is the highest-leverage optimization — it prevents O(total_docs) work when only a small co-change set is relevant.

2. **Index coverage**: Schema migration v2 adds `idx_hotspot_score`, `idx_risk_score`, and `idx_cochange_file_b` indexes. These cover all new query patterns: top-N hotspots/risks use descending index scans (avoiding full table sorts), per-file lookups use PK indexes (hotspot, risk) or the new `file_b` index (cochange UNION ALL).

3. **UNION ALL over OR**: The `cochanges_for_file` query uses `UNION ALL` of two indexed arms instead of `OR`, which allows SQLite to use both the PK index on `file_a` and the secondary index on `file_b`. The comment correctly explains why `UNION ALL` (not `UNION`) is safe given the canonical ordering invariant.

4. **Pre-truncate window**: `resort_partners_by_temporal` is called only after `partners.truncate(resort_window)` with `resort_window = max(limit*5, 100)`. This bounds per-file DB lookups to at most 100 queries in the default case, preventing the N+1 pattern from scaling with total co-change pairs.

5. **Lazy temporal DB open**: The temporal DB is only opened when temporal flags are actually provided (`flags.temporal_sort.is_some() || flags.blast_radius.is_some()`), so non-temporal queries pay zero overhead.

6. **Sorted_paths hoisted**: `manifest.sorted_paths()` is computed once and reused for both file_filter construction and path resolution, avoiding a duplicate O(n) traversal.

7. **Result-set bounded annotation**: `annotate_hotspots`/`annotate_risks` iterate over post-LIMIT results (default 20), so the per-file lookup cost is O(limit) not O(total_files).

The two suggestions (subprocess timeout, clone in permutation) are both below the 80% confidence threshold and have negligible practical impact at current usage scales. No blocking or should-fix performance issues remain after the Cycle 2 resolutions.
