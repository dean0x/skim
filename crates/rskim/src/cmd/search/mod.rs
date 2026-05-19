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

    // Parse flags.
    let flags = parse_flags(args);

    if flags.build {
        return run_build(false, &flags.root_override, analytics);
    }
    if flags.rebuild {
        return run_build(true, &flags.root_override, analytics);
    }
    if flags.update {
        return run_update(&flags.root_override, analytics);
    }
    if flags.stats {
        return run_stats(flags.json, &flags.root_override);
    }
    if flags.install_hooks {
        return run_install_hooks(&flags.root_override);
    }
    if flags.remove_hooks {
        return run_remove_hooks(&flags.root_override);
    }

    // Query mode: remaining args after flags are the query text.
    if !flags.query_text.is_empty() {
        return run_query(
            &flags.query_text,
            flags.limit,
            flags.json,
            &flags.root_override,
            analytics,
        );
    }

    print_help();
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Parsed flags
// ============================================================================

#[derive(Debug)]
struct Flags {
    build: bool,
    rebuild: bool,
    update: bool,
    stats: bool,
    install_hooks: bool,
    remove_hooks: bool,
    json: bool,
    limit: usize,
    root_override: Option<PathBuf>,
    query_text: String,
}

fn parse_flags(args: &[String]) -> Flags {
    let mut build = false;
    let mut rebuild = false;
    let mut update = false;
    let mut stats = false;
    let mut install_hooks = false;
    let mut remove_hooks = false;
    let mut json = false;
    let mut limit: usize = 20;
    let mut root_override: Option<PathBuf> = None;
    let mut query_parts: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--build" => build = true,
            "--rebuild" => rebuild = true,
            "--update" => update = true,
            "--stats" => stats = true,
            "--install-hooks" => install_hooks = true,
            "--remove-hooks" => remove_hooks = true,
            "--json" | "-j" => json = true,
            "--limit" | "-n" => {
                i += 1;
                if let Some(n) = args.get(i).and_then(|v| v.parse::<usize>().ok()) {
                    limit = n;
                }
            }
            "--root" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    root_override = Some(PathBuf::from(val));
                }
            }
            s if s.starts_with("--limit=") => {
                if let Ok(n) = s.trim_start_matches("--limit=").parse::<usize>() {
                    limit = n;
                }
            }
            s if s.starts_with("--root=") => {
                root_override = Some(PathBuf::from(s.trim_start_matches("--root=")));
            }
            // Positional arg (query text) or unrecognised flag treated as query
            s => query_parts.push(s.to_string()),
        }
        i += 1;
    }

    Flags {
        build,
        rebuild,
        update,
        stats,
        install_hooks,
        remove_hooks,
        json,
        limit,
        root_override,
        query_text: query_parts.join(" "),
    }
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

    let manifest = manifest::FileManifest::load(root.clone(), cache_dir.clone())?;
    let git_head = manifest.stored_git_head().map(str::to_string);
    let (staleness_status, _) = staleness::check_staleness(&cache_dir, &root);

    let mut out = BufWriter::new(std::io::stdout());
    if json {
        let extended = serde_json::json!({
            "file_count": stats.file_count,
            "total_ngrams": stats.total_ngrams,
            "index_size_bytes": stats.index_size_bytes,
            "last_updated": stats.last_updated,
            "git_head": git_head,
            "staleness": format!("{staleness_status:?}"),
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
        writeln!(out, "  staleness     : {staleness_status:?}")?;
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
    use std::process::ExitCode;

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

    #[test]
    fn test_parse_flags_build() {
        let flags = parse_flags(&["--build".to_string()]);
        assert!(flags.build);
        assert!(!flags.rebuild);
    }

    #[test]
    fn test_parse_flags_rebuild() {
        let flags = parse_flags(&["--rebuild".to_string()]);
        assert!(flags.rebuild);
        assert!(!flags.build);
    }

    #[test]
    fn test_parse_flags_limit() {
        let flags = parse_flags(&["--limit".to_string(), "5".to_string()]);
        assert_eq!(flags.limit, 5);
    }

    #[test]
    fn test_parse_flags_limit_equals() {
        let flags = parse_flags(&["--limit=10".to_string()]);
        assert_eq!(flags.limit, 10);
    }

    #[test]
    fn test_parse_flags_json() {
        let flags = parse_flags(&["--json".to_string()]);
        assert!(flags.json);
    }

    #[test]
    fn test_parse_flags_query_text() {
        let flags = parse_flags(&["fn".to_string(), "parse_url".to_string()]);
        assert_eq!(flags.query_text, "fn parse_url");
    }

    /// Removed regression test: query text is no longer FAILURE — it dispatches
    /// to query execution now. The test that checked FAILURE on query args was
    /// testing the old stub. This comment documents the intentional removal.
    #[test]
    fn test_stats_flag_parsed_correctly() {
        let flags = parse_flags(&["--stats".to_string()]);
        assert!(flags.stats);
    }

    #[test]
    fn test_install_hooks_flag_parsed() {
        let flags = parse_flags(&["--install-hooks".to_string()]);
        assert!(flags.install_hooks);
    }

    #[test]
    fn test_remove_hooks_flag_parsed() {
        let flags = parse_flags(&["--remove-hooks".to_string()]);
        assert!(flags.remove_hooks);
    }
}
