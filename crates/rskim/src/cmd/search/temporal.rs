//! Temporal query helpers for `skim search` temporal flags.
//!
//! # Responsibilities
//!
//! - Path normalization for `--blast-radius` (cross-platform, repo-relative).
//! - `TemporalDb` open/check helpers.
//! - Standalone temporal dispatch (`--hot`, `--cold`, `--risky`, `--blast-radius`).
//! - Combined text+temporal enrichment (`apply_temporal_enrichment`).
//! - Output formatting for standalone temporal queries.

use std::io::Write;
use std::path::Path;

use rskim_search::{HotspotRow, RiskRow, TemporalDb};

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

    // Resolve to an absolute path.
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        // Prefer project-root-relative resolution so that `src/foo.rs` works
        // regardless of the user's CWD within the repo.
        let root_relative = project_root.join(p);
        if root_relative.exists() {
            root_relative
        } else {
            // Fallback: CWD-relative (e.g. user is in a subdirectory).
            std::env::current_dir()?.join(p)
        }
    };

    // Canonicalize — resolves `..` and symlinks.
    let canonical = abs.canonicalize().unwrap_or_else(|_| abs.clone());

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

/// Check whether the temporal database is stale compared to the current git HEAD.
///
/// Returns `Some(warning_message)` when the stored HEAD differs from the
/// current HEAD, `None` when current or when the staleness check cannot be
/// performed (missing git, non-git repo, missing meta key).
pub(super) fn check_temporal_staleness(db: &TemporalDb, project_root: &Path) -> Option<String> {
    let stored_head = db.get_meta(rskim_search::META_GIT_HEAD).ok().flatten()?;

    let current_head = read_git_head(project_root)?;
    if stored_head.trim() != current_head.trim() {
        Some(format!(
            "skim search: temporal data is stale (stored: {}, current: {}). \
             Run 'skim heatmap' to refresh.",
            &stored_head[..stored_head.len().min(7)],
            &current_head[..current_head.len().min(7)],
        ))
    } else {
        None
    }
}

/// Read the current git HEAD SHA from the project root.
fn read_git_head(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
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

        // Re-sort by temporal metric when a sort mode is specified.
        if let Some(sort_mode) = sort {
            match sort_mode {
                TemporalSort::Hot | TemporalSort::Cold => {
                    // Load hotspots to get scores for the partners.
                    let hotspots = db.load_hotspots()?;
                    let hotspot_map: std::collections::HashMap<&str, f64> = hotspots
                        .iter()
                        .map(|h| (h.file_path.as_str(), h.score))
                        .collect();
                    partners.sort_by(|a, b| {
                        let partner_a = if a.file_a == normalized {
                            &a.file_b
                        } else {
                            &a.file_a
                        };
                        let partner_b = if b.file_a == normalized {
                            &b.file_b
                        } else {
                            &b.file_a
                        };
                        let score_a = hotspot_map.get(partner_a.as_str()).copied().unwrap_or(0.0);
                        let score_b = hotspot_map.get(partner_b.as_str()).copied().unwrap_or(0.0);
                        if sort_mode == TemporalSort::Cold {
                            score_a
                                .partial_cmp(&score_b)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        } else {
                            score_b
                                .partial_cmp(&score_a)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        }
                    });
                }
                TemporalSort::Risky => {
                    let risks = db.load_risks()?;
                    let risk_map: std::collections::HashMap<&str, f64> = risks
                        .iter()
                        .map(|r| (r.file_path.as_str(), r.risk_score))
                        .collect();
                    partners.sort_by(|a, b| {
                        let partner_a = if a.file_a == normalized {
                            &a.file_b
                        } else {
                            &a.file_a
                        };
                        let partner_b = if b.file_a == normalized {
                            &b.file_b
                        } else {
                            &b.file_a
                        };
                        let risk_a = risk_map.get(partner_a.as_str()).copied().unwrap_or(0.0);
                        let risk_b = risk_map.get(partner_b.as_str()).copied().unwrap_or(0.0);
                        risk_b
                            .partial_cmp(&risk_a)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
            }
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
            let rows = db.top_hotspots(limit)?;
            Ok(TemporalQueryOutput::Hotspots(rows))
        }
        Some(TemporalSort::Cold) => {
            let rows = db.top_coldspots(limit)?;
            Ok(TemporalQueryOutput::Coldspots(rows))
        }
        Some(TemporalSort::Risky) => {
            let rows = db.top_risks(limit)?;
            Ok(TemporalQueryOutput::Risks(rows))
        }
    }
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
            writeln!(w, "Hotspots (top {}, 90-day decay):\n", rows.len())?;
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
            writeln!(w, "Coldspots (top {}, least active):\n", rows.len())?;
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
                let partner = if p.file_a == *target {
                    &p.file_b
                } else {
                    &p.file_a
                };
                writeln!(w, "  {:.3}    {:>5}  {}", p.jaccard, p.count, partner)?;
            }
        }
    }
    Ok(())
}

/// Format a standalone temporal query result as JSON.
pub(super) fn format_temporal_json(
    output: &TemporalQueryOutput,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    let json = match output {
        TemporalQueryOutput::Hotspots(rows) => {
            let results: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "path": r.file_path,
                        "hotspot_score": r.score,
                        "changes_30d": r.changes_30d,
                        "changes_90d": r.changes_90d,
                    })
                })
                .collect();
            serde_json::json!({
                "mode": "hot",
                "limit": rows.len(),
                "results": results,
            })
        }
        TemporalQueryOutput::Coldspots(rows) => {
            let results: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "path": r.file_path,
                        "hotspot_score": r.score,
                        "changes_30d": r.changes_30d,
                        "changes_90d": r.changes_90d,
                    })
                })
                .collect();
            serde_json::json!({
                "mode": "cold",
                "limit": rows.len(),
                "results": results,
            })
        }
        TemporalQueryOutput::Risks(rows) => {
            let results: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "path": r.file_path,
                        "risk_score": r.risk_score,
                        "fix_density": r.fix_density,
                        "fix_commits": r.fix_commits,
                        "total_commits": r.total_commits,
                    })
                })
                .collect();
            serde_json::json!({
                "mode": "risky",
                "limit": rows.len(),
                "results": results,
            })
        }
        TemporalQueryOutput::Cochanges { target, partners } => {
            let results: Vec<serde_json::Value> = partners
                .iter()
                .map(|p| {
                    let partner = if p.file_a == *target {
                        &p.file_b
                    } else {
                        &p.file_a
                    };
                    serde_json::json!({
                        "path": partner,
                        "jaccard": p.jaccard,
                        "count": p.count,
                    })
                })
                .collect();
            serde_json::json!({
                "mode": "blast_radius",
                "target": target,
                "limit": partners.len(),
                "results": results,
            })
        }
    };
    writeln!(w, "{}", serde_json::to_string_pretty(&json)?)?;
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
/// Graceful degradation: if the DB query fails, log a debug warning and
/// return without modifying the results.
pub(super) fn apply_temporal_enrichment(
    results: &mut [ResolvedResult],
    sort: TemporalSort,
    db: &TemporalDb,
) -> anyhow::Result<()> {
    match sort {
        TemporalSort::Hot | TemporalSort::Cold => {
            let hotspots = match db.load_hotspots() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("skim search: temporal enrichment warning: {e}");
                    return Ok(());
                }
            };
            let hotspot_map: std::collections::HashMap<&str, &HotspotRow> =
                hotspots.iter().map(|h| (h.file_path.as_str(), h)).collect();

            for result in results.iter_mut() {
                if let Some(row) = hotspot_map.get(result.path.as_str()) {
                    result.temporal = Some(TemporalAnnotation {
                        hotspot_score: Some(row.score),
                        changes_30d: Some(row.changes_30d),
                        changes_90d: Some(row.changes_90d),
                        ..Default::default()
                    });
                }
            }

            if sort == TemporalSort::Hot {
                // Sort descending: annotated files first (by score desc), then unannotated by path.
                results.sort_by(|a, b| {
                    let score_a = a
                        .temporal
                        .as_ref()
                        .and_then(|t| t.hotspot_score)
                        .unwrap_or(-1.0);
                    let score_b = b
                        .temporal
                        .as_ref()
                        .and_then(|t| t.hotspot_score)
                        .unwrap_or(-1.0);
                    score_b
                        .partial_cmp(&score_a)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.path.cmp(&b.path))
                });
            } else {
                // Cold: ascending, unannotated files first (score -1.0 sorts before 0.0).
                results.sort_by(|a, b| {
                    let score_a = a
                        .temporal
                        .as_ref()
                        .and_then(|t| t.hotspot_score)
                        .unwrap_or(-1.0);
                    let score_b = b
                        .temporal
                        .as_ref()
                        .and_then(|t| t.hotspot_score)
                        .unwrap_or(-1.0);
                    score_a
                        .partial_cmp(&score_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.path.cmp(&b.path))
                });
            }
        }
        TemporalSort::Risky => {
            let risks = match db.load_risks() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("skim search: temporal enrichment warning: {e}");
                    return Ok(());
                }
            };
            let risk_map: std::collections::HashMap<&str, &RiskRow> =
                risks.iter().map(|r| (r.file_path.as_str(), r)).collect();

            for result in results.iter_mut() {
                if let Some(row) = risk_map.get(result.path.as_str()) {
                    result.temporal = Some(TemporalAnnotation {
                        risk_score: Some(row.risk_score),
                        fix_density: Some(row.fix_density),
                        ..Default::default()
                    });
                }
            }

            // Sort descending: annotated files first (by risk_score desc), then unannotated by path.
            results.sort_by(|a, b| {
                let risk_a = a
                    .temporal
                    .as_ref()
                    .and_then(|t| t.risk_score)
                    .unwrap_or(-1.0);
                let risk_b = b
                    .temporal
                    .as_ref()
                    .and_then(|t| t.risk_score)
                    .unwrap_or(-1.0);
                risk_b
                    .partial_cmp(&risk_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.path.cmp(&b.path))
            });
        }
    }
    Ok(())
}

// ============================================================================
// Tests (co-located)
// ============================================================================

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;
