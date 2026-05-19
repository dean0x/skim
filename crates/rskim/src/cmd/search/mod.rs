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

pub(crate) mod hooks;
mod index;
mod manifest;
mod query;
mod snippet;
mod staleness;
mod types;
mod walk;

use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

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

    match flags.action {
        SearchAction::Build => run_build(false, &flags.root_override, analytics),
        SearchAction::Rebuild => run_build(true, &flags.root_override, analytics),
        SearchAction::Update => run_update(&flags.root_override, analytics),
        SearchAction::Stats => run_stats(flags.json, &flags.root_override),
        SearchAction::InstallHooks => run_install_hooks(&flags.root_override),
        SearchAction::RemoveHooks => run_remove_hooks(&flags.root_override),
        SearchAction::Query(ref text) if !text.is_empty() => {
            run_query(text, flags.limit, flags.json, &flags.root_override, analytics)
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
}

/// Parse and validate a `--limit` value string.
///
/// Accepts any positive (>= 1) `usize`. Returns an error for non-numeric
/// values or zero.
fn parse_limit_value(raw: &str) -> anyhow::Result<usize> {
    let parsed = raw.parse::<usize>().map_err(|_| {
        anyhow::anyhow!("--limit value must be a positive integer, got {:?}", raw)
    })?;
    if parsed == 0 {
        anyhow::bail!("--limit must be >= 1 (got 0)");
    }
    Ok(parsed)
}

/// Parse the flags from `args`.
///
/// # Errors
///
/// - `--limit` / `-n` without a following value.
/// - `--limit` / `-n` value that is not a valid `usize`.
/// - `--limit=<value>` with a non-numeric value.
/// - `--root` without a following value.
/// - Unrecognised flags (tokens beginning with `--`).
fn parse_flags(args: &[String]) -> anyhow::Result<Flags> {
    let mut action_flag: Option<SearchAction> = None;
    let mut json = false;
    let mut limit: usize = 20;
    let mut root_override: Option<PathBuf> = None;
    let mut query_parts: Vec<String> = Vec::new();

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
                let raw = args.get(i).ok_or_else(|| {
                    anyhow::anyhow!("--limit requires a value (e.g. --limit 10)")
                })?;
                limit = parse_limit_value(raw)?;
            }
            "--root" => {
                i += 1;
                let val = args.get(i).ok_or_else(|| {
                    anyhow::anyhow!("--root requires a path value (e.g. --root /path/to/project)")
                })?;
                root_override = Some(PathBuf::from(val));
            }
            s if s.starts_with("--limit=") => {
                let raw = s.trim_start_matches("--limit=");
                limit = parse_limit_value(raw)?;
            }
            s if s.starts_with("--root=") => {
                root_override = Some(PathBuf::from(s.trim_start_matches("--root=")));
            }
            s if s.starts_with("--") => {
                anyhow::bail!(
                    "unrecognised flag {:?}. Valid flags: --build, --rebuild, --update, \
                     --stats, --install-hooks, --remove-hooks, --json, -j, --limit, --root",
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

fn run_query(
    text: &str,
    limit: usize,
    json: bool,
    root_override: &Option<PathBuf>,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;

    let config = types::QueryConfig {
        text: text.to_string(),
        limit,
        json,
        root,
        cache_dir,
    };

    let output = query::execute_query(&config, analytics)?;

    let mut stdout = BufWriter::new(std::io::stdout());
    if json {
        query::format_json_output(&output, &mut stdout)?;
    } else {
        query::format_text_output(&output, &mut stdout)?;
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

Examples:
  skim search \"authenticate\"          Search for 'authenticate'
  skim search --limit 5 \"parse_url\"   Return at most 5 results
  skim search --json \"UserService\"    JSON output
  skim search --build                 Build the search index
  skim search --rebuild               Rebuild from scratch
  skim search --update                Refresh stale index
  skim search --stats                 Show index statistics
  skim search --install-hooks         Auto-refresh on git commit/merge"
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        assert_eq!(flags.action, SearchAction::Query("fn parse_url".to_string()));
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
        assert_eq!(flags.action, SearchAction::Query("authenticate".to_string()));
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
        assert_eq!(flags.action, SearchAction::Query("authenticate".to_string()));
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
}
