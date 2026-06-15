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
    CochangeRow, DEFAULT_HALF_LIFE_DAYS, GixSource, HistoryResult, HotspotRow, RiskRow, TemporalDb,
    TemporalSource,
};

// ============================================================================
// Constants
// ============================================================================

/// Commits touching more than this many files are excluded from pair enumeration.
///
/// Matches the constant in `crates/rskim/src/cmd/heatmap/metrics.rs`
/// (`COUPLING_MAX_FILES = 50`) to keep coupling signal consistent.
/// Verified against `MIN_JACCARD_THRESHOLD` in `storage_ops.rs` (0.10).
const COUPLING_MAX_FILES: usize = 50;

/// Minimum Jaccard similarity for a co-change pair to be persisted.
///
/// Must match `MIN_JACCARD_THRESHOLD` in `storage_ops.rs` (0.10) exactly —
/// the read query applies the same threshold, so emitting sub-threshold rows
/// is dead weight the reader discards anyway.
const MIN_JACCARD: f64 = 0.10;

/// Lookback window for the hotspot walk (days).
///
/// Changes_30d/changes_90d fields track only windowed counts, so 90 days is
/// the natural cap for the hotspot decay walk. Risk/lifetime stats are computed
/// over the full history (lookback_days = 0). (Applies ADR-003: the 90-day
/// default is grounded in the schema — it is the widest window the persisted
/// stats represent for hotspot scoring.)
const HOTSPOT_LOOKBACK_DAYS: u32 = 90;

// ============================================================================
// Co-change pair builder (D2 / AC10)
// ============================================================================

/// Compute `Vec<CochangeRow>` from a parsed git history.
///
/// Algorithm:
/// 1. Accumulate per-file commit counts and canonical `(file_a < file_b)` pair
///    counts from `history.commits`, skipping commits touching >
///    [`COUPLING_MAX_FILES`] files (matches heatmap/metrics.rs).
/// 2. Compute Jaccard per pair = `count_ab / (count_a + count_b - count_ab)`
///    (same formula as `CochangeMatrixReader::jaccard` in `cochange/reader.rs`).
/// 3. Filter to `jaccard >= MIN_JACCARD` (0.10) at write time to match
///    `MIN_JACCARD_THRESHOLD` used by the read query (AC4).
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
        for i in 0..n_dedup {
            for j in (i + 1)..n_dedup {
                let a = &paths[i];
                let b = &paths[j];
                // Ordering is guaranteed by the sorted paths slice.
                *pair_counts.entry((a.clone(), b.clone())).or_insert(0) += 1;
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
        // most once per commit. The subtraction is therefore safe for u32.
        // The debug_assert guards against future refactors that break this invariant
        // (e.g. a change to the >COUPLING_MAX_FILES branch that counts non-deduped files).
        debug_assert!(
            count_a >= *count_ab && count_b >= *count_ab,
            "union underflow: count_ab={count_ab} but count_a={count_a}, count_b={count_b}"
        );
        let union = count_a + count_b - count_ab;
        if union == 0 {
            continue;
        }
        let jaccard = f64::from(*count_ab) / f64::from(union);
        if jaccard < MIN_JACCARD {
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
/// - `risk_score` = `FileRiskScores.fix_density` (decay-weighted, used for
///   ranking by `ORDER BY risk_score DESC`).
/// - `fix_density` = raw `fix_commits / total_commits` from [`FileTemporalStats`]
///   (matches the schema docs in storage_types.rs: "ratio of fix commits to
///   total commits" — shown in the `Fix%` column of `--risky`).
/// - `total_commits` and `fix_commits` = lifetime counts from [`FileTemporalStats`]
///   (computed over the full-history walk, not the 90-day window — O-C / ADR-003).
pub(super) fn build_risk_rows(
    risk_scores: &HashMap<String, rskim_search::FileRiskScores>,
    temporal_stats: &HashMap<String, rskim_search::FileTemporalStats>,
) -> Vec<RiskRow> {
    union_paths(risk_scores, temporal_stats)
        .into_iter()
        .map(|path| {
            let decay_fix_density = risk_scores.get(path).map(|r| r.fix_density).unwrap_or(0.0);
            let (total_commits, fix_commits) = temporal_stats
                .get(path)
                .map(|s| (s.total_commits, s.fix_commits))
                .unwrap_or((0, 0));
            // raw_fix_density = fix_commits / total_commits (per storage_types.rs schema).
            // Distinct from decay_fix_density (which is decay-weighted and used as risk_score).
            let raw_fix_density = if total_commits > 0 {
                f64::from(fix_commits) / f64::from(total_commits)
            } else {
                0.0
            };
            RiskRow {
                file_path: path.to_string(),
                // risk_score = decay-weighted fix density for ranking (ORDER BY risk_score DESC).
                risk_score: decay_fix_density,
                total_commits,
                fix_commits,
                // fix_density = raw ratio (shown in Fix% column; matches schema contract).
                fix_density: raw_fix_density,
            }
        })
        .collect()
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
/// # Lookback semantics (O-C / ADR-003)
///
/// - Hotspot walk: `HOTSPOT_LOOKBACK_DAYS` (90) — windowed, matches the 90d
///   schema field and decay model.
/// - Risk/lifetime walk: `lookback_days = 0` (full history) — `total_commits`
///   and `fix_commits` are lifetime counts per the schema docs.
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
    let src = GixSource;

    // ── Hotspot walk (90-day windowed) ────────────────────────────────────────
    let hotspot_history = match src.parse_history(root, HOTSPOT_LOOKBACK_DAYS) {
        Ok(h) => h,
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search temporal [debug]: hotspot parse_history failed: {e} — skipping temporal build"
                );
            }
            return Ok(());
        }
    };

    if hotspot_history.commits.is_empty() {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search temporal [debug]: no commits in 90-day window — skipping temporal build"
            );
        }
        return Ok(());
    }

    // ── Risk / lifetime walk (full history) ──────────────────────────────────
    // total_commits / fix_commits are lifetime counts per storage_types.rs docs.
    // A 90-day cap would compute fix_density over a windowed denominator and
    // change the semantic from lifetime to "recent" — incorrect. (ADR-003)
    let risk_history = match src.parse_history(root, 0) {
        Ok(h) => h,
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search temporal [debug]: risk parse_history failed: {e} — skipping temporal build"
                );
            }
            return Ok(());
        }
    };

    // ── Score computation (pure, no I/O) ─────────────────────────────────────
    // risk_scores: decay-weighted hotspot/fix_density from the full-history walk
    // (O-C / ADR-003). Passing risk_history ensures the decay weights are over
    // the full commit lifetime, not capped at 90 days.
    let risk_scores = rskim_search::compute_file_risk_scores(
        &risk_history.commits,
        now_epoch,
        DEFAULT_HALF_LIFE_DAYS,
    );
    // temporal_stats: windowed counts (changes_30d/90d) PLUS lifetime totals
    // (total_commits/fix_commits). Passing risk_history gives correct lifetime
    // counts for RiskRow; the 30d/90d window fields reflect commits
    // inside the window relative to now_epoch. (O-C / ADR-003)
    let temporal_stats =
        rskim_search::compute_file_temporal_stats(&risk_history.commits, now_epoch);
    // cochange intentionally uses full history (lifetime co-change coupling).
    let cochange_rows = build_cochange_rows(&risk_history);

    // ── Row join ──────────────────────────────────────────────────────────────
    let hotspot_rows = build_hotspot_rows(&risk_scores, &temporal_stats);
    let risk_rows = build_risk_rows(&risk_scores, &temporal_stats);

    // ── Acquire lock (D4), then sync ─────────────────────────────────────────
    // The lock serialises temporal writes against concurrent lexical builds.
    // Acquired AFTER compute (pure) to minimise lock hold time.
    // Delegates to `build_lock::acquire` — the SINGLE bounded implementation
    // shared with `build_index` (index.rs). Both callers use the same file,
    // the same poll interval, and the same deadline (applies ADR-006).
    let _lock = super::build_lock::acquire("skim search", cache_dir)?;

    let db_path = cache_dir.join("temporal.db");
    let db = match TemporalDb::open(&db_path) {
        Ok(d) => d,
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search temporal [debug]: failed to open temporal.db: {e} — skipping"
                );
            }
            return Ok(());
        }
    };

    match db.sync(&hotspot_rows, &risk_rows, &cochange_rows, head) {
        Ok(()) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search temporal [debug]: temporal.db updated ({} hotspot, {} risk, {} cochange rows, HEAD={}…)",
                    hotspot_rows.len(),
                    risk_rows.len(),
                    cochange_rows.len(),
                    head.get(..8).unwrap_or(head),
                );
            }
        }
        Err(rskim_search::SearchError::CapacityExceeded(msg)) => {
            // Too many rows (>500k) — degrade gracefully (D5).
            // Emit an actionable debug message rather than silently failing.
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search temporal [debug]: CapacityExceeded — temporal.db not updated: {msg}. \
                     Consider a shorter lookback window or a smaller repository."
                );
            }
        }
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!("skim search temporal [debug]: sync failed: {e} — temporal.db unchanged");
            }
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
