# Architecture Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27
**PR**: #257

## Issues in Your Changes (BLOCKING)

### HIGH

**Bulk table load in `apply_temporal_enrichment` violates per-file lookup API contract** - `crates/rskim/src/cmd/search/temporal.rs:466,507`
**Confidence**: 85%
- Problem: `apply_temporal_enrichment` calls `db.load_hotspots()` / `db.load_risks()` which bulk-load the entire table (up to 500,000 rows) into memory. The feature knowledge for temporal-scoring explicitly states: "Avoid `load_hotspots()` / `load_risks()` / `load_cochanges()` for per-file lookups -- those bulk-load the entire table and impose a 500,000-row capacity cap on the caller." While the current use case is re-sorting all BM25F results (not a single per-file lookup), the number of results is bounded by `--limit` (default 20), so loading all 500K rows to enrich 20 is wasteful. The same issue exists in `query_standalone` at lines 222 and 247 where `load_hotspots()` / `load_risks()` is called just to sort co-change partners.
- Fix: For `apply_temporal_enrichment`, iterate over results and call `hotspot_for_file` / `risk_for_file` per result (N queries where N is limit, typically 20). For the standalone blast-radius + sort case, also use per-file lookups since the partner count is bounded by `--limit`. Only the pure standalone modes (`top_hotspots`, `top_risks`, `top_coldspots`) correctly use the top-N API.

```rust
// Instead of:
let hotspots = db.load_hotspots()?;
let hotspot_map: HashMap<&str, &HotspotRow> = hotspots.iter()...;

// Use per-file lookups:
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

**`run_query` parameter list growing toward "god function" territory** - `crates/rskim/src/cmd/search/mod.rs:386-393`
**Confidence**: 82%
- Problem: `run_query` now accepts 7 parameters (`text`, `limit`, `json`, `root_override`, `temporal_sort`, `blast_radius`, `analytics`). This is on the edge of the SRP threshold -- the function handles BM25F search, blast-radius pre-filtering, temporal DB opening, co-change partner resolution, temporal enrichment, and output formatting. Adding one more feature flag would push this into the clear "too many responsibilities" zone. The existing `QueryConfig` struct carries `text`, `limit`, `json`, `root`, `cache_dir`, and `blast_radius_paths` -- three of the seven parameters are already redundant with the config.
- Fix: Consolidate all query-time parameters into `QueryConfig` (add `temporal_sort: Option<TemporalSort>`) and pass the config directly instead of individual arguments. This makes `run_query` take `(config: &QueryConfig, analytics: &AnalyticsConfig) -> Result<ExitCode>` and moves the config construction to the dispatch site.

### MEDIUM

**Blast-radius pre-filter silently drops the target file itself** - `crates/rskim/src/cmd/search/mod.rs:417-426`
**Confidence**: 83%
- Problem: When building the `blast_radius_paths` allowlist in `run_query`, the code collects only the *partners* of the target file -- not the target file itself. If the user runs `skim search "auth" --blast-radius src/auth.rs`, they expect `src/auth.rs` to appear in the results if it matches. But `src/auth.rs` is excluded from the `file_filter` set, so the search engine discards it before scoring.
- Fix: Include the target file in the allowlist:
```rust
let mut paths: std::collections::HashSet<String> = partners
    .iter()
    .map(|p| {
        if p.file_a == normalized { p.file_b.clone() } else { p.file_a.clone() }
    })
    .collect();
paths.insert(normalized.clone()); // Include the target file itself
blast_radius_paths = Some(paths);
```

**`run_query` silently continues search with no filter on blast-radius normalization failure** - `crates/rskim/src/cmd/search/mod.rs:429-431`
**Confidence**: 80%
- Problem: When `normalize_blast_radius_path` fails, the code prints to stderr and continues. This means `skim search "auth" --blast-radius /nonexistent/file.rs` runs a full unfiltered BM25F search and returns results as if `--blast-radius` was never specified. The user may not notice the stderr warning and assume the filter was applied. This violates the "fail loud" principle from the project's error handling philosophy.
- Fix: Return early with an error or at minimum return empty results when the blast-radius path normalization fails:
```rust
Err(e) => {
    anyhow::bail!("--blast-radius: {e}");
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`output.total` update after temporal enrichment is semantically incorrect** - `crates/rskim/src/cmd/search/mod.rs:451`
**Confidence**: 85%
- Problem: After calling `apply_temporal_enrichment`, line 451 sets `output.total = output.results.len()`. But `apply_temporal_enrichment` never adds or removes results -- it only mutates annotations and re-sorts. So `output.results.len()` is always the same as it was before the call, making this reassignment a no-op that misleadingly suggests the count could change. If a future change to enrichment did remove results, the existing `output.total` (set by the search engine) and the post-enrichment length could diverge silently.
- Fix: Remove the redundant reassignment. If future enrichment needs to drop results, make it explicit by returning the new count.

**`temporal_annotation_tag` duplicated in diff output** - `crates/rskim/src/cmd/search/query.rs:147-169`
**Confidence**: 90%
- Problem: The diff shows the `temporal_annotation_tag` function and its doc comment are duplicated (each line appears twice). This is likely a diff rendering artifact of the tool output, but if the lines are literally duplicated in the source file, it would be a compilation error. Verify the actual file does not contain duplicate lines.
- Fix: Verify the source file is clean. If lines are duplicated, remove the duplicates.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`run_query` calls `.clone()` on `root` and `cache_dir` unnecessarily** - `crates/rskim/src/cmd/search/mod.rs:441-442`
**Confidence**: 80%
- Problem: `root.clone()` and `cache_dir.clone()` allocate new `PathBuf`s just to move them into `QueryConfig`. If `run_query` took ownership of or borrowed these values from the caller, the clones could be avoided. This is a minor efficiency issue but it touches the changed lines.
- Fix: Accept `root: PathBuf` and `cache_dir: PathBuf` by value (moving them into the config) or borrow them via the config struct.

## Suggestions (Lower Confidence)

- **Standalone `query_standalone` loads full hotspot/risk tables to sort co-change partners** - `crates/rskim/src/cmd/search/temporal.rs:222,247` (Confidence: 70%) -- When `--blast-radius` is combined with `--hot` or `--risky`, the function loads the entire hotspot or risk table to sort a small partner list. Per-file lookups would be more proportionate, but the current approach works correctly and partner lists are typically small.

- **`TemporalSort` and `TemporalAnnotation` are `pub(super)` but may need broader visibility** - `crates/rskim/src/cmd/search/types.rs:21,45` (Confidence: 65%) -- If other subcommands or modules need to consume temporal annotations (e.g., a future `skim search --json` pipeline consumer), these types would need re-export. Currently correctly scoped.

- **`check_temporal_staleness` shells out to `git rev-parse HEAD`** - `crates/rskim/src/cmd/search/temporal.rs:146-157` (Confidence: 60%) -- The rest of the codebase uses `gix` for git operations. Shelling out introduces a PATH dependency and potential slowness on cold shells. Acceptable for a one-shot staleness check but inconsistent with the project's gix-based architecture.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

## Rationale

The overall architecture is well-structured. The three-layer separation (types in `types.rs`, temporal logic in `temporal.rs`, I/O orchestration in `mod.rs`) follows the existing module decomposition pattern. The standalone vs. combined dispatch is clean and the `TemporalQueryOutput` enum correctly encodes the four output variants. The `file_filter` addition to `SearchQuery` is the right place for pre-search filtering (inside the engine, not post-hoc).

The two HIGH findings are the most impactful: the bulk table loads in `apply_temporal_enrichment` work against the deliberately-designed per-file lookup API (the feature knowledge explicitly warns against this pattern), and the growing `run_query` parameter list is a coupling smell that will compound with the next feature. The MEDIUM findings (target file exclusion from blast-radius filter, silent continuation on path normalization failure) represent correctness gaps that could confuse users.

The PR demonstrates strong testing discipline (739 lines of test code in `temporal_tests.rs` alone) and clean error handling with `Result` types throughout. The schema v2 migration with performance indexes is properly guarded by `PRAGMA user_version` and tested with a v1-to-v2 migration test.
