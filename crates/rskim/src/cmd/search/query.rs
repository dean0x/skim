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

use super::manifest::FileManifest;
use super::snippet::{SnippetOutcome, extract_snippet};
use super::staleness::auto_refresh_if_stale;
use super::types::{QueryConfig, QueryOutput, ResolvedResult};

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

    // Ensure the index is built and current.  The returned manifest is reused
    // below to avoid a second load from disk (duplicate-manifest-load fix).
    let (_refreshed, manifest) = auto_refresh_if_stale(root, cache_dir, analytics)?;

    // Open the reader.
    let reader = NgramIndexReader::open(cache_dir)?;
    let stats = reader.stats();
    let engine = QueryEngine::new(Box::new(reader));

    // Build the query.
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = Some(config.limit);

    // Hoist sorted_paths() so it is computed once and reused for both the
    // file_filter construction and the path-resolution step below.
    let sorted = manifest.sorted_paths();

    // When blast-radius paths are provided, convert them to FileId allowlist
    // so the search engine filters BEFORE applying the limit. This ensures
    // the limit applies to the filtered set rather than discarding matches
    // that fall outside the top-N unfiltered results.
    if let Some(ref allowed_paths) = config.blast_radius_paths {
        let mut file_ids = std::collections::HashSet::new();
        for (idx, path) in sorted.iter().enumerate() {
            if allowed_paths.contains(*path) {
                file_ids.insert(rskim_search::FileId(idx as u32));
            }
        }
        sq.file_filter = Some(file_ids);
    }

    // Execute the search.
    let raw_results = engine.search(&sq)?;

    // Resolve and enrich results.
    let results = resolve_paths_and_snippets(&raw_results, &sorted, root, &manifest);

    let total = results.len();
    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(QueryOutput {
        query: config.text.clone(),
        total,
        results,
        duration_ms,
        index_stats: Some(stats),
    })
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

            let (line_number, line_range, snippet, stale) =
                match extract_snippet(root, path, &r.match_positions, manifest_entry) {
                    SnippetOutcome::Ok {
                        match_line,
                        line_range,
                        context,
                    } => (Some(match_line), Some(line_range), Some(context), false),
                    SnippetOutcome::Stale => (None, None, None, true),
                    SnippetOutcome::Unavailable => (None, None, None, false),
                };

            Some(ResolvedResult {
                path: path.to_string(),
                score: r.score,
                field: r.field.name().to_string(),
                line_number,
                line_range,
                snippet,
                stale,
                match_positions: r.match_positions.clone(),
                temporal: None,
            })
        })
        .collect()
}

// ============================================================================
// Output formatters
// ============================================================================

/// Build an optional temporal annotation suffix for a single result line.
///
/// Examples:
/// - hotspot only  → `"  hotspot: 0.950"`
/// - risk only     → `"  risk: 0.800"`
/// - both          → `"  hotspot: 0.950  risk: 0.800"`
/// - neither       → `""`
fn temporal_annotation_tag(temporal: Option<&super::types::TemporalAnnotation>) -> String {
    let Some(t) = temporal else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(hs) = t.hotspot_score {
        parts.push(format!("hotspot: {hs:.3}"));
    }
    if let Some(rs) = t.risk_score {
        parts.push(format!("risk: {rs:.3}"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("  {}", parts.join("  "))
}

/// Format query results as human-readable text to `w`.
///
/// Format per result:
/// ```text
/// src/auth/middleware.rs:42  [function_signature]  score: 12.34  hotspot: 0.950
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
        let stale_tag = if r.stale { "  [stale]" } else { "" };

        // Compose optional temporal annotation suffix: "  hotspot: 0.95  risk: 0.80"
        let temporal_tag = temporal_annotation_tag(r.temporal.as_ref());

        writeln!(
            w,
            "{}{}  [{}]  score: {:.2}{}{}",
            r.path, line_info, r.field, r.score, stale_tag, temporal_tag
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
