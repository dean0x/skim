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

use rskim_search::temporal::DEFAULT_LOOKBACK_DAYS;

// ============================================================================
// Parsed CLI arguments
// ============================================================================

/// All search flags parsed and validated from CLI arguments.
///
/// Created by [`ParsedArgs::parse`] so that [`run`] only orchestrates I/O
/// decisions, not argument munging.
struct ParsedArgs {
    json_output: bool,
    build_flag: bool,
    rebuild_flag: bool,
    stats_flag: bool,
    clear_cache_flag: bool,
    build_temporal_flag: bool,
    limit: usize,
    lookback: u32,
    query_text: Option<String>,
    blast_radius_arg: Option<String>,
    hot: bool,
    cold: bool,
    risky: bool,
}

impl ParsedArgs {
    /// Parse and validate `args` through clap.
    ///
    /// Returns `Err(message)` for user-visible errors that should be printed to
    /// stderr and map to [`ExitCode::FAILURE`].  Returns `Ok(None)` when help
    /// was requested (help has already been printed).
    fn parse(args: &[String]) -> Result<Option<Self>, String> {
        // Delegate help to clap (single source of truth).
        if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
            print_help();
            return Ok(None);
        }

        // Prepend the program name so `get_matches_from` sees a valid argv[0].
        let mut argv = vec!["skim search".to_string()];
        argv.extend_from_slice(args);

        let matches = command().get_matches_from(&argv);

        // Reject unimplemented flags early.
        if matches.get_flag("update") {
            return Err(
                "error: --update is not yet implemented\nhint: use --rebuild to recreate the full index"
                    .to_string(),
            );
        }
        if matches.get_one::<String>("ast").is_some() {
            return Err("error: --ast is not yet implemented".to_string());
        }

        let limit: usize =
            parse_flag_or_fail::<usize>(&matches, "limit", "--limit")?.unwrap_or(50);
        let lookback: u32 =
            parse_flag_or_fail::<u32>(&matches, "lookback", "--lookback")?.unwrap_or(DEFAULT_LOOKBACK_DAYS);

        let mut hot = matches.get_flag("hot");
        let mut cold = matches.get_flag("cold");

        // --hot and --cold are mutually exclusive; last one in argv wins.
        if hot && cold {
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
            hot = last == "hot";
            cold = last == "cold";
        }

        let blast_radius_arg = matches.get_one::<String>("blast_radius").cloned();
        let query_text = matches
            .get_one::<String>("query")
            .filter(|q| !q.is_empty())
            .cloned();

        let is_blast_radius = blast_radius_arg.is_some();
        let has_temporal_scoring_flag = hot || cold || matches.get_flag("risky");
        let has_text = query_text.is_some();

        // Validate mutually exclusive flag combinations.
        if is_blast_radius && has_text {
            return Err(
                "error: --blast-radius is a standalone query mode and cannot be combined with text search"
                    .to_string(),
            );
        }
        if is_blast_radius && has_temporal_scoring_flag {
            return Err(
                "error: --blast-radius cannot be combined with --hot/--cold/--risky".to_string(),
            );
        }

        Ok(Some(Self {
            json_output: matches.get_flag("json"),
            build_flag: matches.get_flag("build"),
            rebuild_flag: matches.get_flag("rebuild"),
            stats_flag: matches.get_flag("stats"),
            clear_cache_flag: matches.get_flag("clear_cache"),
            build_temporal_flag: matches.get_flag("build_temporal"),
            limit,
            lookback,
            query_text,
            blast_radius_arg,
            hot,
            cold,
            risky: matches.get_flag("risky"),
        }))
    }
}

// ============================================================================
// Entry point
// ============================================================================

/// Run the search subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    let parsed = match ParsedArgs::parse(args) {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(ExitCode::SUCCESS), // help was printed
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(ExitCode::FAILURE);
        }
    };

    let repo_root = index::find_repo_root()?;
    let index_dir = index::get_index_dir(&repo_root)?;

    if parsed.clear_cache_flag {
        return handle_clear_cache();
    }

    if parsed.stats_flag {
        return handle_stats(&index_dir, parsed.json_output);
    }

    if parsed.build_flag || parsed.rebuild_flag || parsed.build_temporal_flag {
        if let Some(exit) = handle_build(&parsed, &repo_root, &index_dir)? {
            return Ok(exit);
        }
    }

    let temporal_params = dispatch::TemporalParams {
        hot: parsed.hot,
        cold: parsed.cold,
        risky: parsed.risky,
        limit: parsed.limit,
        lookback: parsed.lookback,
        json_output: parsed.json_output,
    };

    let is_blast_radius = parsed.blast_radius_arg.is_some();
    let has_temporal_scoring_flag = parsed.hot || parsed.cold || parsed.risky;

    // ── STANDALONE BLAST-RADIUS QUERY ────────────────────────────────────────
    if is_blast_radius {
        let arg = parsed.blast_radius_arg.as_deref().unwrap_or("");
        return dispatch::run_standalone_blast_radius(arg, &index_dir, &repo_root, &temporal_params);
    }

    // ── STANDALONE TEMPORAL QUERY ─────────────────────────────────────────────
    if has_temporal_scoring_flag && parsed.query_text.is_none() {
        return dispatch::run_standalone_temporal(&index_dir, &repo_root, &temporal_params);
    }

    // ── LEXICAL / COMPOSITE QUERIES ──────────────────────────────────────────

    // Require a non-empty query to proceed with text search.
    let query_str = match parsed.query_text.as_deref() {
        Some(q) => q,
        None => {
            if !parsed.build_flag && !parsed.rebuild_flag && !parsed.build_temporal_flag {
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
// Intent handlers
// ============================================================================

/// `--clear-cache`: delete all search indexes and exit.
fn handle_clear_cache() -> anyhow::Result<ExitCode> {
    index::clear_search_cache()?;
    eprintln!("Search cache cleared.");
    Ok(ExitCode::SUCCESS)
}

/// `--stats`: show index statistics with an optional temporal section.
fn handle_stats(index_dir: &std::path::Path, json_output: bool) -> anyhow::Result<ExitCode> {
    if !index_dir.join("metadata.json").exists() {
        eprintln!("No search index found. Run 'skim search --build' first.");
        return Ok(ExitCode::FAILURE);
    }
    let layer = match rskim_search::lexical::query::LexicalSearchLayer::open(index_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to open search index: {e}");
            return Ok(ExitCode::FAILURE);
        }
    };
    let temporal_opt = {
        let db_path = dispatch::temporal_db_path(index_dir);
        if db_path.exists() {
            rskim_search::temporal::TemporalIndex::open(&db_path).ok()
        } else {
            None
        }
    };
    output::show_stats(&layer, temporal_opt.as_ref(), json_output)
}

/// `--build` / `--rebuild` / `--build-temporal`: build index(es).
///
/// Returns `Ok(Some(exit))` when the build path is complete and `run` should
/// return that exit code immediately.  Returns `Ok(None)` when a query was
/// also requested and execution should continue into dispatch.
fn handle_build(
    parsed: &ParsedArgs,
    repo_root: &std::path::Path,
    index_dir: &std::path::Path,
) -> anyhow::Result<Option<ExitCode>> {
    if parsed.rebuild_flag {
        if let Err(e) = std::fs::remove_dir_all(index_dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("warning: could not remove old index: {e}");
            }
        }
    }
    if parsed.build_flag || parsed.rebuild_flag {
        index::build_index(repo_root, index_dir)?;
    }
    // Temporal build: --build-temporal is explicit so it hard-fails when
    // there is no git repository. --build/--rebuild are "do the right thing"
    // entry points: skip temporal with a warning when not in a git repo.
    if parsed.build_temporal_flag || index::is_repo(repo_root) {
        dispatch::build_temporal_layer(repo_root, index_dir, parsed.lookback)?;
    } else {
        eprintln!("warning: skipping temporal index build: not a git repository");
    }

    let has_text = parsed.query_text.is_some();
    let is_blast_radius = parsed.blast_radius_arg.is_some();
    let has_temporal_scoring_flag = parsed.hot || parsed.cold || parsed.risky;

    // If no query was also requested, we're done after building.
    if !has_text && !is_blast_radius && !has_temporal_scoring_flag {
        return Ok(Some(ExitCode::SUCCESS));
    }
    Ok(None)
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

/// Parse a named flag from `matches` as type `T`, returning:
/// - `Ok(Some(value))` when the flag is present and parses successfully.
/// - `Ok(None)` when the flag is absent.
/// - `Err(message)` when the flag is present but fails to parse — the caller
///   should print the message and exit with failure rather than silently
///   falling back to a default.
fn parse_flag_or_fail<T>(
    matches: &clap::ArgMatches,
    name: &str,
    flag: &str,
) -> Result<Option<T>, String>
where
    T: std::str::FromStr,
{
    match matches.get_one::<String>(name) {
        None => Ok(None),
        Some(raw) => match raw.parse::<T>() {
            Ok(v) => Ok(Some(v)),
            Err(_) => Err(format!(
                "error: {flag} must be a non-negative integer (got: {raw})"
            )),
        },
    }
}
