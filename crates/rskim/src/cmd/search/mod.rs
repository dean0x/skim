//! `skim search` — code search across indexed files (#3)
//!
//! Provides intelligent code search using the 3-layer search architecture
//! defined in rskim-search. Uses BM25F lexical indexing with AST field boosting,
//! with optional temporal signal overlay (hot/cold/risky) and co-change
//! blast-radius queries.
//!
//! # Module layout
//!
//! - [`dispatch`] — three query dispatch paths (blast-radius, temporal, lexical+composite)
//!   plus helper functions (temporal_db_path, build_temporal_layer, ensure_temporal_built,
//!   resolve_blast_target, lexical_to_paths)
//! - [`index`] — repo root discovery, cache directory management, index build
//! - [`output`] — text and JSON result formatting, stats display

mod dispatch;
mod index;
mod output;

use std::process::ExitCode;

use rskim_search::temporal::{TemporalIndex, DEFAULT_LOOKBACK_DAYS};

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
        let layer = match rskim_search::lexical::query::LexicalSearchLayer::open(&index_dir) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("error: failed to open search index: {e}");
                return Ok(ExitCode::FAILURE);
            }
        };
        let temporal_opt = {
            let db_path = dispatch::temporal_db_path(&index_dir);
            if db_path.exists() {
                TemporalIndex::open(&db_path).ok()
            } else {
                None
            }
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
        // Temporal build: --build-temporal is explicit so it hard-fails when
        // there is no git repository. --build/--rebuild are "do the right thing"
        // entry points: skip temporal with a warning when not in a git repo.
        if build_temporal_flag || index::is_repo(&repo_root) {
            dispatch::build_temporal_layer(&repo_root, &index_dir, lookback)?;
        } else {
            eprintln!("warning: skipping temporal index build: not a git repository");
        }

        // If no query/temporal query was also requested, we're done after building.
        if !has_text && !is_blast_radius && !has_temporal_scoring_flag {
            return Ok(ExitCode::SUCCESS);
        }
    }

    let temporal_params = dispatch::TemporalParams {
        hot,
        cold,
        risky,
        limit,
        lookback,
        json_output,
    };

    // ── STANDALONE BLAST-RADIUS QUERY ────────────────────────────────────────
    if is_blast_radius {
        let arg = blast_radius_arg.as_deref().unwrap_or("");
        return dispatch::run_standalone_blast_radius(
            arg,
            &index_dir,
            &repo_root,
            &temporal_params,
        );
    }

    // ── STANDALONE TEMPORAL QUERY ─────────────────────────────────────────────
    if has_temporal_scoring_flag && !has_text {
        return dispatch::run_standalone_temporal(&index_dir, &repo_root, &temporal_params);
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

    dispatch::run_lexical_or_composite(query_str, &index_dir, &repo_root, &temporal_params)
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
