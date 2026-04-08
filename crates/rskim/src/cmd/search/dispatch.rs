//! Query dispatch helpers for `skim search`.
//!
//! Contains the three top-level query dispatch paths extracted from `mod.rs` to
//! keep that file under the 400-line limit, plus the small helper functions they
//! depend on.
//!
//! - [`run_standalone_blast_radius`] — `--blast-radius FILE` (no text)
//! - [`run_standalone_temporal`] — `--hot` / `--cold` / `--risky` (no text)
//! - [`run_lexical_or_composite`] — text search with optional temporal rerank

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rskim_search::{
    lexical::query::LexicalSearchLayer,
    temporal::{TemporalDb, TemporalIndex},
    FileId, SearchIndex, SearchLayer, SearchQuery, TemporalFlags, TemporalQuery,
};

use super::index;
use super::output;

// ============================================================================
// Parameter bundles
// ============================================================================

/// Temporal signal flags and query settings, passed as a bundle to avoid
/// triggering `clippy::too_many_arguments`.
pub(super) struct TemporalParams {
    pub hot: bool,
    pub cold: bool,
    pub risky: bool,
    pub limit: usize,
    pub lookback: u32,
    pub json_output: bool,
}

// ============================================================================
// Standalone blast-radius query
// ============================================================================

/// Handle `skim search --blast-radius FILE` (no text, no hot/cold/risky).
pub(super) fn run_standalone_blast_radius(
    blast_radius_arg: &str,
    index_dir: &Path,
    repo_root: &Path,
    params: &TemporalParams,
) -> anyhow::Result<ExitCode> {
    let db_path = temporal_db_path(index_dir);
    ensure_temporal_built(repo_root, index_dir, &db_path, params.lookback)?;
    let temporal = TemporalIndex::open(&db_path)?;

    let target = resolve_blast_target(blast_radius_arg, repo_root)?;
    let results = temporal.blast_radius(&target, params.limit)?;

    if results.is_empty() {
        if params.json_output {
            println!("[]");
        } else {
            eprintln!("No co-change partners found for {}", target.display());
        }
        return Ok(ExitCode::SUCCESS);
    }
    output::print_temporal_results(&results, repo_root, params.json_output)?;
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Standalone temporal query (--hot / --cold / --risky, no text)
// ============================================================================

/// Handle `skim search --hot / --cold / --risky` without a text query.
///
/// When multiple signals are active, unions candidates from all enabled
/// signals before reranking so that files relevant to any signal are
/// considered (previously only the primary signal's pool was used).
pub(super) fn run_standalone_temporal(
    index_dir: &Path,
    repo_root: &Path,
    params: &TemporalParams,
) -> anyhow::Result<ExitCode> {
    let TemporalParams {
        hot,
        cold,
        risky,
        limit,
        lookback,
        json_output,
    } = *params;
    let db_path = temporal_db_path(index_dir);
    ensure_temporal_built(repo_root, index_dir, &db_path, lookback)?;
    let temporal = TemporalIndex::open(&db_path)?;

    let results = if (hot as u8) + (cold as u8) + (risky as u8) > 1 {
        // Multiple signals: union candidates from every enabled signal so that
        // files relevant to any signal are in the working set before reranking.
        // Previously only the "primary" signal's pool was fetched, silently
        // dropping files that ranked low in that signal but high in others.
        let mut candidate_paths: BTreeSet<PathBuf> = BTreeSet::new();
        if hot {
            for (p, _) in temporal.hotspots(limit * 5)? {
                candidate_paths.insert(p);
            }
        }
        if cold {
            for (p, _) in temporal.coldspots(limit * 5)? {
                candidate_paths.insert(p);
            }
        }
        if risky {
            for (p, _) in temporal.risky(limit * 5)? {
                candidate_paths.insert(p);
            }
        }
        let candidates: Vec<(PathBuf, f32)> =
            candidate_paths.into_iter().map(|p| (p, 0.0)).collect();
        let flags = TemporalFlags {
            blast_radius: None,
            hot,
            cold,
            risky,
        };
        let mut reranked = temporal.rerank(&candidates, &flags)?;
        reranked.truncate(limit);
        reranked
    } else if hot {
        temporal.hotspots(limit)?
    } else if cold {
        temporal.coldspots(limit)?
    } else {
        temporal.risky(limit)?
    };

    if results.is_empty() {
        if json_output {
            println!("[]");
        }
        return Ok(ExitCode::SUCCESS);
    }
    output::print_temporal_results(&results, repo_root, json_output)?;
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Lexical or composite (lexical + temporal) query
// ============================================================================

/// Handle text queries, optionally overlaid with temporal reranking.
pub(super) fn run_lexical_or_composite(
    query_str: &str,
    index_dir: &Path,
    repo_root: &Path,
    params: &TemporalParams,
) -> anyhow::Result<ExitCode> {
    let TemporalParams {
        hot,
        cold,
        risky,
        limit,
        lookback,
        json_output,
    } = *params;
    let has_temporal_scoring_flag = hot || cold || risky;

    // Auto-build lexical index if missing.
    if !index_dir.join("metadata.json").exists() {
        eprintln!("Building search index...");
        index::build_index(repo_root, index_dir)?;
    }

    let layer = match LexicalSearchLayer::open(index_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to open search index: {e}");
            eprintln!("hint: run 'skim search --rebuild' to recreate the index");
            return Ok(ExitCode::FAILURE);
        }
    };

    // For composite queries, fetch more candidates so temporal rerank has a
    // sufficient working set.
    let internal_limit = if has_temporal_scoring_flag {
        (limit * 3).max(150)
    } else {
        limit
    };
    let search_query = SearchQuery::text(query_str).with_limit(internal_limit);
    let lex_results = layer.search(&search_query)?;

    if has_temporal_scoring_flag {
        // Composite: temporal rerank on top of lexical results.
        let db_path = temporal_db_path(index_dir);
        ensure_temporal_built(repo_root, index_dir, &db_path, lookback)?;
        let temporal = TemporalIndex::open(&db_path)?;

        let lex_paths = lexical_to_paths(&lex_results, &layer);
        let flags = TemporalFlags {
            blast_radius: None,
            hot,
            cold,
            risky,
        };
        let mut reranked = temporal.rerank(&lex_paths, &flags)?;
        reranked.truncate(limit);

        if reranked.is_empty() {
            if json_output {
                println!("[]");
            }
            return Ok(ExitCode::SUCCESS);
        }
        output::print_temporal_results(&reranked, repo_root, json_output)?;
        return Ok(ExitCode::SUCCESS);
    }

    // Pure lexical query.
    if lex_results.is_empty() {
        if json_output {
            println!("[]");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if json_output {
        output::print_json_results(&layer, &lex_results, query_str, repo_root)?;
    } else {
        output::print_text_results(&layer, &lex_results, query_str, repo_root)?;
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Shared helpers
// ============================================================================

/// Return the path to the temporal DB file within the per-repo index dir.
pub(super) fn temporal_db_path(index_dir: &Path) -> PathBuf {
    index_dir.join("temporal.db")
}

/// Build the temporal index at `db_path` for `repo_root` with the given lookback.
///
/// Prints progress messages to stderr.
pub(super) fn build_temporal_layer(
    repo_root: &Path,
    index_dir: &Path,
    lookback: u32,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(index_dir)?;
    let db_path = temporal_db_path(index_dir);
    eprintln!("Building temporal index, this may take a while...");
    let _db = TemporalDb::build(repo_root, &db_path, lookback)?;
    eprintln!("Temporal index built.");
    Ok(())
}

/// Auto-build temporal index if it does not yet exist at `db_path`.
pub(super) fn ensure_temporal_built(
    repo_root: &Path,
    index_dir: &Path,
    db_path: &Path,
    lookback: u32,
) -> anyhow::Result<()> {
    if !db_path.exists() {
        build_temporal_layer(repo_root, index_dir, lookback)?;
    }
    Ok(())
}

/// Resolve a user-provided blast-radius target to a canonical repo-relative path.
///
/// Accepts both cwd-relative and repo-relative paths. Returns the repo-relative
/// form in forward-slash format (matching temporal storage convention).
/// If neither resolves to an existing file, the argument is returned as-is so
/// that the temporal layer can return an empty result gracefully.
fn resolve_blast_target(arg: &str, repo_root: &Path) -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let abs_candidate = cwd.join(arg);
    if abs_candidate.exists() {
        let rel = abs_candidate
            .strip_prefix(repo_root)
            .unwrap_or(&abs_candidate);
        return Ok(PathBuf::from(rel.to_string_lossy().replace('\\', "/")));
    }
    let abs_from_root = repo_root.join(arg);
    if abs_from_root.exists() {
        return Ok(PathBuf::from(arg.replace('\\', "/")));
    }
    // Last resort: accept arg as-is; temporal returns empty if not found.
    Ok(PathBuf::from(arg.replace('\\', "/")))
}

/// Convert lexical `(FileId, score)` results to `(PathBuf, score)` for temporal rerank.
fn lexical_to_paths(results: &[(FileId, f32)], layer: &dyn SearchIndex) -> Vec<(PathBuf, f32)> {
    results
        .iter()
        .filter_map(|(id, s)| {
            layer
                .file_table()
                .lookup(*id)
                .map(|p| (p.to_path_buf(), *s))
        })
        .collect()
}
