# Performance Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27
**PR**: #257

## Issues in Your Changes (BLOCKING)

### HIGH

**`load_hotspots()` / `load_risks()` full-table scan in `apply_temporal_enrichment` defeats the purpose of schema v2 indexes** - `crates/rskim/src/cmd/search/temporal.rs:466,507`
**Confidence**: 90%
- Problem: `apply_temporal_enrichment()` calls `db.load_hotspots()` (line 466) and `db.load_risks()` (line 507) which load the ENTIRE table (up to 500k rows) into a `HashMap` just to annotate a handful of search results (typically 20). The PR description targets "combined overhead < 20ms" but loading 500k rows into a HashMap is O(n) in table size, not O(k) in result count. The schema v2 indexes on `score` and `risk_score` are unused by this code path.
- Fix: Use per-file lookups (`hotspot_for_file` / `risk_for_file`) which are already implemented in `storage_ops.rs` and hit the PRIMARY KEY index. The number of lookups equals the result count (typically 20), making it O(k) with k << n:
```rust
// Instead of loading all hotspots into a HashMap:
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

**`load_hotspots()` / `load_risks()` full-table scan in `query_standalone` blast-radius sort** - `crates/rskim/src/cmd/search/temporal.rs:222,247`
**Confidence**: 88%
- Problem: When `--blast-radius src/auth.rs --hot` is used, `query_standalone()` loads ALL hotspot/risk rows (up to 500k) into a HashMap just to look up scores for a small number of co-change partners (typically < 50). The PR targets "blast-radius < 50ms" but the full table scan dominates for large databases.
- Fix: Use per-file lookups for each partner instead of loading the entire table:
```rust
TemporalSort::Hot | TemporalSort::Cold => {
    // Build a score map from per-file lookups (O(k) not O(n))
    let mut partner_scores: Vec<(usize, f64)> = partners
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let partner_path = cochange_partner(p, &normalized);
            let score = db.hotspot_for_file(partner_path)
                .ok()
                .flatten()
                .map(|h| h.score)
                .unwrap_or(0.0);
            (i, score)
        })
        .collect();
    // Sort by score, then reorder partners accordingly
}
```

### MEDIUM

**`cochanges_for_file` OR query cannot use both indexes efficiently** - `crates/rskim-search/src/temporal/storage_ops.rs:156-157`
**Confidence**: 82%
- Problem: The query `WHERE file_a = ?1 OR file_b = ?1` in `cochanges_for_file()` uses an OR condition across two columns. SQLite can only use one index per query scan. The PK covers `(file_a, file_b)` so the `file_a = ?1` arm is indexed, but `file_b = ?1` requires the new `idx_cochange_file_b` index via a separate scan. SQLite's query optimizer may handle this via OR-optimization (using both indexes and merging), but it is not guaranteed for all SQLite versions and depends on `ANALYZE` statistics.
- Fix: Use `UNION ALL` which guarantees both arms use their respective indexes with no planner uncertainty:
```sql
SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_a = ?1
UNION ALL
SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_b = ?1
ORDER BY jaccard DESC
```
  This is a well-known SQLite optimization pattern for bidirectional graph lookups. It removes planner ambiguity and guarantees O(log n + k) per arm.

**Post-scoring `file_filter` in `NgramIndexReader::search` does unnecessary scoring work** - `crates/rskim-search/src/index/reader.rs:382-388`
**Confidence**: 80%
- Problem: The `file_filter` allowlist is applied AFTER all documents have been scored (line 382-388). This means the scoring loop processes every matching document even when only a small subset will survive the filter. For blast-radius queries with a small allowlist (say 10 co-change partners out of 50k indexed files), 99.98% of scoring work is wasted.
- Fix: Move the filter check into the scoring loop so filtered documents are skipped before BM25F computation. The filter check should go at the top of the `score_ngram_postings` second sub-pass or at posting accumulation time:
```rust
// In the tf_per_doc accumulation, skip filtered docs early:
for p in &postings {
    if let Some(ref filter) = query.file_filter {
        if !filter.contains(&FileId(p.doc_id)) {
            continue;
        }
    }
    // ... accumulate TF ...
}
```
  This avoids HashMap insertions, TF accumulations, and BM25F score calculations for documents that will be discarded.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Redundant `sorted_paths()` call in `execute_query` when blast-radius is active** - `crates/rskim/src/cmd/search/query.rs:72,86`
**Confidence**: 85%
- Problem: `manifest.sorted_paths()` is called on line 72 for blast-radius FileId resolution, then called again on line 86 for path resolution. `sorted_paths()` returns `Vec<&str>` which involves iterating and sorting the manifest entries each call. The result should be computed once and reused.
- Fix: Hoist the `sorted_paths()` call above both uses:
```rust
let sorted = manifest.sorted_paths();

// Use `sorted` for both blast-radius resolution and path resolution
if let Some(ref allowed_paths) = config.blast_radius_paths {
    let mut file_ids = std::collections::HashSet::new();
    for (idx, path) in sorted.iter().enumerate() {
        if allowed_paths.contains(*path) {
            file_ids.insert(rskim_search::FileId(idx as u32));
        }
    }
    sq.file_filter = Some(file_ids);
}
// ... later ...
let results = resolve_paths_and_snippets(&raw_results, &sorted, root, &manifest);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`read_git_head` spawns `git rev-parse` subprocess on every staleness check** - `crates/rskim/src/cmd/search/temporal.rs:146-157` (Confidence: 70%) -- For latency-sensitive paths, consider reading `.git/HEAD` directly or caching the result, since spawning a subprocess is ~5-10ms overhead.

- **`temporal_annotation_tag` builds a `Vec<String>` for at most 2 items** - `crates/rskim/src/cmd/search/query.rs:158-168` (Confidence: 65%) -- Minor: the Vec + join pattern allocates unnecessarily for 1-2 items. A direct format! conditional would avoid the allocation, though the impact is negligible at display-output granularity.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The schema v2 indexes are well-designed and the per-file lookup methods (`hotspot_for_file`, `risk_for_file`) already exist, but the main code paths (`apply_temporal_enrichment` and `query_standalone` blast-radius sort) load entire tables into HashMaps instead of using them. This is the dominant performance concern: at 500k rows, `load_hotspots()` allocates ~40MB of Strings and performs a full table scan, while the per-file lookups would hit the PRIMARY KEY index in microseconds. The post-scoring `file_filter` placement is a secondary concern but matters for blast-radius queries where the allowlist is small relative to the index. Both HIGH issues have clear fixes using APIs already present in this PR.
