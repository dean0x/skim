---
feature: search-temporal
name: Search Temporal Integration
description: "Use when adding temporal flags to search, modifying --hot/--cold/--risky/--blast-radius behavior, implementing standalone vs combined temporal query dispatch, or understanding how BM25F results are re-sorted by temporal signals. Keywords: temporal, hotspot, risk, blast-radius, co-change, TemporalAnnotation, TemporalSort, standalone mode, enrichment."
category: domain-knowledge
directories:
  - crates/rskim/src/cmd/search/
referencedFiles:
  - crates/rskim/src/cmd/search/temporal.rs
  - crates/rskim/src/cmd/search/temporal_tests.rs
  - crates/rskim/src/cmd/search/types.rs
  - crates/rskim/src/cmd/search/mod.rs
  - crates/rskim/src/cmd/search/query.rs
  - crates/rskim-search/src/temporal/storage.rs
  - crates/rskim-search/src/lib.rs
created: 2026-05-26
updated: 2026-05-26
---

# Search Temporal Integration

## Overview

This feature integrates temporal intelligence (hotspot scoring, bug-fix density, co-change coupling) from the `rskim-search` library into the `skim search` CLI. It exposes four flags (`--hot`, `--cold`, `--risky`, `--blast-radius`) that operate in two distinct modes: **standalone** (no text query, outputs a ranked table) and **combined** (text query present, BM25F results re-sorted by temporal signals). The temporal data lives in `temporal.db` (SQLite, populated by `skim heatmap`) inside the search cache directory.

The design keeps all I/O in `mod.rs`, all temporal logic in `temporal.rs`, and all shared types in `types.rs`. The `query.rs` module knows nothing about temporal signals until `mod.rs` calls `apply_temporal_enrichment` after the BM25F search returns.

## Business Context

Temporal signals let developers answer questions like "which files in my search results are the highest-churn?" or "what files typically change alongside `src/auth.rs`?". The flags are intentionally additive — `--blast-radius` can be combined with any sort mode and with text queries, because it acts as a pre-filter (allowlist of co-change partners) rather than a sort mode.

## Core Business Rules

**Mutual exclusion:** `--hot`, `--cold`, and `--risky` are mutually exclusive sort modes. Combining any two fails early in `parse_flags` with a clear error message naming both conflicting flags. `--blast-radius` is NOT part of this exclusive group — it is composable with any sort mode and with text queries.

**Dispatch logic:** The `run()` function in `mod.rs` uses two distinct code paths based on whether a non-empty text query is present:
- Non-empty text query → `run_query()` → BM25F search → optional `apply_temporal_enrichment()` + resort
- Empty query with temporal flags → `run_temporal_standalone()` → direct DB table scan, no BM25F

**Graceful degradation:** Missing `temporal.db` is always a warning + exit 0, never an error. If the DB load or query fails after opening, `apply_temporal_enrichment` logs a warning to stderr and returns without modifying results.

**Staleness warning:** When `temporal.db` exists but its stored git HEAD differs from the current repo HEAD, a warning is printed to stderr in standalone mode. The search continues with stale data rather than refusing to operate.

## State Transitions

The dispatch in `run()` follows this decision tree:

```
args parsed
  └── action_flag set (--build, --stats, etc.) → run that action (no temporal)
  └── SearchAction::Query(text)
        └── text non-empty → run_query(text, temporal_sort, blast_radius, ...)
              └── blast_radius set → normalize path, load co-change partners,
                  build HashSet → QueryConfig.blast_radius_paths
                  → execute_query (with file_filter inside BM25F engine)
              └── temporal_sort set → apply_temporal_enrichment after BM25F
        └── text empty AND (temporal_sort OR blast_radius set)
              → run_temporal_standalone(temporal_sort, blast_radius, ...)
              → query_standalone() → TemporalQueryOutput → format
        └── text empty, no temporal flags → print help
```

## Technical Implementation Patterns

### Blast-Radius Pre-filtering for Combined Mode

When `--blast-radius` is combined with a text query, the co-change partners are resolved to a `HashSet<String>` of repo-relative paths before the BM25F query. This set is passed into `QueryConfig.blast_radius_paths`, which `query.rs` converts to a `FileId` allowlist and injects as `SearchQuery.file_filter` before executing the search. This ensures the `--limit` cap applies to the filtered set, not the full unfiltered result set.

The resolution in `run_query` is:
```rust
// Partners resolved before execute_query so the limit applies to the filtered set.
// If temporal_db is None, blast_radius_paths stays None (no pre-filtering).
if let (Some(raw_path), Some(db)) = (blast_radius, &temporal_db) {
    let normalized = temporal::normalize_blast_radius_path(raw_path, &root)?;
    let partners = db.cochanges_for_file(&normalized)?;
    // ... build HashSet from partners
    blast_radius_paths = Some(paths);
}
```

The key insight: `blast_radius_paths` is consumed by `QueryConfig` and the `file_filter` is a `HashSet<FileId>`, so the pre-filtering is zero-cost at the BM25F scoring layer — no results are discarded post-limit.

### Path Normalization for --blast-radius

`normalize_blast_radius_path` in `temporal.rs` handles cross-platform resolution. The algorithm tries project-root-relative resolution first (most common case: `src/foo.rs` from any CWD within the repo), falls back to CWD-relative, and errors with "blast-radius file not found" (not "outside the project root") for nonexistent paths. This distinction matters: the confusing "outside the project root" message from `canonicalize()` failure is suppressed by checking existence before canonicalizing.

Windows cross-platform consistency: backslashes are replaced with `/` in the final normalized path, ensuring the normalized path matches the strings stored in `temporal.db` (which always use forward slashes from git history).

### Temporal Enrichment (Combined Mode)

`apply_temporal_enrichment` in `temporal.rs` annotates `ResolvedResult.temporal` and re-sorts in-place:

- **Hot**: loads all hotspot rows, builds `HashMap<&str, &HotspotRow>`, annotates matching results, sorts descending. Files absent from DB use score `-1.0` and sort last.
- **Cold**: same map, sorts ascending. Files absent sort first (score `-1.0` → lowest score).
- **Risky**: loads all risk rows, annotates with `risk_score` + `fix_density`, sorts descending. Files absent sort last.

The tie-breaker is always `a.path.cmp(&b.path)` for deterministic output.

### TemporalAnnotation on ResolvedResult

`TemporalAnnotation` in `types.rs` uses `#[serde(skip_serializing_if = "Option::is_none")]` on all fields so that JSON output only includes fields relevant to the active sort mode. Hot queries emit `hotspot_score`, `changes_30d`, `changes_90d`. Risky queries emit `risk_score`, `fix_density`. The field `cochange_jaccard` is reserved for blast-radius in combined mode (currently not populated).

### Standalone Query Dispatch

`query_standalone()` in `temporal.rs` maps to the correct `TemporalDb` method:
- `--hot` → `db.top_hotspots(limit)` → `TemporalQueryOutput::Hotspots`
- `--cold` → `db.top_coldspots(limit)` → `TemporalQueryOutput::Coldspots`
- `--risky` → `db.top_risks(limit)` → `TemporalQueryOutput::Risks`
- `--blast-radius FILE` → `db.cochanges_for_file(&normalized)` → `TemporalQueryOutput::Cochanges`
- `--hot/--cold/--risky` + `--blast-radius` → co-change partners, re-sorted in memory by the requested metric

No sort is specified AND no blast-radius: defaults to `TemporalSort::Hot` behavior (top hotspots).

### JSON Output Schema

Standalone temporal JSON is a flat envelope:
```json
// --hot / --cold
{"mode": "hot", "limit": 10, "results": [{"path": "...", "hotspot_score": 0.9, "changes_30d": 5, "changes_90d": 12}]}

// --risky
{"mode": "risky", "limit": 10, "results": [{"path": "...", "risk_score": 0.8, "fix_density": 0.6, "fix_commits": 6, "total_commits": 10}]}

// --blast-radius
{"mode": "blast_radius", "target": "src/auth.rs", "limit": 5, "results": [{"path": "...", "jaccard": 0.75, "count": 8}]}
```

Combined mode (text + temporal): the existing `QueryOutput` JSON structure gains a `"temporal"` field on each result entry when temporal flags are active.

## Error Handling and Recovery

| Failure scenario | Behavior |
|---|---|
| `temporal.db` missing | Warning to stderr, exit 0 (both standalone and combined modes) |
| `temporal.db` corrupt / unreadable | `open_temporal_db` returns `None` → same graceful path |
| DB stale vs current HEAD | Warning to stderr, continue with stale data |
| `--blast-radius` path not found | Hard error: "blast-radius file not found: <path>" (exit 1) |
| `--blast-radius` path outside repo | Hard error: "path is outside the project root" (exit 1) |
| DB query fails after opening | `apply_temporal_enrichment` logs warning, returns without re-sorting |
| No co-change data for target | Warning to stderr ("no co-change data for ..."), empty results, exit 0 |

## Anti-Patterns

**Do not put temporal sorting inside `query.rs`**. The `execute_query` function returns BM25F-ordered results and knows nothing about temporal signals. Temporal re-sorting belongs in `mod.rs` calling `temporal::apply_temporal_enrichment` after `execute_query` returns. Mixing sorting responsibilities would break the I/O boundary between query execution and result enrichment.

**Do not apply the blast-radius filter post-limit**. The `blast_radius_paths` must be resolved to a `QueryConfig.blast_radius_paths` HashSet before calling `execute_query`, so that `query.rs` can inject it as a `SearchQuery.file_filter`. Filtering after the limit would silently discard co-change partners that happened to rank outside the top-N of the full result set.

**Do not treat missing `temporal.db` as an error**. Temporal data is optional — a fresh repo that has never run `skim heatmap` has no DB. Returning exit 1 would break any script that runs `skim search --hot` on CI before the first heatmap run.

## Gotchas

**Co-change pairs are stored lexicographically**: `CochangeRow.file_a` is always the lexically smaller path. The helper `cochange_partner(row, target)` resolves both directions. Callers that access `row.file_b` directly without this helper will miss half the pairs.

**Empty query with temporal flags is standalone mode, not an error**. The `run()` dispatch treats `SearchAction::Query("")` with temporal flags as standalone — this is deliberate. A user who types `skim search --hot` gets a hotspot table, not a "query required" error.

**`--blast-radius` without text query in standalone mode re-sorts co-change partners in memory** (not via `db.top_hotspots`). The in-memory re-sort builds a `HashMap` from a full DB scan. For large repos with many hotspot rows, this is O(n) on the hotspot table, not the partner list.

**`apply_temporal_enrichment` uses `load_hotspots()` / `load_risks()` (full table scan)**, not the paginated `top_hotspots()`. This is intentional: the text search already limited the result set to ≤ `limit` files, and we need all temporal scores to annotate them correctly. If the temporal DB grows very large, this may become a bottleneck.

**Staleness check requires `git` on PATH**. `check_temporal_staleness` shells out to `git -C root rev-parse HEAD`. If git is absent or the directory is not a repo, `read_git_head` returns `None` and staleness checking is silently skipped (no warning, no error).

## Key Files

- `crates/rskim/src/cmd/search/temporal.rs` — all temporal helpers: path normalization, DB open/check, standalone query dispatch, text+temporal enrichment, output formatters
- `crates/rskim/src/cmd/search/temporal_tests.rs` — co-located tests for temporal.rs (linked via `#[path]` attribute)
- `crates/rskim/src/cmd/search/types.rs` — `TemporalSort`, `TemporalAnnotation`, `ResolvedResult` (with `temporal` field), `QueryConfig` (with `blast_radius_paths`)
- `crates/rskim/src/cmd/search/mod.rs` — top-level dispatch: `run_query` (combined mode), `run_temporal_standalone` (standalone mode), `parse_flags` (mutual exclusion enforcement)
- `crates/rskim/src/cmd/search/query.rs` — BM25F search execution; `file_filter` injection from `blast_radius_paths`; `temporal_annotation_tag` for text output suffix
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb`, `HotspotRow`, `RiskRow`, `CochangeRow`, `META_GIT_HEAD`

## Related

- Feature knowledge: `temporal-scoring` — the `TemporalDb`, `HotspotRow`, `RiskRow`, and scoring algorithms this feature consumes
- Feature knowledge: `cochange` — the co-change matrix and `CochangeRow` this feature uses for blast-radius queries
- `crates/rskim-search/src/lib.rs` — public re-exports: `TemporalDb`, `HotspotRow`, `RiskRow`, `CochangeRow`, `META_GIT_HEAD`
