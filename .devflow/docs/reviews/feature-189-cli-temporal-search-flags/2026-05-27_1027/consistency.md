# Consistency Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27
**PR**: #257

## Issues in Your Changes (BLOCKING)

### HIGH

**JSON field name "limit" should be "total" for result count** - `crates/rskim/src/cmd/search/temporal.rs:398,417,435`
**Confidence**: 92%
- Problem: The standalone temporal JSON output (`format_temporal_json`) uses `"limit"` as the JSON key for the actual number of results returned (`rows.len()` / `partners.len()`). However, the existing text-query JSON output via `QueryOutput` (serialized in `format_json_output`) uses `"total"` for this same concept. The value assigned to `"limit"` is not the user-requested limit -- it is the count of results returned, which makes the field name both inconsistent with the existing API and semantically misleading.
- Fix: Rename `"limit"` to `"total"` in all three `serde_json::json!` blocks in `format_temporal_json`:
```rust
// In Hotspots/Coldspots block:
serde_json::json!({
    "mode": mode,
    "total": rows.len(),
    "results": results,
})

// In Risks block:
serde_json::json!({
    "mode": "risky",
    "total": rows.len(),
    "results": results,
})

// In Cochanges block:
serde_json::json!({
    "mode": "blast_radius",
    "target": target,
    "total": partners.len(),
    "results": results,
})
```

### MEDIUM

**Manual JSON construction diverges from serde derive pattern** - `crates/rskim/src/cmd/search/temporal.rs:375-442`
**Confidence**: 82%
- Problem: The existing `format_json_output` in `query.rs:217-224` serializes `QueryOutput` via `serde_json::to_string_pretty(output)`, leveraging the struct's `#[derive(Serialize)]`. The new `format_temporal_json` manually constructs JSON objects with `serde_json::json!` macros. This means two different serialization approaches produce the same command's JSON output, increasing the risk of field naming drift (as demonstrated by the "limit" vs "total" issue above). If `TemporalQueryOutput` had a `Serialize` derive with `#[serde(rename = "...")]` annotations, the field names would be centralized in the type definition.
- Fix: Consider defining a `#[derive(Serialize)]` struct for the temporal JSON envelope (e.g., `TemporalJsonEnvelope`) and using `serde_json::to_string_pretty` for consistency. This is not urgent since the manual approach works correctly apart from the naming issue, but it would prevent future drift.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`TemporalAnnotation` has 6 Optional fields but Hot enrichment only fills 3** - `crates/rskim/src/cmd/search/temporal.rs:478-483` (Confidence: 65%) -- When `TemporalSort::Hot` enriches results, it populates `hotspot_score`, `changes_30d`, and `changes_90d`, but leaves `risk_score`, `fix_density`, and `cochange_jaccard` as `None`. Similarly, `Risky` enrichment fills `risk_score` and `fix_density` but not the hotspot fields. While this is not incorrect (the `skip_serializing_if` annotation hides the null fields), a consumer cannot easily distinguish "not applicable" from "data missing." This may be intentional for composability, but worth noting.

- **`query_standalone` with `sort: None` defaults to Hot** - `crates/rskim/src/cmd/search/temporal.rs:278` (Confidence: 70%) -- When `sort` is `None` and no `blast_radius` is provided, the `match` arm `Some(TemporalSort::Hot) | None` defaults to returning hotspots. The function is currently only called when at least one temporal flag is set, so this path is unreachable in practice. But the implicit default could surprise future callers who pass `None` expecting an error or a different default.

- **`run_query` opens temporal DB eagerly even when not used by the query** - `crates/rskim/src/cmd/search/mod.rs:404-408` (Confidence: 62%) -- The temporal DB is opened (filesystem access) whenever `temporal_sort.is_some() || blast_radius.is_some()`, even before checking whether the DB will actually be used. The `open_temporal_db` call is cheap (just an `exists()` + `open()`) so this is not a performance concern, but it is a pattern divergence from the lazy-open approach used by the search index reader.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The PR is well-structured and follows established codebase conventions for module organization, test placement, error handling, visibility, serde annotation patterns, and doc comments. The single blocking issue is the `"limit"` vs `"total"` naming inconsistency in JSON output -- a small but user-facing divergence from the existing API contract. The serde derive vs manual JSON construction is a secondary concern that explains how the naming drift occurred.
