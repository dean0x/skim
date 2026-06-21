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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use rskim_search::{
    CompositeWeights, FileId, IndexStats, NgramIndexReader, QueryEngine, SearchLayer, SearchQuery,
    SearchResult, StructuralMetrics, intersect_and_rank, merge_layer_scores,
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
    let blast_file_ids: Option<HashSet<FileId>> = config
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
            QueryContext {
                engine: &engine,
                sorted: &sorted,
                root,
                manifest: &manifest,
                stats,
                start,
            },
        );
    }
    // ── End compound text+AST path ────────────────────────────────────────────

    // ── Composite UNION blast-radius path (#200) ──────────────────────────────
    //
    // When blast_radius_paths is set AND there is no AST filter, replace the old
    // file_filter (set-intersection) approach with UNION re-ranking via composite
    // weighted RRF:
    //
    //   1. Fetch a WIDER lexical pool (limit * CANDIDATE_POOL_K) WITHOUT a
    //      file_filter so text-only matches outside the co-change partner set
    //      are still present in the lexical ranked list.
    //   2. Build a temporal ranked list from the co-change partner set:
    //      each partner gets an equal score of 1.0 so they all contribute rank
    //      terms to the RRF fusion.
    //   3. Run merge_layer_scores over [lexical, temporal] with the composite
    //      weights from config or the default profile.
    //   4. Recompose: carry the lexical SearchResult (snippet + line_range)
    //      for files that appear in the lexical pool.  Files present ONLY in the
    //      temporal list (co-change-only) get a stub result with the fused score.
    //   5. Truncate to --limit LAST (rank-then-truncate-LAST invariant).
    //
    // UNION semantics (AC12): a co-change partner absent from the lexical list
    // is still returned, ranked by its temporal RRF term alone (lexical absent →
    // contributes 0 under graceful absence).
    //
    // AC11 (temporal source): temporal ranked list is built from blast_radius_paths
    // which is resolved from TemporalDb::cochanges_for_file — the same store the
    // CLI blast-radius used before #200.  This satisfies AC11 (source identity).
    if config.blast_radius_paths.is_some() {
        return run_blast_radius_composite_query(
            config,
            &blast_file_ids,
            QueryContext {
                engine: &engine,
                sorted: &sorted,
                root,
                manifest: &manifest,
                stats,
                start,
            },
        );
    }
    // ── End composite UNION blast-radius path ─────────────────────────────────

    // ── Pure-lexical path (no blast-radius, no AST — unchanged) ──────────────
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = Some(config.limit);

    // Execute the search.
    let raw_results = engine.search(&sq)?;

    // Resolve and enrich results.
    let results = resolve_paths_and_snippets(&raw_results, &sorted, root, &manifest, &[]);

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

/// Execution context threaded through the compound query path.
///
/// Bundles the read-only index state that is computed once in
/// [`execute_query_with_manifest`] and forwarded to [`run_compound_query`].
/// This eliminates the >5 positional argument count and makes the caller
/// site self-documenting.
struct QueryContext<'a> {
    engine: &'a QueryEngine,
    sorted: &'a [&'a str],
    root: &'a Path,
    manifest: &'a FileManifest,
    stats: IndexStats,
    start: Instant,
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
fn run_compound_query(
    config: &super::types::QueryConfig,
    ast_scored_vec: &[(FileId, f64)],
    blast_file_ids: Option<HashSet<FileId>>,
    ctx: QueryContext<'_>,
) -> anyhow::Result<QueryOutput> {
    // Wider lexical pool before compound ranking.
    // K=4: lighter than temporal.rs (K=5 with .max(100) floor) because the
    // intersection gate already narrows candidates — no floor needed.
    // NOTE: K=4 is a heuristic with no measured corpus basis; a file that is
    // in both AST and lexical sets but ranks beyond position limit*4 in the
    // unfiltered lexical ranking will be silently excluded.  This is the
    // intentional trade-off (enables composite ranking vs old file_filter gate).
    // Calibrating K for large corpora is tracked in #290.
    const CANDIDATE_POOL_K: usize = 4;
    let mut sq = SearchQuery::new(config.text.clone());
    // saturating_mul: a hostile `--limit` near usize::MAX must not overflow.
    sq.limit = Some(config.limit.saturating_mul(CANDIDATE_POOL_K));

    // Apply blast-radius pre-filter when present (blast ∩ AST path).
    // Build a HashSet of AST FileIds once for O(1) membership tests — avoids
    // the O(blast × ast) nested scan that a linear .any() scan would produce.
    if let Some(ref blast) = blast_file_ids {
        let ast_fid_set: HashSet<FileId> = ast_scored_vec.iter().map(|&(fid, _)| fid).collect();
        let intersection: HashSet<FileId> = blast
            .iter()
            .filter(|id| ast_fid_set.contains(*id))
            .copied()
            .collect();
        sq.file_filter = Some(intersection);
    }
    // (No else: without blast-radius, no lexical file_filter — the compound
    // intersection acts as the filter, not the lexical engine's file_filter.)

    let raw_lex = ctx.engine.search(&sq)?;

    // Compound intersect + RRF fusion (pure, no I/O, closures only).
    //
    // WAVE 4a — structural seam is a no-op:
    // The structural_lookup and avg_max_depth parameters exist to enable
    // depth-based AST re-ranking (AC2/AC12), but on this production path both
    // are placeholders deferred to #290.  As a result every entry's depth_key
    // is 0.0 and the AST decorate-sort reduces to pure ast_score-DESC order.
    // The shipped Wave 4a ranking is lexical-rank + AST-score-rank RRF only.
    let ranked = intersect_and_rank(
        &raw_lex,
        ast_scored_vec,
        |_: FileId| -> Option<StructuralMetrics> { None }, // structural seam — placeholder until #290
        0.0_f32,                                           // avg_max_depth — placeholder until #290
        CompositeWeights::default(),
    );

    // Truncate to --limit BEFORE recompose (rank-then-truncate-LAST invariant, Amendment).
    // Truncating here bounds the clone work in recompose_with_lexical to O(limit) rather
    // than O(limit * CANDIDATE_POOL_K); without this, recompose clones every candidate
    // and .take(limit) discards up to 3*limit clones immediately.
    let ranked_limited: Vec<_> = ranked.into_iter().take(config.limit).collect();

    // Recompose: carry lexical SearchResult (snippet + line_range), replace score (AC11).
    let recomposed = recompose_with_lexical(&ranked_limited, &raw_lex);

    // AC-F6: text+AST compound path → layers_matched = ["lexical","ast"] (stable order).
    let results = resolve_paths_and_snippets(
        &recomposed,
        ctx.sorted,
        ctx.root,
        ctx.manifest,
        &["lexical", "ast"],
    );
    let total = results.len();
    let duration_ms = ctx.start.elapsed().as_millis() as u64;
    Ok(QueryOutput {
        query: config.text.clone(),
        total,
        results,
        duration_ms,
        index_stats: Some(ctx.stats),
    })
}

/// Execute the composite UNION blast-radius re-ranking path (#200).
///
/// Fuses the lexical ranked list and the temporal co-change ranked list into
/// a single composite ranking via weighted RRF (UNION semantics):
///
/// - Files present ONLY in the lexical list: contribute their lexical rank term.
/// - Files present ONLY in the temporal (co-change) list: contribute their
///   temporal rank term alone (graceful absence = 0 from the lexical layer).
///   These are co-change-only files that the text query did not match — they
///   APPEAR in UNION mode (AC12) but would be ABSENT under old filter mode.
/// - Files present in BOTH: accumulate both rank terms (AC2 multi-layer bonus).
///
/// The output score field carries the fused RRF value, NOT a BM25F magnitude
/// (AC14: score is documented as composite fused RRF in the doc comment below
/// and in the `ResolvedResult::score` field doc).
///
/// # Temporal ranked list construction (AC11 source identity)
///
/// The temporal ranked list is built from `blast_paths` (already resolved from
/// `TemporalDb::cochanges_for_file` — the same SQLite source the CLI used
/// before #200).  Each co-change partner path is assigned an equal score of
/// `1.0` (uniform temporal rank input) and converted to `FileId` via the
/// manifest's `sorted_paths`.  The Jaccard-value-aware ranking within the
/// temporal list is not preserved here; the RRF framework uses rank, not
/// magnitude, so the order within the temporal list only matters when there
/// are many co-change partners.  Improvement tracked for follow-up: use the
/// Jaccard score as the raw temporal score for better rank ordering (#200+).
fn run_blast_radius_composite_query(
    config: &super::types::QueryConfig,
    blast_file_ids: &Option<HashSet<FileId>>,
    ctx: QueryContext<'_>,
) -> anyhow::Result<QueryOutput> {
    // Effective weights: use caller-supplied override or the six-signal #200 profile.
    let weights = config
        .composite_weights
        .unwrap_or_else(CompositeWeights::with_six_signal_defaults);

    // Step 1: fetch the FULL lexical ranked list WITHOUT a file_filter and WITHOUT
    // a limit cap.  The UNION contract requires ranking the complete candidate set
    // (all files that appear in *either* the lexical or temporal list) before
    // truncation.  Applying a pre-limit here would silently drop co-change partners
    // whose lexical rank exceeds the cap, violating the rank-then-truncate-LAST
    // invariant (Cross-Plan Amendment, Intent Drift 3 fix).
    let mut sq = SearchQuery::new(config.text.clone());
    // No limit: we truncate AFTER fusion (Step 5). No file_filter: UNION mode.
    sq.limit = None;
    // No file_filter: UNION mode requires the full lexical ranked list.
    let raw_lex = ctx.engine.search(&sq)?;

    // Step 2: build the temporal ranked list from blast_paths.
    // Each co-change partner path → FileId (via sorted_paths index).
    // Score = 1.0 (uniform; RRF uses rank not magnitude, so this suffices).
    // The target file itself is included in blast_paths by resolve_blast_radius_paths.
    // When blast_file_ids is None (temporal DB absent), degrades to lexical-only ranking.
    let mut temporal_layer: Vec<(FileId, f64)> = blast_file_ids
        .as_ref()
        .map(|ids| ids.iter().map(|&fid| (fid, 1.0)).collect())
        .unwrap_or_default();
    // Sort by FileId for deterministic rank assignment within the layer.
    // All have equal scores, so the sort order determines their temporal ranks.
    temporal_layer.sort_unstable_by_key(|&(fid, _)| fid.0);

    // Step 3: lexical ranked list from raw_lex (already sorted DESC by score).
    let lexical_layer: Vec<(FileId, f64)> = raw_lex.iter().map(|r| (r.file_id, r.score)).collect();

    // Step 4: N-signal RRF UNION merge.
    // Only lexical and temporal signals are used in the blast-radius path.
    // AST, import_graph, dir_proximity, structural_coupling are all at 0.0
    // in the default profile (extended signals gated per ADR-003).
    let layers: &[(Vec<(FileId, f64)>, f64)] = &[
        (lexical_layer, weights.lexical),
        (temporal_layer, weights.temporal),
    ];
    let ranked = merge_layer_scores(layers);

    // Step 5: truncate to --limit LAST (rank-then-truncate-LAST invariant).
    let ranked_limited: Vec<_> = ranked.into_iter().take(config.limit).collect();

    // Step 6: recompose results.
    // For files present in the lexical pool: carry snippet + line_range from
    // the lexical SearchResult, replace score with the composite RRF value.
    // For co-change-only files (absent from lexical pool): produce a stub
    // ResolvedResult with score = fused RRF score and no snippet.
    let lex_map: HashMap<FileId, &SearchResult> = raw_lex.iter().map(|r| (r.file_id, r)).collect();

    let results: Vec<super::types::ResolvedResult> =
        ranked_limited
            .iter()
            .filter_map(|&(fid, composite_score)| {
                let path = ctx.sorted.get(fid.0 as usize)?;
                let manifest_entry = ctx.manifest.lookup(path);

                if let Some(&lex_result) = lex_map.get(&fid) {
                    // File has a lexical hit: carry its snippet/line data.
                    let mut r = lex_result.clone();
                    r.score = composite_score;
                    let (line_number, line_range, snippet, stale) = decode_snippet(
                        extract_snippet(ctx.root, path, &r.match_positions, manifest_entry),
                    );
                    Some(super::types::ResolvedResult {
                        path: path.to_string(),
                        score: composite_score,
                        field: r.field.name().to_string(),
                        line_number,
                        line_range,
                        snippet,
                        stale,
                        match_positions: r.match_positions.clone(),
                        temporal: None,
                        layers_matched: vec![],
                    })
                } else {
                    // Co-change-only file: no lexical hit → no snippet (AC12, UNION mode).
                    // These files appear because their temporal rank contributes to the
                    // fused score even though the text query did not match them.
                    Some(super::types::ResolvedResult {
                        path: path.to_string(),
                        score: composite_score,
                        field: "co_change_partner".to_string(),
                        line_number: None,
                        line_range: None,
                        snippet: None,
                        stale: false,
                        match_positions: vec![],
                        temporal: None,
                        layers_matched: vec![],
                    })
                }
            })
            .collect();

    let total = results.len();
    let duration_ms = ctx.start.elapsed().as_millis() as u64;
    Ok(QueryOutput {
        query: config.text.clone(),
        total,
        results,
        duration_ms,
        index_stats: Some(ctx.stats),
    })
}

/// Decode a `SnippetOutcome` into the 4-tuple used by `ResolvedResult`.
fn decode_snippet(
    outcome: SnippetOutcome,
) -> (
    Option<u32>,
    Option<std::ops::Range<usize>>,
    Option<super::types::SnippetContext>,
    bool,
) {
    match outcome {
        SnippetOutcome::Ok {
            match_line,
            line_range,
            context,
        } => (Some(match_line), Some(line_range), Some(context), false),
        SnippetOutcome::Stale => (None, None, None, true),
        SnippetOutcome::Unavailable => (None, None, None, false),
    }
}

/// Map `FileId`s to paths and extract snippets.
///
/// `layers_matched` is the set of layers that contributed non-zero signal for
/// every result on this path. For the pure-lexical path, pass `&[]` (empty →
/// serialised as absent via `skip_serializing_if`). For the text+AST compound
/// path, pass `&["lexical","ast"]` (AC-F6).
fn resolve_paths_and_snippets(
    raw_results: &[SearchResult],
    sorted_paths: &[&str],
    root: &Path,
    manifest: &FileManifest,
    layers_matched: &[&'static str],
) -> Vec<ResolvedResult> {
    raw_results
        .iter()
        .filter_map(|r| {
            let path = sorted_paths.get(r.file_id.0 as usize)?;

            let manifest_entry = manifest.lookup(path);

            let (line_number, line_range, snippet, stale) = decode_snippet(extract_snippet(
                root,
                path,
                &r.match_positions,
                manifest_entry,
            ));

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
                layers_matched: layers_matched.to_vec(),
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
