# Rust Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27

## Issues in Your Changes (BLOCKING)

### MEDIUM

**`load_hotspots()` / `load_risks()` full-table scan in `apply_temporal_enrichment` and `query_standalone` re-sort** - `crates/rskim/src/cmd/search/temporal.rs:222`, `temporal.rs:247`, `temporal.rs:466`, `temporal.rs:507`
**Confidence**: 82%
- Problem: `apply_temporal_enrichment` (lines 466-505) and the blast-radius re-sort path in `query_standalone` (lines 222-265) call `db.load_hotspots()` / `db.load_risks()` which deserialise the ENTIRE table (up to 500,000 rows) into memory just to build a HashMap for scoring a handful of results. The new per-file lookup methods (`hotspot_for_file`, `risk_for_file`) were added in this same PR but are not used here.
- Fix: For `apply_temporal_enrichment`, iterate over the (typically small) `results` slice and call `db.hotspot_for_file(path)` / `db.risk_for_file(path)` per result instead of loading all rows. For the blast-radius re-sort, do the same with the (typically small) `partners` vec. This avoids allocating a HashMap of potentially hundreds of thousands of entries.
```rust
// apply_temporal_enrichment — Hot/Cold path (suggested fix)
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

---

**Silently swallowed `--blast-radius` path normalisation error in `run_query`** - `crates/rskim/src/cmd/search/mod.rs:429-431`
**Confidence**: 85%
- Problem: When `normalize_blast_radius_path` fails in the combined text+temporal path (`run_query`), the error is printed to stderr but execution continues with `blast_radius_paths` remaining `None`. This means the query runs unfiltered (the user asked for blast-radius filtering but gets full results), which is silently incorrect behaviour. The standalone path (`query_standalone`) correctly propagates the error via `?`.
- Fix: Propagate the error so the user knows their `--blast-radius` argument was invalid, rather than returning misleading unfiltered results.
```rust
// mod.rs line 411-432 — propagate instead of swallow
if let (Some(raw_path), Some(db)) = (blast_radius, &temporal_db) {
    let normalized = temporal::normalize_blast_radius_path(raw_path, &root)?;
    let partners = db.cochanges_for_file(&normalized)?;
    if partners.is_empty() {
        eprintln!("skim search: no co-change data for {raw_path:?}");
    }
    let paths: std::collections::HashSet<String> = partners
        .iter()
        .map(|p| {
            if p.file_a == normalized { p.file_b.clone() } else { p.file_a.clone() }
        })
        .collect();
    blast_radius_paths = Some(paths);
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`std::env::set_current_dir` in tests is not thread-safe** - `crates/rskim/src/cmd/search/temporal_tests.rs:55`, `temporal_tests.rs:131`
**Confidence**: 80%
- Problem: Two tests call `std::env::set_current_dir(&root)` which mutates process-global state. With `cargo test` running tests in parallel within the same process, this can cause flaky failures in other tests that depend on the current directory. This is a known Rust testing anti-pattern.
- Fix: Use `#[serial]` from the `serial_test` crate, or restructure the tests to avoid relying on `set_current_dir`. Alternatively, since `normalize_blast_radius_path` has the CWD fallback as a secondary resolution strategy and the primary path (project-root-relative) does not need CWD, the `set_current_dir` call may not even be necessary — the root-relative resolution should succeed without it.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`temporal_annotation_tag` allocates a `Vec<String>` for at most 2 items** - `crates/rskim/src/cmd/search/query.rs:158` (Confidence: 65%) -- Could use a fixed-size array or `write!` directly to a `String` to avoid the Vec allocation, though the allocation is trivially small per-result.

- **`query_standalone` blast-radius re-sort loads full table redundantly** - `crates/rskim/src/cmd/search/temporal.rs:222-265` (Confidence: 70%) -- The blast-radius + sort code path calls `db.load_hotspots()` to build a HashMap for sorting a handful of co-change partners. Since the per-file lookup methods exist, using `hotspot_for_file` per-partner would be O(k) queries instead of loading the full table. However, for small partner sets this is a micro-optimization.

- **Missing `#[must_use]` on `TemporalSort::flag_name()`** - `crates/rskim/src/cmd/search/types.rs:32` (Confidence: 65%) -- Per project conventions (`#[must_use]` on functions with important return values), this pure function returning `&'static str` should have the attribute.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

Overall this is a well-structured PR. The new `temporal.rs` module is cleanly separated, error handling consistently uses `Result`, the `#[serde(skip)]` / `skip_serializing_if` usage is correct, the `&mut [ResolvedResult]` signature follows clippy guidance, and the per-file lookup methods use the proper `QueryReturnedNoRows -> Ok(None)` pattern from the feature knowledge. The DB migration from v1 to v2 with performance indexes is well-implemented with the correct observation that the PK already covers `file_a`. Test coverage is thorough (33+ temporal tests, flag parsing, enrichment, standalone dispatch, format output).

The two blocking MEDIUM issues are: (1) the full-table scan in enrichment paths when per-file lookups are available in the same PR, and (2) silently swallowing the blast-radius normalisation error in the combined query path, which can produce misleading unfiltered results.
