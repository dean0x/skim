//! Temporal query helpers for `skim search` temporal flags.
//!
//! # Responsibilities
//!
//! - Path normalization for `--blast-radius` (cross-platform, repo-relative).
//! - `TemporalDb` open/check helpers.
//! - Standalone temporal dispatch (`--hot`, `--cold`, `--risky`, `--blast-radius`).
//! - Combined text+temporal enrichment (`apply_temporal_enrichment`).
//! - Output formatting for standalone temporal queries.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use rskim_search::{FileId, HotspotRow, RiskRow, TemporalDb};
use serde::Serialize;

use super::types::{ResolvedResult, TemporalAnnotation, TemporalSort};

// ============================================================================
// Path normalization
// ============================================================================

/// Normalize a user-provided file path to repo-root-relative form.
///
/// Algorithm:
/// 1. If absolute, use as-is. If relative, try joining to `project_root`
///    first; fall back to CWD when the root-relative path doesn't exist.
/// 2. Canonicalize (resolve symlinks, normalize `../`).
/// 3. Strip `project_root` prefix → repo-relative.
/// 4. Replace `\\` with `/` for Windows cross-platform consistency.
///
/// The root-first resolution makes `--blast-radius src/foo.rs` work correctly
/// when the user's CWD is the repo root or any subdirectory thereof.
///
/// # Errors
///
/// Returns an error when the path is outside the repository root or cannot
/// be canonicalized.
pub(super) fn normalize_blast_radius_path(
    raw: &str,
    project_root: &Path,
) -> anyhow::Result<String> {
    let p = std::path::Path::new(raw);

    // Resolve to an absolute path, trying existence in order:
    // 1. project-root-relative (most common for `--blast-radius src/foo.rs`)
    // 2. CWD-relative (user is in a subdirectory of the repo)
    // 3. Neither exists → bail with a clear "not found" error.
    //
    // The existence check happens before canonicalization so that missing files
    // produce "blast-radius file not found: <path>" instead of the confusing
    // "outside the project root" message that canonicalize() fallback would yield.
    let abs = if p.is_absolute() {
        // Absolute paths: check existence directly before proceeding.
        if !p.exists() {
            anyhow::bail!("blast-radius file not found: {}", raw);
        }
        p.to_path_buf()
    } else {
        // Prefer project-root-relative resolution so that `src/foo.rs` works
        // regardless of the user's CWD within the repo.
        let root_relative = project_root.join(p);
        if root_relative.exists() {
            root_relative
        } else {
            // Fallback: CWD-relative (e.g. user is in a subdirectory).
            // If current_dir() fails (deleted temp dir in tests, unusual in
            // production), treat it as "not found" rather than propagating a
            // confusing OS error.
            let cwd_relative = std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(p))
                .filter(|candidate| candidate.exists());

            match cwd_relative {
                Some(path) => path,
                None => anyhow::bail!("blast-radius file not found: {}", raw),
            }
        }
    };

    // Canonicalize — resolves `..` and symlinks.
    // Fallback to the raw path if canonicalize fails (e.g. race: file deleted
    // between the existence check above and this call).
    let canonical = abs.canonicalize().unwrap_or_else(|e| {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search: canonicalize failed for {:?}: {e} — using raw path",
                abs
            );
        }
        abs.clone()
    });

    // Canonicalize the project root too for fair comparison.
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    // Strip the root prefix.
    let rel = canonical
        .strip_prefix(&canonical_root)
        .map_err(|_| {
            anyhow::anyhow!(
                "path {:?} is outside the project root {:?}",
                raw,
                canonical_root
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");

    // Strip leading `./` if present (edge case on some platforms).
    //
    // NOTE (#373 scope): this extra `strip_prefix("./")` step is intentional and
    // is NOT consolidated into `walk::normalize_rel_path`.  That helper only
    // covers the manifest-key/assignment/cache-lookup triple (the walk sort key
    // plus the `path_key` bindings in `index.rs` `consume`/`read_and_classify`);
    // it does not carry the `./` strip.  Combining the two would change
    // `--blast-radius ./foo/bar.rs` lookup behavior and widen the regression
    // blast-radius beyond #373's narrow scope.
    let normalized = rel.strip_prefix("./").unwrap_or(&rel).to_string();

    Ok(normalized)
}

// ============================================================================
// DB helpers
// ============================================================================

/// Try to open the temporal database at `db_path`.
///
/// Returns `None` when the file does not exist, is corrupt, or cannot be opened.
/// This allows callers to degrade gracefully rather than hard-fail.
pub(super) fn open_temporal_db(db_path: &Path) -> Option<TemporalDb> {
    if !db_path.exists() {
        return None;
    }
    TemporalDb::open(db_path).ok()
}

// ============================================================================
// Bounded re-sort window
// ============================================================================

/// Compute the bounded candidate window for a temporal re-sort.
///
/// `limit * 5`, clamped to at least 100, mirroring the original inline bound in
/// `query_standalone`.  Callers fetch this many candidates (in raw ranked order)
/// before enriching + re-sorting by temporal score, then truncate to `limit`.
/// This keeps per-file DB lookups bounded (`O(window)`, not `O(all matches)` —
/// AC-P1) while ensuring a temporally-hot file that ranks beyond `limit` in raw
/// order can still surface after the re-sort (AC-F4).
///
/// `saturating_mul` guards a hostile `--limit` near `usize::MAX` from overflowing.
pub(super) fn resort_window(limit: usize) -> usize {
    limit.saturating_mul(5).max(100)
}

// ============================================================================
// Blast-radius → FileId resolution (shared helper)
// ============================================================================

/// Convert a set of repo-relative path strings to the corresponding `FileId`s.
///
/// Iterates the pre-computed `sorted_paths` slice once, collecting `FileId`s for
/// every path in `allowed_paths`.  Applies PF-004 widening (`u32::try_from(idx)`)
/// — never `as u32`.  Emits a one-line stderr warning when the result set is empty
/// (the blast-radius paths are not indexed), so callers do not have to repeat the
/// check.
///
/// Accepts a `&[&str]` slice (from `manifest.sorted_paths()`) so that callers
/// which already hold the slice can pass it directly without a second allocation.
///
/// This function is the single source of truth for the path→FileId conversion
/// used by all three blast-radius call sites (ast.rs standalone, query.rs lexical
/// filter, and mod.rs resolve_blast_radius_filter).
pub(super) fn paths_to_file_ids(
    sorted_paths: &[&str],
    allowed_paths: &HashSet<String>,
) -> HashSet<FileId> {
    let mut file_ids = HashSet::new();
    for (idx, path) in sorted_paths.iter().enumerate() {
        if allowed_paths.contains(*path) {
            // PF-004: widen idx (usize) to u32 before constructing FileId.
            // The file cap (50 000) guarantees no overflow, but `try_from`
            // makes the widening explicit and safe by construction.
            if let Ok(id) = u32::try_from(idx) {
                file_ids.insert(FileId(id));
            }
        }
    }
    if file_ids.is_empty() {
        eprintln!(
            "skim search: blast-radius filter matched 0 indexed files \
             (allowed {} paths, index has {} files)",
            allowed_paths.len(),
            sorted_paths.len()
        );
    }
    file_ids
}

/// Resolve a `--blast-radius` raw path to the set of co-change partner paths.
///
/// Shared core for both `resolve_blast_radius_file_ids` (standalone AST path) and
/// `resolve_blast_radius_filter` (text-query path in `mod.rs`).  Returns the set of
/// repo-relative path strings that the blast-radius filter should allow, including
/// the target file itself.  JSON-aware warning emitted when the temporal DB is absent.
///
/// Returns `Ok(None)` when `blast_radius` is `None` or the DB is absent/corrupt.
///
/// # Errors
///
/// Returns `Err` only when path normalization fails (outside-repo or missing file).
pub(super) fn resolve_blast_radius_paths(
    blast_radius: Option<&str>,
    root: &Path,
    db_path: &Path,
    json: bool,
) -> anyhow::Result<Option<std::collections::HashSet<String>>> {
    let Some(raw_path) = blast_radius else {
        return Ok(None);
    };

    let Some(db) = open_temporal_db(db_path) else {
        // Compose from the shared constant in mod.rs so this message can't drift
        // from run_temporal_standalone's message (#357 cycle-2 finding 2).
        // The constant lives in mod.rs (the parent module — `super::NO_TEMPORAL_DATA_MSG`)
        // and is the single source of truth for AC9 (no "skim heatmap" advice).
        let msg = format!(
            "no temporal data for --blast-radius — {}",
            super::NO_TEMPORAL_DATA_MSG
        );
        if json {
            let envelope = serde_json::json!({ "warning": msg });
            eprintln!("{}", serde_json::to_string(&envelope)?);
        } else {
            eprintln!("skim search: {msg}");
        }
        return Ok(None);
    };

    let normalized = normalize_blast_radius_path(raw_path, root)?;
    let partners = db.cochanges_for_file(&normalized)?;
    if partners.is_empty() {
        eprintln!("skim search: no co-change data for {raw_path:?}");
    }
    let mut allowed_paths = cochange_partner_paths(&partners, &normalized);
    // Include the target file itself so queries like `skim search auth --blast-radius src/auth.rs`
    // surface matches within the target file in addition to its co-change partners.
    allowed_paths.insert(normalized);
    Ok(Some(allowed_paths))
}

/// Resolve a `--blast-radius` raw path to the set of matching `FileId`s.
///
/// Unified resolver used by every blast-radius call site:
/// - `run_ast_standalone` caller in `mod.rs` (standalone `--ast --blast-radius`)
/// - `execute_query_with_manifest` blast-radius arm (query.rs, via `paths_to_file_ids`)
/// - `resolve_blast_radius_filter` (mod.rs, text + blast-radius)
///
/// Algorithm:
/// 1. If `blast_radius` is `None`, return `Ok(None)` immediately.
/// 2. Open `temporal.db` at `db_path`.  If absent/corrupt, emit the
///    "no temporal data" warning (JSON-aware when `json=true`) and return `Ok(None)`.
/// 3. Normalize the raw path to repo-relative form.
/// 4. Look up co-change partners, add the target file itself.
/// 5. Convert the path set to `FileId`s via `paths_to_file_ids`.
/// 6. Return `Ok(Some(file_ids))`.
///
/// # Errors
///
/// Returns `Err` only when path normalization fails (outside-repo or missing file).
pub(super) fn resolve_blast_radius_file_ids(
    blast_radius: Option<&str>,
    root: &Path,
    db_path: &Path,
    sorted_paths: &[&str],
    json: bool,
) -> anyhow::Result<Option<HashSet<FileId>>> {
    let Some(allowed_paths) = resolve_blast_radius_paths(blast_radius, root, db_path, json)? else {
        return Ok(None);
    };
    let file_ids = paths_to_file_ids(sorted_paths, &allowed_paths);
    Ok(Some(file_ids))
}

/// Check whether the temporal database is stale compared to the current git HEAD.
///
/// Returns `Some(warning_message)` when the stored HEAD differs from the
/// current HEAD, `None` when current or when the staleness check cannot be
/// performed (missing git, non-git repo, missing meta key).
///
/// # Usage note (Decision O-B)
///
/// This function is no longer called on the production query path —
/// `auto_refresh_if_stale` in `staleness.rs` guarantees freshness before any
/// query executes, making this staleness warning dead code on the happy path.
/// It is retained for test use only (AC6 discriminating assertion in
/// `temporal_build_tests.rs`).
#[cfg(test)]
pub(super) fn check_temporal_staleness(db: &TemporalDb, project_root: &Path) -> Option<String> {
    let stored_head = db.get_meta(rskim_search::META_GIT_HEAD).ok().flatten()?;

    let current_head = read_git_head(project_root)?;
    if stored_head.trim() != current_head.trim() {
        Some(format!(
            "skim search: temporal data is stale (stored: {}, current: {}). \
             Run 'skim search' on this repo to auto-refresh.",
            stored_head.get(..7).unwrap_or(&stored_head),
            current_head.get(..7).unwrap_or(&current_head),
        ))
    } else {
        None
    }
}

/// Read the current git HEAD SHA from the project root.
///
/// Spawns `git rev-parse HEAD` with a 5-second timeout. Returns `None` on
/// timeout, spawn failure, non-zero exit, or non-git directory.
///
/// The timeout prevents indefinite hangs on network-mounted repos or
/// corrupted `.git` directories. The staleness check is advisory, so
/// timing out is safe — the caller degrades gracefully.
///
/// Only compiled in test builds — see `check_temporal_staleness` doc.
#[cfg(test)]
fn read_git_head(root: &Path) -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(5);

    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("HEAD")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let child_id = child.id();
    let (tx, rx) = mpsc::channel::<Option<String>>();

    std::thread::spawn(move || {
        let result = child.wait_with_output().ok().and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        });
        let _ = tx.send(result);
    });

    match rx.recv_timeout(TIMEOUT) {
        Ok(result) => result,
        Err(_timeout) => {
            // Kill the subprocess so it doesn't linger after we give up.
            #[cfg(unix)]
            {
                // SAFETY: kill(2) is always safe to call with a valid pid.
                unsafe {
                    libc::kill(child_id as libc::pid_t, libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/PID", &child_id.to_string()])
                    .status();
            }
            None
        }
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Given a co-change row, return the path of the file that is NOT `target`.
///
/// Co-change pairs are stored with the lexically smaller path in `file_a`. This
/// helper resolves both directions so callers don't need to repeat the pattern.
fn cochange_partner<'a>(row: &'a rskim_search::CochangeRow, target: &str) -> &'a str {
    if row.file_a == target {
        &row.file_b
    } else {
        &row.file_a
    }
}

/// Extract the set of partner paths from a slice of co-change rows.
///
/// Uses `cochange_partner` to resolve both `file_a`/`file_b` directions. The
/// `target` file itself is NOT included — callers add it separately when needed.
pub(super) fn cochange_partner_paths(
    partners: &[rskim_search::CochangeRow],
    target: &str,
) -> std::collections::HashSet<String> {
    partners
        .iter()
        .map(|p| cochange_partner(p, target).to_string())
        .collect()
}

// ============================================================================
// Standalone temporal query
// ============================================================================

/// Output variants from a standalone temporal query.
#[derive(Debug)]
pub(super) enum TemporalQueryOutput {
    /// Top hotspot files (--hot).
    Hotspots(Vec<HotspotRow>),
    /// Top coldspot files (--cold).
    Coldspots(Vec<HotspotRow>),
    /// Top risky files (--risky).
    Risks(Vec<RiskRow>),
    /// Co-change partners of a target file (--blast-radius).
    Cochanges {
        target: String,
        partners: Vec<rskim_search::CochangeRow>,
    },
}

/// Execute a standalone temporal query (no text query).
///
/// - `sort`: optional sort mode (Hot, Cold, Risky).
/// - `blast_radius`: optional file path for co-change partner lookup.
/// - `limit`: maximum number of results.
/// - `db`: open temporal database.
/// - `project_root`: needed for path normalization of `blast_radius`.
///
/// # Errors
///
/// Returns an error if path normalization fails or the database query fails.
pub(super) fn query_standalone(
    sort: Option<TemporalSort>,
    blast_radius: Option<&str>,
    limit: usize,
    db: &TemporalDb,
    project_root: &Path,
) -> anyhow::Result<TemporalQueryOutput> {
    if let Some(raw_path) = blast_radius {
        let normalized = normalize_blast_radius_path(raw_path, project_root)?;
        let mut partners = db.cochanges_for_file(&normalized)?;

        if let Some(sort_mode) = sort {
            // Pre-truncate before the re-sort to bound per-file DB lookups.
            // The cochange query already returns results sorted by Jaccard DESC,
            // so the highest co-change partners are at the front. Window is
            // limit*5 clamped to at least 100 so small limits don't over-prune.
            partners.truncate(resort_window(limit));
            resort_partners_by_temporal(&mut partners, sort_mode, &normalized, db)?;
        }

        partners.truncate(limit);
        return Ok(TemporalQueryOutput::Cochanges {
            target: normalized,
            partners,
        });
    }

    // No blast-radius — pure temporal sort.
    match sort {
        Some(TemporalSort::Hot) | None => {
            Ok(TemporalQueryOutput::Hotspots(db.top_hotspots(limit)?))
        }
        Some(TemporalSort::Cold) => Ok(TemporalQueryOutput::Coldspots(db.top_coldspots(limit)?)),
        Some(TemporalSort::Risky) => Ok(TemporalQueryOutput::Risks(db.top_risks(limit)?)),
    }
}

/// Re-sort blast-radius partners by temporal score using per-file lookups.
///
/// Callers MUST pre-truncate `partners` to a reasonable window before calling
/// this function to bound the number of per-file DB queries.
///
/// Uses `hotspot_for_file` / `risk_for_file` for each partner individually,
/// avoiding bulk table loads. Absent entries sort last (score 0.0).
///
/// # Errors
///
/// Returns an error if any per-file DB query fails.
fn resort_partners_by_temporal(
    partners: &mut Vec<rskim_search::CochangeRow>,
    sort_mode: TemporalSort,
    normalized: &str,
    db: &TemporalDb,
) -> anyhow::Result<()> {
    // Compute scores eagerly into a parallel Vec — one entry per partner.
    // Scores are keyed by position so we can sort an index Vec without
    // touching `partners` until the final permutation step.
    let scores: Vec<f64> = partners
        .iter()
        .map(|row| -> anyhow::Result<f64> {
            let partner = cochange_partner(row, normalized);
            match sort_mode {
                TemporalSort::Hot | TemporalSort::Cold => Ok(db
                    .hotspot_for_file(partner)?
                    .map(|h| h.score)
                    .unwrap_or(0.0)),
                TemporalSort::Risky => Ok(db
                    .risk_for_file(partner)?
                    .map(|r| r.risk_score)
                    .unwrap_or(0.0)),
            }
        })
        .collect::<anyhow::Result<_>>()?;

    // Sort an index Vec by score, then apply the permutation to `partners`.
    let mut indices: Vec<usize> = (0..partners.len()).collect();
    if sort_mode == TemporalSort::Cold {
        indices.sort_by(|&a, &b| {
            scores[a]
                .partial_cmp(&scores[b])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        indices.sort_by(|&a, &b| {
            scores[b]
                .partial_cmp(&scores[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // Apply permutation: collect in sorted order, then replace `partners`.
    *partners = indices.into_iter().map(|i| partners[i].clone()).collect();
    Ok(())
}

// ============================================================================
// Output formatters
// ============================================================================

/// Format a standalone temporal query result as human-readable text.
pub(super) fn format_temporal_text(
    output: &TemporalQueryOutput,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    match output {
        TemporalQueryOutput::Hotspots(rows) => {
            if rows.is_empty() {
                writeln!(w, "No hotspot data available.")?;
                return Ok(());
            }
            // Single newline after header (writeln! already appends \n; no
            // extra \n in the format string — that would insert a blank line).
            writeln!(w, "Hotspots (top {}, 90-day decay):", rows.len())?;
            writeln!(w, "  Score  30d  90d  Path")?;
            writeln!(w, "  ─────  ───  ───  ────────────────────────────────")?;
            for r in rows {
                writeln!(
                    w,
                    "  {:.3}   {:>4} {:>4}  {}",
                    r.score, r.changes_30d, r.changes_90d, r.file_path
                )?;
            }
        }
        TemporalQueryOutput::Coldspots(rows) => {
            if rows.is_empty() {
                writeln!(w, "No coldspot data available.")?;
                return Ok(());
            }
            writeln!(w, "Coldspots (top {}, least active):", rows.len())?;
            writeln!(w, "  Score  30d  90d  Path")?;
            writeln!(w, "  ─────  ───  ───  ────────────────────────────────")?;
            for r in rows {
                writeln!(
                    w,
                    "  {:.3}   {:>4} {:>4}  {}",
                    r.score, r.changes_30d, r.changes_90d, r.file_path
                )?;
            }
        }
        TemporalQueryOutput::Risks(rows) => {
            if rows.is_empty() {
                writeln!(w, "No risk data available.")?;
                return Ok(());
            }
            writeln!(w, "Risk hotspots (top {}):\n", rows.len())?;
            writeln!(w, "  Risk   Fix%   Fixes  Total  Path")?;
            writeln!(
                w,
                "  ─────  ─────  ─────  ─────  ────────────────────────────────"
            )?;
            for r in rows {
                writeln!(
                    w,
                    "  {:.3}  {:>5.1}%  {:>5}  {:>5}  {}",
                    r.risk_score,
                    r.fix_density * 100.0,
                    r.fix_commits,
                    r.total_commits,
                    r.file_path
                )?;
            }
        }
        TemporalQueryOutput::Cochanges { target, partners } => {
            if partners.is_empty() {
                writeln!(w, "No co-change data for {target:?}.")?;
                return Ok(());
            }
            writeln!(
                w,
                "Co-change partners of {} ({} files):\n",
                target,
                partners.len()
            )?;
            writeln!(w, "  Jaccard  Count  Path")?;
            writeln!(w, "  ───────  ─────  ────────────────────────────────")?;
            for p in partners {
                let partner = cochange_partner(p, target);
                writeln!(w, "  {:.3}    {:>5}  {}", p.jaccard, p.count, partner)?;
            }
        }
    }
    Ok(())
}

// ============================================================================
// JSON serialization types
// ============================================================================

/// A single hotspot/coldspot entry in standalone JSON output.
#[derive(Serialize)]
struct HotspotJsonRow<'a> {
    path: &'a str,
    hotspot_score: f64,
    changes_30d: u32,
    changes_90d: u32,
}

/// A single risk entry in standalone JSON output.
#[derive(Serialize)]
struct RiskJsonRow<'a> {
    path: &'a str,
    risk_score: f64,
    fix_density: f64,
    fix_commits: u32,
    total_commits: u32,
}

/// A single co-change partner entry in standalone JSON output.
#[derive(Serialize)]
struct CochangeJsonRow<'a> {
    path: &'a str,
    jaccard: f64,
    count: u32,
}

/// Top-level envelope for hotspot/coldspot standalone JSON.
#[derive(Serialize)]
struct HotColdJson<'a> {
    mode: &'a str,
    total: usize,
    results: Vec<HotspotJsonRow<'a>>,
}

/// Top-level envelope for risk standalone JSON.
#[derive(Serialize)]
struct RiskyJson<'a> {
    mode: &'a str,
    total: usize,
    results: Vec<RiskJsonRow<'a>>,
}

/// Top-level envelope for blast-radius standalone JSON.
#[derive(Serialize)]
struct BlastRadiusJson<'a> {
    mode: &'a str,
    target: &'a str,
    total: usize,
    results: Vec<CochangeJsonRow<'a>>,
}

/// Serialize a hotspot/coldspot row slice to JSON and write it.
fn write_hotcold_json(mode: &str, rows: &[HotspotRow], w: &mut impl Write) -> anyhow::Result<()> {
    let envelope = HotColdJson {
        mode,
        total: rows.len(),
        results: rows
            .iter()
            .map(|r| HotspotJsonRow {
                path: &r.file_path,
                hotspot_score: r.score,
                changes_30d: r.changes_30d,
                changes_90d: r.changes_90d,
            })
            .collect(),
    };
    writeln!(w, "{}", serde_json::to_string_pretty(&envelope)?)?;
    Ok(())
}

/// Format a standalone temporal query result as JSON.
///
/// Uses `#[derive(Serialize)]` typed structs so field names are defined in one
/// place, preventing the hand-built `serde_json::json!()` approach from drifting
/// independently.
pub(super) fn format_temporal_json(
    output: &TemporalQueryOutput,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    match output {
        TemporalQueryOutput::Hotspots(rows) => write_hotcold_json("hot", rows, w)?,
        TemporalQueryOutput::Coldspots(rows) => write_hotcold_json("cold", rows, w)?,
        TemporalQueryOutput::Risks(rows) => {
            let envelope = RiskyJson {
                mode: "risky",
                total: rows.len(),
                results: rows
                    .iter()
                    .map(|r| RiskJsonRow {
                        path: &r.file_path,
                        risk_score: r.risk_score,
                        fix_density: r.fix_density,
                        fix_commits: r.fix_commits,
                        total_commits: r.total_commits,
                    })
                    .collect(),
            };
            writeln!(w, "{}", serde_json::to_string_pretty(&envelope)?)?;
        }
        TemporalQueryOutput::Cochanges { target, partners } => {
            let envelope = BlastRadiusJson {
                mode: "blast-radius",
                target,
                total: partners.len(),
                results: partners
                    .iter()
                    .map(|p| CochangeJsonRow {
                        path: cochange_partner(p, target),
                        jaccard: p.jaccard,
                        count: p.count,
                    })
                    .collect(),
            };
            writeln!(w, "{}", serde_json::to_string_pretty(&envelope)?)?;
        }
    }
    Ok(())
}

// ============================================================================
// Combined text+temporal enrichment (Step 10)
// ============================================================================

/// Annotate and re-sort text search results with temporal data.
///
/// - For `Hot`: annotate with hotspot scores, sort descending. Files absent
///   from temporal DB sort last (by path for determinism).
/// - For `Cold`: annotate with hotspot scores, sort ascending. Files absent
///   sort first (score 0.0).
/// - For `Risky`: annotate with risk scores, sort descending. Files absent
///   sort last.
///
/// Uses per-file lookups (`hotspot_for_file` / `risk_for_file`) to avoid
/// bulk table loads when annotating a small result set.
///
/// Graceful degradation: if a per-file DB query fails, the result is left
/// unannotated and a warning is emitted; other results are still annotated.
pub(super) fn apply_temporal_enrichment(
    results: &mut [ResolvedResult],
    sort: TemporalSort,
    db: &TemporalDb,
) -> anyhow::Result<()> {
    match sort {
        TemporalSort::Hot | TemporalSort::Cold => {
            annotate_hotspots(results, db);
            let hotspot_score = |r: &ResolvedResult| {
                r.temporal
                    .as_ref()
                    .and_then(|t| t.hotspot_score)
                    .unwrap_or(-1.0)
            };
            results.sort_by(|a, b| {
                let cmp = hotspot_score(a)
                    .partial_cmp(&hotspot_score(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.path.cmp(&b.path));
                if sort == TemporalSort::Hot {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
        }
        TemporalSort::Risky => {
            annotate_risks(results, db);
            let risk_score = |r: &ResolvedResult| {
                r.temporal
                    .as_ref()
                    .and_then(|t| t.risk_score)
                    .unwrap_or(-1.0)
            };
            results.sort_by(|a, b| {
                risk_score(b)
                    .partial_cmp(&risk_score(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.path.cmp(&b.path))
            });
        }
    }
    Ok(())
}

/// Annotate results with hotspot data using per-file lookups.
///
/// Performs one DB query per result (O(N)). The default `--limit` of 20 keeps
/// this negligible. At `--limit 1000` this becomes 1000 queries — acceptable
/// for an interactive CLI but not for batch workloads.
///
/// On lookup failure, emits a warning and leaves that result unannotated.
fn annotate_hotspots(results: &mut [ResolvedResult], db: &TemporalDb) {
    for result in results.iter_mut() {
        match db.hotspot_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(TemporalAnnotation {
                    hotspot_score: Some(row.score),
                    changes_30d: Some(row.changes_30d),
                    changes_90d: Some(row.changes_90d),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
            }
        }
    }
}

/// Annotate results with risk data using per-file lookups.
///
/// Performs one DB query per result (O(N)). See [`annotate_hotspots`] for the
/// complexity note.
///
/// On lookup failure, emits a warning and leaves that result unannotated.
fn annotate_risks(results: &mut [ResolvedResult], db: &TemporalDb) {
    for result in results.iter_mut() {
        match db.risk_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(TemporalAnnotation {
                    risk_score: Some(row.risk_score),
                    fix_density: Some(row.fix_density),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
            }
        }
    }
}

// ============================================================================
// Standalone-AST temporal enrichment (full-CLI integration)
// ============================================================================

/// Annotate and re-sort standalone `--ast` results with temporal data.
///
/// The AST analogue of [`apply_temporal_enrichment`]: it applies the **identical**
/// ordering contract — absent files sort last (score sentinel `-1.0`) and equal
/// temporal scores tie-break by `path.cmp` — so the two query paths expose one
/// observable sort behaviour (design decision 4 / AC-A2).
///
/// It operates on [`rskim_search::AstResult`] and writes the library-side
/// [`rskim_search::TemporalAnnotation`].  The small mirror (rather than a shared
/// generic) is deliberate: the two row types carry different annotation structs,
/// and a trait abstraction would add more indirection than the duplication saves.
///
/// Callers MUST pre-truncate `results` to the bounded re-sort window
/// ([`resort_window`]) before calling so per-file DB lookups stay bounded (AC-P1).
pub(super) fn enrich_ast_results(
    results: &mut [rskim_search::AstResult],
    sort: TemporalSort,
    db: &TemporalDb,
) {
    match sort {
        TemporalSort::Hot | TemporalSort::Cold => {
            annotate_ast_hotspots(results, db);
            let hotspot_score = |r: &rskim_search::AstResult| {
                r.temporal
                    .as_ref()
                    .and_then(|t| t.hotspot_score)
                    .unwrap_or(-1.0)
            };
            results.sort_by(|a, b| {
                let cmp = hotspot_score(a)
                    .partial_cmp(&hotspot_score(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.path.cmp(&b.path));
                if sort == TemporalSort::Hot {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
        }
        TemporalSort::Risky => {
            annotate_ast_risks(results, db);
            let risk_score = |r: &rskim_search::AstResult| {
                r.temporal
                    .as_ref()
                    .and_then(|t| t.risk_score)
                    .unwrap_or(-1.0)
            };
            results.sort_by(|a, b| {
                risk_score(b)
                    .partial_cmp(&risk_score(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.path.cmp(&b.path))
            });
        }
    }
}

/// Annotate `AstResult`s with hotspot data via per-file lookups (one DB query
/// per result). On lookup failure, emits a warning and leaves the row unannotated.
fn annotate_ast_hotspots(results: &mut [rskim_search::AstResult], db: &TemporalDb) {
    for result in results.iter_mut() {
        match db.hotspot_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(rskim_search::TemporalAnnotation {
                    hotspot_score: Some(row.score),
                    changes_30d: Some(row.changes_30d),
                    changes_90d: Some(row.changes_90d),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
            }
        }
    }
}

/// Annotate `AstResult`s with risk data via per-file lookups (one DB query per
/// result). On lookup failure, emits a warning and leaves the row unannotated.
fn annotate_ast_risks(results: &mut [rskim_search::AstResult], db: &TemporalDb) {
    for result in results.iter_mut() {
        match db.risk_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(rskim_search::TemporalAnnotation {
                    risk_score: Some(row.risk_score),
                    fix_density: Some(row.fix_density),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
            }
        }
    }
}

// ============================================================================
// Tests (co-located)
// ============================================================================

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;
