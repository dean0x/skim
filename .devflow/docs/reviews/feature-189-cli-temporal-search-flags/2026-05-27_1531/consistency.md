# Consistency Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### MEDIUM

**JSON mode field uses snake_case (`blast_radius`) while other modes use bare lowercase (`hot`, `cold`, `risky`)** - `crates/rskim/src/cmd/search/temporal.rs:447`
**Confidence**: 85%
- Problem: The JSON `mode` field is `"blast_radius"` (snake_case) while the other three modes are `"hot"`, `"cold"`, `"risky"` (bare lowercase, no separators). This is an internal inconsistency in the same function. A consumer parsing the `mode` field cannot predict the naming convention. The CLI flag is `--blast-radius` (kebab-case), making the JSON value a third convention.
- Fix: Choose one convention. The simplest consistent option is `"blast-radius"` (matching the CLI flag, as the other modes already match their flags), or `"blast_radius"` is acceptable if you document that JSON fields always use snake_case. Either way, the test on `temporal_tests.rs:1014` must be updated to match.

```rust
// Option A: Match CLI flag convention (kebab-case)
"mode": "blast-radius",

// Option B: Keep snake_case but document the convention
// (current code — acceptable if intentional)
"mode": "blast_radius",
```

**Standalone temporal JSON output uses hand-built `serde_json::json!` while combined-mode JSON uses serde `Serialize` derive** - `crates/rskim/src/cmd/search/temporal.rs:392-456`
**Confidence**: 82%
- Problem: The combined text+temporal path serializes `QueryOutput` via `serde_json::to_string_pretty(output)` (derived `Serialize`), while the standalone temporal path manually builds JSON with `serde_json::json!({...})`. This dual-approach means field naming and structure are maintained in two different places. The `QueryOutput` path uses `snake_case` field names from the derive attributes. The standalone path hardcodes field names as string literals. If a field is renamed in one place, the other will silently drift.
- Fix: Define a `TemporalStandaloneOutput` struct with `#[derive(Serialize)]` and use `serde_json::to_string_pretty` for both paths. This eliminates the hand-built JSON and ensures a single source of truth for field names.

```rust
// Example: typed output for standalone temporal JSON
#[derive(Serialize)]
struct StandaloneTemporalJson {
    mode: String,
    total: usize,
    results: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`run_temporal_standalone` JSON warning uses `println!` while `run_stats` JSON error uses `println!` -- mixed stderr/stdout degradation patterns** - `crates/rskim/src/cmd/search/mod.rs:475` vs `crates/rskim/src/cmd/search/mod.rs:317`
**Confidence**: 83%
- Problem: Both `run_stats` (line 317: `println!("{{\"error\": \"no index found\"}}")`) and `run_temporal_standalone` (line 475: `println!("{{\"warning\": \"no temporal data...\"}}"))`) emit degradation JSON to stdout. However, `run_stats` uses the key `"error"` and returns `ExitCode::FAILURE`, while `run_temporal_standalone` uses the key `"warning"` and returns `ExitCode::SUCCESS`. The inconsistency is in what the JSON consumer should expect: is a missing prerequisite an `"error"` or a `"warning"`? Both are missing-prerequisite conditions (no index vs no temporal data). The exit code difference is intentional per the AC (F12), but the JSON key naming should follow a consistent pattern for machine consumers.
- Fix: Consider using a consistent JSON structure for degradation messages. If the difference in exit code is intentional (it is), then the JSON key difference (`error` vs `warning`) is defensible but should be documented. No code change strictly required -- this is a consistency observation for future API documentation.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Text formatter column alignment drift potential** - `crates/rskim/src/cmd/search/temporal.rs:316-383` (Confidence: 68%) -- The text table formatters for Hotspots, Coldspots, Risks, and Cochanges each hardcode their own column headers and separator widths independently. If column widths change, the four format blocks must be updated in lockstep. A shared table formatting helper would prevent drift, but the current duplication is manageable at 4 variants.

- **`TemporalAnnotation` struct fields use `Option<f64>` for all scores but the struct has no validation** - `crates/rskim/src/cmd/search/types.rs:44-58` (Confidence: 65%) -- Fields like `hotspot_score`, `risk_score`, `fix_density` are all `Option<f64>` with no range constraint. The upstream DB ensures values are in `[0.0, 1.0]`, so this is low risk, but it means the struct is a bag of optional floats with no type-level distinction between them. Branded newtypes (`HotspotScore(f64)`) would add type safety, but the current approach matches the existing `SearchResult.score: f64` pattern elsewhere.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new temporal search module is well-structured and follows existing codebase conventions closely. Error handling patterns (`Result` types, `map_err(db_err)?` chains), module organization (`types.rs` for data, `temporal.rs` for logic, `temporal_tests.rs` co-located), doc-comment style (with `# Errors` sections), and the `eprintln!("skim search: ...")` prefix pattern are all consistent with the existing search module.

The two blocking MEDIUM findings are:
1. The `mode` field naming inconsistency in standalone temporal JSON (`blast_radius` vs bare lowercase for other modes) -- minor but worth aligning before the API is consumed.
2. The dual JSON serialization approach (hand-built `json!` for standalone vs derived `Serialize` for combined) creates a maintenance divergence risk. Not urgent, but worth addressing to prevent field-naming drift as the temporal API evolves.

The `collect::<rusqlite::Result<Vec<_>>>()` pattern in the new storage_ops methods is consistent with the existing `load_*` methods. The `pub(super)` visibility on all new temporal helpers matches the existing module-private convention. The `#[serde(skip_serializing_if = "Option::is_none")]` pattern on `TemporalAnnotation` matches the project convention for optional JSON fields. Overall, this is a clean, well-patterned addition.
