//! Output formatting for `skim search` results and stats.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use rskim_search::{FileId, SearchIndex};

// ============================================================================
// Stats
// ============================================================================

/// Print index statistics.
pub(super) fn show_stats(layer: &dyn SearchIndex, json_output: bool) -> anyhow::Result<ExitCode> {
    let stats = layer.stats();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        eprintln!("Search Index Statistics:");
        eprintln!("  Files indexed:   {}", stats.file_count);
        eprintln!("  N-grams:         {}", stats.total_ngrams);
        eprintln!("  Index size:      {} KB", stats.index_size_bytes / 1024);
        eprintln!(
            "  Last updated:    {}",
            format_unix_timestamp(stats.last_updated)
        );
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
// Result output
// ============================================================================

/// A resolved search result with the display path and optional snippet.
struct ResolvedResult {
    path_str: String,
    score: f32,
    snippet: Option<(usize, String)>,
}

/// Resolve a `(FileId, score)` pair to a path string and snippet.
fn resolve_result(
    layer: &dyn SearchIndex,
    file_id: FileId,
    score: f32,
    query_text: &str,
    repo_root: &Path,
) -> ResolvedResult {
    let rel_path = layer.file_table().lookup(file_id);

    let path_str = rel_path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    let snippet = rel_path.and_then(|p| {
        let abs = repo_root.join(p);
        std::fs::read_to_string(&abs)
            .ok()
            .and_then(|content| find_snippet(&content, query_text))
    });

    ResolvedResult {
        path_str,
        score,
        snippet,
    }
}

/// Print results as human-readable text to stdout.
pub(super) fn print_text_results(
    layer: &dyn SearchIndex,
    results: &[(FileId, f32)],
    query_text: &str,
    repo_root: &Path,
) -> anyhow::Result<()> {
    let mut stdout = std::io::BufWriter::new(std::io::stdout());

    for &(file_id, score) in results {
        let r = resolve_result(layer, file_id, score, query_text, repo_root);
        writeln!(stdout, "{}  score: {:.2}", r.path_str, r.score)?;
        if let Some((line_num, line_text)) = &r.snippet {
            writeln!(stdout, "  {}:  {}", line_num, line_text.trim())?;
        }
        writeln!(stdout)?;
    }

    stdout.flush()?;
    Ok(())
}

/// Print results as JSON to stdout.
pub(super) fn print_json_results(
    layer: &dyn SearchIndex,
    results: &[(FileId, f32)],
    query_text: &str,
    repo_root: &Path,
) -> anyhow::Result<()> {
    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|&(file_id, score)| {
            let r = resolve_result(layer, file_id, score, query_text, repo_root);
            serde_json::json!({
                "file": r.path_str,
                "score": r.score,
                "line": r.snippet.as_ref().map(|(n, _)| n),
                "snippet": r.snippet.as_ref().map(|(_, t)| t.trim()),
            })
        })
        .collect();

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
