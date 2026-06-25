//! Query execution — search the index and format results.
//!
//! # Data flow
//!
//! 1. Check for `index.skidx` — auto-build on cold start.
//! 2. Check staleness (git HEAD) — rebuild if stale.
//! 3. Open `NgramIndexReader`, wrap in `QueryEngine`.
//! 4. Execute the query, get `Vec<SearchResult>` with `FileId`s.
//! 5. Load `FileManifest`, map `FileId → path` via `sorted_paths()`.
//! 6. For each result, verify substring membership + extract snippet (single read,
//!    AD-355-1).
//! 7. Truncate to `--limit` LAST — after verification drops non-matching candidates
//!    (AD-355-2).
//! 8. Return `QueryOutput`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use rskim_search::{
    CompositeWeights, FileId, IndexStats, NgramIndexReader, QueryEngine, SearchLayer, SearchQuery,
    SearchResult, StructuralMetrics, intersect_and_rank, merge_layer_scores,
    recompose_with_lexical,
};

use super::manifest::FileManifest;
use super::snippet::{SnippetOutcome, extract_snippet_and_verify};
use super::staleness::auto_refresh_if_stale;
use super::types::{QueryConfig, QueryOutput, ResolvedResult};

// ============================================================================
// Candidate-pool sizing (AD-355-2)
// ============================================================================
//
// The three paths (pure-lexical, compound text+AST, blast-radius) each widen
// their candidate pool before the verify-then-truncate-LAST step.  All three
// pools are defined here in one place so the "how wide must the pre-verify pool
// be" decision has a single reason to change.
//
// `candidate_pool(limit, k)` returns `max(limit * k, CANDIDATE_POOL_FLOOR)` so
// every path uses the same floor policy.  Calibrating K per-path is tracked in
// #356 (pool-K calibration) per ADR-003 (grounded measurements before changing).
//
// Current values — no measured corpus basis; #356 owns calibration:
//   LEXICAL_CANDIDATE_POOL_K = 5  (pure-lexical, with floor)
//   COMPOUND_CANDIDATE_POOL_K = 4 (text+AST compound, no floor: intersection already narrows)
//   BLAST_CANDIDATE_POOL_K   = 10 (blast-radius composite UNION, with floor)
//
// Note: COMPOUND_CANDIDATE_POOL_K intentionally has no floor here; the compound
// path uses `saturating_mul` without `.max(floor)` because the intersection gate
// already narrows candidates aggressively.  If this becomes a correctness concern
// for small --limit values, add a floor in run_compound_query and update #356.

/// Shared floor for `candidate_pool`: every widened pool has at least this many
/// slots so small `--limit` values do not starve the verify step.
const CANDIDATE_POOL_FLOOR: usize = 100;

/// Compute the pre-verify candidate pool size for a given path K multiplier.
///
/// Returns `limit.saturating_mul(k).max(CANDIDATE_POOL_FLOOR)`.
///
/// This is the single place that enforces the floor policy; callers that
/// intentionally omit the floor (compound path — see note above) call
/// `limit.saturating_mul(k)` directly.
#[inline]
fn candidate_pool(limit: usize, k: usize) -> usize {
    limit.saturating_mul(k).max(CANDIDATE_POOL_FLOOR)
}

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

    // ── Pure-lexical path (no blast-radius, no AST) ──────────────────────────
    //
    // AD-355-2: The candidate pool is widened before verification and truncation.
    //
    // Without widening, the reader truncates to `--limit` BEFORE we can verify
    // substring membership: the definer file may already have been discarded below
    // incidental-overlap junk that happens to share a few trigrams.  By fetching
    // LEXICAL_CANDIDATE_POOL_K × limit candidates first, we ensure verification
    // acts as a true filter over the ranked list, not over an already-truncated
    // stub.  After verification the result set is truncated to `--limit` LAST.
    //
    // K=5, floor CANDIDATE_POOL_FLOOR: matches the temporal.rs resort_window() heuristic
    // so the two paths behave consistently.  This value has no measured corpus basis;
    // calibrating K for large corpora is tracked in #356.
    //
    // AD-355-4: Dropping non-matching candidates is a relevance gate, NOT an output
    // elision/cap under #317 "compress-never-truncate".  It does not hide output that
    // the user would otherwise see; it removes candidates that do not satisfy the
    // literal query.  No `elision_marker` is needed here.
    const LEXICAL_CANDIDATE_POOL_K: usize = 5;
    let pool_limit = candidate_pool(config.limit, LEXICAL_CANDIDATE_POOL_K);
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = Some(pool_limit);

    // Execute the search over the wider candidate pool.
    let raw_results = engine.search(&sq)?;

    // Resolve snippets, verify substring membership, then truncate to --limit LAST.
    //
    // AD-355-2 / AD-355-4: verify-then-truncate-LAST invariant.
    let results = resolve_paths_and_snippets_verified(
        &raw_results,
        &sorted,
        root,
        &manifest,
        &[],
        &config.text,
        config.limit,
    );

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
    // Wider lexical pool before compound ranking (see module-level pool-sizing note).
    // K=4: lighter than lexical (K=5) / blast (K=10) because the intersection gate
    // already narrows candidates — no floor applied here (see COMPOUND_CANDIDATE_POOL_K
    // note at the top of this file).  A file that is in both AST and lexical sets but
    // ranks beyond position limit*4 in the unfiltered lexical ranking will be silently
    // excluded.  Calibrating K for large corpora is tracked in #290.
    //
    // Performance note (AD-355-2): `recompose_with_lexical` (below) does `(*lex).clone()`
    // per surviving entry and operates on the FULL `ranked` list (limit×K entries), NOT
    // a pre-truncated slice.  This is required to preserve the verify-then-truncate-LAST
    // invariant: pre-truncation would drop the real definer before verification can keep
    // it.  The accepted cost is up to K×limit `SearchResult` clones (each carrying a
    // `Vec<Range<usize>>`) instead of `limit` — a bounded (4×) increase in clone work.
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

    // Recompose: carry lexical SearchResult (snippet + line_range), replace score (AC11).
    // NOTE: recompose_with_lexical operates on the FULL `ranked` list (limit×CANDIDATE_POOL_K
    // entries), not a pre-truncated slice — this preserves the AD-355-2 verify-then-truncate-LAST
    // invariant.  We MUST NOT truncate to config.limit here; if we did, and the top `limit`
    // RRF slots were occupied by incidental-overlap junk, the real definer at slot limit+1
    // would be dropped before verification could keep it and the junk is removed.
    // Truncation happens LAST in resolve_paths_and_snippets_verified (after verification
    // filters non-matching candidates), matching the pure-lexical and blast-radius paths.
    let recomposed = recompose_with_lexical(&ranked, &raw_lex);

    // AC-F6: text+AST compound path → layers_matched = ["lexical","ast"] (stable order).
    //
    // AD-355-2/AD-355-4: verify substring membership over the FULL recomposed list,
    // then truncate to --limit LAST.  The candidate pool is limit×CANDIDATE_POOL_K (K=4),
    // so recomposed has up to limit*4 entries.  Verification drops non-matching candidates
    // (relevance gate, not a #317 cap); truncation to config.limit happens inside
    // resolve_paths_and_snippets_verified as the final step.
    let results = resolve_paths_and_snippets_verified(
        &recomposed,
        ctx.sorted,
        ctx.root,
        ctx.manifest,
        &["lexical", "ast"],
        &config.text,
        config.limit,
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

    // Step 1: fetch a WIDE lexical ranked list WITHOUT a file_filter.
    //
    // The UNION contract requires ranking the complete candidate set (all files
    // that appear in *either* the lexical or temporal list) before truncation.
    // Applying a bare `config.limit` pre-cap here would silently drop co-change
    // partners whose lexical rank exceeds the cap, violating the rank-then-
    // truncate-LAST invariant.
    //
    // We set sq.limit = Some(K × limit).max(100) on EVERY path — trigram-scored
    // and short-query fallback alike.  The reader's `unwrap_or(20)` default is
    // never reached on this path because we always pass Some(N>=100).
    //
    // K=10: generous multiple of limit so RRF fusion still sees enough candidates
    // for the co-change-UNION to work correctly even if many lexical hits fail
    // verification.  The worst-case file reads are O(K × limit) per query; on
    // the AD-355-7 short-query fallback the candidate set is still bounded to
    // Some(K × limit).max(100) before the verify step.  Calibrating K for large
    // corpora is tracked in #356 (pool-K calibration).
    const BLAST_CANDIDATE_POOL_K: usize = 10;
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = Some(candidate_pool(config.limit, BLAST_CANDIDATE_POOL_K));
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
    // The blast-radius path fuses only the lexical and co-change (temporal)
    // signals, so only those two layers are constructed here. The `ast` weight
    // (0.3 by default) and the extended signals (import_graph, dir_proximity,
    // structural_coupling — all 0.0 by default per ADR-003) have no layer to
    // apply to on this path; wiring the full text+AST+temporal compound dispatch
    // is tracked in #339.
    let layers: &[(Vec<(FileId, f64)>, f64)] = &[
        (lexical_layer, weights.lexical),
        (temporal_layer, weights.temporal),
    ];
    let ranked = merge_layer_scores(layers);

    // Step 5: rank the full UNION set, then apply verification + truncation LAST.
    //
    // AD-355-2: do NOT truncate before verification.  The UNION contract requires
    // all candidates to be ranked before any are dropped.  After verification the
    // count is capped at --limit.
    //
    // AD-355-4: dropping a lexical-hit candidate that fails substring verification
    // is a relevance gate, not a #317 output cap.  No elision_marker needed.
    let lex_map: HashMap<FileId, &SearchResult> = raw_lex.iter().map(|r| (r.file_id, r)).collect();

    // Step 6: recompose results with verification for lexical-hit candidates.
    //
    // For files present in the lexical pool: read snippet + verify substring
    // membership in a SINGLE file read via extract_snippet_and_verify (AD-355-1).
    // Drop the candidate if verification fails.
    //
    // For co-change-only files (absent from lexical pool): no file content is
    // available here; these are pure temporal results that the text query did not
    // match — include them unconditionally (AC12, UNION mode).
    let results: Vec<super::types::ResolvedResult> = ranked
        .iter()
        .filter_map(|&(fid, composite_score)| {
            let path = ctx.sorted.get(fid.0 as usize)?;
            let manifest_entry = ctx.manifest.lookup(path);

            if let Some(&lex_result) = lex_map.get(&fid) {
                // File has a lexical hit: verify and extract snippet in one read
                // (AD-355-1 — no second I/O).
                let mut r = lex_result.clone();
                r.score = composite_score;

                let (snippet_outcome, verified) = extract_snippet_and_verify(
                    ctx.root,
                    path,
                    &r.match_positions,
                    manifest_entry,
                    &config.text,
                );

                // Drop lexical-hit candidates that do not contain the query.
                // Stale files produce verified=false and are dropped — positions
                // may be wrong and we cannot confirm content without re-reading.
                if !verified {
                    return None;
                }

                let (line_number, line_range, snippet, stale) = decode_snippet(snippet_outcome);
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
                // No file content is read here; include unconditionally.
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
        // AD-355-2: truncate to --limit LAST — after verification, not before.
        .take(config.limit)
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

/// Map `FileId`s to paths, extract snippets, **verify substring membership**,
/// and truncate to `limit` — all in one pass with a single file read per result.
///
/// # Design (AD-355-1 / AD-355-2 / AD-355-3 / AD-355-4)
///
/// Candidate-then-verify: the caller fetches a **wider** candidate pool
/// (`LEXICAL_CANDIDATE_POOL_K × limit`) so the definer file is not truncated
/// before verification.  This fn then:
///
/// 1. Reads each file once via [`extract_snippet_and_verify`] — no second I/O.
/// 2. Drops any candidate whose file content does not contain the literal query
///    as an AND-of-whitespace-tokens (case-sensitive; see AD-355-3).
/// 3. Truncates to `limit` LAST — after verification, not before (AD-355-2).
///
/// Dropping non-matching candidates is a **relevance gate**, not a #317 output
/// cap.  No `elision_marker` is needed (AD-355-4).
fn resolve_paths_and_snippets_verified(
    raw_results: &[SearchResult],
    sorted_paths: &[&str],
    root: &Path,
    manifest: &FileManifest,
    layers_matched: &[&'static str],
    query: &str,
    limit: usize,
) -> Vec<ResolvedResult> {
    raw_results
        .iter()
        .filter_map(|r| {
            let path = sorted_paths.get(r.file_id.0 as usize)?;
            let manifest_entry = manifest.lookup(path);

            // Read file once; verify and extract snippet in one call (AD-355-1).
            let (outcome, verified) =
                extract_snippet_and_verify(root, path, &r.match_positions, manifest_entry, query);

            // Drop candidates that do not contain the literal query.
            // Stale files produce verified=false and are also dropped — we
            // cannot confirm their content matches without re-reading.
            if !verified {
                return None;
            }

            let (line_number, line_range, snippet, stale) = decode_snippet(outcome);

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
        // AD-355-2: truncate LAST — after verification removes non-matching candidates.
        .take(limit)
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
