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
/// This is the canonical interface used by `query_tests.rs` and `ast_tests.rs`.
/// Production dispatch in `mod.rs` calls [`execute_query_with_manifest`] directly
/// to thread a pre-loaded manifest and avoid a redundant refresh on the combined
/// text+`--ast` path.
///
/// # Errors
///
/// Returns `Err` on I/O failures or if the index is corrupt.
// Used by query_tests.rs and ast_tests.rs (both #[cfg(test)] callers); the
// production path in mod.rs calls execute_query_with_manifest directly.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn execute_query(
    config: &QueryConfig,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<QueryOutput> {
    execute_query_with_manifest(config, None, analytics)
}

/// Execute a search query, optionally reusing a pre-loaded manifest.
///
/// `pre_loaded_manifest` may be `Some` when the caller has already called
/// `auto_refresh_if_stale` (e.g. the combined text+`--ast` path in `run_query`
/// refreshes before opening the AST engine and passes the resulting manifest
/// here to avoid a redundant disk load). When `None`, the function calls
/// `auto_refresh_if_stale` itself — this is the pure-lexical (no `--ast`) path.
///
/// # Errors
///
/// Returns `Err` on I/O failures or if the index is corrupt.
pub(super) fn execute_query_with_manifest(
    config: &QueryConfig,
    pre_loaded_manifest: Option<FileManifest>,
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

    // Ensure the index is built and current.  When the caller already refreshed
    // (combined text+--ast path), reuse the manifest they provide to avoid a
    // redundant check_staleness + FileManifest::load on an already-current index.
    // Pure-lexical path (no --ast): refreshes here exactly once.
    let manifest = match pre_loaded_manifest {
        Some(m) => m,
        None => {
            let (_refreshed, m) = auto_refresh_if_stale(root, cache_dir, analytics)?;
            m
        }
    };

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

    // Build the FileId allowlist from blast-radius paths + AST file IDs.
    // Intersection logic:
    // - blast-radius only: path-based allowlist (path → FileId).
    // - AST only: use the AST FileId set directly (moved — no clone).
    // - Both: intersection of the two sets (FileId-level, no path round-trip).
    // - Neither: no filter (sq.file_filter stays None).
    let blast_file_ids: Option<std::collections::HashSet<rskim_search::FileId>> =
        if let Some(ref allowed_paths) = config.blast_radius_paths {
            let mut file_ids = std::collections::HashSet::new();
            for (idx, path) in sorted.iter().enumerate() {
                if allowed_paths.contains(*path) {
                    // PF-004: widen idx (usize) to u32 before constructing FileId.
                    // The file cap (50 000) guarantees no overflow, but `try_from`
                    // makes the widening explicit and safe by construction.
                    if let Ok(id) = u32::try_from(idx) {
                        file_ids.insert(rskim_search::FileId(id));
                    }
                }
            }
            if file_ids.is_empty() {
                eprintln!(
                    "skim search: blast-radius filter matched 0 indexed files \
                     (allowed {} paths, index has {} files)",
                    allowed_paths.len(),
                    sorted.len()
                );
            }
            Some(file_ids)
        } else {
            None
        };

    // Clone once so all match arms receive an owned value without per-arm clones.
    let ast_file_ids = config.ast_file_ids.clone();

    match (blast_file_ids, ast_file_ids) {
        (Some(blast), Some(ast)) => {
            // Intersection: only files in BOTH sets.
            let intersection: std::collections::HashSet<rskim_search::FileId> = blast
                .iter()
                .filter(|id| ast.contains(*id))
                .copied()
                .collect();
            sq.file_filter = Some(intersection);
        }
        (Some(blast), None) => {
            sq.file_filter = Some(blast);
        }
        (None, Some(ast)) => {
            sq.file_filter = Some(ast);
        }
        (None, None) => {
            // No filter.
        }
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
