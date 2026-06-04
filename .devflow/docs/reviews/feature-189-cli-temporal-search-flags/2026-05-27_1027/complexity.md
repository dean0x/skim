# Complexity Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27
**PR**: #257

## Issues in Your Changes (BLOCKING)

### HIGH

**`query_standalone` has high cyclomatic complexity from nested sort-mode dispatch** - `crates/rskim/src/cmd/search/temporal.rs:207-291`
**Confidence**: 85%
- Problem: `query_standalone` (84 lines) contains a two-level nested match: first on `blast_radius` presence, then on `sort` mode, each with three sort-specific branches that load full tables and build temporary HashMaps. The combined cyclomatic complexity is approximately 10 (blast_radius presence + sort-mode triple + inner sort-mode triple within blast_radius + None fallback). The blast-radius-with-sort case (lines 219-267) alone has three deeply nested branches with repeated load-table-then-sort-by-partner patterns.
- Fix: Extract the blast-radius-with-sort re-sorting into a separate function:

```rust
fn resort_partners_by_temporal(
    partners: &mut Vec<CochangeRow>,
    sort: TemporalSort,
    normalized: &str,
    db: &TemporalDb,
) -> anyhow::Result<()> {
    match sort {
        TemporalSort::Hot | TemporalSort::Cold => {
            let hotspots = db.load_hotspots()?;
            let map: HashMap<&str, f64> = hotspots.iter().map(|h| (h.file_path.as_str(), h.score)).collect();
            partners.sort_by(|a, b| {
                let sa = map.get(cochange_partner(a, normalized)).copied().unwrap_or(0.0);
                let sb = map.get(cochange_partner(b, normalized)).copied().unwrap_or(0.0);
                let cmp = sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal);
                if sort == TemporalSort::Cold { cmp } else { cmp.reverse() }
            });
        }
        TemporalSort::Risky => { /* similar extraction */ }
    }
    Ok(())
}
```
This reduces `query_standalone` to ~30 lines and makes each sort strategy independently testable.

---

**`apply_temporal_enrichment` has duplicated Hot/Cold vs Risky branches** - `crates/rskim/src/cmd/search/temporal.rs:459-547`
**Confidence**: 82%
- Problem: This function (88 lines) has two large arms in the match: `Hot | Cold` (lines 465-505, 40 lines) and `Risky` (lines 506-544, 38 lines). Both arms follow the identical pattern: load table -> build HashMap -> annotate results -> sort. The only differences are (1) which table is loaded, (2) which TemporalAnnotation fields are set, and (3) the sort comparator. This structural duplication increases cognitive load and makes it easy to introduce inconsistencies when modifying one arm but not the other.
- Fix: Extract a generic enrichment helper parameterized by a score-extraction closure:

```rust
fn enrich_and_sort<F>(
    results: &mut [ResolvedResult],
    scores: &HashMap<&str, f64>,
    annotate: F,
    descending: bool,
) where
    F: Fn(f64) -> TemporalAnnotation,
{
    for result in results.iter_mut() {
        if let Some(&score) = scores.get(result.path.as_str()) {
            result.temporal = Some(annotate(score));
        }
    }
    results.sort_by(|a, b| {
        let extract = |r: &ResolvedResult| { /* score from annotation or -1.0 */ };
        let cmp = extract(a).partial_cmp(&extract(b)).unwrap_or(Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path));
        if descending { cmp.reverse() } else { cmp }
    });
}
```

### MEDIUM

**`run_query` accumulates too many concerns in one function** - `crates/rskim/src/cmd/search/mod.rs:386-463`
**Confidence**: 82%
- Problem: `run_query` (77 lines) handles: (1) root/cache resolution, (2) temporal DB opening, (3) blast-radius path normalization and partner resolution, (4) QueryConfig construction, (5) query execution, (6) temporal enrichment, and (7) output formatting. This is 7 distinct responsibilities in one function. The blast-radius resolution block alone (lines 402-435) contains nested conditionals with early-print-and-continue fallbacks. Cyclomatic complexity is approximately 8.
- Fix: Extract the blast-radius resolution into its own function:

```rust
fn resolve_blast_radius(
    blast_radius: Option<&str>,
    temporal_db: Option<&TemporalDb>,
    root: &Path,
) -> Option<HashSet<String>> { ... }
```
This makes `run_query` a straightforward orchestrator: resolve -> query -> enrich -> format.

---

**`format_temporal_text` has nested match on output variant with repeated write patterns** - `crates/rskim/src/cmd/search/temporal.rs:298-372`
**Confidence**: 80%
- Problem: 74 lines with a match on 4 variant cases. The `Hotspots | Coldspots` arm contains a secondary match to determine the header/empty message strings (lines 304-312), adding a nesting level. Each arm repeats the pattern: check empty -> write header -> write column headers -> write divider -> write rows. The column-header writing is copy-pasted across arms.
- Fix: Consider extracting a `write_table` helper that accepts headers and a row-formatting closure. The Hotspot/Coldspot inner match is acceptable given it only selects strings, but the overall function would benefit from row-iteration extraction.

---

**`parse_flags` is a linear scan with 18 match arms** - `crates/rskim/src/cmd/search/mod.rs:155-243`
**Confidence**: 80%
- Problem: 88 lines, 18 match arms (6 action flags, 2 json aliases, --limit with 2 forms, --root with 2 forms, 3 temporal sort flags, --blast-radius with 2 forms, unknown flag guard, positional fallback). Cyclomatic complexity is approximately 12. While each arm is simple, the function as a whole requires reading ~90 lines to understand the full flag surface. This is manageable now but will scale poorly if more flags are added.
- Fix: Not immediately blocking, but consider migrating to a struct-based flag parser (either clap or a custom `FlagParser` that separates flag-arm matching from validation). This would reduce the linear scan to a declarative structure.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Repeated `load_hotspots()`/`load_risks()` in `query_standalone` blast-radius path** - `temporal.rs:222-264` (Confidence: 70%) -- When `--blast-radius --hot` is used, `load_hotspots()` loads the entire table into memory just to sort ~N partner rows. For small datasets this is fine, but for 500K-row tables this is wasteful. A targeted `SELECT ... WHERE file_path IN (...)` would be more efficient.

- **`storage_ops.rs` methods have high structural repetition** - `crates/rskim-search/src/temporal/storage_ops.rs:95-269` (Confidence: 65%) -- The six new `TemporalDb` methods (`hotspot_for_file`, `risk_for_file`, `cochanges_for_file`, `top_hotspots`, `top_risks`, `top_coldspots`) share identical error-mapping boilerplate. A generic `query_single` or `query_list` helper could reduce this.

- **`mod.rs` file length is 842 lines including tests** - `crates/rskim/src/cmd/search/mod.rs` (Confidence: 65%) -- Approaching the 500-line warning threshold for the non-test portion (roughly 500 lines without tests). Not critical given tests are co-located, but worth monitoring as temporal features continue to grow.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new temporal module is well-structured at the module level -- `temporal.rs` for logic, `types.rs` for data, `temporal_tests.rs` for tests. The separation between standalone dispatch and combined enrichment is clean. However, the two HIGH findings (`query_standalone` and `apply_temporal_enrichment`) have enough cyclomatic complexity and structural duplication to warrant extraction before the next round of temporal features builds on top of them. The MEDIUM findings on `run_query` and `format_temporal_text` are worth addressing but do not block merge.
