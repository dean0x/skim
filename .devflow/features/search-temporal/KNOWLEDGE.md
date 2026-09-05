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
updated: 2026-09-03
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

**Staleness and degraded state:** When temporal data cannot be served (missing DB,
corrupt DB, newer schema, repository anchor mismatch, or empty rows), the #414
degraded-state vocabulary provides typed notices to both stderr and JSON output via
the `degraded[]` array. `--stats --json` reports `temporal_state` and `staleness`
from the PRE-self-heal state (AD-414-10). See the `cmd-search` feature knowledge entry
for the full degraded-state contract.

## State Transitions

The dispatch in `run()` follows this decision tree:

```
args parsed
  └── action_flag set (--build, --stats, etc.) → run that action (no temporal)
  └── SearchAction::Query(text)
        └── text non-empty → run_query(text, temporal_sort, blast_radius, ...)
              └── blast_radius set → normalize path, load co-change partners,
                  build BlastRadiusStrengths (HashMap<String, f64>), seed target at SEED_STRENGTH=2.0
                  → BlastRadiusResolution::Allowed → QueryConfig.blast_radius_paths
                  → execute_query (composite UNION RRF; file_filter only on the
                    compound text+--ast arm)
              └── temporal_sort set → apply_temporal_enrichment after BM25F
        └── text empty AND (temporal_sort OR blast_radius set)
              → run_temporal_standalone(temporal_sort, blast_radius, ...)
              → query_standalone() → TemporalQueryOutput → format
        └── text empty, no temporal flags → print help
```

## Technical Implementation Patterns

### Blast-Radius Resolution for Combined Mode

When `--blast-radius` is combined with a text query, the co-change partners are resolved to a `BlastRadiusStrengths` map (`HashMap<String, f64>`: partner paths to Jaccard scores, plus the blast-radius target itself at `SEED_STRENGTH = 2.0`) before the BM25F query. The resolution result (`BlastRadiusResolution`) is stored in `QueryConfig.blast_radius_paths`.

**Two different downstream arms consume that map — do not conflate them:**

| Arm | Dispatch condition | How the map is used |
|-----|--------------------|---------------------|
| Compound text+`--ast` | `ast_scored.is_some()` | `paths_to_file_ids` → `HashSet<FileId>`, intersected with the AST set and injected as `SearchQuery.file_filter` so `--limit` applies to the filtered set. |
| Composite blast-radius | `ast_scored.is_none()` and `blast_radius_paths.is_some()` | `paths_to_scored_file_ids` → a Jaccard-scored `Vec<(FileId, f64)>` temporal layer. The lexical search runs with **no** `file_filter` (a WIDE `K × limit` pool) because UNION semantics require ranking the full candidate set; truncation happens LAST, after RRF fusion and verification. |

The resolution in `run_query` (simplified) is:
```rust
// resolve_blast_radius_paths returns BlastRadiusResolution (not Option<HashSet<String>>).
let resolution = temporal::resolve_blast_radius_paths(blast_radius, &root, &cache_dir, json, &head)?;
// ::NotRequested            → blast_radius_paths = None
// ::Allowed(strengths)      → blast_radius_paths = Some(strengths)
// ::Filtered{allow, .. }    → blast_radius_paths = Some(allow)  ← Some(EMPTY) on RepositoryMismatch
// ::Degraded(u)             → blast_radius_paths = None
```

**`Some(empty)` is NOT `None` (AD-413-16 / AC-9).** `Filtered` carries an intentionally empty allowlist for `RepositoryMismatch` (the temporal DB belongs to a different repository); the composite arm must return ZERO results for it. Collapsing `Filtered` to `None` would fall through to a plain unfiltered lexical search — wrong-repo data silently expanded rather than isolated.

The key insight: carrying Jaccard scores (rather than a plain path set) is what lets the temporal RRF layer rank partners by co-change strength instead of by `FileId` (which is the alphabetical index position — the #409 defect).

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

The degraded-state vocabulary is maintained in `cmd-search` (feature knowledge entry).
The high-level behaviors:

| Failure scenario | Behavior |
|---|---|
| `temporal.db` missing / corrupt / newer-schema | `open_temporal_state` returns `TemporalOpen::Unavailable` with a typed `DegradedReason`; JSON output gains a `degraded[]` array; exit 0 |
| Standalone temporal arm unavailable | `applied = "none"` in `degraded[0]`, no `results` key in JSON (AD-414-19) |
| Text+temporal arm unavailable | `applied = "lexical"` in `degraded[0]`, BM25F results served (AD-414-19) |
| Repository mismatch (`meta.git_toplevel` differs) | `DegradedReason::RepositoryMismatch`; no rows served; exit 0 |
| `--blast-radius` path not found | Hard error: "blast-radius file not found: <path>" (exit 1) |
| DB query fails after opening | `apply_temporal_enrichment` warns to stderr, returns without re-sorting |
| `parse_history` fails on rebuild | Build-backoff sentinel written (`temporal.db.build_backoff`); `META_GIT_HEAD` NOT written; retry bounded to one walk per HEAD (AD-414-17 / AD-414-21) |

For the complete degraded-state type hierarchy (`DegradedReason`, `DegradedJson`,
`TemporalOpen`, `Fallback`, `degraded_notice`) and the `applied` per-arm contract, see
the `cmd-search` feature knowledge entry (Degraded-State Vocabulary section).

## Anti-Patterns

**Do not put temporal sorting inside `query.rs`**. The `execute_query` function returns BM25F-ordered results and knows nothing about temporal signals. Temporal re-sorting belongs in `mod.rs` calling `temporal::apply_temporal_enrichment` after `execute_query` returns. Mixing sorting responsibilities would break the I/O boundary between query execution and result enrichment.

**Do not truncate the blast-radius candidate set before ranking**. `blast_radius_paths` must be resolved into a `BlastRadiusStrengths` map before `execute_query` so both arms see the full partner set: the compound text+`--ast` arm intersects it into `SearchQuery.file_filter` before LIMIT, and the composite arm ranks the full lexical∪temporal UNION and truncates LAST (after RRF fusion and verification). Applying `--limit` earlier on either arm silently discards co-change partners that ranked outside the top-N of the unfiltered lexical list.

**Do not add a `file_filter` to the composite blast-radius arm**. `run_blast_radius_composite_query` deliberately runs the lexical search *unfiltered* over a wide `K × limit` pool. A `file_filter` there would drop every text-only match outside the partner set, collapsing the UNION back to an intersection and removing the lexical signal the RRF fusion needs.

**Do not treat missing `temporal.db` as an error**. Temporal data is optional — a fresh repo that has never run `skim heatmap` has no DB. Returning exit 1 would break any script that runs `skim search --hot` on CI before the first heatmap run.

## Gotchas

**Co-change pairs are stored lexicographically**: `CochangeRow.file_a` is always the lexically smaller path. The helper `cochange_partner(row, target)` resolves both directions. Callers that access `row.file_b` directly without this helper will miss half the pairs.

**Empty query with temporal flags is standalone mode, not an error**. The `run()` dispatch treats `SearchAction::Query("")` with temporal flags as standalone — this is deliberate. A user who types `skim search --hot` gets a hotspot table, not a "query required" error.

**`--blast-radius` without text query in standalone mode re-sorts co-change partners in memory** (not via `db.top_hotspots`). The in-memory re-sort builds a `HashMap` from a full DB scan. For large repos with many hotspot rows, this is O(n) on the hotspot table, not the partner list.

**`apply_temporal_enrichment` uses `load_hotspots()` / `load_risks()` (full table scan)**, not the paginated `top_hotspots()`. This is intentional: the text search already limited the result set to ≤ `limit` files, and we need all temporal scores to annotate them correctly. If the temporal DB grows very large, this may become a bottleneck.

**`check_temporal_staleness` is `#[cfg(test)]` only**. It is not used from any
production query path (Decision O-B in `cmd-search`). Production temporal staleness
uses `temporal_db_is_stale(cache_dir, current_head, git_dir)` in `staleness.rs`
(three lightweight SQLite reads: HEAD match, data_version, shallow→full probe).

## Key Files

- `crates/rskim/src/cmd/search/temporal.rs` — all temporal helpers: path normalization, DB open/check, standalone query dispatch, text+temporal enrichment, output formatters
- `crates/rskim/src/cmd/search/temporal_tests.rs` — co-located tests for temporal.rs (linked via `#[path]` attribute)
- `crates/rskim/src/cmd/search/types.rs` — `TemporalSort`, `TemporalAnnotation`, `ResolvedResult` (with `temporal` field), `QueryConfig` (with `blast_radius_paths: Option<BlastRadiusStrengths>`), `BlastRadiusStrengths` type alias (`HashMap<String, f64>`)
- `crates/rskim/src/cmd/search/mod.rs` — top-level dispatch: `run_query` (combined mode), `run_temporal_standalone` (standalone mode), `parse_flags` (mutual exclusion enforcement)
- `crates/rskim/src/cmd/search/query.rs` — BM25F search execution; `file_filter` injection from `blast_radius_paths`; `temporal_annotation_tag` for text output suffix
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb`, `HotspotRow`, `RiskRow`, `CochangeRow`, `META_GIT_HEAD`

## Related

- Feature knowledge: `temporal-scoring` — the `TemporalDb`, `HotspotRow`, `RiskRow`, and scoring algorithms this feature consumes
- Feature knowledge: `cochange` — the co-change matrix and `CochangeRow` this feature uses for blast-radius queries
- `crates/rskim-search/src/lib.rs` — public re-exports: `TemporalDb`, `HotspotRow`, `RiskRow`, `CochangeRow`, `META_GIT_HEAD`
