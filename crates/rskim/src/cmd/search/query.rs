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

use rskim_search::{
    CompositeWeights, NgramIndexReader, QueryEngine, SearchLayer, SearchQuery, intersect_and_rank,
    recompose_with_lexical,
};

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

    // Hoist sorted_paths() so it is computed once and reused for both the
    // file_filter construction and the path-resolution step below.
    let sorted = manifest.sorted_paths();

    // Build the FileId allowlist from blast-radius paths.
    // Used for blast-radius-only and blast+AST paths.
    let blast_file_ids: Option<std::collections::HashSet<rskim_search::FileId>> = config
        .blast_radius_paths
        .as_ref()
        .map(|allowed_paths| super::temporal::paths_to_file_ids(&sorted, allowed_paths));

    // ── Compound text+AST path (#198) ─────────────────────────────────────────
    //
    // When `ast_scored` is Some, replace the old file_filter gate with true
    // weighted-RRF composite intersection:
    //
    //   1. Fetch a WIDER lexical candidate pool (limit * CANDIDATE_POOL_K) so
    //      files that rank lower in pure-lexical order but higher in composite
    //      order are not truncated before the blend sees them.
    //      temporal.rs uses limit.saturating_mul(5) with a .max(100) floor;
    //      the compound path uses K=4 (no floor) — a deliberately lighter pool
    //      because the intersection gate already narrows the candidate set.
    //   2. Optionally restrict lexical candidates by blast-radius (if set).
    //   3. Run intersect_and_rank: HashMap join + weighted RRF fusion.
    //   4. Recompose: carry the lexical SearchResult (snippet + line_range)
    //      with the composite RRF score replacing the raw lexical score (AC11).
    //   5. Truncate to --limit LAST (rank-then-truncate-LAST invariant).
    //
    // Structural refinement (depth-based via AstIndexReader) is not yet threaded
    // through the CLI layer — the AstIndexReader is opened in mod.rs and dropped
    // before execute_query_with_manifest is called.  Wiring it through is tracked
    // in #290 (thread AstIndexReader / pre-fetched FileId→StructuralMetrics map
    // into QueryConfig / execute_query_with_manifest to close this seam).
    // For 4a the structural lookup is a no-op; the RRF fusion of lexical+AST rank
    // alone replaces the old file_filter gate (#198).
    if let Some(ref ast_scored_vec) = config.ast_scored {
        return run_compound_query(
            config,
            ast_scored_vec,
            blast_file_ids,
            &engine,
            &sorted,
            root,
            &manifest,
            stats,
            start,
        );
    }
    // ── End compound text+AST path ────────────────────────────────────────────

    // ── Pure-lexical / blast-radius-only path (unchanged from #199) ──────────
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = Some(config.limit);

    // Build the FileId allowlist from blast-radius paths only (no AST in this path).
    if let Some(blast) = blast_file_ids {
        sq.file_filter = Some(blast);
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

/// Execute the compound text+AST query branch (#198).
///
/// Fetches a wider lexical candidate pool, applies an optional blast-radius
/// pre-filter, runs `intersect_and_rank` (HashMap join + weighted RRF fusion),
/// recomposes the results with lexical snippets, and returns a [`QueryOutput`].
///
/// Extracted from [`execute_query_with_manifest`] to give each path a
/// single-responsibility scope and eliminate the duplicated `QueryOutput`
/// construction tail.
///
/// # Errors
///
/// Returns `Err` when the lexical engine search fails.
#[allow(clippy::too_many_arguments)]
fn run_compound_query(
    config: &super::types::QueryConfig,
    ast_scored_vec: &[(rskim_search::FileId, f64)],
    blast_file_ids: Option<std::collections::HashSet<rskim_search::FileId>>,
    engine: &QueryEngine,
    sorted: &[&str],
    root: &Path,
    manifest: &FileManifest,
    stats: rskim_search::IndexStats,
    start: Instant,
) -> anyhow::Result<QueryOutput> {
    // Wider lexical pool before compound ranking.
    // K=4: lighter than temporal.rs (K=5 with .max(100) floor) because the
    // intersection gate already narrows candidates — no floor needed.
    const CANDIDATE_POOL_K: usize = 4;
    let mut sq = SearchQuery::new(config.text.clone());
    // saturating_mul: a hostile `--limit` near usize::MAX must not overflow.
    sq.limit = Some(config.limit.saturating_mul(CANDIDATE_POOL_K));

    // Apply blast-radius pre-filter when present (blast ∩ AST path).
    // Build a HashSet of AST FileIds once for O(1) membership tests — avoids
    // the O(blast × ast) nested scan that a linear .any() scan would produce.
    if let Some(ref blast) = blast_file_ids {
        let ast_fid_set: std::collections::HashSet<rskim_search::FileId> =
            ast_scored_vec.iter().map(|&(fid, _)| fid).collect();
        let intersection: std::collections::HashSet<rskim_search::FileId> = blast
            .iter()
            .filter(|id| ast_fid_set.contains(*id))
            .copied()
            .collect();
        sq.file_filter = Some(intersection);
    }
    // (No else: without blast-radius, no lexical file_filter — the compound
    // intersection acts as the filter, not the lexical engine's file_filter.)

    let raw_lex = engine.search(&sq)?;

    // Compound intersect + RRF fusion (pure, no I/O, closures only).
    // Structural lookup is a no-op until #290 threads AstIndexReader through
    // QueryConfig, at which point this closure is replaced with a real lookup.
    let no_metrics =
        |_fid: rskim_search::FileId| -> Option<rskim_search::StructuralMetrics> { None };
    let ranked = intersect_and_rank(
        &raw_lex,
        ast_scored_vec,
        no_metrics,
        0.0_f32, // avg_max_depth — placeholder until #290
        CompositeWeights::default(),
    );

    // Recompose: carry lexical SearchResult (snippet + line_range), replace score.
    // Truncate to --limit LAST (rank-then-truncate-LAST invariant, Amendment).
    let recomposed: Vec<rskim_search::SearchResult> = recompose_with_lexical(&ranked, &raw_lex)
        .into_iter()
        .take(config.limit)
        .collect();

    let results = resolve_paths_and_snippets(&recomposed, sorted, root, manifest);
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
