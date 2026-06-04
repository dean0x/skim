# Consistency Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23
**Cycle**: 3 (cross-cycle aware -- 23 issues resolved in cycle 2)

## Issues in Your Changes (BLOCKING)

### HIGH

**JSON warning output uses raw `println!` while all other JSON output uses typed structs or `serde_json`** - `crates/rskim/src/cmd/search/mod.rs:510`
**Confidence**: 82%
- Problem: The standalone temporal path emits a degradation warning as hand-formatted JSON via `println!("{{\"warning\": \"no temporal data...\"}}");`. This is inconsistent with the approach taken throughout the rest of this PR, where all JSON output uses typed `#[derive(Serialize)]` structs (e.g., `HotColdJson`, `RiskyJson`, `BlastRadiusJson`). The cycle 2 resolution specifically fixed `json!` macro usage and moved to typed structs for exactly this reason -- preventing key drift. The existing `run_stats` also uses hand-formatted JSON for its error case (`line 338`), so there is a pre-existing precedent, but the new code should follow the improved pattern it established elsewhere in this PR.
- Fix: Create a small typed struct for warning/error JSON responses and use it consistently:
  ```rust
  #[derive(Serialize)]
  struct StatusJson<'a> { warning: &'a str }
  // then:
  let msg = StatusJson { warning: "no temporal data -- run 'skim heatmap' to populate" };
  println!("{}", serde_json::to_string(&msg)?);
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Staleness warning prefix inconsistency** - `crates/rskim/src/cmd/search/mod.rs:519` (Confidence: 70%) -- The staleness warning emitted by `check_temporal_staleness` uses the prefix `"skim search: temporal data is stale"` (from `temporal.rs:135`), but the `eprintln!("{warning}")` at line 519 passes it through raw. Other warnings in this module use the `"skim search: ..."` prefix directly at the call site. This is a minor difference since the warning is formatted inside the function, but the inconsistency between constructing messages at the call site vs. inside helper functions could drift over time.

- **`println!` vs `BufWriter` for JSON output in degradation path** - `crates/rskim/src/cmd/search/mod.rs:510` (Confidence: 65%) -- The standalone temporal path's happy-path JSON uses `BufWriter::new(std::io::stdout())` (line 524-530), but the degradation path uses unbuffered `println!` (line 510). For a single short line this has no performance impact, but it creates a pattern inconsistency where two stdout writes in the same function use different mechanisms. The pre-existing `run_stats` has the same pattern (line 338 vs line 365), so this is not novel to this PR.

- **`resolve_blast_radius_filter` emits warning to stderr but returns `Ok(None)` silently when `--json` mode is active** - `crates/rskim/src/cmd/search/mod.rs:427` (Confidence: 62%) -- When `--blast-radius` is used with `--json` in combined text+temporal mode, and the temporal DB is missing, the warning goes to stderr via `eprintln!`. In standalone temporal mode (line 509-513), the same situation produces a JSON warning on stdout when `--json` is active. The combined mode does not check `flags.json` before deciding where to send the warning. This is a minor UX inconsistency for JSON-consuming tools.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR demonstrates strong consistency overall. The new temporal module follows established codebase patterns faithfully: `pub(super)` visibility, `anyhow::Result` error handling, section separators (`// ==...==`), `#[path = ...]` test co-location, snake_case JSON keys matching Rust field names, `BufWriter` for stdout output, and the `"skim search: ..."` warning prefix. The cycle 2 resolutions (typed JSON structs, `rusqlite::Result` collect, format consolidation) are cleanly applied. The single blocking issue is a small remnant of the hand-formatted JSON pattern that the PR itself improved away from elsewhere.
