//! `skim search` — code search across indexed files (#3)
//!
//! Provides intelligent code search using the 3-layer search architecture
//! defined in rskim-search. Uses BM25F lexical indexing with AST field boosting,
//! with optional temporal signal overlay (hot/cold/risky) and co-change
//! blast-radius queries.
//!
//! # Module layout
//!
//! - [`index`] — Repo root discovery, cache directory management, index build
//! - [`output`] — Text and JSON result formatting, stats display

mod index;
mod output;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rskim_search::{
    lexical::query::LexicalSearchLayer,
    temporal::{TemporalDb, TemporalIndex, DEFAULT_LOOKBACK_DAYS},
    FileId, SearchIndex, SearchLayer, SearchQuery, TemporalFlags, TemporalQuery,
};

/// Run the search subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Delegate help to clap (single source of truth).
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Parse args via clap to eliminate the hand-rolled flag scanner.
    // We include the program name as argv[0] so get_matches_from works correctly.
    let mut argv = vec!["skim search".to_string()];
    argv.extend_from_slice(args);

    let matches = command().get_matches_from(&argv);

    let json_output = matches.get_flag("json");
    let build_flag = matches.get_flag("build");
    let rebuild_flag = matches.get_flag("rebuild");
    let stats_flag = matches.get_flag("stats");
    let clear_cache_flag = matches.get_flag("clear_cache");
    let build_temporal_flag = matches.get_flag("build_temporal");
    let limit: usize = matches
        .get_one::<String>("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let lookback: u32 = matches
        .get_one::<String>("lookback")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_LOOKBACK_DAYS);
    let query_text = matches.get_one::<String>("query").map(|s| s.as_str());

    let blast_radius_arg = matches.get_one::<String>("blast_radius").cloned();
    let hot = matches.get_flag("hot");
    let cold = matches.get_flag("cold");
    let risky = matches.get_flag("risky");

    // Reject --update and --ast (not yet implemented).
    if matches.get_flag("update") {
        eprintln!("error: --update is not yet implemented");
        eprintln!("hint: use --rebuild to recreate the full index");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_one::<String>("ast").is_some() {
        eprintln!("error: --ast is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }

    // Resolve repo root and per-repo index directory.
    let repo_root = index::find_repo_root()?;
    let index_dir = index::get_index_dir(&repo_root)?;

    // Shorthand predicates used throughout the orchestration logic.
    let is_blast_radius = blast_radius_arg.is_some();
    let has_temporal_scoring_flag = hot || cold || risky;
    let has_text = query_text.is_some_and(|q| !q.is_empty());

    // --clear-cache: delete all search indexes and exit.
    if clear_cache_flag {
        index::clear_search_cache()?;
        eprintln!("Search cache cleared.");
        return Ok(ExitCode::SUCCESS);
    }

    // Validate mutually exclusive flag combinations before doing any I/O.
    if is_blast_radius && has_text {
        eprintln!(
            "error: --blast-radius is a standalone query mode and cannot be combined with text search"
        );
        return Ok(ExitCode::FAILURE);
    }
    if is_blast_radius && has_temporal_scoring_flag {
        eprintln!("error: --blast-radius cannot be combined with --hot/--cold/--risky");
        return Ok(ExitCode::FAILURE);
    }

    // Resolve --hot --cold conflict: warn and use the last one in argv.
    let (hot, cold) = if hot && cold {
        eprintln!("warning: --hot and --cold are mutually exclusive, using the last one");
        let mut last = "hot";
        for a in argv.iter().rev() {
            if a == "--hot" {
                last = "hot";
                break;
            }
            if a == "--cold" {
                last = "cold";
                break;
            }
        }
        (last == "hot", last == "cold")
    } else {
        (hot, cold)
    };

    // --stats: show index statistics (with optional temporal section).
    if stats_flag {
        if !index_dir.join("metadata.json").exists() {
            eprintln!("No search index found. Run 'skim search --build' first.");
            return Ok(ExitCode::FAILURE);
        }
        let layer = match LexicalSearchLayer::open(&index_dir) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("error: failed to open search index: {e}");
                return Ok(ExitCode::FAILURE);
            }
        };
        let temporal_opt = if temporal_db_path(&index_dir).exists() {
            TemporalIndex::open(&temporal_db_path(&index_dir)).ok()
        } else {
            None
        };
        return output::show_stats(&layer, temporal_opt.as_ref(), json_output);
    }

    // --build / --rebuild / --build-temporal: build index(es).
    if build_flag || rebuild_flag || build_temporal_flag {
        if rebuild_flag {
            if let Err(e) = std::fs::remove_dir_all(&index_dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("warning: could not remove old index: {e}");
                }
            }
        }
        // --build and --rebuild build the lexical layer.
        // --build-temporal alone only builds temporal (skip lexical).
        if build_flag || rebuild_flag {
            index::build_index(&repo_root, &index_dir)?;
        }
        // All three flags trigger a temporal build.
        build_temporal_layer(&repo_root, &index_dir, lookback)?;

        // If no query/temporal query was also requested, we're done after building.
        if !has_text && !is_blast_radius && !has_temporal_scoring_flag {
            return Ok(ExitCode::SUCCESS);
        }
    }

    // ── STANDALONE TEMPORAL QUERIES ──────────────────────────────────────────

    if is_blast_radius {
        let db_path = temporal_db_path(&index_dir);
        ensure_temporal_built(&repo_root, &index_dir, &db_path, lookback)?;
        let temporal = TemporalIndex::open(&db_path)?;

        let arg = blast_radius_arg.as_deref().unwrap_or("");
        let target = resolve_blast_target(arg, &repo_root)?;
        let results = temporal.blast_radius(&target, limit)?;

        if results.is_empty() {
            if json_output {
                println!("[]");
            } else {
                eprintln!("No co-change partners found for {}", target.display());
            }
            return Ok(ExitCode::SUCCESS);
        }
        output::print_temporal_results(&results, &repo_root, json_output)?;
        return Ok(ExitCode::SUCCESS);
    }

    if has_temporal_scoring_flag && !has_text {
        // Standalone temporal: hot / cold / risky without text.
        let db_path = temporal_db_path(&index_dir);
        ensure_temporal_built(&repo_root, &index_dir, &db_path, lookback)?;
        let temporal = TemporalIndex::open(&db_path)?;

        let results = if (hot as u8) + (cold as u8) + (risky as u8) > 1 {
            // Multiple signals: fetch candidates from the primary signal and rerank.
            let candidates = if hot {
                temporal.hotspots(limit * 5)?
            } else if cold {
                temporal.coldspots(limit * 5)?
            } else {
                temporal.risky(limit * 5)?
            };
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
        output::print_temporal_results(&results, &repo_root, json_output)?;
        return Ok(ExitCode::SUCCESS);
    }

    // ── LEXICAL / COMPOSITE QUERIES ──────────────────────────────────────────

    // Require a non-empty query to proceed with text search.
    let query_str = match query_text {
        Some(q) if !q.is_empty() => q,
        _ => {
            if !build_flag && !rebuild_flag && !build_temporal_flag {
                eprintln!("Usage: skim search <query>");
                eprintln!("       skim search --build");
                eprintln!("Run 'skim search --help' for more options.");
            }
            return Ok(ExitCode::FAILURE);
        }
    };

    // Auto-build lexical index if missing.
    if !index_dir.join("metadata.json").exists() {
        eprintln!("Building search index...");
        index::build_index(&repo_root, &index_dir)?;
    }

    let layer = match LexicalSearchLayer::open(&index_dir) {
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
        let db_path = temporal_db_path(&index_dir);
        ensure_temporal_built(&repo_root, &index_dir, &db_path, lookback)?;
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
        output::print_temporal_results(&reranked, &repo_root, json_output)?;
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
        output::print_json_results(&layer, &lex_results, query_str, &repo_root)?;
    } else {
        output::print_text_results(&layer, &lex_results, query_str, &repo_root)?;
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Helper functions
// ============================================================================

/// Return the path to the temporal DB file within the per-repo index dir.
fn temporal_db_path(index_dir: &Path) -> PathBuf {
    index_dir.join("temporal.db")
}

/// Build the temporal index at `db_path` for `repo_root` with the given lookback.
///
/// Prints progress messages to stderr.
fn build_temporal_layer(repo_root: &Path, index_dir: &Path, lookback: u32) -> anyhow::Result<()> {
    std::fs::create_dir_all(index_dir)?;
    let db_path = temporal_db_path(index_dir);
    eprintln!("Building temporal index, this may take a while...");
    let _db = TemporalDb::build(repo_root, &db_path, lookback)?;
    eprintln!("Temporal index built.");
    Ok(())
}

/// Auto-build temporal index if it does not yet exist at `db_path`.
fn ensure_temporal_built(
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

// ============================================================================
// Clap command definition (used for shell completions and arg parsing)
// ============================================================================

/// Build clap command definition for shell completions and `run()` arg parsing.
pub(super) fn command() -> clap::Command {
    clap::Command::new("search")
        .about("Search code using the 3-layer index")
        .arg(
            clap::Arg::new("query")
                .help("Search query string")
                .value_name("QUERY"),
        )
        .arg(
            clap::Arg::new("build")
                .long("build")
                .action(clap::ArgAction::SetTrue)
                .help("Build the search index before querying"),
        )
        .arg(
            clap::Arg::new("rebuild")
                .long("rebuild")
                .action(clap::ArgAction::SetTrue)
                .help("Force rebuild the entire search index"),
        )
        .arg(
            clap::Arg::new("update")
                .long("update")
                .action(clap::ArgAction::SetTrue)
                .help("Update the search index incrementally"),
        )
        .arg(
            clap::Arg::new("stats")
                .long("stats")
                .action(clap::ArgAction::SetTrue)
                .help("Show index statistics"),
        )
        .arg(
            clap::Arg::new("clear_cache")
                .long("clear-cache")
                .action(clap::ArgAction::SetTrue)
                .help("Delete all search indexes"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Output results as JSON"),
        )
        .arg(
            clap::Arg::new("ast")
                .long("ast")
                .value_name("PATTERN")
                .help("AST pattern to search for"),
        )
        .arg(
            clap::Arg::new("blast_radius")
                .long("blast-radius")
                .value_name("FILE")
                .help("Show files that historically co-change with FILE (standalone query)"),
        )
        .arg(
            clap::Arg::new("build_temporal")
                .long("build-temporal")
                .action(clap::ArgAction::SetTrue)
                .help("Build only the temporal index"),
        )
        .arg(
            clap::Arg::new("lookback")
                .long("lookback")
                .value_name("DAYS")
                .help("Temporal lookback window in days (default: 365)"),
        )
        .arg(
            clap::Arg::new("limit")
                .long("limit")
                .value_name("N")
                .help("Maximum number of results to return"),
        )
        .arg(
            clap::Arg::new("hot")
                .long("hot")
                .action(clap::ArgAction::SetTrue)
                .help("Filter for recently active files"),
        )
        .arg(
            clap::Arg::new("cold")
                .long("cold")
                .action(clap::ArgAction::SetTrue)
                .help("Filter for stable/unchanged files"),
        )
        .arg(
            clap::Arg::new("risky")
                .long("risky")
                .action(clap::ArgAction::SetTrue)
                .help("Filter for files with high churn or complexity"),
        )
}

fn print_help() {
    // Delegate to clap's command definition — single source of truth for flags.
    let _ = command().name("skim search").print_help();
    println!();
}
