//! `skim search` — code search across indexed files (#3)
//!
//! Provides intelligent code search using the 3-layer search architecture
//! defined in rskim-search. Uses BM25F lexical indexing with AST field boosting.
//!
//! # Module layout
//!
//! - [`index`] — Repo root discovery, cache directory management, index build
//! - [`output`] — Text and JSON result formatting, stats display

mod index;
mod output;

use std::process::ExitCode;

use rskim_search::{lexical::query::LexicalSearchLayer, SearchLayer, SearchQuery};

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

    let matches = command().get_matches_from(argv);

    let json_output = matches.get_flag("json");
    let build_flag = matches.get_flag("build");
    let rebuild_flag = matches.get_flag("rebuild");
    let stats_flag = matches.get_flag("stats");
    let clear_cache_flag = matches.get_flag("clear_cache");
    let limit: usize = matches
        .get_one::<String>("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let query_text = matches.get_one::<String>("query").map(|s| s.as_str());

    // Reject flags that appear in --help but have no implementation yet.
    // Passing them should fail loudly, not silently do nothing.
    if matches.get_flag("update") {
        eprintln!("error: --update is not yet implemented");
        eprintln!("hint: use --rebuild to recreate the full index");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_one::<String>("ast").is_some() {
        eprintln!("error: --ast is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_one::<String>("blast_radius").is_some() {
        eprintln!("error: --blast-radius is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_flag("build_temporal") {
        eprintln!("error: --build-temporal is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_one::<String>("lookback").is_some() {
        eprintln!("error: --lookback is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_flag("hot") {
        eprintln!("error: --hot is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_flag("cold") {
        eprintln!("error: --cold is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }
    if matches.get_flag("risky") {
        eprintln!("error: --risky is not yet implemented");
        return Ok(ExitCode::FAILURE);
    }

    // Resolve repo root and per-repo index directory.
    let repo_root = index::find_repo_root()?;
    let index_dir = index::get_index_dir(&repo_root)?;

    // --clear-cache: delete all search indexes.
    if clear_cache_flag {
        index::clear_search_cache()?;
        eprintln!("Search cache cleared.");
        return Ok(ExitCode::SUCCESS);
    }

    // --stats: show index statistics.
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
        return output::show_stats(&layer, json_output);
    }

    // --build / --rebuild: build (or force-rebuild) the index.
    if build_flag || rebuild_flag {
        if rebuild_flag {
            if let Err(e) = std::fs::remove_dir_all(&index_dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("warning: could not remove old index: {e}");
                }
            }
        }
        index::build_index(&repo_root, &index_dir)?;
        // If no query was supplied, we're done after building.
        if query_text.is_none() {
            return Ok(ExitCode::SUCCESS);
        }
    }

    // Require a non-empty query to proceed with search.
    let query_str = match query_text {
        Some(q) if !q.is_empty() => q,
        _ => {
            // No query and no build flag: show usage.
            if !build_flag && !rebuild_flag {
                eprintln!("Usage: skim search <query>");
                eprintln!("       skim search --build");
                eprintln!("Run 'skim search --help' for more options.");
            }
            return Ok(ExitCode::FAILURE);
        }
    };

    // Auto-build index if it does not yet exist.
    if !index_dir.join("metadata.json").exists() {
        eprintln!("Building search index...");
        index::build_index(&repo_root, &index_dir)?;
    }

    // Open index and execute search.
    let layer = match LexicalSearchLayer::open(&index_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to open search index: {e}");
            eprintln!("hint: run 'skim search --rebuild' to recreate the index");
            return Ok(ExitCode::FAILURE);
        }
    };

    let search_query = SearchQuery::text(query_str).with_limit(limit);
    let results = layer.search(&search_query)?;

    if results.is_empty() {
        if json_output {
            println!("[]");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if json_output {
        output::print_json_results(&layer, &results, query_str, &repo_root)?;
    } else {
        output::print_text_results(&layer, &results, query_str, &repo_root)?;
    }

    Ok(ExitCode::SUCCESS)
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
