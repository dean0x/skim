//! `skim search` — code search across indexed files (#3)
//!
//! Provides intelligent code search using the 3-layer search architecture
//! defined in rskim-search. Uses BM25F lexical indexing with AST field boosting.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rskim_core::Language;
use rskim_search::{
    LayerBuilder, SearchIndex, SearchLayer, SearchQuery,
    lexical::builder::LexicalLayerBuilder,
    lexical::query::LexicalSearchLayer,
    FileId,
};

/// Run the search subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Extract --json flag first.
    let (args, json_output) = super::extract_json_flag(args);

    // Parse boolean flags.
    let build_flag = args.iter().any(|a| a == "--build");
    let rebuild_flag = args.iter().any(|a| a == "--rebuild");
    let stats_flag = args.iter().any(|a| a == "--stats");
    let clear_cache_flag = args.iter().any(|a| a == "--clear-cache");

    // Parse --limit value.
    let limit: usize = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);

    // Collect positional arguments: skip flag-like args and values that follow --limit.
    let query_text: Option<&str> = {
        let mut skip_next = false;
        let mut found: Option<&str> = None;
        for arg in &args {
            if skip_next {
                skip_next = false;
                continue;
            }
            // These flags consume the next arg as their value.
            if matches!(arg.as_str(), "--limit" | "--ast") {
                skip_next = true;
                continue;
            }
            if arg.starts_with("--") {
                continue;
            }
            if !arg.is_empty() {
                found = Some(arg.as_str());
                break;
            }
        }
        found
    };

    // Resolve repo root and per-repo index directory.
    let repo_root = find_repo_root()?;
    let index_dir = get_index_dir(&repo_root)?;

    // --clear-cache: delete all search indexes.
    if clear_cache_flag {
        clear_search_cache()?;
        eprintln!("Search cache cleared.");
        return Ok(ExitCode::SUCCESS);
    }

    // --stats: show index statistics.
    if stats_flag {
        return show_stats(&index_dir, json_output);
    }

    // --build / --rebuild: build (or force-rebuild) the index.
    if build_flag || rebuild_flag {
        if rebuild_flag {
            let _ = std::fs::remove_dir_all(&index_dir);
        }
        build_index(&repo_root, &index_dir)?;
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
        build_index(&repo_root, &index_dir)?;
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
        print_json_results(&layer, &results, query_str, &repo_root)?;
    } else {
        print_text_results(&layer, &results, query_str, &repo_root)?;
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Index directory helpers
// ============================================================================

/// Walk up directory tree to find a `.git` directory (repo root).
///
/// Falls back to CWD if no `.git` ancestor is found.
fn find_repo_root() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let mut dir: &Path = cwd.as_path();
    loop {
        if dir.join(".git").exists() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return Ok(cwd),
        }
    }
}

/// Return the per-repo index directory under the skim cache.
///
/// Uses `SKIM_CACHE_DIR` environment variable if set; otherwise uses
/// the platform cache dir (`~/.cache/skim/` on Linux/macOS).
fn get_index_dir(repo_root: &Path) -> anyhow::Result<PathBuf> {
    let cache_dir = if let Ok(dir) = std::env::var("SKIM_CACHE_DIR") {
        PathBuf::from(dir)
    } else {
        dirs::cache_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine platform cache directory"))?
            .join("skim")
    };

    // Hash the repo root path to produce a stable, collision-resistant directory name.
    let repo_hash = hash_path(repo_root);
    Ok(cache_dir.join("search").join(repo_hash))
}

/// Delete the entire skim search cache directory.
fn clear_search_cache() -> anyhow::Result<()> {
    let cache_dir = if let Ok(dir) = std::env::var("SKIM_CACHE_DIR") {
        PathBuf::from(dir)
    } else {
        dirs::cache_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine platform cache directory"))?
            .join("skim")
    };

    let search_cache = cache_dir.join("search");
    if search_cache.exists() {
        std::fs::remove_dir_all(&search_cache)?;
    }
    Ok(())
}

/// Stable hex hash of a path for use as a cache directory name.
///
/// Uses a simple FxHash-style mix to avoid pulling in a full hash crate.
fn hash_path(path: &Path) -> String {
    let path_str = path.to_string_lossy();
    let bytes = path_str.as_bytes();
    let seed: u64 = 0x517c_c1b7_2722_0a95;
    let mut hash: u64 = 0;
    for &b in bytes {
        hash = (hash.rotate_left(5) ^ u64::from(b)).wrapping_mul(seed);
    }
    format!("{hash:016x}")
}

// ============================================================================
// Index build
// ============================================================================

/// Build a lexical index over `repo_root`, writing it to `index_dir`.
fn build_index(repo_root: &Path, index_dir: &Path) -> anyhow::Result<()> {
    use ignore::WalkBuilder;

    std::fs::create_dir_all(index_dir)?;

    let mut builder = LexicalLayerBuilder::new(index_dir.to_path_buf(), repo_root.to_path_buf());
    let mut file_count: u64 = 0;

    let walker = WalkBuilder::new(repo_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();

        let language = match Language::from_path(path) {
            Some(lang) => lang,
            None => continue,
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // Skip binary or unreadable files.
        };

        // Use relative path from repo root so stored paths are portable.
        let rel_path = path.strip_prefix(repo_root).unwrap_or(path);

        if let Err(e) = builder.add_file(rel_path, &content, language) {
            eprintln!("warning: failed to index {}: {e}", rel_path.display());
            continue;
        }

        file_count += 1;
    }

    let _layer = Box::new(builder).build()?;
    eprintln!("Indexed {file_count} files.");
    Ok(())
}

// ============================================================================
// Stats
// ============================================================================

/// Print index statistics.
fn show_stats(index_dir: &Path, json_output: bool) -> anyhow::Result<ExitCode> {
    if !index_dir.join("metadata.json").exists() {
        eprintln!("No search index found. Run 'skim search --build' first.");
        return Ok(ExitCode::FAILURE);
    }

    let layer = match LexicalSearchLayer::open(index_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to open search index: {e}");
            return Ok(ExitCode::FAILURE);
        }
    };

    let stats = layer.stats();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        eprintln!("Search Index Statistics:");
        eprintln!("  Files indexed:   {}", stats.file_count);
        eprintln!("  N-grams:         {}", stats.total_ngrams);
        eprintln!("  Index size:      {} KB", stats.index_size_bytes / 1024);
        eprintln!("  Last updated:    {}", format_unix_timestamp(stats.last_updated));
        eprintln!("  Format version:  {}", stats.format_version);
    }

    Ok(ExitCode::SUCCESS)
}

/// Format a Unix timestamp as a human-readable string.
///
/// NOTE: Full date formatting would require chrono or time, which are not
/// current deps. We display the raw Unix timestamp and a UTC note instead.
/// Callers that need structured time should use `--json` and parse the field.
fn format_unix_timestamp(unix_secs: u64) -> String {
    format!("{unix_secs} (Unix timestamp)")
}

// ============================================================================
// Output
// ============================================================================

/// Print results as human-readable text to stdout.
fn print_text_results(
    layer: &LexicalSearchLayer,
    results: &[(FileId, f32)],
    query_text: &str,
    repo_root: &Path,
) -> anyhow::Result<()> {
    let mut stdout = std::io::BufWriter::new(std::io::stdout());

    for (file_id, score) in results {
        let rel_path = layer.file_table().lookup(*file_id);

        let path_str = rel_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());

        // Try to read the file to find a snippet.
        let snippet = rel_path.and_then(|p| {
            let abs = repo_root.join(p);
            std::fs::read_to_string(&abs)
                .ok()
                .and_then(|content| find_snippet(&content, query_text))
        });

        writeln!(stdout, "{}  score: {score:.2}", path_str)?;
        if let Some((line_num, line_text)) = &snippet {
            writeln!(stdout, "  {}:  {}", line_num, line_text.trim())?;
        }
        writeln!(stdout)?;
    }

    stdout.flush()?;
    Ok(())
}

/// Print results as JSON to stdout.
fn print_json_results(
    layer: &LexicalSearchLayer,
    results: &[(FileId, f32)],
    query_text: &str,
    repo_root: &Path,
) -> anyhow::Result<()> {
    let mut json_results = Vec::with_capacity(results.len());

    for (file_id, score) in results {
        let rel_path = layer.file_table().lookup(*file_id);

        let path_str = rel_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());

        let snippet = rel_path.and_then(|p| {
            let abs = repo_root.join(p);
            std::fs::read_to_string(&abs)
                .ok()
                .and_then(|content| find_snippet(&content, query_text))
        });

        json_results.push(serde_json::json!({
            "file": path_str,
            "score": score,
            "line": snippet.as_ref().map(|(n, _)| n),
            "snippet": snippet.as_ref().map(|(_, t)| t.trim()),
        }));
    }

    println!("{}", serde_json::to_string_pretty(&json_results)?);
    Ok(())
}

/// Find the first line in `content` that contains `query` (case-insensitive).
///
/// Returns `(1-indexed line number, line text)` on success.
/// Falls back to the first line of the file if no match is found.
fn find_snippet(content: &str, query: &str) -> Option<(usize, String)> {
    let lower_query = query.to_lowercase();

    for (idx, line) in content.lines().enumerate() {
        if line.to_lowercase().contains(&lower_query) {
            return Some((idx + 1, line.to_string()));
        }
    }

    // Fallback: return first non-empty line.
    content
        .lines()
        .enumerate()
        .find(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| (i + 1, l.to_string()))
}

// ============================================================================
// Clap command definition (used for shell completions)
// ============================================================================

/// Build clap command definition for shell completions.
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
                .action(clap::ArgAction::SetTrue)
                .help("Filter results by blast radius (high-impact changes)"),
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
