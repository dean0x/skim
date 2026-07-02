//! Temporal index builder for `skim search` auto-refresh.
//!
//! # Responsibilities
//!
//! - Parse git history (incremental via `lookback_days`).
//! - Compute per-file hotspot/risk scores and co-change pairs.
//! - Join the two maps into the row types that [`TemporalDb::sync`] expects.
//! - Write all three tables atomically via [`TemporalDb::sync`].
//!
//! # Architecture
//!
//! Lives in the CLI crate (not `rskim-search`) because it orchestrates row
//! assembly and the sync call; all library primitives are imported from
//! `rskim_search`.  The function is called from the #289 hook point in
//! `staleness.rs:auto_refresh_if_stale`, after the lexical+AST manifest
//! persists (applies ADR-006 ordering invariant).
//!
//! # Failure isolation (D5)
//!
//! A temporal rebuild failure (non-git directory, gix parse error, capacity
//! exceeded) must NOT fail the lexical/AST query path.  `rebuild_temporal`
//! returns `Ok(())` with a debug-gated warning on recoverable errors; only
//! unexpected internal errors propagate.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rskim_search::{
    COUPLING_MAX_FILES, CochangeRow, DEFAULT_HALF_LIFE_DAYS, GixSource, HistoryResult, HotspotRow,
    MIN_COCHANGE_JACCARD, RiskRow, TemporalDb,
};

// ============================================================================
// Constants
// ============================================================================
// NOTE: COUPLING_MAX_FILES and MIN_COCHANGE_JACCARD are re-exported from
// rskim-search (Decision O-D) — this file does NOT redeclare them. The
// single source of truth lives in:
//   - COUPLING_MAX_FILES  → rskim_search::cochange::builder (pub)
//   - MIN_COCHANGE_JACCARD → rskim_search::temporal::storage (pub)

// ============================================================================
// Co-change pair builder (D2 / AC10)
// ============================================================================

/// Compute `Vec<CochangeRow>` from a parsed git history.
///
/// Algorithm:
/// 1. Accumulate per-file commit counts and canonical `(file_a < file_b)` pair
///    counts from `history.commits`, skipping commits touching >
///    [`COUPLING_MAX_FILES`] files (matches `rskim_search::COUPLING_MAX_FILES`).
/// 2. Compute Jaccard per pair = `count_ab / (count_a + count_b - count_ab)`
///    (same formula as `CochangeMatrixReader::jaccard` in `cochange/reader.rs`).
/// 3. Filter to `jaccard >= MIN_COCHANGE_JACCARD` (0.10) at write time to match
///    `MIN_COCHANGE_JACCARD` used by the read query (AC4 / Decision O-D).
///
/// # Pair ordering invariant
///
/// `file_a < file_b` lexically.  The `UNION ALL` query in
/// `TemporalDb::cochanges_for_file` relies on strict ordering to avoid
/// double-returning the same pair.
///
/// # Pure function
///
/// No I/O, no global state. Fully testable from a hand-built `HistoryResult`.
pub(super) fn build_cochange_rows(history: &HistoryResult) -> Vec<CochangeRow> {
    // per-file commit count (for Jaccard denominator)
    let mut file_counts: HashMap<String, u32> = HashMap::new();
    // canonical pair count: (smaller_path, larger_path) → count
    let mut pair_counts: HashMap<(String, String), u32> = HashMap::new();

    for commit in &history.commits {
        let n = commit.changed_files.len();
        if !(2..=COUPLING_MAX_FILES).contains(&n) {
            // Commits with 0 or 1 file produce no pairs.
            // Commits with >COUPLING_MAX_FILES files are excluded from pair
            // enumeration (large reformats; avoids O(n^2) blowup).
            // Still count each file toward file_counts for the denominator.
            for file in &commit.changed_files {
                *file_counts.entry(file.path_str().into_owned()).or_insert(0) += 1;
            }
            continue;
        }

        // Collect de-duplicated string paths for this commit.
        // We materialise exactly one `String` per unique path per commit
        // (into_owned). The pair-key clones below (a.clone()/b.clone()) are
        // inherent to HashMap ownership — they happen only for pairs in the
        // 2..=COUPLING_MAX_FILES range, not for excluded commits.
        let paths: Vec<String> = {
            let mut v: Vec<String> = commit
                .changed_files
                .iter()
                .map(|f| f.path_str().into_owned())
                .collect();
            // Dedup in-place so a file appearing twice in one commit is counted once.
            v.sort_unstable();
            v.dedup();
            v
        };
        let n_dedup = paths.len();

        // Increment per-file counts.
        for p in &paths {
            *file_counts.entry(p.clone()).or_insert(0) += 1;
        }

        // Enumerate canonical (a < b) pairs.
        // Ordering is guaranteed by the sorted-and-deduped paths slice.
        for i in 0..n_dedup {
            for j in (i + 1)..n_dedup {
                *pair_counts
                    .entry((paths[i].clone(), paths[j].clone()))
                    .or_insert(0) += 1;
            }
        }
    }

    // Build CochangeRow for each pair that meets the Jaccard threshold.
    let mut rows = Vec::new();
    for ((a, b), count_ab) in &pair_counts {
        let count_a = *file_counts.get(a).unwrap_or(&0);
        let count_b = *file_counts.get(b).unwrap_or(&0);
        // Invariant: count_ab <= min(count_a, count_b) because per-commit paths are
        // deduped (sort_unstable+dedup above) and git tree-diff yields each path at
        // most once per commit.  Use saturating arithmetic on u64 so a future refactor
        // that breaks this invariant produces a 0 union (skipped row) rather than a
        // u32 wrap in release builds — fail-safe rather than silent corruption.
        debug_assert!(
            count_a >= *count_ab && count_b >= *count_ab,
            "union underflow: count_ab={count_ab} but count_a={count_a}, count_b={count_b}"
        );
        let union = (count_a as u64)
            .saturating_add(count_b as u64)
            .saturating_sub(*count_ab as u64);
        if union == 0 {
            continue;
        }
        let jaccard = f64::from(*count_ab) / union as f64;
        if jaccard < MIN_COCHANGE_JACCARD {
            continue;
        }
        rows.push(CochangeRow {
            file_a: a.clone(),
            file_b: b.clone(),
            count: *count_ab,
            jaccard,
        });
    }
    rows
}

// ============================================================================
// Row join helpers (D1 step 5 / AC11)
// ============================================================================

/// Collect the union of path keys from two maps into a `HashSet<&str>`.
///
/// Used by both row-join functions so the same pattern is not repeated twice.
fn union_paths<'a, V1, V2>(
    a: &'a HashMap<String, V1>,
    b: &'a HashMap<String, V2>,
) -> std::collections::HashSet<&'a str> {
    a.keys()
        .map(String::as_str)
        .chain(b.keys().map(String::as_str))
        .collect()
}

/// Join `compute_file_risk_scores` and `compute_file_temporal_stats` outputs
/// into `Vec<HotspotRow>`.
///
/// Both maps are keyed by repo-relative path string.  For the join:
/// - A path present in BOTH maps → one row with fields from each source.
/// - A path present in ONLY the risk map → `changes_30d/90d` zeroed.
/// - A path present in ONLY the stats map → `score` zeroed (not in hotspot map).
///
/// The "only stats" case is unlikely in practice (stats are computed over the
/// same commits as risk scores) but is handled without panic per AC11.
pub(super) fn build_hotspot_rows(
    risk_scores: &HashMap<String, rskim_search::FileRiskScores>,
    temporal_stats: &HashMap<String, rskim_search::FileTemporalStats>,
) -> Vec<HotspotRow> {
    union_paths(risk_scores, temporal_stats)
        .into_iter()
        .map(|path| {
            let score = risk_scores.get(path).map(|r| r.hotspot).unwrap_or(0.0);
            let (changes_30d, changes_90d) = temporal_stats
                .get(path)
                .map(|s| (s.changes_30d, s.changes_90d))
                .unwrap_or((0, 0));
            HotspotRow {
                file_path: path.to_string(),
                score,
                changes_30d,
                changes_90d,
            }
        })
        .collect()
}

/// Join `compute_file_risk_scores` and `compute_file_temporal_stats` outputs
/// into `Vec<RiskRow>`.
///
/// Same union-of-keys strategy as [`build_hotspot_rows`] (AC11 contract).
///
/// - `risk_score` = volume-weighted bug-fix risk (#378):
///   [`rskim_search::risk_score_wilson_decay`]`(decay_fix_factor, fix_commits,
///   total_commits)` = `decay_fix_factor * WilsonLB(fix_commits, total_commits)`.
///   `decay_fix_factor` is `FileRiskScores.fix_density` — the **decay-weighted
///   fix proportion** (`Σ decay·is_fix / Σ decay`), in which the decay weight
///   largely cancels (it is `1.0` for an all-fix file, recency only shifts it
///   when fix and non-fix commits differ in age), NOT a pure recency term. The
///   Wilson lower bound is read from the **raw** lifetime counts and is the
///   factor that fixes the saturation bug: it suppresses tiny samples (a
///   1-fix/1-commit file, whose `decay_fix_factor` is also `1.0`) below a
///   50-fix/50-commit file, which the old bare decay-weighted ratio did not.
///   Used for ranking by `ORDER BY risk_score DESC`.
/// - `fix_density` = raw `fix_commits / total_commits` from [`FileTemporalStats`]
///   (matches the schema docs in storage_types.rs: "ratio of fix commits to
///   total commits" — shown in the `Fix%` column of `--risky`). Intentionally
///   distinct from `risk_score` (AD-378-3 two-field separation).
/// - `total_commits` and `fix_commits` = lifetime counts from [`FileTemporalStats`]
///   (computed over the full-history walk, not the 90-day window — O-C / ADR-003).
pub(super) fn build_risk_rows(
    risk_scores: &HashMap<String, rskim_search::FileRiskScores>,
    temporal_stats: &HashMap<String, rskim_search::FileTemporalStats>,
) -> Vec<RiskRow> {
    union_paths(risk_scores, temporal_stats)
        .into_iter()
        .map(|path| {
            // decay_fix_factor = decay-weighted fix proportion (Σ decay·is_fix / Σ decay).
            // The decay weight largely cancels (==1.0 for an all-fix file); this is the
            // #378 decay term, NOT a pure recency weight — see risk_score_wilson_decay docs.
            let decay_fix_factor = risk_scores.get(path).map(|r| r.fix_density).unwrap_or(0.0);
            let (total_commits, fix_commits) = temporal_stats
                .get(path)
                .map(|s| (s.total_commits, s.fix_commits))
                .unwrap_or((0, 0));
            // raw_fix_density = fix_commits / total_commits (per storage_types.rs schema).
            // Distinct from both risk_score (volume-weighted) and decay_fix_factor
            // (decay-weighted) — AD-378-3 two-field separation.
            let raw_fix_density = if total_commits > 0 {
                f64::from(fix_commits) / f64::from(total_commits)
            } else {
                0.0
            };
            RiskRow {
                file_path: path.to_string(),
                // risk_score = decay-weighted-fix-proportion × Wilson-LB volume weighting
                // (#378, AD-378-1). Wilson reads the RAW (fix_commits, total_commits) so
                // tiny samples no longer saturate at 1.0 (the #378 ranking bug).
                risk_score: rskim_search::risk_score_wilson_decay(
                    decay_fix_factor,
                    fix_commits,
                    total_commits,
                ),
                total_commits,
                fix_commits,
                // fix_density = raw ratio (shown in Fix% column; matches schema contract).
                fix_density: raw_fix_density,
            }
        })
        .collect()
}

// ============================================================================
// Internal helper (D5 graceful-degradation pattern)
// ============================================================================

/// Emit a debug-gated warning and return `Ok(())` to degrade gracefully.
///
/// All recoverable early-return arms in `rebuild_temporal` use this helper so
/// the D5 isolation policy ("temporal failure MUST NOT fail lexical") is
/// expressed once and the function body reads as sequential happy-path logic.
macro_rules! warn_skip {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {{
        if crate::debug::is_debug_enabled() {
            eprintln!(
                concat!("skim search [debug]: ", $fmt, " — skipping temporal build"),
                $($arg)*
            );
        }
        return Ok(());
    }};
}

// ============================================================================
// Main entry point (D1 / D3 / D4 / D5)
// ============================================================================

/// Rebuild the temporal database after a successful lexical+AST index build.
///
/// # Call site contract (applies ADR-006)
///
/// This function MUST be called AFTER the lexical+AST manifest is persisted.
/// The hook point in `staleness.rs:auto_refresh_if_stale` (the "#289 temporal
/// build hook point" comment, after `FileManifest::load`) is correctly
/// post-manifest — do not move it earlier.
///
/// # Empty-history repos (LOCKED DECISION 2026-06-24)
///
/// When a git repo has zero commits (`parse_history` returns an empty commit
/// list), this function acquires the build lock and writes a **present-but-empty**
/// `temporal.db` containing only the `META_GIT_HEAD` row.  This prevents the
/// per-query rebuild loop that would otherwise occur because `temporal_db_is_stale`
/// returns `true` whenever `temporal.db` is absent — so without the file the
/// next query would attempt another rebuild, fail the same way, and loop forever.
/// The empty-DB invariant: `top_hotspots()` returns `[]`, but `get_meta(GIT_HEAD)`
/// returns the HEAD SHA so the staleness gate sees `Current` on the next query.
/// `TemporalDb::open` creates the file on disk before `sync` is called; if `sync`
/// fails, the file may exist with no `META_GIT_HEAD` row (same partial-file risk
/// as the non-empty path) — see inline comment on the `Err` arm.
///
/// **Production reachability note**: a genuine zero-commit git repo has an
/// *unborn branch* — `read_git_head` returns `None` because `resolve_symbolic_ref`
/// finds no loose ref and no packed-refs entry.  With `current_head = None`, both
/// the BUG-B self-heal gate (`if let Some(ref head) = current_head && …` in
/// `staleness.rs`) and `try_rebuild_temporal_nonfatal` (early `let Some(head) =
/// head else { return }`) short-circuit before this function is ever invoked with
/// a `Some(head)`.  The no-rebuild-loop guarantee for zero-commit repos therefore
/// derives from the `read_git_head = None` short-circuit, **not** from the
/// empty-DB write.  The empty-DB code path is exercised only by the direct-call
/// test (`rebuild_temporal_with_source` with a synthetic `fake_head`).  Both
/// rationales are valid and complementary — the empty-DB write remains correct
/// for any future call path that does supply a synthetic HEAD.
///
/// # Lookback semantics (O-C / ADR-003)
///
/// A single full-history walk (`lookback_days = 0`) supplies all data:
/// - `compute_file_risk_scores` applies exponential decay internally.
/// - `compute_file_temporal_stats` computes windowed counts (30d/90d) via
///   timestamp arithmetic against `now_epoch`, so no separate 90-day walk
///   is needed (Decision O-B: the former 90-day hotspot walk was dead I/O).
/// - `total_commits` and `fix_commits` are lifetime counts per schema docs.
///
/// # Failure isolation (D5)
///
/// Returns `Ok(())` on recoverable errors (non-git directory, gix parse error,
/// `CapacityExceeded`) with a debug-gated warning.  Only unexpected internal
/// errors propagate as `Err`.
///
/// # HEAD threading (O-A)
///
/// `head` must be the full 40/64-hex SHA read at function entry in
/// `auto_refresh_if_stale` — not a truncated display form — so that
/// `check_temporal_staleness`'s `git rev-parse HEAD` comparison succeeds (AC6).
///
/// # Parameters
///
/// - `root`: project root (used by `GixSource::parse_history`).
/// - `cache_dir`: directory containing `temporal.db`.
/// - `head`: full git HEAD SHA to record in the `meta` table.
/// - `now_epoch`: injectable clock for deterministic tests (pass
///   `SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()` in production).
pub(super) fn rebuild_temporal(
    root: &Path,
    cache_dir: &Path,
    head: &str,
    now_epoch: u64,
) -> anyhow::Result<()> {
    rebuild_temporal_with_source(&GixSource, root, cache_dir, head, now_epoch)
}

/// Inner implementation of `rebuild_temporal` with an injectable `TemporalSource`.
///
/// Separated from `rebuild_temporal` so tests can supply a counting or fake
/// source (ADR-003 PERFORMANCE criterion: assert parse_history call-count == 1).
/// Production always uses `GixSource` via `rebuild_temporal`.
pub(super) fn rebuild_temporal_with_source(
    src: &dyn rskim_search::TemporalSource,
    root: &Path,
    cache_dir: &Path,
    head: &str,
    now_epoch: u64,
) -> anyhow::Result<()> {
    // ── Single full-history walk ──────────────────────────────────────────────
    // One parse_history call supplies all data. The 30d/90d windowing for
    // changes_30d/changes_90d is done inside compute_file_temporal_stats via
    // timestamp comparison against now_epoch — no separate windowed walk needed
    // (Decision O-B: the former 90-day hotspot walk was dead I/O; it was only
    // used for an is_empty() guard that risk_history already provides).
    let risk_history = match src.parse_history(root, 0) {
        Ok(h) => h,
        Err(e) => warn_skip!("parse_history failed: {}", e),
    };

    // ── Score computation (pure, no I/O) ─────────────────────────────────────
    // For empty-history repos (zero commits): all row slices are empty but we
    // still fall through to the single lock+open+sync block below, which writes
    // a present-but-empty temporal.db with META_GIT_HEAD set.
    //
    // LOCKED DECISION (2026-06-24, plan lines 14/146/349): a present-but-empty
    // temporal.db prevents the per-query no-op rebuild loop — on the next query,
    // temporal_db_is_stale reads META_GIT_HEAD and sees Current, so no rebuild.
    //
    // Falling through (rather than an early-return empty branch) also avoids
    // duplicating the lock+open+sync block and eliminates the partial-file risk
    // that the prior early-return had: if sync fails after TemporalDb::open
    // creates the file, the file exists with no META_GIT_HEAD row, making
    // temporal_db_is_stale return true on the next query → rebuild loop.
    // The single sync path addresses this: if sync fails we warn+skip and the
    // file may still exist headless, but this is the same risk that already
    // exists on the non-empty path (pre-existing, not introduced here).
    let (hotspot_rows, risk_rows, cochange_rows);
    if risk_history.commits.is_empty() {
        hotspot_rows = vec![];
        risk_rows = vec![];
        cochange_rows = vec![];
    } else {
        // risk_scores: decay-weighted hotspot/fix_density from the full-history walk
        // (O-C / ADR-003). Full history ensures decay weights span the commit
        // lifetime rather than being capped at 90 days.
        let risk_scores = rskim_search::compute_file_risk_scores(
            &risk_history.commits,
            now_epoch,
            DEFAULT_HALF_LIFE_DAYS,
        );
        // temporal_stats: windowed counts (changes_30d/90d) PLUS lifetime totals
        // (total_commits/fix_commits). The 30d/90d fields reflect commits inside
        // the window relative to now_epoch (timestamp arithmetic, no walk cap).
        let temporal_stats =
            rskim_search::compute_file_temporal_stats(&risk_history.commits, now_epoch);
        // cochange uses full history (lifetime co-change coupling).
        cochange_rows = build_cochange_rows(&risk_history);
        hotspot_rows = build_hotspot_rows(&risk_scores, &temporal_stats);
        risk_rows = build_risk_rows(&risk_scores, &temporal_stats);
    }

    // ── Acquire lock (D4), then sync ─────────────────────────────────────────
    // Single sync path for both the empty-history and non-empty cases:
    // eliminates the duplicated lock+open+sync block and consolidates the
    // partial-file-on-sync-failure risk in one location.
    // The lock serialises temporal writes against concurrent lexical builds.
    // Acquired AFTER compute (pure) to minimise lock hold time.
    // Delegates to `build_lock::acquire` — the SINGLE bounded implementation
    // shared with `build_index` (index.rs). Both callers use the same file,
    // the same poll interval, and the same deadline (applies ADR-006).
    let _lock = super::build_lock::acquire("skim search", cache_dir)?;

    let db_path = cache_dir.join("temporal.db");
    let db = match TemporalDb::open(&db_path) {
        Ok(d) => d,
        Err(e) => warn_skip!("failed to open temporal.db: {}", e),
    };

    match db.sync(&hotspot_rows, &risk_rows, &cochange_rows, head) {
        Ok(()) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: temporal.db updated ({} hotspot, {} risk, {} cochange rows, HEAD={}…)",
                    hotspot_rows.len(),
                    risk_rows.len(),
                    cochange_rows.len(),
                    head.get(..8).unwrap_or(head),
                );
            }
        }
        Err(rskim_search::SearchError::CapacityExceeded(msg)) => {
            // Too many rows (>500k) — degrade gracefully (D5).
            warn_skip!("CapacityExceeded — {}. Consider a smaller repository", msg);
        }
        Err(e) => {
            warn_skip!("sync failed: {}", e);
        }
    }

    Ok(())
}

/// Return the current Unix epoch timestamp in seconds.
///
/// Used by `rebuild_temporal`'s call site in `staleness.rs` to pin `now_epoch`
/// at the start of the refresh — all score computations use the same reference
/// point rather than reading `SystemTime::now()` inside library functions.
///
/// Returns `0` if the system clock is before the Unix epoch (impossible in
/// production, but safe).
#[must_use]
pub(super) fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

// ============================================================================
// Tests (co-located)
// ============================================================================

#[cfg(test)]
#[path = "temporal_build_tests.rs"]
mod tests;
