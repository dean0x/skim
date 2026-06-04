# Performance Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### HIGH

**file_filter applied after full scoring -- wasted BM25F computation for filtered-out documents** - `crates/rskim-search/src/index/reader.rs:381-389`
**Confidence**: 85%
- Problem: The `file_filter` allowlist is applied AFTER the entire scoring loop (lines 346-379) has already accumulated BM25F scores, per-field TFs, match positions, and file meta entries for ALL matching documents. For a typical blast-radius query, the allowlist may contain 10-50 files while the index has thousands of scored documents. All the scoring work for documents not in the allowlist is thrown away.
- Impact: When `--blast-radius` is combined with a text query, the search engine does full BM25F scoring for every matching document in the entire index, then discards most of them. This wastes CPU proportional to `(total_matches - filtered_matches)`. For common query terms in large repos this could mean scoring hundreds or thousands of documents needlessly.
- Fix: Move the file_filter check into the `score_ngram_postings` inner loop at line 214, skipping documents not in the allowlist before computing BM25F scores. This would require threading the `file_filter` through to `score_ngram_postings`, or more simply, adding an early-continue in the first sub-pass (line 354) to skip postings for filtered-out doc_ids:

```rust
// In the first sub-pass (line 354), add:
for p in &postings {
    if p.doc_id >= self.header.file_count {
        continue;
    }
    // Early skip for file_filter
    if let Some(ref filter) = query.file_filter {
        if !filter.contains(&FileId(p.doc_id)) {
            continue;
        }
    }
    // ... rest of TF accumulation
}
```

### MEDIUM

**Per-file SQL lookups in `resort_partners_by_temporal` -- N individual queries** - `crates/rskim/src/cmd/search/temporal.rs:255-284`
**Confidence**: 82%
- Problem: `resort_partners_by_temporal` issues one `hotspot_for_file` or `risk_for_file` SQL query per co-change partner. The co-change query returns up to 10,000 rows (LIMIT on line 158 of storage_ops.rs). Before truncation at `limit` (line 222 of temporal.rs), re-sorting happens on the full set. This means up to 10,000 individual `SELECT ... WHERE file_path = ?` queries to SQLite.
- Impact: While SQLite in-process queries are fast (~1-10us each with indexes), 10,000 queries still adds 10-100ms of latency. For the default `--limit 20`, only 20 rows survive truncation, so up to 9,980 lookups are wasted.
- Fix: Truncate the partners list to `limit` BEFORE re-sorting when no sort mode is specified, or use a batch `WHERE file_path IN (...)` query when re-sorting is needed. A pragmatic approach: since the cochange query already returns results sorted by Jaccard DESC, and the user asked for at most `limit` results, consider truncating to a reasonable window (e.g., `limit * 5`) before the re-sort to bound the number of per-file lookups:

```rust
// In query_standalone, before resort:
let reasonable_window = limit.saturating_mul(5).max(100);
partners.truncate(reasonable_window);

if let Some(sort_mode) = sort {
    resort_partners_by_temporal(&mut partners, sort_mode, &normalized, db)?;
}
partners.truncate(limit);
```

**Per-file SQL lookups in `annotate_hotspots` / `annotate_risks` -- N queries per text result** - `crates/rskim/src/cmd/search/temporal.rs:525-563`
**Confidence**: 80%
- Problem: `annotate_hotspots` and `annotate_risks` each issue one SQL query per search result. With `--limit` defaulting to 20, this is 20 queries -- acceptable. But the limit can be set up to any value by the user.
- Impact: For default usage (20 results), this adds ~0.2ms and is negligible. For large limits (e.g., `--limit 1000`), this adds 1-10ms. Low real-world impact since large limits are uncommon.
- Fix: No immediate action needed for the default path. If performance at large limits becomes a concern, a batch query (`WHERE file_path IN (...)`) would reduce to a single round-trip.

**`sorted_paths()` called twice in `execute_query` when blast-radius is active** - `crates/rskim/src/cmd/search/query.rs:72,86`
**Confidence**: 82%
- Problem: When `blast_radius_paths` is `Some`, `manifest.sorted_paths()` is called at line 72 to build the file_filter, and again at line 86 for path resolution. Each call iterates the BTreeMap and allocates a `Vec<&str>`.
- Impact: For a manifest with N files, this allocates and iterates twice instead of once. The cost is O(N) per extra call -- minor for typical repos (<10k files) but wasteful.
- Fix: Hoist the `sorted_paths()` call before the file_filter block and reuse it:

```rust
let sorted = manifest.sorted_paths();

if let Some(ref allowed_paths) = config.blast_radius_paths {
    let mut file_ids = std::collections::HashSet::new();
    for (idx, path) in sorted.iter().enumerate() {
        if allowed_paths.contains(*path) {
            file_ids.insert(rskim_search::FileId(idx as u32));
        }
    }
    sq.file_filter = Some(file_ids);
}

let raw_results = engine.search(&sq)?;
// Reuse `sorted` below instead of calling sorted_paths() again
let results = resolve_paths_and_snippets(&raw_results, &sorted, root, &manifest);
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`cochanges_for_file` OR query cannot use both indexes efficiently** - `crates/rskim-search/src/temporal/storage_ops.rs:156-158`
**Confidence**: 83%
- Problem: The query `WHERE file_a = ?1 OR file_b = ?1` with `ORDER BY jaccard DESC LIMIT 10000` cannot be served by a single index scan. SQLite will either: (a) use the PK index on `(file_a, file_b)` for the `file_a = ?1` half and do a full scan for `file_b = ?1`, or (b) use `idx_cochange_file_b` for the `file_b = ?1` half and do a full scan for `file_a = ?1`. In practice SQLite's query optimizer may use a `MULTI-INDEX OR` plan that merges both, but this depends on statistics and is not guaranteed.
- Impact: For cochange tables with tens of thousands of rows, the OR query could degrade to a partial or full table scan. The `LIMIT 10000` mitigates the output size but not the scan cost.
- Fix: Replace the OR query with a `UNION ALL` of two indexed lookups, which guarantees each half uses its respective index:

```sql
SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_a = ?1
UNION ALL
SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_b = ?1
ORDER BY jaccard DESC LIMIT 10000
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`git rev-parse HEAD` subprocess in combined query path could add latency** - `crates/rskim/src/cmd/search/temporal.rs:146-157` (Confidence: 65%) -- The `read_git_head` function spawns a `git` subprocess. Currently only called from `run_temporal_standalone` (not `run_query`), so combined text+temporal queries are unaffected. If staleness checking is ever added to the combined path, this would add ~5-15ms of subprocess overhead per query.

- **JSON output uses `serde_json::to_string_pretty` for standalone temporal** - `crates/rskim/src/cmd/search/temporal.rs:451` (Confidence: 62%) -- Pretty-printing JSON adds whitespace allocation overhead. For machine-consumed output (the primary use case for `--json`), compact JSON (`to_string`) would be slightly faster and produce less output. Minor impact.

- **`HashSet<String>` for blast_radius_paths could use borrowed strings** - `crates/rskim/src/cmd/search/mod.rs:416-425` (Confidence: 60%) -- Each cochange partner path is cloned into a `HashSet<String>`. If the TemporalDb connection lifetime could be extended, borrowed `&str` references would avoid these allocations. Low impact for typical co-change set sizes (<100 partners).

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The overall performance architecture is sound -- per-file lookups instead of bulk loads, HashSet-based pre-filtering pushed into the search engine, proper database indexes on sort columns. The main concern is the file_filter being applied post-scoring rather than pre-scoring in the BM25F loop, which causes unnecessary computation proportional to the total index size when only a small subset of files is relevant. The N per-file SQL lookups in the re-sort path could also become costly at the 10,000-partner upper bound, though a simple truncation-before-sort would bound this effectively.
