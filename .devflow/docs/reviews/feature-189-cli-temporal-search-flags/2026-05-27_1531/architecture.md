# Architecture Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### MEDIUM

**File filter applied post-scoring instead of pre-scoring wastes CPU** - `crates/rskim-search/src/index/reader.rs:381-389`
**Confidence**: 82%
- Problem: The `file_filter` allowlist is applied after all documents have been scored (line 381-389). Every document in every posting list is still decoded, TF-accumulated, and BM25F-scored before being discarded by the filter. For a blast-radius set of, say, 20 files in a 50k-file index, this means 99.96% of scoring work is wasted. The comment on `QueryConfig.blast_radius_paths` (types.rs:101-106) says "applied inside the search engine (before LIMIT)" but "before LIMIT" is not the same as "before scoring" -- the filter happens after scoring but before limit truncation.
- Fix: Move the filter check into the `score_ngram_postings` loop or the `tf_per_doc` accumulation pass so documents not in the filter are skipped before scoring. This is an optimization concern rather than a correctness bug, but the architecture comment is misleading about when filtering occurs.

```rust
// In the first sub-pass (line 352-364), skip non-allowed docs early:
for p in &postings {
    if p.doc_id >= self.header.file_count {
        continue;
    }
    // Skip documents not in the allowlist before accumulating TF
    if let Some(ref filter) = query.file_filter {
        if !filter.contains(&FileId(p.doc_id)) {
            continue;
        }
    }
    // ... rest of accumulation
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`run_query` blast-radius resolution duplicates logic from `temporal.rs`** - `crates/rskim/src/cmd/search/mod.rs:410-430`
**Confidence**: 85%
- Problem: The partner-path extraction logic in `run_query` (lines 416-429) manually reimplements the co-change partner resolution that `cochange_partner()` already encapsulates in `temporal.rs:168-174`. The inline `if p.file_a == normalized { p.file_b.clone() } else { p.file_a.clone() }` duplicates that helper. This is a DRY violation -- if the co-change storage format changes (e.g. canonical ordering rules), this inline code would need a parallel update.
- Fix: Extract the partner-path-set construction into a helper in `temporal.rs` (or reuse `cochange_partner` via a public iterator/collect pattern):

```rust
// In temporal.rs, add:
pub(super) fn cochange_partner_paths(
    partners: &[rskim_search::CochangeRow],
    target: &str,
) -> std::collections::HashSet<String> {
    let mut paths: std::collections::HashSet<String> = partners
        .iter()
        .map(|p| cochange_partner(p, target).to_string())
        .collect();
    paths.insert(target.to_string());
    paths
}

// In mod.rs run_query, replace lines 416-430 with:
let paths = temporal::cochange_partner_paths(&partners, &normalized);
blast_radius_paths = Some(paths);
```

**`run_query` function signature growing toward 7+ parameters** - `crates/rskim/src/cmd/search/mod.rs:386-394`
**Confidence**: 80%
- Problem: `run_query` now takes 7 parameters: `text`, `limit`, `json`, `root_override`, `temporal_sort`, `blast_radius`, `analytics`. This is approaching the 7+ parameter threshold that signals a god-function or a missing configuration struct. The existing `Flags` struct already holds all these values but is destructured before the call. Similarly, `run_temporal_standalone` takes 5 parameters that overlap heavily with `run_query`.
- Fix: Pass `&Flags` (or a subset struct) directly to `run_query` and `run_temporal_standalone` instead of destructuring into individual parameters. This also makes future flag additions (e.g. `--author`, `--since`) a non-breaking change:

```rust
fn run_query(
    flags: &Flags,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let text = match &flags.action {
        SearchAction::Query(t) => t.as_str(),
        _ => unreachable!(),
    };
    // ... use flags.limit, flags.json, etc.
}
```

## Pre-existing Issues (Not Blocking)

(none found at CRITICAL severity)

## Suggestions (Lower Confidence)

- **`TemporalDb` not behind a trait** - `crates/rskim/src/cmd/search/temporal.rs` (Confidence: 70%) -- The `temporal.rs` module directly depends on the concrete `TemporalDb` type from `rskim_search`. For testability in the CLI layer, a trait abstraction (`TemporalStore`) would allow injecting a mock without requiring a real SQLite database. The test suite works around this by creating real temp DBs, which is acceptable but slower than trait-based mocks.

- **`resort_partners_by_temporal` clone-and-replace pattern** - `crates/rskim/src/cmd/search/temporal.rs:249-301` (Confidence: 65%) -- The function builds a `scored` vec of `(index, score)`, sorts it, then clones partners via index lookup into a new vec and replaces the original via `*partners = reordered`. This allocates a second full copy. A `sort_by_cached_key` approach would sort in-place with a single allocation for the key cache. Minor for typical blast-radius sizes (tens of files) but worth noting.

- **Hardcoded `LIMIT 10000` on cochange query** - `crates/rskim-search/src/temporal/storage_ops.rs:158` (Confidence: 72%) -- The `cochanges_for_file` query has `LIMIT 10000` which is a safety bound. However, this limit is not configurable and not documented as a constant. If a file has more than 10,000 co-change partners, results are silently truncated. Consider extracting to a named constant and documenting the truncation behavior.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The architecture is well-structured overall. The new `temporal.rs` module follows the existing codebase pattern of separating I/O orchestration (mod.rs) from business logic helpers. Responsibilities are clearly documented in the module-level doc comment. The type hierarchy (`TemporalSort`, `TemporalAnnotation`, `TemporalQueryOutput`) is well-designed with proper enum dispatch. The layering between `rskim-search` (storage/DB) and `rskim` (CLI/formatting) is clean -- the CLI layer correctly depends on the search crate, not the reverse.

Key strengths:
- Clean separation: DB queries in `rskim-search`, path normalization / formatting / orchestration in `rskim`
- `SearchQuery.file_filter` extends the existing query API cleanly without breaking existing callers
- Schema migration (v1 to v2) is properly gated and idempotent
- Graceful degradation pattern (missing DB returns exit 0 with warning) is consistent with the project philosophy
- `TemporalQueryOutput` enum properly models the four output variants, enabling exhaustive match

Conditions for approval: Address the DRY violation in partner-path extraction (Should Fix, MEDIUM). The post-scoring filter and parameter count issues are worth tracking but are not blocking.
