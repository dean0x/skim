//! Search subcommand — code search via layered n-gram indexing.
//!
//! # Architecture
//!
//! All I/O lives here (this module). Business logic is split across:
//! - `types` — shared configuration and result types
//! - `walk` — project-root discovery and file traversal
//! - `manifest` — JSONL sidecar for incremental build caching
//! - `index` — full pipeline orchestration (`skim search index`)
//! - `query` — query execution and result formatting
//! - `snippet` — source context extraction
//! - `staleness` — git HEAD comparison and auto-refresh
//! - `hooks` — git hook installation/removal
//! - `rskim-search` crate — index building, n-gram extraction, BM25F scoring

mod ast;
pub(crate) mod hooks;
mod index;
mod manifest;
mod query;
mod snippet;
mod staleness;
mod temporal;
mod types;
mod walk;

use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search` subcommand.
///
/// Dispatches to:
/// - `skim search index [OPTIONS]` — build or update the search index
/// - `skim search --build` — build incrementally (alias for index)
/// - `skim search --rebuild` — force full rebuild
/// - `skim search --update` — auto-refresh if stale
/// - `skim search --stats [--json]` — print index statistics
/// - `skim search --install-hooks` — install git hooks
/// - `skim search --remove-hooks` — remove git hooks
/// - `skim search [--json] [--limit N] <QUERY>` — search
/// - No args / `--help` / `-h` — print help
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // `skim search index [OPTIONS]` — legacy subcommand path.
    if args.first().is_some_and(|a| a == "index") {
        let rest = &args[1..];
        return index::run(rest, analytics);
    }

    // No args or --help/-h → print help
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Parse flags — propagate errors (invalid --limit, unrecognised flags, etc.).
    let flags = parse_flags(args)?;

    // ── Validation order (deterministic — tests rely on this ordering) ──────
    // 1. --ast + temporal flags (--hot/--cold/--risky/--blast-radius) → #202 error.
    //    (--blast-radius is a co-change filter and IS composable with --ast per the
    //     plan, but the plan also says --ast + temporal_sort → #202.  --blast-radius
    //     alone is handled in run_query via FileId intersection — not an error.)
    // 2. single-node pattern → #283 error.
    // 3. unknown pattern → lists available names.
    // Dispatch happens after validation passes.
    if let Some(ref raw_ast) = flags.ast {
        // Validation #1: --ast + temporal sort is not yet supported (#202).
        if let Some(sort) = flags.temporal_sort {
            anyhow::bail!(
                "--ast and {} are not yet composable (tracked in #202).\n\
                 Use --ast alone or use {} without --ast.",
                sort.flag_name(),
                sort.flag_name()
            );
        }
        // Validations #2 and #3: single-node (#283) + unknown pattern.
        ast::validate_ast_pattern(raw_ast)?;
    }
    // ────────────────────────────────────────────────────────────────────────

    match flags.action {
        SearchAction::Build => run_build(false, &flags.root_override, analytics),
        SearchAction::Rebuild => run_build(true, &flags.root_override, analytics),
        SearchAction::Update => run_update(&flags.root_override, analytics),
        SearchAction::Stats => run_stats(flags.json, &flags.root_override),
        SearchAction::InstallHooks => run_install_hooks(&flags.root_override),
        SearchAction::RemoveHooks => run_remove_hooks(&flags.root_override),
        SearchAction::Query(ref text) if !text.is_empty() => run_query(text, &flags, analytics),
        // Empty query + --ast only → standalone AST dispatch.
        SearchAction::Query(_)
            if flags.ast.is_some()
                && flags.temporal_sort.is_none()
                && flags.blast_radius.is_none() =>
        {
            let raw = flags.ast.as_deref().unwrap();
            let (root, cache_dir) = resolve_root_and_cache(&flags.root_override)?;
            std::fs::create_dir_all(&cache_dir)?;
            // Ensure both indexes are fresh before querying.
            let (_refreshed, manifest) =
                staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;
            ast::run_ast_standalone(raw, flags.limit, flags.json, &cache_dir, &manifest)
        }
        // Empty query with temporal flags (no --ast) → standalone temporal dispatch.
        SearchAction::Query(_) if flags.temporal_sort.is_some() || flags.blast_radius.is_some() => {
            run_temporal_standalone(
                flags.limit,
                flags.json,
                &flags.root_override,
                flags.temporal_sort,
                flags.blast_radius.as_deref(),
            )
        }
        SearchAction::Query(_) => {
            // Empty query (no positional args and no action flag) → help.
            print_help();
            Ok(ExitCode::SUCCESS)
        }
    }
}

// ============================================================================
// Parsed flags
// ============================================================================

/// The action the user wants to perform, derived from CLI flags.
///
/// Encodes the mutually-exclusive mode flags as a single enum variant so that
/// dispatch is a `match` rather than a cascade of `if flags.X` checks.
#[derive(Debug, PartialEq, Eq)]
enum SearchAction {
    Build,
    Rebuild,
    Update,
    Stats,
    InstallHooks,
    RemoveHooks,
    /// Run a search query with the given text.
    Query(String),
}

/// Parsed flags from the CLI args passed to `skim search`.
#[derive(Debug)]
struct Flags {
    action: SearchAction,
    json: bool,
    limit: usize,
    root_override: Option<PathBuf>,
    /// Sort mode for temporal queries — mutually exclusive.
    temporal_sort: Option<types::TemporalSort>,
    /// Raw path for blast-radius pre-filtering. Normalized later in run_query.
    blast_radius: Option<String>,
    /// Raw AST pattern string for structural pattern search (#199).
    ///
    /// Validated at dispatch time (before opening the index).  Space-separated
    /// `--ast try-catch` and equals form `--ast=try-catch` are both accepted.
    /// Whitespace-only values are rejected in `parse_flags`.
    ast: Option<String>,
}

/// Parse and validate a `--limit` value string.
///
/// Accepts any positive (>= 1) `usize`. Returns an error for non-numeric
/// values or zero.
fn parse_limit_value(raw: &str) -> anyhow::Result<usize> {
    let parsed = raw
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("--limit value must be a positive integer, got {:?}", raw))?;
    if parsed == 0 {
        anyhow::bail!("--limit must be >= 1 (got 0)");
    }
    Ok(parsed)
}

/// Parse a temporal flag arm (`--hot`, `--cold`, `--risky`, `--blast-radius`).
///
/// Returns `Ok(true)` when the flag consumed an extra token (i.e. the space-
/// separated `--blast-radius <path>` form), `Ok(false)` for single-token arms,
/// and `Err` on validation failure.
///
/// The caller is responsible for advancing `i` by one additional position when
/// this function returns `Ok(true)`.
fn parse_temporal_flag(
    arg: &str,
    next_arg: Option<&String>,
    temporal_sort: &mut Option<types::TemporalSort>,
    blast_radius: &mut Option<String>,
) -> anyhow::Result<bool> {
    match arg {
        "--hot" | "--cold" | "--risky" => {
            let new_sort = match arg {
                "--hot" => types::TemporalSort::Hot,
                "--cold" => types::TemporalSort::Cold,
                _ => types::TemporalSort::Risky,
            };
            if let Some(existing) = *temporal_sort {
                anyhow::bail!(
                    "{} and {} are mutually exclusive",
                    new_sort.flag_name(),
                    existing.flag_name()
                );
            }
            *temporal_sort = Some(new_sort);
            Ok(false)
        }
        "--blast-radius" => {
            let val =
                next_arg.ok_or_else(|| anyhow::anyhow!("--blast-radius requires a file path"))?;
            *blast_radius = Some(val.clone());
            Ok(true)
        }
        s if s.starts_with("--blast-radius=") => {
            let val = s.trim_start_matches("--blast-radius=");
            if val.is_empty() {
                anyhow::bail!("--blast-radius requires a file path");
            }
            *blast_radius = Some(val.to_string());
            Ok(false)
        }
        _ => unreachable!("parse_temporal_flag called with non-temporal arg: {arg}"),
    }
}

/// Parse the flags from `args`.
///
/// # Errors
///
/// - `--limit` / `-n` without a following value.
/// - `--limit` / `-n` value that is not a valid `usize`.
/// - `--limit=<value>` with a non-numeric value.
/// - `--root` without a following value.
/// - `--ast` without a value or with a whitespace-only value.
/// - Unrecognised flags (tokens beginning with `--`).
fn parse_flags(args: &[String]) -> anyhow::Result<Flags> {
    let mut action_flag: Option<SearchAction> = None;
    let mut json = false;
    let mut limit: usize = 20;
    let mut root_override: Option<PathBuf> = None;
    let mut query_parts: Vec<String> = Vec::new();
    let mut temporal_sort: Option<types::TemporalSort> = None;
    let mut blast_radius: Option<String> = None;
    let mut ast: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--build" => action_flag = Some(SearchAction::Build),
            "--rebuild" => action_flag = Some(SearchAction::Rebuild),
            "--update" => action_flag = Some(SearchAction::Update),
            "--stats" => action_flag = Some(SearchAction::Stats),
            "--install-hooks" => action_flag = Some(SearchAction::InstallHooks),
            "--remove-hooks" => action_flag = Some(SearchAction::RemoveHooks),
            "--json" | "-j" => json = true,
            "--limit" | "-n" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--limit requires a value (e.g. --limit 10)"))?;
                limit = parse_limit_value(raw)?;
            }
            "--root" => {
                i += 1;
                let val = args.get(i).ok_or_else(|| {
                    anyhow::anyhow!("--root requires a path value (e.g. --root /path/to/project)")
                })?;
                root_override = Some(PathBuf::from(val));
            }
            "--ast" => {
                // Space-separated form: `--ast try-catch`
                i += 1;
                let val = args.get(i).ok_or_else(|| {
                    anyhow::anyhow!("--ast requires a pattern value (e.g. --ast try-catch)")
                })?;
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    anyhow::bail!("--ast pattern must not be empty or whitespace-only");
                }
                ast = Some(trimmed.to_string());
            }
            s if s.starts_with("--limit=") => {
                let raw = s.trim_start_matches("--limit=");
                limit = parse_limit_value(raw)?;
            }
            s if s.starts_with("--root=") => {
                root_override = Some(PathBuf::from(s.trim_start_matches("--root=")));
            }
            s if s.starts_with("--ast=") => {
                // Equals form: `--ast=try-catch` or `--ast=for_statement > await_expression`
                let val = s.trim_start_matches("--ast=");
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    anyhow::bail!("--ast pattern must not be empty or whitespace-only");
                }
                ast = Some(trimmed.to_string());
            }
            s if matches!(s, "--hot" | "--cold" | "--risky" | "--blast-radius")
                || s.starts_with("--blast-radius=") =>
            {
                let consumed_next =
                    parse_temporal_flag(s, args.get(i + 1), &mut temporal_sort, &mut blast_radius)?;
                if consumed_next {
                    i += 1;
                }
            }
            s if s.starts_with("--") => {
                anyhow::bail!(
                    "unrecognised flag {:?}. Valid flags: --build, --rebuild, --update, \
                     --stats, --install-hooks, --remove-hooks, --json, -j, --limit, --root, \
                     --ast, --hot, --cold, --risky, --blast-radius",
                    s
                );
            }
            // Positional arg — part of the query text.
            s => query_parts.push(s.to_string()),
        }
        i += 1;
    }

    let action = action_flag.unwrap_or_else(|| SearchAction::Query(query_parts.join(" ")));

    Ok(Flags {
        action,
        json,
        limit,
        root_override,
        temporal_sort,
        blast_radius,
        ast,
    })
}

// ============================================================================
// Shared project-root + cache-dir resolution
// ============================================================================

fn resolve_root_and_cache(root_override: &Option<PathBuf>) -> anyhow::Result<(PathBuf, PathBuf)> {
    let root = match root_override {
        Some(r) => r.canonicalize().unwrap_or_else(|_| r.clone()),
        None => {
            let cwd = std::env::current_dir()?;
            walk::discover_project_root(&cwd)?
        }
    };
    let cache_dir = index::resolve_search_cache_dir(&root)?;
    Ok((root, cache_dir))
}

// ============================================================================
// --build / --rebuild
// ============================================================================

fn run_build(
    force: bool,
    root_override: &Option<PathBuf>,
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;
    let config = types::IndexConfig {
        root,
        max_files: None,
        force,
        cache_dir_override: Some(cache_dir),
    };
    let result = index::build_index(&config)?;
    eprintln!(
        "skim search: indexed {} files ({} skipped, {} cache hits) in {:.1}s",
        result.file_count,
        result.skipped,
        result.cache_hits,
        result.duration.as_secs_f64(),
    );
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --update
// ============================================================================

fn run_update(
    root_override: &Option<PathBuf>,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;
    let (refreshed, _manifest) = staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;
    if !refreshed {
        eprintln!("skim search: index is current");
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --stats
// ============================================================================

fn run_stats(json: bool, root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;

    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        if json {
            println!("{{\"error\": \"no index found\"}}");
        } else {
            eprintln!("skim search: no index found — run `skim search --build` first");
        }
        return Ok(ExitCode::FAILURE);
    }

    let reader = rskim_search::NgramIndexReader::open(&cache_dir)?;
    let stats = reader.stats();

    // check_staleness returns the loaded manifest as part of its work.
    // Reuse it here instead of loading the manifest a second time.
    let (staleness_status, loaded_manifest) = staleness::check_staleness(&cache_dir, &root);
    let git_head = loaded_manifest
        .as_ref()
        .and_then(|m| m.stored_git_head().map(str::to_string));

    let mut out = BufWriter::new(std::io::stdout());
    if json {
        let extended = serde_json::json!({
            "file_count": stats.file_count,
            "total_ngrams": stats.total_ngrams,
            "index_size_bytes": stats.index_size_bytes,
            "last_updated": stats.last_updated,
            "git_head": git_head,
            "staleness": staleness_status.to_string(),
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&extended)?)?;
    } else {
        writeln!(out, "skim search index stats:")?;
        writeln!(out, "  files indexed : {}", stats.file_count)?;
        writeln!(out, "  total n-grams : {}", stats.total_ngrams)?;
        writeln!(out, "  index size    : {} bytes", stats.index_size_bytes)?;
        if let Some(ts) = stats.last_updated {
            writeln!(out, "  last updated  : {ts}")?;
        }
        writeln!(
            out,
            "  git HEAD      : {}",
            git_head.as_deref().unwrap_or("(none)")
        )?;
        writeln!(out, "  staleness     : {staleness_status}")?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --install-hooks / --remove-hooks
// ============================================================================

fn run_install_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    hooks::install_search_hooks(&root)?;
    eprintln!("skim search: git hooks installed in {}", root.display());
    Ok(ExitCode::SUCCESS)
}

fn run_remove_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    hooks::remove_search_hooks(&root)?;
    eprintln!("skim search: git hooks removed from {}", root.display());
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Query execution
// ============================================================================

/// Resolve blast-radius partner paths from the temporal database.
///
/// Returns co-change partners **plus the target file itself**, so text queries
/// like `skim search auth --blast-radius src/auth.rs` surface matches within
/// `src/auth.rs` in addition to its co-change partners.
///
/// Returns `None` when no blast-radius was requested, or when the temporal DB
/// is unavailable.  When `json` is true the warning is emitted as a JSON
/// object to stdout (consistent with the JSON degradation path in
/// `run_temporal_standalone`); otherwise it goes to stderr.
fn resolve_blast_radius_filter(
    blast_radius: Option<&str>,
    temporal_db: &Option<rskim_search::TemporalDb>,
    root: &std::path::Path,
    json: bool,
) -> anyhow::Result<Option<std::collections::HashSet<String>>> {
    let raw_path = match blast_radius {
        Some(p) => p,
        None => return Ok(None),
    };

    let db = match temporal_db {
        Some(db) => db,
        None => {
            const MSG: &str = "no temporal data — run 'skim heatmap' to populate";
            if json {
                let w = WarningJson { warning: MSG };
                println!("{}", serde_json::to_string(&w)?);
            } else {
                eprintln!("skim search: {MSG}");
            }
            return Ok(None);
        }
    };

    let normalized = temporal::normalize_blast_radius_path(raw_path, root)?;
    let partners = db.cochanges_for_file(&normalized)?;
    if partners.is_empty() {
        eprintln!("skim search: no co-change data for {raw_path:?}");
    }

    // Include the target file itself so text queries like
    // `skim search auth --blast-radius src/auth.rs` surface matches
    // within src/auth.rs in addition to its co-change partners.
    let mut paths = temporal::cochange_partner_paths(&partners, &normalized);
    paths.insert(normalized);
    Ok(Some(paths))
}

fn run_query(
    text: &str,
    flags: &Flags,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(&flags.root_override)?;
    std::fs::create_dir_all(&cache_dir)?;

    // Open the temporal DB once. Used for both blast-radius filtering (before
    // the query, so LIMIT applies to the filtered set) and temporal enrichment
    // (after the query, to annotate/sort results).
    let temporal_db = if flags.temporal_sort.is_some() || flags.blast_radius.is_some() {
        temporal::open_temporal_db(&cache_dir.join("temporal.db"))
    } else {
        None
    };

    // Warn when temporal data is stale (same check as run_temporal_standalone).
    if let Some(ref db) = temporal_db
        && let Some(warning) = temporal::check_temporal_staleness(db, &root)
    {
        eprintln!("{warning}");
    }

    // Resolve blast-radius partner paths BEFORE querying so the file_filter
    // is applied inside the search engine (before LIMIT). This ensures the
    // limit applies to the filtered set rather than silently discarding
    // co-change partners that ranked beyond the top-N unfiltered results.
    let blast_radius_paths = resolve_blast_radius_filter(
        flags.blast_radius.as_deref(),
        &temporal_db,
        &root,
        flags.json,
    )?;

    // Resolve AST file filter (#199): open the AST engine, execute the
    // structural query, collect matching FileIds.  Applied at the FileId level
    // inside execute_query (no path round-trip).
    let ast_file_ids = if let Some(ref raw_ast) = flags.ast {
        // ensure_indexes_fresh already ran in the dispatch arm above via
        // auto_refresh_if_stale; here we just open the engine.
        match ast::open_ast_engine(&cache_dir) {
            Ok(engine) => {
                match ast::resolve_ast_file_filter(&engine, raw_ast, None) {
                    Ok(ids) => {
                        if ids.is_empty() {
                            eprintln!("skim search: --ast {:?} matched no indexed files", raw_ast);
                        }
                        Some(ids)
                    }
                    Err(e) => {
                        // AST query failure: degrade gracefully (warn, no AST filter).
                        if flags.json {
                            let w = WarningJson {
                                warning: &format!("AST query failed: {e}"),
                            };
                            println!("{}", serde_json::to_string(&w)?);
                        } else {
                            eprintln!("skim search: AST query warning: {e}");
                        }
                        None
                    }
                }
            }
            Err(e) => {
                // Missing AST index when --ast was specified: fail loud (#199).
                return Err(e);
            }
        }
    } else {
        None
    };

    let config = types::QueryConfig {
        text: text.to_string(),
        limit: flags.limit,
        json: flags.json,
        root,
        cache_dir,
        blast_radius_paths,
        ast_file_ids,
    };

    let mut output = query::execute_query(&config, analytics)?;

    // Apply temporal sort/annotation to the results.
    if let (Some(sort), Some(db)) = (flags.temporal_sort, &temporal_db) {
        temporal::apply_temporal_enrichment(&mut output.results, sort, db)?;
    }

    let mut stdout = BufWriter::new(std::io::stdout());
    if flags.json {
        query::format_json_output(&output, &mut stdout)?;
    } else {
        query::format_text_output(&output, &mut stdout)?;
    }
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Typed JSON envelope for a warning-only response (no temporal data available).
#[derive(Serialize)]
struct WarningJson<'a> {
    warning: &'a str,
}

/// Execute a standalone temporal query (no text search term provided).
///
/// Opens the temporal DB from the resolved cache directory, checks for
/// staleness, dispatches the query (hotspot, cold, risky, or blast-radius),
/// and writes the result as JSON or plain text to stdout. Degrades gracefully
/// when the temporal DB is absent — prints a warning and returns exit 0.
fn run_temporal_standalone(
    limit: usize,
    json: bool,
    root_override: &Option<PathBuf>,
    temporal_sort: Option<types::TemporalSort>,
    blast_radius: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    let temporal_db_path = cache_dir.join("temporal.db");

    let Some(db) = temporal::open_temporal_db(&temporal_db_path) else {
        if json {
            let msg = WarningJson {
                warning: "no temporal data — run 'skim heatmap' to populate",
            };
            println!("{}", serde_json::to_string(&msg)?);
        } else {
            eprintln!("skim search: no temporal data — run 'skim heatmap' to populate");
        }
        return Ok(ExitCode::SUCCESS);
    };

    // Check staleness.
    if let Some(warning) = temporal::check_temporal_staleness(&db, &root) {
        eprintln!("{warning}");
    }

    let output = temporal::query_standalone(temporal_sort, blast_radius, limit, &db, &root)?;

    let mut stdout = BufWriter::new(std::io::stdout());
    if json {
        temporal::format_temporal_json(&output, &mut stdout)?;
    } else {
        temporal::format_temporal_text(&output, &mut stdout)?;
    }
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Help text
// ============================================================================

fn print_help() {
    println!(
        "\
Usage: skim search [OPTIONS] [QUERY]

Search code using layered n-gram BM25F indexing.

Subcommands / modes:
  (none)           Print this help message
  index            Build or update the search index (legacy)

Options:
  --build          Build the index incrementally (auto-build on first query)
  --rebuild        Rebuild the index from scratch
  --update         Refresh if index is stale (git HEAD changed)
  --stats          Show index statistics
  --install-hooks  Install git post-commit/merge hooks for auto-refresh
  --remove-hooks   Remove skim git hooks
  --json           Output results as JSON
  --limit N        Maximum results to return (default: 20)
  --root PATH      Override project root (default: walk up to .git)
  -h, --help       Print this help message

AST structural query options (#199):
  --ast PATTERN    Filter/list by AST structural pattern.
                   PATTERN is a named catalog pattern or a containment query:
                     Named:        --ast try-catch
                     Containment:  --ast \"for_statement > await_expression\"
                   Use `--ast` alone for standalone AST-only output (file-level),
                   or combine with a text query for intersection results.

  Limitations:
    #283 — Single-node queries (e.g. --ast try_statement) are not yet supported;
           use a named pattern or a containment query instead.
    #202 — --ast combined with --hot / --cold / --risky is not yet supported.

AST standalone examples:
  skim search --ast try-catch                   Files with try/catch blocks
  skim search --ast \"for_statement > await_expression\"  Async-in-loop pattern
  skim search \"error\" --ast try-catch           Text+AST intersection (lexical snippets preserved)
  skim search --ast try-catch --blast-radius src/auth.rs  AST ∩ co-change

Temporal query options (require 'skim heatmap' data):
  --hot                        Sort/list by hotspot score descending
  --cold                       Sort/list by hotspot score ascending
  --risky                      Sort/list by bug-fix density descending
  --blast-radius FILE          Restrict to co-change partners of FILE

Temporal flag composition:
  --hot and --cold/--risky are mutually exclusive (pick one sort mode).
  --blast-radius is composable with any sort mode and with text queries.

General examples:
  skim search \"authenticate\"                Search for 'authenticate'
  skim search --limit 5 \"parse_url\"         Return at most 5 results
  skim search --json \"UserService\"          JSON output
  skim search --build                       Build the search index
  skim search --rebuild                     Rebuild from scratch
  skim search --update                      Refresh stale index
  skim search --stats                       Show index statistics
  skim search --install-hooks               Auto-refresh on git commit/merge
  skim search --hot                         Top hotspot files (standalone)
  skim search --risky                       Top risky files (standalone)
  skim search --blast-radius src/auth.rs    Co-change partners of auth.rs
  skim search \"auth\" --hot                  Text results sorted by hotspot
  skim search \"auth\" --blast-radius src/auth.rs  Text within co-change partners"
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Stub analytics config for tests — analytics disabled, no cost override.
    const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };

    #[test]
    fn test_search_help_returns_success() {
        let result = run(&[], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_help_flag_returns_success() {
        let result = run(&["--help".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_short_help_flag_returns_success() {
        let result = run(&["-h".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    /// Regression: `skim search index --help` must dispatch to index help,
    /// not the parent search help. The parent help check must not intercept
    /// flags intended for a known subcommand.
    #[test]
    fn test_index_help_dispatches_to_index_not_parent() {
        let result = run(
            &["index".to_string(), "--help".to_string()],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    // ============================================================================
    // parse_flags — action dispatch
    // ============================================================================

    #[test]
    fn test_parse_flags_build() {
        let flags = parse_flags(&["--build".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::Build);
    }

    #[test]
    fn test_parse_flags_rebuild() {
        let flags = parse_flags(&["--rebuild".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::Rebuild);
    }

    #[test]
    fn test_stats_flag_parsed_correctly() {
        let flags = parse_flags(&["--stats".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::Stats);
    }

    #[test]
    fn test_install_hooks_flag_parsed() {
        let flags = parse_flags(&["--install-hooks".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::InstallHooks);
    }

    #[test]
    fn test_remove_hooks_flag_parsed() {
        let flags = parse_flags(&["--remove-hooks".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::RemoveHooks);
    }

    // ============================================================================
    // parse_flags — modifier flags
    // ============================================================================

    #[test]
    fn test_parse_flags_limit() {
        let flags = parse_flags(&["--limit".to_string(), "5".to_string()]).unwrap();
        assert_eq!(flags.limit, 5);
    }

    #[test]
    fn test_parse_flags_limit_equals() {
        let flags = parse_flags(&["--limit=10".to_string()]).unwrap();
        assert_eq!(flags.limit, 10);
    }

    #[test]
    fn test_parse_flags_short_n() {
        let flags = parse_flags(&["-n".to_string(), "3".to_string()]).unwrap();
        assert_eq!(flags.limit, 3);
    }

    #[test]
    fn test_parse_flags_json() {
        let flags = parse_flags(&["--json".to_string()]).unwrap();
        assert!(flags.json);
    }

    #[test]
    fn test_parse_flags_root_space() {
        let flags = parse_flags(&["--root".to_string(), "/tmp/proj".to_string()]).unwrap();
        assert_eq!(flags.root_override, Some(PathBuf::from("/tmp/proj")));
    }

    #[test]
    fn test_parse_flags_root_equals() {
        let flags = parse_flags(&["--root=/tmp/other".to_string()]).unwrap();
        assert_eq!(flags.root_override, Some(PathBuf::from("/tmp/other")));
    }

    // ============================================================================
    // parse_flags — query text
    // ============================================================================

    #[test]
    fn test_parse_flags_query_text() {
        let flags = parse_flags(&["fn".to_string(), "parse_url".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("fn parse_url".to_string())
        );
    }

    #[test]
    fn test_parse_flags_combined_json_limit_query() {
        let flags = parse_flags(&[
            "--json".to_string(),
            "--limit".to_string(),
            "5".to_string(),
            "authenticate".to_string(),
        ])
        .unwrap();
        assert!(flags.json);
        assert_eq!(flags.limit, 5);
        assert_eq!(
            flags.action,
            SearchAction::Query("authenticate".to_string())
        );
    }

    // ============================================================================
    // parse_flags — error cases
    // ============================================================================

    #[test]
    fn test_parse_flags_limit_missing_value_is_error() {
        let err = parse_flags(&["--limit".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit requires a value"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_non_numeric_is_error() {
        let err = parse_flags(&["--limit".to_string(), "abc".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_equals_non_numeric_is_error() {
        let err = parse_flags(&["--limit=abc".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_root_missing_value_is_error() {
        let err = parse_flags(&["--root".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--root requires a path"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_unrecognised_flag_is_error() {
        let err = parse_flags(&["--unknown-flag".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("unrecognised flag"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_short_n_missing_value_is_error() {
        let err = parse_flags(&["-n".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit requires a value"),
            "unexpected error message: {err}"
        );
    }

    // ============================================================================
    // Regression: -j short alias for --json (issue mod.rs:136)
    // ============================================================================

    #[test]
    fn test_parse_flags_short_j_sets_json() {
        let flags = parse_flags(&["-j".to_string()]).unwrap();
        assert!(flags.json, "-j must set json=true");
    }

    #[test]
    fn test_parse_flags_short_j_combined_with_query() {
        let flags = parse_flags(&["-j".to_string(), "authenticate".to_string()]).unwrap();
        assert!(flags.json);
        assert_eq!(
            flags.action,
            SearchAction::Query("authenticate".to_string())
        );
    }

    // ============================================================================
    // Regression: --limit 0 must be rejected (issue mod.rs:142)
    // ============================================================================

    #[test]
    fn test_parse_flags_limit_zero_space_is_error() {
        let err = parse_flags(&["--limit".to_string(), "0".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit must be >= 1"),
            "expected rejection of 0, got: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_zero_equals_is_error() {
        let err = parse_flags(&["--limit=0".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit must be >= 1"),
            "expected rejection of 0, got: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_one_is_valid() {
        let flags = parse_flags(&["--limit".to_string(), "1".to_string()]).unwrap();
        assert_eq!(flags.limit, 1);
    }

    // ============================================================================
    // resolve_blast_radius_filter — None DB degradation path
    // ============================================================================

    /// When blast_radius is Some but temporal_db is None (user hasn't run
    /// `skim heatmap` yet), the function must return Ok(None) without panicking.
    /// A stderr warning is expected but the caller handles the degradation.
    #[test]
    fn test_resolve_blast_radius_filter_no_db_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let result = resolve_blast_radius_filter(Some("src/auth.rs"), &None, root, false);
        assert!(
            result.is_ok(),
            "must not error when temporal_db is None, got: {:?}",
            result.unwrap_err()
        );
        assert_eq!(
            result.unwrap(),
            None,
            "must return None (graceful degradation) when temporal_db is None"
        );
    }

    // ============================================================================
    // F12: Missing temporal.db must produce exit 0 (graceful degradation), not
    //      exit 1. AC says: "Missing temporal.db → warning on stderr, exit 0".
    // ============================================================================

    /// Standalone temporal mode (e.g. `skim search --hot`) with no temporal.db must
    /// return `ExitCode::SUCCESS` (not FAILURE). The missing DB is a graceful-
    /// degradation case, not an error.
    #[test]
    fn test_standalone_temporal_no_db_returns_exit_0() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        // No temporal.db exists in the temp dir's cache — standalone path should
        // degrade gracefully with exit 0.
        let result = run(
            &["--hot".to_string(), "--root".to_string(), root],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "missing temporal.db must be a warning (exit 0), not an error (exit 1)"
        );
    }
}
