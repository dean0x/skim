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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rskim_search::{
    COUPLING_MAX_FILES, CochangeRow, DEFAULT_HALF_LIFE_DAYS, HistoryResult, HotspotRow,
    MIN_COCHANGE_JACCARD, RiskRow, SearchError, TemporalDb,
};

use super::degraded::{DegradedReason, Fallback, TemporalUnavailable, degraded_notice};

// ============================================================================
// Constants
// ============================================================================
// NOTE: COUPLING_MAX_FILES and MIN_COCHANGE_JACCARD are re-exported from
// rskim-search (Decision O-D) — this file does NOT redeclare them. The
// single source of truth lives in:
//   - COUPLING_MAX_FILES  → rskim_search::cochange::builder (pub)
//   - MIN_COCHANGE_JACCARD → rskim_search::temporal::storage (pub)

// ============================================================================
// BuildLoudness
// ============================================================================

/// Whether the temporal build was requested explicitly by the user or triggered
/// as a background auto-refresh from a lexical/AST query.
///
/// SE-1: Only explicit build/rebuild/update invocations emit an open-failure
/// notice on stderr.  Query-path auto-refreshes demote the same failure to a
/// debug-gated message so that a plain `skim search foo` does not permanently
/// grow a stderr line.
///
/// This is a separate axis from [`super::staleness::ReanchorPolicy`], which
/// governs whether `temporal.db` may be re-anchored to a different repository
/// toplevel.  The two happen to correlate for current callers, but they are
/// independent concerns: a future caller could need Allow without loudness, or
/// Refuse with it.  Keeping them separate in the function signature makes that
/// extension straightforward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BuildLoudness {
    /// Explicit `--build`, `--rebuild`, or `--update` invocation — emit notices.
    Loud,
    /// Background auto-refresh triggered by a lexical or AST query — stay quiet.
    Silent,
}

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
/// **Production reachability note**: the remaining causes of `read_git_head = None`
/// in production are unborn branch, unsupported ref backend (reftable — #481), corrupt
/// HEAD, and fs error — the linked-worktree route that previously caused `None` is fixed
/// by #413.  With `current_head = None` for any of those reasons, both the BUG-B
/// self-heal gate (`if let Some(ref head) = current_head && …` in `staleness.rs`) and
/// the `HeadState::Resolved` arm of `try_rebuild_temporal_nonfatal` short-circuit
/// before this function is ever invoked.
/// The no-rebuild-loop guarantee for zero-commit repos therefore derives from the
/// `read_git_head = None` short-circuit, **not** from the empty-DB write.
/// AD-414-22: the explicit build arms no longer stop there — they route an unborn
/// HEAD to [`build_empty_temporal_for_unborn_head`], which writes a zero-row
/// `temporal.db` with **no** `META_GIT_HEAD` and emits the AC-16 notice.  The quiet
/// query path is unchanged, so the short-circuit above still supplies the loop bound.
/// The empty-DB
/// code path is also exercised from `auto_refresh_if_stale` when the subtree under a
/// subdirectory root (OD-3/AD-413-14) has zero qualifying commits; the direct-call test
/// (`rebuild_temporal_with_source` with a synthetic `fake_head`) exercises the same path
/// at unit level.  The subdirectory-root route is now live in production: AD-408-5's ghost
/// anchor (the `ghost_root` binding) is reachable for the first time via `--root <subdir>` (#413).
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
#[cfg(test)]
pub(super) fn rebuild_temporal(
    root: &Path,
    cache_dir: &Path,
    head: &str,
    now_epoch: u64,
) -> anyhow::Result<()> {
    use rskim_search::GixSource;
    rebuild_temporal_with_source(
        &GixSource,
        root,
        cache_dir,
        head,
        now_epoch,
        super::staleness::ReanchorPolicy::Allow,
        BuildLoudness::Loud,
    )
}

/// Return `true` when `rel` names a regular file that exists under `root`.
///
/// Performs two checks in order:
/// 1. **Containment guard** via [`crate::cmd::is_repo_relative_safe`] — rejects
///    any `rel` with absolute, `..` (ParentDir), or drive-relative (Prefix)
///    components to mitigate path-traversal risk (applies ADR-008; single
///    canonical helper shared with `walk::list_tracked_files` and `heatmap::resolve_diff_files`).
///    Git never emits such components in tree-diff output, so no legitimate row
///    is dropped by this guard.
/// 2. **`is_file()` existence check** — the correct predicate for "a path an
///    agent can Read" (AD-408-2): a former-file path that is now a directory
///    passes `.exists()` but fails `.is_file()` and must be excluded from the
///    temporal surface (OD2, 2026-07-17).
///
/// # Symlink note (AD-408-2)
///
/// `is_file()` follows symlinks. A committed in-tree symlink that is relative
/// and `..`-free (e.g. `link.rs -> /etc/passwd`) passes the containment guard
/// and `is_file()` traverses the target, stat'ing outside `root`. The security
/// impact is nil — only the boolean result is used to retain or drop a ranking
/// row; no file content is read and no resolved path is emitted. An unreadable
/// but present file (EACCES) is also silently treated as a ghost and dropped.
/// Both are consciously accepted; use `symlink_metadata()` if strict containment
/// is ever required.
fn rel_is_regular_file(root: &Path, rel: &str) -> bool {
    let rel_path = std::path::Path::new(rel);
    if !crate::cmd::is_repo_relative_safe(rel_path) {
        return false;
    }
    root.join(rel_path).is_file()
}

/// Discover the git worktree working directory by walking upward from `root`.
///
/// Returns the path of the worktree root (the directory containing `.git`) when
/// `root` is inside a git repository, or `None` when gix cannot find a repo or
/// the repo is bare. This is a pure path-discovery call — no object lookup or
/// history walk occurs.
///
/// # Why this is needed
///
/// History paths in [`rskim_search::HistoryResult`] are REPO-ROOT-relative
/// because [`GixSource::parse_history`] calls `gix::discover(root)` which walks
/// **upward** to find `.git`. When `root` is a subdirectory of the worktree
/// (e.g. `--root crates/rskim-search`), naive `root.join(rel)` double-nests the
/// prefix — `<root>/crates/rskim-search/src/lib.rs` instead of the correct
/// `<workdir>/crates/rskim-search/src/lib.rs` — causing every row to fail the
/// `is_file()` check and be silently dropped. Using the discovered workdir as
/// the anchor mirrors the approach in `heatmap/mod.rs` which joins against
/// `git_source.get_repo_root()` (AD-408-5).
///
/// Failure is silently absorbed per D5 (temporal failure must not fail the
/// lexical query path); callers fall back to `root` when `None` is returned.
fn discover_git_workdir(root: &Path) -> Option<std::path::PathBuf> {
    gix::discover(root)
        .ok()
        .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()))
}

/// Probe whether `root` sits in a shallow git clone by inspecting the
/// `<commondir>/shallow` file directly.
///
/// Returns a tri-state (`Option<bool>`) so callers can distinguish three cases:
/// - `Some(true)`  — the shallow file is present and non-empty → shallow clone.
/// - `Some(false)` — the shallow file is absent or empty (an explicit `NotFound`
///   or zero-length file) → definitively not shallow.
/// - `None`        — the git dir cannot be resolved or an unexpected I/O error
///   (`EACCES`, `EIO`, `ESTALE`, …) occurred → unknown; callers must NOT
///   persist a concrete "0"/"1" value in this case.
///
/// **Directory resolution differs from Check 3** (`temporal_state.rs:243-262`).
/// Check 3 receives the result of `resolve_git_dir(root)` directly (which
/// returns `None` for an adopted subdirectory root, so Check 3 skips the whole
/// block for such roots). This probe adds an `resolve_repo_toplevel` ancestor
/// fallback so it also works for subdirectory roots — a distinction that matters
/// for the parse-failure case where we want to record whatever we can.
///
/// This is the shared helper that replaces the former hardcoded `is_shallow:
/// false` in the `parse_history`-failure fall-through path (F3 / AD-414-17).
/// P2-1 (2026-09-03 review): changed from `bool` to `Option<bool>` so
/// "unknown" is not silently written as an authoritative "0".
fn probe_is_shallow_from_root(root: &Path) -> Option<bool> {
    let git_dir = super::staleness::resolve_git_dir(root).or_else(|| {
        super::staleness::resolve_repo_toplevel(root)
            .and_then(|top| super::staleness::resolve_git_dir(&top))
    })?; // None → no git dir resolved → unknown
    let shallow_dir = super::staleness::resolve_common_dir(&git_dir).unwrap_or(git_dir);
    match shallow_dir.join("shallow").metadata() {
        Ok(m) => Some(m.is_file() && m.len() > 0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None, // EACCES / EIO / ESTALE / other → unknown
    }
}

/// Write the two-line build-backoff sentinel file (A / AD-414-21).
///
/// Format: `<head>\n<shallow>` where `<shallow>` is `1`, `0`, or `?`.
/// The `?` value is stored when the shallow state cannot be determined; see
/// [`backoff_sentinel_matches`] for how it is treated on reads.
///
/// Old single-line sentinels (written before this format was introduced) contain
/// no `\n` and therefore never match `backoff_sentinel_matches`, causing them to
/// self-clear on the next explicit build or quiet rebuild attempt.
fn write_backoff_sentinel(cache_dir: &Path, head: &str, shallow: Option<bool>) {
    let shallow_char = match shallow {
        Some(true) => '1',
        Some(false) => '0',
        None => '?',
    };
    let contents = format!("{head}\n{shallow_char}");
    let _ = std::fs::write(
        cache_dir.join("temporal.db.build_backoff"),
        contents.as_bytes(),
    );
}

/// Check whether the build-backoff sentinel gates the current HEAD (A / AD-414-21).
///
/// Returns `true` (honour the sentinel, skip the rebuild) when ALL of:
/// 1. The sentinel file can be read.
/// 2. Its first line equals `head`.
/// 3. Either shallow flag (stored or probed) is unknown (`?` / `None`), OR
///    the stored flag equals the probed flag.
///
/// Returns `false` (allow the rebuild, open the gate) when:
/// - The file is absent or unreadable.
/// - The sentinel is in the old single-line format (no `\n` separator) —
///   these were written before the shallow flag was added and self-clear on
///   the next invocation.
/// - The stored HEAD does not match `head`.
/// - The stored shallow flag is concrete (`0`/`1`) AND the probed flag is
///   concrete but differs — e.g. `git fetch --unshallow` changed `1` → `0`.
///
/// "Do not reopen" for `?`/`None` means we conservatively keep the gate
/// closed when we cannot determine whether the shallow state changed; false
/// positives (staying closed when the state changed) are preferable to false
/// negatives (reopening when nothing changed).
fn backoff_sentinel_matches(cache_dir: &Path, head: &str, shallow: Option<bool>) -> bool {
    let Ok(bytes) = std::fs::read(cache_dir.join("temporal.db.build_backoff")) else {
        return false; // missing or unreadable
    };
    let Ok(stored) = std::str::from_utf8(&bytes) else {
        return false; // garbled
    };
    // Old single-line format (no '\n') — self-clear.
    let Some(nl_pos) = stored.find('\n') else {
        return false;
    };
    let stored_head = &stored[..nl_pos];
    let stored_shallow = stored[nl_pos + 1..].trim_end();
    if stored_head != head {
        return false;
    }
    // Both sides known and concrete — open the gate when they differ.
    let current_shallow_char = match shallow {
        Some(true) => "1",
        Some(false) => "0",
        None => return true, // current unknown → do not reopen
    };
    if stored_shallow == "?" {
        return true; // stored unknown → do not reopen
    }
    stored_shallow == current_shallow_char
}

/// Remove temporal rows whose backing files no longer exist on disk.
///
/// This is the build-time ghost filter (AD-408-1). It runs on freshly-computed
/// rows *before* [`TemporalDb::sync`] persists them, so the prior DB survives
/// intact when sync fails and the self-heal invariant holds (applies ADR-006).
///
/// Hotspot and risk rows are retained only when the file exists on disk as a
/// regular file; cochange rows survive only when **both** `file_a` and `file_b`
/// exist. Scores are NOT renormalized after the drop — each file's score derives
/// solely from its own git history, and cochange rows carry a baked-in Jaccard.
///
/// Existence is resolved once per unique path across all three row types to
/// bound stat syscall count at 1× per path (worst case) rather than up to 3×.
/// For the empty-history case the slices are already empty, so `retain` is a
/// no-op.
///
/// Per PF-012: uses sequential `HashSet + retain` (bounded full-completion),
/// NOT an early-terminated parallel walk. Parallelism would introduce PF-012
/// racy-truncation risk for no measurable gain on this non-query path.
fn apply_ghost_filter(
    root: &Path,
    hotspot_rows: &mut Vec<rskim_search::HotspotRow>,
    risk_rows: &mut Vec<rskim_search::RiskRow>,
    cochange_rows: &mut Vec<rskim_search::CochangeRow>,
) {
    // Collect each unique path referenced by any row type (1× stat per path).
    let unique_paths: HashSet<&str> = hotspot_rows
        .iter()
        .map(|r| r.file_path.as_str())
        .chain(risk_rows.iter().map(|r| r.file_path.as_str()))
        .chain(
            cochange_rows
                .iter()
                .flat_map(|r| [r.file_a.as_str(), r.file_b.as_str()]),
        )
        .collect();
    // Resolve existence once per unique path.
    let existing: HashSet<String> = unique_paths
        .into_iter()
        .filter(|p| rel_is_regular_file(root, p))
        .map(String::from)
        .collect();
    hotspot_rows.retain(|r| existing.contains(&r.file_path));
    risk_rows.retain(|r| existing.contains(&r.file_path));
    // Cochange: both sides must exist on disk (AD-408-1 both-sides rule).
    cochange_rows.retain(|r| existing.contains(&r.file_a) && existing.contains(&r.file_b));
}

/// Strip a scope-prefix from temporal rows in-place, retaining only rows within
/// the subtree and rewriting their paths to be `root`-relative.
///
/// This is the AD-413-17 scope filter, extracted from `rebuild_temporal_with_source`
/// to match the structure of [`apply_ghost_filter`].  Called only when `root` is a
/// proper subdirectory of the git worktree root; when `root == ghost_root` the
/// caller holds `scope = None` and this function is never invoked.
///
/// # Allocation (Finding 2)
///
/// Prefix stripping uses [`String::drain`] (zero extra allocation per retained
/// row) rather than `*path = stripped.to_string()`, which cloned the tail on
/// every retained row.  `starts_with` is checked first so `drain` is never
/// called on a path that would be dropped anyway.
///
/// # Cochange both-sides rule
///
/// A cochange row where only one side falls within the scope would silently
/// reference an unreachable path on the query side — it is dropped rather than
/// emitted as a ghost co-change result.
fn apply_scope_filter(
    pfx: &str,
    hotspot_rows: &mut Vec<rskim_search::HotspotRow>,
    risk_rows: &mut Vec<rskim_search::RiskRow>,
    cochange_rows: &mut Vec<rskim_search::CochangeRow>,
) {
    // In-place prefix drain: O(n_chars_shifted) per row, zero allocation.
    let strip_in_place = |path: &mut String| -> bool {
        if path.starts_with(pfx) {
            path.drain(..pfx.len());
            true
        } else {
            false
        }
    };
    hotspot_rows.retain_mut(|r| strip_in_place(&mut r.file_path));
    risk_rows.retain_mut(|r| strip_in_place(&mut r.file_path));
    cochange_rows.retain_mut(|r| {
        if r.file_a.starts_with(pfx) && r.file_b.starts_with(pfx) {
            r.file_a.drain(..pfx.len());
            r.file_b.drain(..pfx.len());
            true
        } else {
            false
        }
    });
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
    reanchor: super::staleness::ReanchorPolicy,
    loudness: BuildLoudness,
) -> anyhow::Result<()> {
    // ── Build-backoff sentinel (Finding 2 / D5) ──────────────────────────────
    // Written when TemporalDb::open fails, a fallback empty sync fails, or
    // parse_history fails (see F3 fix below). If the sentinel records the same
    // HEAD AND the same shallow state as the current invocation, a prior
    // non-transient failure already occurred for this HEAD; skip the expensive
    // parse_history call.  Probe is_shallow now so the gate can re-open after
    // `git fetch --unshallow` without HEAD advancing.
    //
    // AD-414-16: explicit --build/--rebuild/--update always clears the sentinel
    // at the START OF THE TEMPORAL REBUILD (here, before the backoff gate) so
    // the user-documented recovery path ("run 'skim search --rebuild'") works
    // even when a prior failure wrote the sentinel.  On --update the clear is
    // reached only when auto_refresh_if_stale decides a rebuild is warranted.
    // R1 (never overwrite a newer-schema DB) is still enforced downstream by
    // open_or_discard_temporal_db, which refuses without modifying the file.
    //
    // AD-414-21 (2026-09-03): sentinel format extended to `<head>\n<shallow>`
    // where shallow is "1", "0", or "?" (unknown). Old single-line sentinels
    // (no '\n') are treated as non-matching and self-clear on the next build.
    // See write_backoff_sentinel / backoff_sentinel_matches for the protocol.
    let probed_shallow = probe_is_shallow_from_root(root);
    let backoff_sentinel = cache_dir.join("temporal.db.build_backoff");
    if loudness == BuildLoudness::Loud {
        // Explicit rebuild: clear any stale sentinel so the recovery flows
        // printed on stderr ("re-run 'skim search --rebuild'") work correctly.
        let _ = std::fs::remove_file(&backoff_sentinel);
    } else if backoff_sentinel_matches(cache_dir, head, probed_shallow) {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: temporal rebuild skipped — \
                 build-backoff sentinel present for HEAD {}… is_shallow={probed_shallow:?} \
                 (prior open/sync failure); will retry when HEAD advances or shallow state changes",
                head.get(..8).unwrap_or(head),
            );
        }
        return Ok(());
    }

    // ── Single full-history walk ──────────────────────────────────────────────
    // One parse_history call supplies all data. The 30d/90d windowing for
    // changes_30d/changes_90d is done inside compute_file_temporal_stats via
    // timestamp comparison against now_epoch — no separate windowed walk needed
    // (Decision O-B: the former 90-day hotspot walk was dead I/O; it was only
    // used for an is_empty() guard that risk_history already provides).
    //
    // F3 fix (AD-414-17): on parse_history failure write the build-backoff
    // sentinel and return early.  This replaces the former LOCKED DECISION
    // 2026-06-24 "fall through with empty HistoryResult to write META_GIT_HEAD".
    // The sentinel now bounds quiet-path retries (once per HEAD AND per shallow
    // state) without asserting that the history was successfully read.
    // Consequences (corrected from the original comment, P1-1 / 2026-09-03):
    //
    //   1. META_GIT_HEAD is NOT written, so the DB truthfully stays stale.
    //      On the next quiet query, the sentinel matches the same HEAD+shallow
    //      → no retry.  When HEAD advances the sentinel no longer matches.
    //      When `git fetch --unshallow` removes the shallow file WITHOUT moving
    //      HEAD, the probed shallow state changes (Some(true) → Some(false)),
    //      so the stored "1" no longer matches the probed "0" → the gate opens
    //      and parse_history is retried on the next silent query (AD-414-21).
    //
    //   2. is_shallow is probed from the filesystem (tri-state Option<bool>)
    //      rather than fabricated as false.  Only a CONCLUSIVE probe (Some) is
    //      written to META_IS_SHALLOW in an existing DB so a correct "1" is
    //      never overwritten by an unknown "0" (P2-1 / 2026-09-03).
    //      Check 3 (shallow→full transition, AD-414-14) relies on META_IS_SHALLOW
    //      being truthful; it is correct when consulted, but while a sentinel is
    //      set for the current HEAD+shallow pair, Check 3's rebuild is gated
    //      further by backoff_sentinel_matches — the sentinel incorporating the
    //      shallow flag (AD-414-21) is what makes the Check-3 self-heal work.
    //
    //   3. Explicit --rebuild (F1 / AD-414-16) clears the sentinel at the start
    //      of the temporal rebuild (before this point), so the recovery
    //      instruction in our own stderr output ("re-run 'skim search
    //      --rebuild'") always works.
    let risk_history = match src.parse_history(root, 0) {
        Ok(h) => h,
        Err(e) => {
            // probed_shallow was computed above (before the gate) using the
            // tri-state probe.  Use it for both the sentinel and the meta write.
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: parse_history failed: {e} — \
                     writing sentinel for HEAD {}…; is_shallow={probed_shallow:?}; \
                     will retry when HEAD advances, shallow state changes, or on explicit --rebuild",
                    head.get(..8).unwrap_or(head),
                );
            }
            // Bound quiet-path retries to once per HEAD+shallow pair.
            write_backoff_sentinel(cache_dir, head, probed_shallow);
            // If a DB exists from a prior successful build, update its
            // META_IS_SHALLOW (only when the probe is conclusive) so Check 3
            // can still detect a shallow→full transition.  Open WITHOUT
            // SQLITE_OPEN_CREATE so a deleted file is not silently recreated
            // as an empty schema-2 DB (P3-5 / 2026-09-03).
            // Non-fatal: if the open fails we simply lose Check 3 for now.
            let temporal_db_path = cache_dir.join("temporal.db");
            if temporal_db_path.try_exists().unwrap_or(false)
                && let Ok(db) = rskim_search::TemporalDb::open_existing(&temporal_db_path)
                && let Some(is_shallow) = probed_shallow
            {
                let _ = db.set_meta(
                    rskim_search::META_IS_SHALLOW,
                    if is_shallow { "1" } else { "0" },
                );
            }
            return Ok(());
        }
    };

    // ── Score computation (pure, no I/O) ─────────────────────────────────────
    // Empty-history path falls through to the single lock+open+sync block below
    // (LOCKED DECISION 2026-06-24): a present-but-empty temporal.db with
    // META_GIT_HEAD set prevents the per-query rebuild loop — temporal_db_is_stale
    // reads META_GIT_HEAD and sees Current, so no rebuild. Falling through avoids
    // duplicating the lock+open+sync block and eliminates the partial-file risk
    // of an early-return (if sync fails after TemporalDb::open creates the file,
    // the file exists with no META_GIT_HEAD row → rebuild loop).
    let (mut hotspot_rows, mut risk_rows, mut cochange_rows) = if risk_history.commits.is_empty() {
        (vec![], vec![], vec![])
    } else {
        // Full-history walk feeds all score computation (O-C / ADR-003).
        // risk_scores: decay-weighted hotspot/fix_density.
        let risk_scores = rskim_search::compute_file_risk_scores(
            &risk_history.commits,
            now_epoch,
            DEFAULT_HALF_LIFE_DAYS,
        );
        // temporal_stats: windowed counts (changes_30d/90d) PLUS lifetime totals.
        let temporal_stats =
            rskim_search::compute_file_temporal_stats(&risk_history.commits, now_epoch);
        (
            build_hotspot_rows(&risk_scores, &temporal_stats),
            build_risk_rows(&risk_scores, &temporal_stats),
            build_cochange_rows(&risk_history),
        )
    };

    // AD-414-9: capture pre-ghost-filter row counts so the zero-row notice (below,
    // at the sync success arm) can name the STAGE that zeroed the data.
    // hotspot_rows is the representative slice (hotspot and risk are computed from
    // the same source and reach zero together; cochange can independently be zero).
    let pre_ghost_hotspot = hotspot_rows.len();

    // ── Build-time ghost filter (AD-408-1 / AD-408-5) ────────────────────────
    // Applied on freshly-computed rows *before* `db.sync` persists them so the
    // prior DB survives on failure and the self-heal invariant holds (ADR-006).
    // See `apply_ghost_filter` for the full invariant documentation.
    //
    // AD-408-5: history paths from `parse_history` are REPO-ROOT-relative
    // because `gix::discover` walks upward to find `.git` from `root`. When
    // `root` is a subdirectory of the worktree (e.g. `--root crates/rskim-search`),
    // naive `root.join(rel)` double-nests the path prefix and causes every row
    // to be false-ghosted — all temporal output silently becomes empty (exit 0).
    // We discover the actual git workdir and use it as the anchor for
    // `rel_is_regular_file`, mirroring `heatmap/mod.rs` which joins against
    // `git_source.get_repo_root()`. Falls back to `root` if discovery fails
    // (D5: temporal failure must not fail the lexical query path).
    let ghost_root = discover_git_workdir(root).unwrap_or_else(|| root.to_path_buf());
    apply_ghost_filter(
        &ghost_root,
        &mut hotspot_rows,
        &mut risk_rows,
        &mut cochange_rows,
    );

    // ── AD-413-17: subdirectory scope filter ─────────────────────────────────
    // When `root` is a proper subdirectory of `ghost_root`, git history paths
    // are repo-root-relative (e.g. `crates/rskim-search/src/lib.rs`) while
    // skim-search query consumers expect paths relative to `root`
    // (e.g. `src/lib.rs`). Compute the scope prefix once and delegate to
    // `apply_scope_filter`, which retains only paths within the subtree and
    // rewrites them to be root-relative.
    // When `root == ghost_root` (plain single-root repo), `strip_prefix` yields
    // an empty string which is filtered out, so `scope` is `None` and
    // `apply_scope_filter` is not called — identity path for every non-subdirectory
    // invocation.
    //
    // Prefix construction: Path components joined with '/', using to_str() (not
    // to_string_lossy()) so a non-UTF-8 component produces None → scope = None
    // → filter skipped with a debug notice, rather than a U+FFFD-mangled prefix
    // that matches no history path and silently drops every row (Finding 4).
    // Component-joining omits replace('\\', "/"), preventing corruption of Unix
    // paths that legitimately contain '\' as a filename byte.
    let scope: Option<String> = root
        .canonicalize()
        .ok()
        .zip(ghost_root.canonicalize().ok())
        .and_then(|(r, g)| {
            r.strip_prefix(&g).ok().and_then(|p| {
                let result = p
                    .components()
                    .map(|c| c.as_os_str().to_str())
                    .collect::<Option<Vec<_>>>()
                    .map(|parts| parts.join("/"));
                if result.is_none() && crate::debug::is_debug_enabled() {
                    eprintln!(
                        "skim search [debug]: scope filter: subdirectory path \
                         contains non-UTF-8 components — skipping scope filter",
                    );
                }
                result
            })
        })
        .filter(|p| !p.is_empty())
        .map(|p| format!("{p}/"));

    if let Some(ref pfx) = scope {
        apply_scope_filter(pfx, &mut hotspot_rows, &mut risk_rows, &mut cochange_rows);
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

    // SE-1: the open-failure loud notice fires only on explicit build/rebuild/update.
    // `loudness` is passed explicitly by the caller (never inferred from `reanchor`)
    // so the two axes remain independently controllable (see BuildLoudness doc).
    let is_loud = loudness == BuildLoudness::Loud;

    let db_path = cache_dir.join("temporal.db");
    // AD-414-3 + SE-1: open with discard-on-corrupt semantics; SE-1 loudness
    // controlled by the BuildLoudness parameter.  Returns None when the open
    // fails non-fatally (caller returns Ok(()) — D5 isolation).
    // parse_history succeeded → is_shallow is concrete from the metadata.
    let is_shallow_opt = Some(risk_history.metadata.is_shallow);
    let Some(db) = open_or_discard_temporal_db(
        &db_path,
        cache_dir,
        is_loud,
        cache_dir,
        Some(head),
        is_shallow_opt,
    ) else {
        return Ok(());
    };

    // Helper: called on any successful sync (full-rows or fallback empty-rows).
    // Deletes the stale backoff sentinel and records the git-toplevel anchor.
    // AD-413-16: the anchor write is a SECOND, separate transaction after sync
    // so that process death between the two leaves the anchor absent (Absent →
    // adopt-and-record on the next query) rather than mismatched.
    let on_sync_ok = |db: &TemporalDb| {
        let _ = std::fs::remove_file(&backoff_sentinel);
        record_temporal_anchor(db, root, &ghost_root, reanchor);
    };

    // is_shallow from parse_history metadata (AD-414-14): recorded via sync() so
    // Check 3 in temporal_db_is_stale can detect a shallow→full transition.
    let is_shallow = risk_history.metadata.is_shallow;

    match db.sync(&hotspot_rows, &risk_rows, &cochange_rows, head, is_shallow) {
        Ok(()) => {
            on_sync_ok(&db);
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: temporal.db updated ({} hotspot, {} risk, {} cochange rows, HEAD={}…)",
                    hotspot_rows.len(),
                    risk_rows.len(),
                    cochange_rows.len(),
                    head.get(..8).unwrap_or(head),
                );
            }
            // AD-414-9: zero-row build notice — one stderr line, NOT debug-gated,
            // naming the STAGE that produced zero data.  Derived from captured
            // pre-/post-ghost-filter counts plus risk_history.metadata.is_shallow.
            // AC-18 guard (E-13): a healthy 1-commit repo always has hotspot rows,
            // so this fires only in genuinely degraded states.
            if hotspot_rows.is_empty() && risk_rows.is_empty() && cochange_rows.is_empty() {
                // AD-414-9: delegate stage classification to the pure helper
                // so this arm stays a single readable expression (Finding 4).
                let notice = zero_row_notice(&risk_history, pre_ghost_hotspot, is_shallow);
                eprintln!("skim search: {notice}");
            }
        }
        Err(SearchError::CapacityExceeded(msg)) => {
            // Too many rows (>500k) — degrade gracefully (D5).
            // Finding 2 backoff: try an empty-row sync so META_GIT_HEAD is written
            // and temporal_db_is_stale returns false on subsequent queries.
            // CapacityExceeded is a pre-transaction check so the DB is clean;
            // db.sync(&[], ...) starts a fresh transaction.  If it also fails,
            // write the sentinel as a last resort.
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: CapacityExceeded — {msg}. Consider a smaller repository — \
                     attempting empty-row fallback sync to prevent retry loop",
                );
            }
            if db.sync(&[], &[], &[], head, is_shallow).is_ok() {
                on_sync_ok(&db);
            } else {
                write_backoff_sentinel(cache_dir, head, is_shallow_opt);
            }
        }
        Err(e) => {
            // Sync failed for a non-capacity reason (DB error, disk full, etc.).
            // The failed transaction was rolled back, leaving the connection clean.
            // Apply the same fallback pattern: try an empty-row sync to write
            // META_GIT_HEAD; write the sentinel if that also fails.
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: sync failed: {e} — \
                     attempting empty-row fallback sync to prevent retry loop",
                );
            }
            if db.sync(&[], &[], &[], head, is_shallow).is_ok() {
                on_sync_ok(&db);
            } else {
                write_backoff_sentinel(cache_dir, head, is_shallow_opt);
            }
        }
    }

    Ok(())
}

/// AD-414-22: build a present-but-EMPTY `temporal.db` for a repository whose
/// HEAD is unborn — `git init` with files but no commits — and emit the AC-16
/// zero-row notice.
///
/// # Why this exists
///
/// A repository with no commits is an **empty history**, not a build failure and
/// not a non-repository.  Before this function the CLI could express neither:
/// `HeadState::Unresolved.sha()` is `None`, so
/// [`super::staleness::try_rebuild_temporal_nonfatal`] returned before
/// [`rebuild_temporal_with_source`] was ever entered.  `skim search --build` on a
/// no-commit repository therefore printed only the lexical "indexed N files"
/// line, created no `temporal.db`, and emitted no temporal notice — AC-16's
/// case (i) ("no commits") was unreachable from every CLI arm even though
/// [`zero_row_notice`] has produced its text since #414 landed and
/// `GixSource::parse_history` has returned `Ok(empty_result(is_shallow))` for an
/// unborn HEAD since #408 (`git_parser.rs`, `is_unborn_error`).
///
/// # Contract
///
/// - **Explicit build arms only.**  Called from `try_rebuild_temporal_nonfatal`
///   solely under [`super::staleness::ReanchorPolicy::Allow`] (`--build`,
///   `--rebuild`, `--update`).  The quiet query path must stay silent
///   (wave-wide loudness policy) and must not re-walk history per query.
/// - **No fabricated HEAD.**  The DB is written through
///   [`rskim_search::TemporalDb::sync_empty_unborn`], which *removes* the
///   `git_head` / `data_version` attestation pair rather than recording a
///   placeholder (avoids PF-016).  An absent `META_GIT_HEAD` is the state
///   `warn_if_temporal_unverifiable` already documents as the "unborn-branch
///   no-loop case", and it makes `temporal_db_is_stale` Check 1 report stale the
///   moment the repository's first commit lands.
/// - **`is_shallow` is probed, never fabricated** — it comes from
///   `parse_history`'s own `HistoryResult::metadata`, the same source the
///   resolved-HEAD path uses.
/// - **No rebuild loop.**  On the quiet path `current_head` is `None` for an
///   unborn HEAD, so neither the BUG-B self-heal gate nor the post-rebuild hook
///   reaches any temporal rebuild; the loop guarantee still derives from that
///   short-circuit, not from the DB's contents.
///
/// # Non-cases (deliberately left untouched)
///
/// `HeadState::Unresolved` also covers a corrupt `HEAD` file, an unsupported ref
/// backend (reftable, #481) and fs errors on repositories that *do* have history.
/// Two guards keep those out: a `parse_history` `Err` returns without touching
/// `temporal.db` (F3 semantics — a failure to read history must not be reported
/// as "no history"), and a successful parse that yields commits also returns
/// without writing, because this function's notice would then be a lie.
///
/// # Failure isolation (D5)
///
/// Every failure mode returns `Ok(())` with a debug-gated diagnostic: a temporal
/// failure must never fail the explicit build that triggered it (ADR-006/D5).
pub(super) fn build_empty_temporal_for_unborn_head(
    src: &dyn rskim_search::TemporalSource,
    root: &Path,
    cache_dir: &Path,
    reanchor: super::staleness::ReanchorPolicy,
    loudness: BuildLoudness,
) -> anyhow::Result<()> {
    // `parse_history` returns Ok(empty) for a genuinely unborn HEAD and Err for a
    // repository whose history could not be read at all; only the former is ours.
    let history = match src.parse_history(root, 0) {
        Ok(h) => h,
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: HEAD is unresolvable and parse_history failed: {e} — \
                     leaving temporal.db untouched (no empty-history claim without evidence)",
                );
            }
            return Ok(());
        }
    };
    if !history.commits.is_empty() {
        // HEAD is unresolvable for some reason OTHER than an unborn branch
        // (corrupt HEAD, reftable backend) on a repository that does have
        // history.  Recording zero rows and announcing "no commit history"
        // would both be false; leave the DB as it is.
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: HEAD is unresolvable but {} commits are readable — \
                 not an unborn branch; leaving temporal.db untouched",
                history.commits.len(),
            );
        }
        return Ok(());
    }

    // Lock ordering matches `rebuild_temporal_with_source`: acquired after the
    // (pure) history read, around the open+sync phase only (D4).
    let _lock = super::build_lock::acquire("skim search", cache_dir)?;

    let db_path = cache_dir.join("temporal.db");
    let is_shallow = history.metadata.is_shallow;
    // AD-414-3 discard-on-corrupt semantics are shared with the resolved-HEAD
    // path; `head = None` suppresses the sentinel write (see that fn's doc).
    let Some(db) = open_or_discard_temporal_db(
        &db_path,
        cache_dir,
        loudness == BuildLoudness::Loud,
        cache_dir,
        None,
        Some(is_shallow),
    ) else {
        return Ok(());
    };

    if let Err(e) = db.sync_empty_unborn(is_shallow) {
        if crate::debug::is_debug_enabled() {
            eprintln!("skim search [debug]: empty-history sync failed (non-fatal): {e}");
        }
        return Ok(());
    }

    // Mirrors `on_sync_ok` on the resolved-HEAD path: record (or clear) the
    // repository anchor in a second transaction (AD-413-16).  No sentinel is
    // cleared here because none can have been written for an absent HEAD.
    let ghost_root = discover_git_workdir(root).unwrap_or_else(|| root.to_path_buf());
    record_temporal_anchor(&db, root, &ghost_root, reanchor);

    // AC-16 case (i): exactly one non-debug-gated stderr line naming the stage
    // that produced zero data.  `commits.is_empty()` selects the "no commits"
    // branch of `zero_row_notice`, whose text must not mention `shallow` even
    // when this unborn repository happens to be a shallow clone.
    eprintln!("skim search: {}", zero_row_notice(&history, 0, is_shallow));

    Ok(())
}

// ============================================================================
// Extracted helpers (Finding 4 — complexity reduction)
// ============================================================================

/// Open `temporal.db`, discarding and retrying once on `DatabaseCorrupt`.
///
/// Implements the AD-414-3 "exactly one retry — never a loop" invariant.
/// On success returns `Some(db)`.  On any non-fatal failure, writes the
/// backoff sentinel (when safe to do so), prints a diagnostic, and returns
/// `None` so the caller can immediately `return Ok(())` (temporal failure is
/// non-fatal per ADR-006/D5).
///
/// `is_loud` gates the open-failure notice for the `Err(other)` arm (SE-1).
/// `is_shallow` is passed to [`write_backoff_sentinel`] so the two-line
/// format is written consistently from all failure arms (AD-414-21).
///
/// AD-414-22: `head` is `None` for the unborn-HEAD caller
/// ([`build_empty_temporal_for_unborn_head`]).  The backoff sentinel is keyed on
/// HEAD, so with no HEAD there is no key to bound retries with and no sentinel is
/// written — which is sound because that caller is reachable only from the
/// explicit build arms (one attempt per user-initiated invocation), never from
/// the per-query quiet path.
fn open_or_discard_temporal_db(
    db_path: &std::path::Path,
    cache_dir: &std::path::Path,
    is_loud: bool,
    _backoff_sentinel: &std::path::Path, // kept for caller compat; use cache_dir internally
    head: Option<&str>,
    is_shallow: Option<bool>,
) -> Option<TemporalDb> {
    // AD-414-22: no HEAD → no per-HEAD backoff key → no sentinel (see fn doc).
    let bound_retry_for_this_head = || {
        if let Some(head) = head {
            write_backoff_sentinel(cache_dir, head, is_shallow);
        }
    };
    match TemporalDb::open(db_path) {
        Ok(d) => Some(d),
        // AD-414-3: DatabaseCorrupt is the ONLY variant that licenses deleting
        // on-disk state.  Discard, then exactly ONE re-open attempt — never a loop.
        Err(SearchError::DatabaseCorrupt(m)) => {
            eprintln!("skim search: temporal.db was corrupt ({m}) — discarding and rebuilding it");
            match std::fs::remove_file(db_path) {
                Ok(()) => {
                    // SE-3: sidecars only after a SUCCESSFUL main unlink.  Removing
                    // them after a failed unlink can strip a still-valid DB's WAL
                    // under a concurrent reader (.skim-build.lock serialises writers).
                    for sidecar in ["temporal.db-wal", "temporal.db-shm"] {
                        let _ = std::fs::remove_file(cache_dir.join(sidecar));
                    }
                }
                Err(e) => {
                    // AC-29: loud, non-debug-gated; names the absolute path and
                    // gives an actionable manual-deletion instruction.
                    // F8: {:?} quotes ESC/CR/LF in cache-derived paths (ADR-008).
                    eprintln!(
                        "skim search: could not delete the corrupt temporal.db at {:?} ({e}) — \
                         delete this file manually, then re-run 'skim search --rebuild'",
                        db_path
                    );
                    // D5 backoff: the corrupt DB remains on disk (unlink failed), so
                    // `temporal_db_is_stale` will open it read-only, find no META_GIT_HEAD,
                    // return stale, and re-run the full parse_history walk — producing an
                    // unbounded retry loop that prints two stderr lines per invocation.
                    // The sentinel bounds it to once per HEAD, matching both sibling arms
                    // (recreate-failed and Err(other)).  SE-3 is not violated: no sidecar
                    // removal is attempted here (the main unlink itself failed).
                    bound_retry_for_this_head();
                    return None;
                }
            }
            // EXACTLY ONE retry (AD-414-3) — never a loop.
            match TemporalDb::open(db_path) {
                Ok(d) => Some(d),
                Err(e2) => {
                    // F8: {:?} quotes ESC/CR/LF in cache-derived paths (ADR-008).
                    eprintln!(
                        "skim search: temporal.db could not be recreated after discard ({e2}) \
                         — delete {:?} manually",
                        db_path
                    );
                    // D5 backoff: the discard SUCCEEDED, so temporal.db is now absent;
                    // the sentinel prevents the per-query rebuild loop.
                    bound_retry_for_this_head();
                    None
                }
            }
        }
        Err(other) => {
            // SE-1: loud ONLY on explicit build/rebuild/update; debug-gated otherwise.
            // The loud text is built by `degraded_notice` from the CLASSIFIED reason
            // (UnsupportedSchemaVersion → UnsupportedVersion, everything else →
            // Unreadable), NEVER from `{other}` alone.  AD-414-11: the
            // UnsupportedSchemaVersion case is invisible to temporal_db_is_stale,
            // so this arm is the only path that can tell --build/--rebuild which
            // version was found, which is supported, and that upgrading (not
            // deleting) is the remedy.  AC-19(b) forbids cause substrings outside
            // the degraded_notice builder.
            let (reason, detail) = match other {
                SearchError::UnsupportedSchemaVersion { found, supported } => (
                    DegradedReason::UnsupportedVersion,
                    DegradedReason::unsupported_version_detail(found, supported),
                ),
                ref e => (DegradedReason::Unreadable, e.to_string()),
            };
            let u = TemporalUnavailable { reason, detail };
            let notice = degraded_notice(&u, "", Fallback::NoResults);
            if is_loud {
                eprintln!("skim search: {notice}");
            } else if crate::debug::is_debug_enabled() {
                eprintln!("skim search [debug]: temporal open failed — {notice}");
            }
            // D5 + backoff: write the sentinel so subsequent queries skip the
            // rebuild for this HEAD.  Best-effort — if the cache dir is also
            // unwritable the retry continues until HEAD advances (D5).
            // UnsupportedSchemaVersion leaves temporal.db byte-for-byte unchanged
            // (R1 contract); the sentinel prevents an infinite retry without touching
            // the DB.
            bound_retry_for_this_head();
            None
        }
    }
}

/// Compute the zero-row build notice string (AD-414-9, pure, DB-free).
///
/// Called from within the `Ok(())` arm of `db.sync(...)` after a sync that
/// wrote zero rows.  Classifies the stage that produced zero data and routes
/// through [`degraded_notice`] (AD-414-1 SSOT) in every branch.
///
/// Returns the notice string to pass to `eprintln!("skim search: {notice}")`.
///
/// # Classification
///
/// Evaluation order (AD-407-7): the shallow arm (case ii) is evaluated BEFORE
/// the empty-commits arm (case i).  After #407 skips merge commits, a shallow
/// clone whose HEAD is a merge yields zero commits AND `is_shallow = true`.
/// Evaluating case (i) first would emit the untruthful "no commits" wording
/// instead of the truthful "shallow" explanation.  The shallow arm now subsumes
/// both sub-cases: commits empty + is_shallow, and commits present but all diffs
/// absent + is_shallow (the pre-#407 shape).  Case (i) is still reachable for
/// a fresh `git init` where `is_shallow` is false.
///
/// - Case (ii): `pre_ghost_hotspot == 0 && is_shallow` → `Empty` with "shallow"
///   detail (evaluated FIRST; shallow cause must not be masked by case i).
/// - Case (i): commits empty AND `is_shallow` is false → `Empty` (no shallow
///   suffix per T-16/AC-16).
/// - Case (iii): rows computed, ghost filter zeroed them → `GhostFilter` with
///   count detail.  Routes through `degraded_notice` so the SSOT guard catches
///   any future drift (AD-414-1, `t19b_no_cause_substring_outside_the_builder`).
/// - Fallback: commits present, no extractable diffs, not shallow → treat as
///   case (i).
fn zero_row_notice(
    risk_history: &HistoryResult,
    pre_ghost_hotspot: usize,
    is_shallow: bool,
) -> String {
    if pre_ghost_hotspot == 0 && is_shallow {
        // Case (ii) — evaluated FIRST (AD-407-7).
        // Covers both: commits empty + shallow clone, and commits present but
        // all diffs failed under a shallow clone (changed_files == [] for every
        // commit).  Shallow wording is permitted only when is_shallow is true.
        let u = TemporalUnavailable {
            reason: DegradedReason::Empty,
            detail: "shallow".to_string(),
        };
        degraded_notice(&u, "", Fallback::NoResults)
    } else if risk_history.commits.is_empty() {
        // Case (i): no commits at all and not shallow — empty history.
        // Must NOT contain "shallow" or "unshallow" (T-16 AC-16).
        let u = TemporalUnavailable {
            reason: DegradedReason::Empty,
            detail: String::new(),
        };
        degraded_notice(&u, "", Fallback::NoResults)
    } else if pre_ghost_hotspot > 0 {
        // Case (iii): rows were computed but the ghost filter dropped all of them.
        // Must NOT contain "shallow" or "unshallow" (T-16 AC-16).
        // Routes through degraded_notice (AD-414-1 SSOT) via GhostFilter variant.
        let u = TemporalUnavailable {
            reason: DegradedReason::GhostFilter,
            detail: pre_ghost_hotspot.to_string(),
        };
        degraded_notice(&u, "", Fallback::NoResults)
    } else {
        // Fallback: commits present but no diffs extractable and not shallow —
        // treat as case (i) (no useful history).
        let u = TemporalUnavailable {
            reason: DegradedReason::Empty,
            detail: String::new(),
        };
        degraded_notice(&u, "", Fallback::NoResults)
    }
}

/// Write the git repository toplevel anchor into an open [`TemporalDb`] after a
/// successful [`TemporalDb::sync`] for a root that is inside (but not at) a git
/// repository (AD-413-16).
///
/// This must be called as a **second, separate transaction** after `db.sync`
/// completes successfully.  Process death between `sync` and this call leaves
/// [`rskim_search::META_GIT_TOPLEVEL`] absent, which maps to
/// [`super::staleness::AnchorState::Absent`] — the "adopt-and-record" path in
/// [`super::staleness::temporal_anchor_state`], never a false refusal.
///
/// # Gate 1 — not adopted (Finding 1)
///
/// Compares canonicalized `root` against `ghost_root` (the gix-discovered
/// worktree workdir, already computed by the caller) instead of re-deriving via
/// `resolve_repo_toplevel` (a hand-rolled ancestor walk that ignores env vars).
/// When `root == ghost_root` (plain single-root repo, or gix discovery failed
/// and the fallback `ghost_root = root` is in force), the function deletes any
/// stale anchor row and returns — zero DB reads for all plain-repo users.
///
/// # Finding 5 — stale anchor cleanup
///
/// When Gate 1 fires (NotAdopted), any pre-existing [`rskim_search::META_GIT_TOPLEVEL`]
/// row is deleted so a leftover anchor from a prior invocation cannot trigger
/// false re-anchor refusals on subsequent queries.
///
/// # PF-017 — anchor-write guard on `Refuse` policy
///
/// When `reanchor == ReanchorPolicy::Refuse` and the DB already holds a
/// *different* anchor value, the write is skipped with a debug notice.  This
/// prevents the auto-refresh path (a plain lexical query that happens to
/// trigger a HEAD-stale rebuild) from silently retargeting a linked-worktree DB
/// whose anchor was set by a prior explicit `--build` or `--rebuild`.
fn record_temporal_anchor(
    db: &TemporalDb,
    root: &Path,
    ghost_root: &Path,
    reanchor: super::staleness::ReanchorPolicy,
) {
    // Gate 1: is root a proper subdirectory of ghost_root?
    // When gix discovery failed, ghost_root == root (caller's unwrap_or fallback),
    // so canonicalize equality holds → NotAdopted → correct no-op per D5.
    let adopted = root
        .canonicalize()
        .ok()
        .zip(ghost_root.canonicalize().ok())
        .is_some_and(|(r, g)| r != g);
    // AD-414-18: `gix::discover` and the filesystem ancestor walk
    // (`resolve_repo_toplevel`) use different mechanisms and can disagree when
    // gix cannot open a repository format it doesn't support (e.g. reftable).
    // In that case `ghost_root` falls back to `root` while `resolve_repo_toplevel`
    // returns the enclosing toplevel, so a `debug_assert_eq!` here would abort
    // debug builds and `cargo test` runs inside the D5-isolated path whose
    // entire contract is "temporal failure must NOT fail the lexical query" —
    // the worst possible place for a panic (applies ADR-006).  Downgraded to a
    // debug-gated `eprintln!` using the module's own idiom so the disagreement
    // is observable without crashing anything.
    if crate::debug::is_debug_enabled() {
        let walk = super::gitdir::resolve_repo_toplevel(root)
            .as_deref()
            .unwrap_or(root)
            .canonicalize()
            .ok();
        let gix_top = ghost_root.canonicalize().ok();
        if walk != gix_top {
            eprintln!(
                "skim search [debug]: record_temporal_anchor: ghost_root {:?} \
                 disagrees with hand-rolled resolve_repo_toplevel for root {:?} \
                 (walk={walk:?}, gix={gix_top:?}); gix may not support this repo format",
                ghost_root, root,
            );
        }
    }
    if !adopted {
        // Finding 5: remove any stale anchor from a previous invocation so it
        // cannot drive false refusals on the next query-path anchor check.
        let _ = db
            .delete_meta(rskim_search::META_GIT_TOPLEVEL)
            .inspect_err(|e| {
                if crate::debug::is_debug_enabled() {
                    eprintln!("skim search [debug]: temporal anchor clear failed (non-fatal): {e}");
                }
            });
        return;
    }
    // Canonicalize before storing so the recorded path agrees with the live
    // path returned by `resolve_repo_toplevel` (which calls `.canonicalize()`).
    // On macOS, `gix::discover` can return `/var/...` while the live path is
    // `/private/var/...` — without this step the anchor comparison always
    // disagrees on `/tmp`-rooted temp dirs and the anchor check is unreliable.
    let canonical_ghost = ghost_root
        .canonicalize()
        .unwrap_or_else(|_| ghost_root.to_path_buf());
    let Some(top_str) = canonical_ghost.to_str() else {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: temporal anchor: git workdir path is not valid \
                 UTF-8, skipping anchor write"
            );
        }
        return;
    };
    // PF-017: Refuse policy — if the DB already has a *different* anchor value,
    // skip the write.  The auto-refresh path must not silently retarget a DB
    // anchored by a prior explicit build arm.  Use `skim search --rebuild` to
    // force re-anchoring.
    if reanchor == super::staleness::ReanchorPolicy::Refuse
        && let Ok(Some(existing)) = db.get_meta(rskim_search::META_GIT_TOPLEVEL)
        && existing.as_str() != top_str
    {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: temporal anchor write skipped \
                 (Refuse policy, anchor would change from {:?} to {:?}); \
                 use `skim search --rebuild` to re-anchor (PF-017)",
                existing, top_str,
            );
        }
        return;
    }
    if let Err(e) = db.set_meta(rskim_search::META_GIT_TOPLEVEL, top_str)
        && crate::debug::is_debug_enabled()
    {
        eprintln!("skim search [debug]: temporal anchor write failed (non-fatal): {e}");
    }
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
