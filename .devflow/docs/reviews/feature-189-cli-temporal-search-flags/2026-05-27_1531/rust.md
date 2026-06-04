# Rust Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**`resort_partners_by_temporal` accepts `&mut Vec<T>` instead of `&mut [T]`** - `crates/rskim/src/cmd/search/temporal.rs:248`
**Confidence**: 82%
- Problem: The function signature takes `partners: &mut Vec<rskim_search::CochangeRow>` but Clippy's `clippy::ptr_arg` lint (which the project enforces via `-D warnings`) generally prefers `&mut [T]` over `&mut Vec<T>`. However, this function reassigns the entire vector (`*partners = reordered`), which requires `&mut Vec<T>`. The current approach allocates a second `Vec` to reorder, then replaces the original. An in-place sort-by-key approach would allow the more idiomatic `&mut [CochangeRow]` signature and eliminate the extra allocation + clone.
- Fix: Replace the index-collect-reassign pattern with an in-place sort using `sort_by_cached_key`:
```rust
fn resort_partners_by_temporal(
    partners: &mut [rskim_search::CochangeRow],
    sort_mode: TemporalSort,
    normalized: &str,
    db: &TemporalDb,
) -> anyhow::Result<()> {
    // Pre-compute scores into a parallel vec (one DB lookup per partner).
    let scores: Vec<f64> = partners
        .iter()
        .map(|row| {
            let partner = cochange_partner(row, normalized);
            match sort_mode {
                TemporalSort::Hot | TemporalSort::Cold => db
                    .hotspot_for_file(partner)?
                    .map(|h| h.score)
                    .unwrap_or(0.0),
                TemporalSort::Risky => db
                    .risk_for_file(partner)?
                    .map(|r| r.risk_score)
                    .unwrap_or(0.0),
            }
            .pipe(Ok)
        })
        .collect::<anyhow::Result<_>>()?;

    // Sort in-place using precomputed scores.
    let mut indices: Vec<usize> = (0..partners.len()).collect();
    if sort_mode == TemporalSort::Cold {
        indices.sort_by(|&a, &b| scores[a].partial_cmp(&scores[b]).unwrap_or(std::cmp::Ordering::Equal));
    } else {
        indices.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap_or(std::cmp::Ordering::Equal));
    }
    // Reorder in-place via permutation.
    apply_permutation(partners, &indices);
    Ok(())
}
```
Note: The current code works correctly and Clippy passes, so this is a should-fix style improvement, not a functional defect. Downgraded from blocking given that Clippy currently accepts `&mut Vec` when the body replaces the whole vector.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`run_query` clones `root` and `cache_dir` into `QueryConfig` unnecessarily** - `crates/rskim/src/cmd/search/mod.rs:439-440`
**Confidence**: 83%
- Problem: The diff shows `root: root.clone()` and `cache_dir: cache_dir.clone()` were introduced to satisfy the borrow checker after `temporal_db` was added. The original code moved the `PathBuf` values into `QueryConfig`, which is consumed by `execute_query`. The clones are needed because `temporal_db_path` borrows `cache_dir`. However, the temporal DB is opened before the clone, so the borrow of `cache_dir` could be scoped to avoid the clone.
- Fix: Compute `temporal_db_path` and open the DB in a narrower scope, then move `root` and `cache_dir` into `QueryConfig` without cloning:
```rust
let temporal_db = if temporal_sort.is_some() || blast_radius.is_some() {
    let temporal_db_path = cache_dir.join("temporal.db");
    temporal::open_temporal_db(&temporal_db_path)
} else {
    None
};
// ... blast-radius resolution using temporal_db ...
let config = types::QueryConfig {
    root,       // move, no clone
    cache_dir,  // move, no clone
    // ...
};
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Multiple `String` allocations in `run_query` blast-radius path collection** - `crates/rskim/src/cmd/search/mod.rs:416-424` (Confidence: 68%) -- The closure clones `file_b` or `file_a` from each `CochangeRow` into a new `HashSet<String>`. Since these strings are only used for a membership check against `sorted_paths`, a `HashSet<&str>` borrowing from `partners` could avoid the allocations, but lifetime management would be more complex.

- **`cochanges_for_file` LIMIT 10000 is a magic number** - `crates/rskim-search/src/temporal/storage_ops.rs:158` (Confidence: 65%) -- The SQL query uses `LIMIT 10000` as a hardcoded safety bound. For consistency with the `MAX_ROWS_PER_TABLE` constant used elsewhere in the module, this could be extracted into a named constant (e.g., `MAX_COCHANGE_PARTNERS`).

- **`query_standalone` falls through to `Hot` when `sort` is `None`** - `crates/rskim/src/cmd/search/temporal.rs:231` (Confidence: 72%) -- The match arm `Some(TemporalSort::Hot) | None` means a standalone `--blast-radius` without a sort flag still enters the Hotspots branch. The intent appears correct (standalone blast-radius without sort is handled by the early return above), but the `None` fallthrough could confuse readers. A comment or an explicit unreachable would clarify.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Detailed Assessment

**Strengths:**
- Consistent `Result`-based error handling throughout; no `.unwrap()` in production code.
- `thiserror`-derived `SearchError` in the library crate and `anyhow` at the application (CLI) layer -- follows the Rust skill's prescribed pattern.
- `#[serde(skip)]` correctly used on `file_filter` to prevent runtime-only data from leaking into serialized output.
- Per-file DB lookups (`hotspot_for_file`, `risk_for_file`) avoid unnecessary bulk table loads for small result sets.
- Good use of `Option<T>` for graceful degradation (missing temporal DB returns `None`, not an error).
- `check_temporal_staleness` uses `.get(..7)` on SHA strings safely (hex-only = ASCII-safe).
- Clippy passes with `-D warnings`.
- 40+ new tests covering all code paths: normal, empty-table, error, bidirectional lookup, sort ordering, JSON format validity.
- `canonicalize().unwrap_or_else(|_| ...)` in path normalization avoids panics on broken symlinks.
- Schema migration from v1 to v2 is incremental and forward-compatible (rejects future versions).

**The two MEDIUM findings are style/efficiency improvements, not correctness issues.** The code is functionally sound and well-tested.
