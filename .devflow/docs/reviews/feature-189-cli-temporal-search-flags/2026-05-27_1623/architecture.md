# Architecture Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23
**Prior Resolutions**: Cycle 2 resolved 19 of 23 issues (4 false positive). This cycle focuses on residual architectural concerns not addressed in prior resolution rounds.

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Redundant file_filter guard in reader.rs scoring pipeline** - `crates/rskim-search/src/index/reader.rs:362,391`
**Confidence**: 85%
- Problem: The `file_filter` allowlist is checked twice: once in the first sub-pass (line 362, inside the posting iteration loop) and again when collecting `scored` from `doc_scores` (line 391). The first-sub-pass check already prevents non-allowlisted docs from entering `tf_per_doc`, which means they never accumulate scores in `doc_scores`. The second filter on lines 391-398 is therefore a no-op when the first-sub-pass check is present. This is a defense-in-depth choice, but it adds unnecessary iteration over `doc_scores` for blast-radius queries and obscures the actual filtering boundary. A clearer architecture would filter at exactly one layer.
- Fix: Remove the redundant second filter (lines 390-398) and replace with the unconditional `doc_scores.into_iter().collect()`. Add a comment at the first-sub-pass check noting it is the sole enforcement point. Alternatively, if defense-in-depth is intentional, add a comment explaining why both checks exist.

**Missing staleness check in combined text+temporal path** - `crates/rskim/src/cmd/search/mod.rs:446-496`
**Confidence**: 82%
- Problem: `run_temporal_standalone` (line 498) checks temporal staleness via `check_temporal_staleness(&db, &root)` and emits a warning when the temporal DB is behind the current git HEAD. However, `run_query` (line 446) -- the combined text+temporal path -- opens the temporal DB (line 457) and uses it for enrichment (line 482) without ever checking staleness. Users running `skim search "auth" --hot` get temporally-enriched results from a potentially stale DB with no warning, while `skim search --hot` warns them. This asymmetry means the combined path silently serves stale data.
- Fix: Add a staleness check in `run_query` after opening the temporal DB:
  ```rust
  if let Some(ref db) = temporal_db {
      if let Some(warning) = temporal::check_temporal_staleness(db, &root) {
          eprintln!("{warning}");
      }
  }
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`output.total` reassignment after enrichment masks actual result count** - `crates/rskim/src/cmd/search/mod.rs:484`
**Confidence**: 83%
- Problem: After `apply_temporal_enrichment` (which sorts in-place but does not add or remove elements), `output.total` is reassigned to `output.results.len()`. Since enrichment never changes the slice length, this assignment is always a no-op. However, it signals to future readers that enrichment might change the count, which is misleading. Worse, if someone later adds filtering inside `apply_temporal_enrichment`, the `total` field (which originated from the search engine and may have semantic meaning beyond just the Vec length) would silently diverge from the engine's count.
- Fix: Remove the `output.total = output.results.len();` line. If future filtering is anticipated, add a comment explaining the contract: enrichment mutates annotations and order but never changes the result set size.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`TemporalSort` and `TemporalAnnotation` live in CLI types.rs but are consumed by library-level `TemporalDb` indirectly** - `crates/rskim/src/cmd/search/types.rs:21,45` (Confidence: 65%) -- The sort-mode enum and annotation struct are defined in the CLI crate (`rskim`) rather than the library crate (`rskim-search`). Currently the library returns raw rows (`HotspotRow`, `RiskRow`) and the CLI maps them to annotations. This is a reasonable boundary today, but if a second consumer (e.g., an API server or different CLI) needs temporal enrichment, the mapping logic in `temporal.rs` would need to be duplicated. Consider whether `TemporalAnnotation` belongs in the library.

- **`resort_partners_by_temporal` clones the full Vec for permutation** - `crates/rskim/src/cmd/search/temporal.rs:328` (Confidence: 70%) -- The permutation is applied by collecting into a new `Vec<_>` via `indices.into_iter().map(|i| partners[i].clone()).collect()` and then replacing `*partners = temp`. For small result sets (clamped to `limit*5` or 100) this is fine, but an in-place permutation using `swap` would avoid the clone. Minor efficiency concern.

- **`normalize_blast_radius_path` uses `std::env::current_dir()` at call time** - `crates/rskim/src/cmd/search/temporal.rs:70` (Confidence: 62%) -- The function reads CWD as a fallback. If this function were ever called from a long-running server where CWD could change, it would produce non-deterministic results. For the current CLI use case this is safe, but capturing CWD once at process start and threading it through would be more robust.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The architecture is well-structured overall. The feature follows the existing crate boundary cleanly: temporal storage operations (`hotspot_for_file`, `risk_for_file`, `cochanges_for_file`, top-N queries) are correctly placed in the library crate (`rskim-search`), while CLI-specific concerns (flag parsing, path normalization, output formatting, enrichment orchestration) are in the binary crate (`rskim`). The new `temporal.rs` module has a clear single responsibility with well-documented public helpers. The `SearchQuery.file_filter` extension is a clean, non-breaking addition to the library's query model. The schema migration from v1 to v2 with performance indexes is handled correctly with forward-compatible version checks.

Conditions for approval:
1. Address the missing staleness check in the combined text+temporal path (BLOCKING MEDIUM).
2. Resolve the redundant file_filter guard -- either remove the second check or document the defense-in-depth intent (BLOCKING MEDIUM).
