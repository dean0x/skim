//! Query execution — search the index and format results.
//!
//! # Data flow
//!
//! 1. Check for `index.skidx` — auto-build on cold start.
//! 2. Check staleness (git HEAD) — rebuild if stale.
//! 3. Open `NgramIndexReader`, wrap in `QueryEngine`.
//! 4. Execute the query, get `Vec<SearchResult>` with `FileId`s.
//! 5. Load `FileManifest`, map `FileId → path` via `sorted_paths()`.
//! 6. For each result, attempt `extract_snippet`.
//! 7. Return `QueryOutput`.

use std::path::Path;
use std::time::Instant;

use rskim_search::{NgramIndexReader, QueryEngine, SearchLayer, SearchQuery};

use super::index::build_index;
use super::manifest::FileManifest;
use super::snippet::extract_snippet;
use super::staleness::{StalenessCheck, check_staleness};
use super::types::{IndexConfig, QueryConfig, QueryOutput, ResolvedResult};

// ============================================================================
// Query execution
// ============================================================================

/// Execute a search query against the index.
///
/// Handles auto-build on cold start and staleness refresh transparently.
///
/// # Errors
///
/// Returns `Err` on I/O failures or if the index is corrupt.
pub(super) fn execute_query(
    config: &QueryConfig,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<QueryOutput> {
    let start = Instant::now();

    // Empty query short-circuits before any I/O.
    if config.text.is_empty() {
        return Ok(QueryOutput {
            query: config.text.clone(),
            total: 0,
            results: vec![],
            duration_ms: start.elapsed().as_millis() as u64,
            index_stats: None,
        });
    }

    let cache_dir = &config.cache_dir;
    let root = &config.root;

    // Ensure the index is built and current.
    ensure_index_ready(root, cache_dir, analytics)?;

    // Open the reader.
    let reader = NgramIndexReader::open(cache_dir)?;
    let stats = reader.stats();
    let engine = QueryEngine::new(Box::new(reader));

    // Build the query.
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = Some(config.limit);

    // Execute the search.
    let raw_results = engine.search(&sq)?;

    // Load manifest for FileId → path resolution.
    let manifest = FileManifest::load(root.to_path_buf(), cache_dir.to_path_buf())?;
    let sorted = manifest.sorted_paths();

    // Resolve and enrich results.
    let results = resolve_paths_and_snippets(&raw_results, &sorted, root, &manifest);

    let total = results.len();
    let duration_ms = start.elapsed().as_millis() as u64;

    let _ = analytics; // future: record query analytics

    Ok(QueryOutput {
        query: config.text.clone(),
        total,
        results,
        duration_ms,
        index_stats: Some(stats),
    })
}

/// Ensure the index is present and not stale, building or refreshing as needed.
fn ensure_index_ready(
    root: &Path,
    cache_dir: &Path,
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<()> {
    match check_staleness(cache_dir, root) {
        StalenessCheck::Current => {
            // Index is up to date — nothing to do.
        }
        StalenessCheck::NoIndex => {
            eprintln!("skim search: building index…");
            let config = IndexConfig {
                root: root.to_path_buf(),
                max_files: None,
                force: false,
                cache_dir_override: Some(cache_dir.to_path_buf()),
            };
            let result = build_index(&config)?;
            eprintln!(
                "skim search: indexed {} files in {:.1}s",
                result.file_count,
                result.duration.as_secs_f64()
            );
        }
        StalenessCheck::HeadChanged { stored, current } => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: HEAD changed ({} -> {}), refreshing…",
                    &stored[..8.min(stored.len())],
                    &current[..8.min(current.len())]
                );
            } else {
                eprintln!("skim search: index stale (HEAD changed), refreshing…");
            }
            let config = IndexConfig {
                root: root.to_path_buf(),
                max_files: None,
                force: false,
                cache_dir_override: Some(cache_dir.to_path_buf()),
            };
            build_index(&config)?;
        }
        StalenessCheck::NoStoredHead => {
            eprintln!("skim search: refreshing index (no HEAD recorded)…");
            let config = IndexConfig {
                root: root.to_path_buf(),
                max_files: None,
                force: false,
                cache_dir_override: Some(cache_dir.to_path_buf()),
            };
            build_index(&config)?;
        }
    }
    Ok(())
}

/// Map `FileId`s to paths and extract snippets.
fn resolve_paths_and_snippets(
    raw_results: &[rskim_search::SearchResult],
    sorted_paths: &[&str],
    root: &Path,
    manifest: &FileManifest,
) -> Vec<ResolvedResult> {
    raw_results
        .iter()
        .filter_map(|r| {
            let path = sorted_paths.get(r.file_id.0 as usize)?;

            let manifest_entry = manifest.lookup(path);
            let match_positions: Vec<std::ops::Range<usize>> = r.match_positions.clone();

            let (line_number, snippet) =
                match extract_snippet(root, path, &match_positions, manifest_entry) {
                    Some((ln, ctx)) => (Some(ln), Some(ctx)),
                    None => (None, None),
                };

            Some(ResolvedResult {
                path: path.to_string(),
                score: r.score,
                field: r.field.name().to_string(),
                line_number,
                snippet,
                match_positions,
            })
        })
        .collect()
}

// ============================================================================
// Output formatters
// ============================================================================

/// Format query results as human-readable text to `w`.
///
/// Format per result:
/// ```text
/// src/auth/middleware.rs:42  [function_signature]  score: 12.34
///   41│ /// Validates JWT token
///   42│ pub fn authenticate(req: &Request) -> Result<Claims> {
///   43│     let header = req.header("Authorization")
/// ```
pub(super) fn format_text_output(
    output: &QueryOutput,
    w: &mut impl std::io::Write,
) -> anyhow::Result<()> {
    if output.results.is_empty() {
        writeln!(w, "no results for {:?}", output.query)?;
        return Ok(());
    }

    for r in &output.results {
        let line_info = r.line_number.map(|ln| format!(":{ln}")).unwrap_or_default();
        writeln!(
            w,
            "{}{}  [{}]  score: {:.2}",
            r.path, line_info, r.field, r.score
        )?;

        if let Some(ctx) = &r.snippet {
            for line in &ctx.lines {
                let marker = if line.is_match { ">" } else { " " };
                writeln!(w, "  {}  {:>4}│ {}", marker, line.line_number, line.content)?;
            }
        }
        writeln!(w)?;
    }

    writeln!(w, "{} result(s) in {}ms", output.total, output.duration_ms)?;

    Ok(())
}

/// Format query results as a JSON object to `w`.
pub(super) fn format_json_output(
    output: &QueryOutput,
    w: &mut impl std::io::Write,
) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(output)?;
    writeln!(w, "{json}")?;
    Ok(())
}

// ============================================================================
// Tests (co-located in query_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
