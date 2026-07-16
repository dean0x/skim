//! AST structural pattern query helpers for `skim search --ast`.
//!
//! # Responsibilities
//!
//! - Open the AST query engine with a clear error when the index is absent (#199).
//! - Validate the raw pattern string at the CLI boundary (before opening the index).
//! - Resolve a `--ast` pattern to scored `Vec<(FileId, f64)>` for compound RRF ranking (#198).
//! - Standalone AST query dispatch (`--ast` only, no text query).
//! - Output formatters: text and JSON for AST-only results (delegates to `rskim_search::compound::output`).
//! - Line anchor: for real-node patterns, gate + anchor run atomically before
//!   truncation (AD-397-4, #397); for synthetic patterns, anchor is recovered
//!   post-truncation via `recover_line` (AC-F1, #201).
//!
//! # Relationship to temporal.rs
//!
//! This module mirrors the structure of `temporal.rs` — one focused module per
//! search layer, with minimal hooks in `mod.rs`.
//!
//! # Crate split (#201)
//!
//! - Pure row type + format logic → `rskim_search::compound::output` (no I/O).
//! - File-reading snippet extraction → this module (reads disk, applies mtime guard).

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

use rskim_search::{AstIndexReader, AstQuery, AstQueryEngine, FileId, TemporalDb};
use rskim_search::{all_patterns, is_synthetic_id, parse_ast_query};
// #201: enriched row type + formatters from rskim-search.
// pub(super) re-exports so test module (ast_tests.rs) can call them as super::.
pub(super) use rskim_search::AstResult;
use rskim_search::{find_first_strict_match, pattern_occurs_in_file, recover_line};
pub(super) use rskim_search::{format_ast_json, format_ast_text};

// ============================================================================
// Local type aliases
// ============================================================================

/// Anchor returned by `find_first_strict_match` for a real-node AST match:
/// `(line_number, byte_range_in_file, snippet_text)`.
type AnchorHit = (u32, std::ops::Range<usize>, String);

/// One entry in the verified candidate pool before FileId→path resolution:
/// `(file_id, score, optional_anchor)`.
///
/// `None` anchor ⇒ synthetic-marker match (anchor computed post-truncation by
/// `recover_line`).  `Some` anchor ⇒ real-node match with cached first-match
/// position (AD-397-4).
type VerifiedEntry = (FileId, f64, Option<AnchorHit>);

use super::types::{Page, TemporalSort};

// ============================================================================
// Engine helpers
// ============================================================================

/// Open the AST query engine from `cache_dir`.
///
/// Returns a clear, actionable error when `ast_index.skidx` is missing or
/// corrupt — the user's intent was `--ast`, so we fail loud rather than degrade.
///
/// # Errors
///
/// Returns `Err` with build guidance when the index is absent or unreadable.
pub(super) fn open_ast_engine(cache_dir: &Path) -> anyhow::Result<AstQueryEngine<AstIndexReader>> {
    let idx_path = cache_dir.join("ast_index.skidx");
    if !idx_path.exists() {
        anyhow::bail!(
            "AST index not found at {}\n\
             Run `skim search --build` or `skim search --rebuild` to build the index.",
            idx_path.display()
        );
    }
    AstQueryEngine::open(cache_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to open AST index at {}: {e}\n\
             Run `skim search --rebuild` to rebuild from scratch.",
            cache_dir.display()
        )
    })
}

// ============================================================================
// Pattern validation
// ============================================================================

/// Validate a raw `--ast` string at the CLI boundary and return the parsed query.
///
/// Calls [`parse_ast_query`] after trimming whitespace.  Returns a friendly
/// error for:
/// - [`AstQuery::SingleNode`] → deferred to #283 (unigram index not yet built).
/// - Unknown pattern name → surfaces the library message (lists valid patterns).
/// - Query > 4096 bytes → surfaces the library message.
/// - Empty / whitespace-only → caught before this call in `parse_flags`.
///
/// Returning the parsed `AstQuery` avoids a second `parse_ast_query` call in
/// callers that need both validation and the query object (e.g. `run_ast_standalone`).
/// Callers that only need validation can discard the return value with `?; let _ =`.
///
/// # Errors
///
/// Returns `Err` on any invalid query, with a message ready for user display.
pub(super) fn validate_ast_pattern(raw: &str) -> anyhow::Result<AstQuery> {
    match parse_ast_query(raw.trim()) {
        Ok(AstQuery::SingleNode(_)) => {
            anyhow::bail!(
                "single-node structural search is not yet supported (tracked in #283).\n\
                 Use a named pattern (e.g. `--ast try-catch`) or a containment query \
                 (e.g. `--ast \"for_statement > await_expression\"`)."
            );
        }
        Ok(query) => Ok(query),
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

// ============================================================================
// FileId resolution
// ============================================================================

/// Resolve a `--ast` pattern to scored AST results for compound ranking (#198).
///
/// Parses the pattern and calls [`AstQueryEngine::search_ast`] directly,
/// which returns `Vec<(FileId, f64)>` sorted FileId-ASC — the frozen Wave-4
/// contract used by the compound intersector.  Scores are preserved so the
/// compound module can refine the AST ranked list with structural metrics.
///
/// **Changed from #199:** previously returned a lossy `HashSet<FileId>` (scores
/// discarded).  Now returns the full scored vec so `intersect_and_rank` can
/// build the AST rank map from actual scores.
///
/// **#374 scope note:** this compound-path entry applies Part A (AND-intersect,
/// inside `search_ast`) but NOT the Part B structural verify gate
/// (`pattern_occurs_in_file`) that `run_ast_standalone` applies. The compound
/// results are subsequently intersected with the lexical text set, so structural
/// false positives are bounded by the text match; extending the gate to this path
/// is a deliberate out-of-scope follow-up for #374 (would change ranking inputs and
/// needs its own tests).
///
/// # Errors
///
/// Returns `Err` when the pattern is invalid or the query fails.
pub(super) fn resolve_ast_scored(
    engine: &AstQueryEngine<AstIndexReader>,
    raw: &str,
) -> anyhow::Result<Vec<(FileId, f64)>> {
    let query =
        parse_ast_query(raw.trim()).map_err(|e| anyhow::anyhow!("invalid AST pattern: {e}"))?;
    let hits = engine
        .search_ast(&query)
        .map_err(|e| anyhow::anyhow!("AST query failed: {e}"))?;
    Ok(hits)
}

// ============================================================================
// Standalone AST query
// ============================================================================

/// Execute a standalone `--ast` query (no text query, only AST pattern),
/// writing output to `w`.
///
/// Mirrors `run_temporal_standalone` in `mod.rs`:
/// - Validates the pattern before opening the index.
/// - Opens the AST engine.
/// - `blast_file_ids`: pre-resolved co-change FileId allowlist from the caller
///   (see `temporal::resolve_blast_radius_file_ids`).  When `Some`, intersects
///   with the AST result set BEFORE truncation (avoids PF-006 silent-drop,
///   applies ADR-006 fail-loud counterpart on the read side).  The caller owns
///   resolution so the JSON-aware warning lives in one place.
/// - `temporal_sort` / `temporal_db`: when both are `Some`, the AST matches are
///   temporally enriched and re-sorted (hot/cold/risky) before truncation.  When
///   `temporal_db` is `None` (absent heatmap data) results stay in raw AST order,
///   unannotated — graceful degradation (the caller emits the warning).
/// - Executes the query and formats output.
///
/// `w` is the output sink — production callers pass a `BufWriter` over stdout;
/// tests pass a `Vec<u8>` buffer to capture output (satisfies PF-007: regression
/// tests must observe production output, not just assert exit 0).
///
/// # Truncation order
///
/// 1. blast-radius filter → matching files in raw AST/FileId order.
/// 2. take a bounded window (`limit` without a sort; `limit*5 ≥ 100` with one).
/// 3. temporal enrichment + re-sort (when a sort is active).
/// 4. truncate to `limit` — AFTER the re-sort (AC-F4), so the top-`limit` by
///    temporal score survive rather than the top-`limit` by raw order.
///
/// # Gate + line-anchor (#397 — real-node patterns)
///
/// For **real-node patterns**, the verify gate and the line anchor are now ONE
/// atomic operation via `rskim_search::find_first_strict_match` (AD-397-1/4):
///
/// - Gate + anchor: `find_first_strict_match` re-parses the file with the
///   strict `node.parent()` ancestor walk (AD-374-6) and returns
///   `Some((line, byte_range, snippet))` on the FIRST matching node, or `None`
///   (file dropped — not emitted line-less). This eliminates the TOCTOU race
///   between the gate parse and a separate snippet read.
/// - Bounded: the gate runs over the candidate pool (pre-truncation, AD-374-3).
///   The anchor is cached in the pool tuple and survives temporal sort/truncation.
/// - Every emitted row ALWAYS carries `:line` (AD-397-4 supersedes AD-374-7):
///   `None` from `find_first_strict_match` means "gate failed" → file dropped,
///   never emitted as a bare path-only row.
///
/// # Line-span recovery (#394 — synthetic patterns)
///
/// For **synthetic-marker patterns** (god-function, deep-nesting, empty-function,
/// empty-catch, excessive-params), the two-function flow is preserved unchanged:
/// `pattern_occurs_in_file` (AND-semantics gate, early-exit #419 fix) runs at
/// gate time, and `recover_line` (extraction-reuse recovery, AD-394-5) runs
/// post-truncation. The anchor is bounded to at most `limit` files (AC-API3).
///
/// # Errors
///
/// Returns `Err` when the index is absent, the query is invalid, or I/O fails.
/// Returns `Ok(ExitCode::SUCCESS)` for empty result sets (AC-F8).
// Ten cohesive runtime inputs for the `search --ast` CLI entrypoint (pattern,
// page, json, cache dir, manifest, blast-radius filter, temporal sort, temporal
// DB, root, writer). Bundling them into a struct would be artificial — they are
// all independent caller-supplied knobs — so the argument count is allowed here
// intentionally.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_ast_standalone(
    raw_pattern: &str,
    page: Page,
    json: bool,
    cache_dir: &Path,
    manifest: &super::manifest::FileManifest,
    blast_file_ids: Option<HashSet<FileId>>,
    temporal_sort: Option<TemporalSort>,
    temporal_db: Option<&TemporalDb>,
    root: &Path,
    w: &mut impl std::io::Write,
) -> anyhow::Result<ExitCode> {
    // Validate before opening the index — fail fast with good error messages.
    // Returns the parsed AstQuery to avoid a second parse_ast_query call below.
    let query = validate_ast_pattern(raw_pattern)?;

    let engine = open_ast_engine(cache_dir)?;

    // AD-374-1: search_ast now uses AND-intersect candidate set so raw_results
    // contains only files that appear in EVERY posting list of the pattern.
    let raw_results = engine
        .search_ast(&query)
        .map_err(|e| anyhow::anyhow!("AST query failed: {e}"))?;

    // Hoist sorted_paths() once — reused for the blast-radius membership check
    // (fid.0 as usize), the verify gate, and the path-resolution step below.
    let sorted = manifest.sorted_paths();
    let is_synthetic = ast_query_is_synthetic(&query);

    // AD-374-3 / AD-355-2: verify-then-truncate-LAST with a candidate pool.
    //
    // Because the Part B gate (pattern_occurs_in_file) can DROP candidates,
    // taking exactly `limit` before the gate would under-fill results. We fetch
    // `candidate_pool(page.limit(), K).saturating_add(page.offset())` candidates
    // (the additive AST candidate pool — NEVER the multiplicative `K × depth`
    // form that D-2 / AD-404-6 forbid), gate them, then truncate to `limit` LAST
    // — mirroring the lexical verify-then-truncate-LAST architecture (AD-355-2).
    //
    // With AND-intersect upstream (AD-374-1) the pool is already small, so K=5
    // is adequate. The K value has no measured corpus basis and is tracked under
    // #361 per ADR-003.
    //
    // Temporal path: the temporal resort needs a wider window so hot files
    // beyond raw rank `limit` can be promoted (AC-F4 temporal contract).
    //
    // AD-404-5: pool uses page.depth() (= limit + offset) for raw-order widening
    // so offset pages are fillable. Depth at offset 0 == limit — zero regression.
    //
    // AD-404-6: temporal window stays resort_window(page.limit()), offset-independent,
    // so pages are provably disjoint (no duplicate/miss defect, D-2 / AD-404-7).
    // Only the raw-order (verify) pool widens with offset — never the temporal window.
    let temporal_active = temporal_sort.is_some() && temporal_db.is_some();
    let temporal_window = if temporal_active {
        // AD-404-7: fixed at resort_window(page.limit()); never resort_window(page.depth()).
        super::temporal::resort_window(page.limit())
    } else {
        page.depth()
    };
    // AD-374-3 / AC11 (#419): AST candidate pool.
    //
    // Real-node patterns: reuse LEXICAL_CANDIDATE_POOL_K from query.rs (K=5,
    // floor=100) — the same over-fetch logic as the lexical path, needed because
    // the strict ancestor-walk gate can drop a significant fraction of candidates
    // for patterns with many structural false positives.
    //
    // Synthetic-marker patterns (god-function, deep-nesting, empty-function,
    // empty-catch, excessive-params): the extraction-reuse verify gate
    // (`synthetic_key_present`) has near-zero drop rate — the AST index correctly
    // identifies which files contain the marker, so a K=5 pool and a 100-file
    // floor are pure overhead. For these, size the pool to exactly `page.depth()`
    // (with a floor of 1 to avoid an empty pool when limit=0). At offset 0,
    // depth() == limit — zero regression (AD-404-2).
    //
    // This is the companion to the `synthetic_key_present` early-exit fix in
    // `compound/reparse.rs` — together they reduce measured `--ast deep-nesting`
    // latency from ~6.9s to within AC11's <500ms target.
    let ast_pool = if is_synthetic {
        page.depth().max(1)
    } else {
        // Additive widening: candidate_pool(page.limit(), K) + page.offset().
        // At offset 0: pool = candidate_pool(limit, K), byte-identical to pre-change.
        // With offset: pool grows by exactly offset, not by K*offset (D-3).
        // saturating_add (not `+`) guards a hostile `--offset` near usize::MAX
        // from overflowing (applies PF-004, matches Page::depth()); byte-identical
        // for every realistic offset since it only clamps at the usize::MAX ceiling.
        super::query::candidate_pool(page.limit(), super::query::LEXICAL_CANDIDATE_POOL_K)
            .saturating_add(page.offset())
    };
    // When temporal is active the verify gate outputs at most `temporal_window`
    // rows (see the truncate below), so sizing the pool to ast_pool's wider
    // offset-inflated value would only waste up to `offset` extra file opens.
    // Use temporal_window directly on that path; keep ast_pool for the pure-AST
    // path where no truncation follows.
    let window = if temporal_active {
        temporal_window
    } else {
        ast_pool
    };

    // Intersect AST results with blast-radius FileIds (when set), then take the
    // working window (pool). The filter runs BEFORE truncation so the window reflects
    // the filtered set size (avoids PF-006 silent feature-drop).
    //
    // Track pool_was_capped: if we filled the entire window from raw_results,
    // there may be more candidates beyond the pool ceiling — conservatively sets
    // has_more = true (AD-404-11 / D-5 pool-cap edge case for AC-404-18).
    let raw_count = raw_results.len();
    let pooled: Vec<_> = raw_results
        .into_iter()
        .filter(|(fid, _)| {
            blast_file_ids
                .as_ref()
                .is_none_or(|allowed| allowed.contains(fid))
        })
        .take(window)
        .collect();
    let pool_was_capped = pooled.len() == window && raw_count > window;

    // AD-374-2 / AD-397-4: Structural verify gate + anchor — combined for real-node,
    // separate for synthetic.
    //
    // REAL-NODE patterns: `find_first_strict_match` (AD-397-1) replaces the
    // separate `pattern_occurs_in_file` + `recover_line` calls. A single strict
    // `node.parent()` ancestor walk (AD-374-6) both gates the file AND returns
    // the first-match `(line, byte_range, snippet)` atomically (no TOCTOU).
    // `None` → file dropped; `Some(anchor)` → file kept + anchor cached in the
    // pool entry, surviving temporal sort/truncation. Every emitted real-node
    // row ALWAYS carries `:line` (AD-397-4 supersedes AD-374-7).
    //
    // SYNTHETIC patterns: unchanged two-function flow (AD-394-1/AD-394-2).
    // `pattern_occurs_in_file` uses `synthetic_key_present` with AND-semantics
    // and early-exit (#419). `recover_line` runs post-truncation (≤ limit files).
    //
    // AC-10 / AD-373 dependency note: FileId → path resolution via `sorted.get(fid.0)`
    // is sound only because #373 aligned FileId assignment order with BTreeMap
    // resolution order. Without this alignment the wrong file would be re-parsed.
    //
    // AD-374-5: Non-tree-sitter files (JSON/TOML/YAML) return None/false and
    // are dropped here. AD-374-4: dropping is a relevance filter, not a #317
    // output cap; no `output::elision_marker` is required.
    //
    // AD-405-11: byte-budget gate — bounds query-time I/O to the pre-raise total
    // (window * AST_VERIFY_BYTES_PER_SLOT), so adding 1 MiB-eligible files does not
    // inflate verify-gate latency past the AC11 <500ms target.
    let verify_budget = (window as u64).saturating_mul(AST_VERIFY_BYTES_PER_SLOT);
    let mut bytes_verified: u64 = 0;
    let total_candidates = pooled.len();
    let mut budget_omitted: usize = 0;
    let mut verified_pool: Vec<VerifiedEntry> = Vec::with_capacity(window.min(total_candidates));

    for (fid, score) in pooled {
        let idx = fid.0 as usize;
        let rel_path = match sorted.get(idx) {
            Some(p) => p,
            None => {
                // Out-of-range FileId — warn and drop (ADR-006 counterpart).
                eprintln!(
                    "skim search: AST verify gate warning: FileId({idx}) is out of \
                     manifest range (manifest has {} files) — index may be out of sync; \
                     run `skim search --rebuild`",
                    sorted.len()
                );
                continue;
            }
        };

        // AD-405-11: accrue file size against the query I/O budget before opening
        // the file. Fall back to one slot's worth when the manifest size is absent
        // (conservative: assumes a full-slot file rather than silently granting
        // unlimited budget).
        let entry = manifest.lookup(rel_path);
        let file_size = entry
            .and_then(|e| e.size)
            .unwrap_or(AST_VERIFY_BYTES_PER_SLOT);
        if bytes_verified.saturating_add(file_size) > verify_budget {
            budget_omitted += 1;
            continue;
        }
        bytes_verified = bytes_verified.saturating_add(file_size);

        let abs_path = root.join(rel_path);
        let stored_mtime = entry.and_then(|e| e.mtime);

        if is_synthetic {
            // Synthetic: two-function flow (AND-semantics gate, AC-10/#419 preserved).
            if pattern_occurs_in_file(&abs_path, &query, stored_mtime) {
                verified_pool.push((fid, score, None));
            }
        } else {
            // Real-node: find_first_strict_match (gate + anchor in one pass).
            if let Some(anchor) = find_first_strict_match(&abs_path, &query, stored_mtime) {
                verified_pool.push((fid, score, Some(anchor)));
            }
        }
    }

    // AD-405-11: emit budget-exhausted notice to stderr when candidates were skipped.
    if budget_omitted > 0 {
        eprintln!(
            "skim search: AST verify: byte budget exhausted ({budget_omitted} of \
             {total_candidates} candidates unverified; budget = {window} slots × {} KiB); \
             use SKIM_PASSTHROUGH=1 or rebuild with `skim search --rebuild`.",
            AST_VERIFY_BYTES_PER_SLOT / 1024,
        );
    }

    // Resolve FileIds → repo-relative paths. For real-node patterns, populate
    // line/snippet from the cached anchor (no post-truncation re-parse needed).
    let mut resolved: Vec<AstResult> = Vec::with_capacity(verified_pool.len());
    for (fid, score, anchor) in &verified_pool {
        let idx = fid.0 as usize;
        // Safety: all verified_pool entries have valid sorted.get(idx) — the
        // gate above already dropped out-of-range FileIds. Use get() with a
        // defensive fallback to avoid panicking on an edge case.
        if let Some(rel_path) = sorted.get(idx) {
            let (line, snippet) = if let Some((ln, byte_range, text)) = anchor {
                // Real-node: anchor from find_first_strict_match (AD-397-4).
                // Suppress snippet when byte_range is empty (defensive; matched
                // real nodes always have non-empty ranges, but zero-width
                // MISSING/ERROR nodes are theoretically possible).
                (
                    Some(*ln),
                    if byte_range.is_empty() {
                        None
                    } else {
                        Some(text.clone())
                    },
                )
            } else {
                // Synthetic: line/snippet populated post-truncation by recover_line.
                (None, None)
            };
            resolved.push(AstResult::ast_only(
                rel_path.to_string(),
                *score,
                line,
                snippet,
            ));
        }
    }

    // AD-404-6/7: pre-truncate to resort_window(limit) BEFORE enrich_ast_results.
    //
    // `enrich_ast_results` caller contract: MUST receive ≤ resort_window entries
    // so per-file DB lookups stay bounded at O(resort_window) — AC-P1.
    //
    // D-2 / AD-404-7: the temporal re-sort window is offset-INDEPENDENT.
    // With temporal active, window is set to exactly temporal_window above
    // (not ast_pool), so both the pool step and the verify gate produce at
    // most temporal_window entries.  This truncate is therefore a no-op on
    // the temporal path and is kept as defence-in-depth against future
    // window-computation changes.
    //
    // Pages are disjoint for both pattern kinds:
    //   - Synthetic: AND-semantics verify is exact; verified ≤ temporal_window.
    //   - Real-node: find_first_strict_match is lossy (verified_count can be
    //     < temporal_window) but never exceeds the pool ceiling, so no
    //     offset-dependent set growth occurs and the disjoint bound holds.
    if temporal_active {
        resolved.truncate(temporal_window);
    }
    // Temporal enrichment + re-sort before the paginate step (AC-F4).
    // When absent, results stay in raw AST order (graceful degradation — AC-A3).
    if let (Some(sort), Some(db)) = (temporal_sort, temporal_db) {
        super::temporal::enrich_ast_results(&mut resolved, sort, db);
    }

    // AD-405-7 / AC-405-17: compute coverage from the already-loaded manifest
    // (zero extra I/O). Emit notice on the standalone --ast surface (D-4 cadence).
    let coverage = manifest.ast_coverage();
    if let Some(notice) = super::query::ast_coverage_notice(&coverage) {
        eprintln!("{notice}");
    }

    write_ast_page_output(
        &mut resolved,
        page,
        pool_was_capped,
        temporal_active,
        is_synthetic,
        raw_pattern,
        &query,
        root,
        manifest,
        json,
        coverage,
        w,
    )
}

// ============================================================================
// Output phase: pagination + notice + synthetic recovery + format
// ============================================================================

/// Apply pagination, emit the bounded-page notice, run synthetic line recovery,
/// and write the final output for a standalone `--ast` query.
///
/// Extracted from `run_ast_standalone` to keep that function below the 200-line
/// threshold. All pagination and output logic lives here; the caller owns the
/// verify gate, the temporal pre-truncation, and the temporal enrichment steps.
///
/// `temporal_active` is consistent with the `resolved.truncate(temporal_window)`
/// the caller already applied before `enrich_ast_results` (AD-404-6/7).
// Arguments are all independent caller-supplied knobs (pagination cursor,
// pool cap flag, temporal flag, synthetic-vs-real routing, pattern string,
// query handle for recover_line, paths for file I/O, format flag, writer).
// No natural grouping exists — mirrors run_ast_standalone's own allow.
#[allow(clippy::too_many_arguments)]
fn write_ast_page_output(
    resolved: &mut Vec<AstResult>,
    page: Page,
    pool_was_capped: bool,
    temporal_active: bool,
    is_synthetic: bool,
    raw_pattern: &str,
    query: &AstQuery,
    root: &Path,
    manifest: &super::manifest::FileManifest,
    json: bool,
    coverage: rskim_search::AstCoverage,
    w: &mut impl std::io::Write,
) -> anyhow::Result<ExitCode> {
    // AD-404-5 / AD-404-11: page.apply() — skip + take. Position is load-bearing
    // (AD-404-3): AFTER verify gate and AFTER temporal re-sort.
    // At offset 0: page.apply() == truncate(limit) — zero regression.
    let pre_page_len = resolved.len();
    page.apply(resolved);
    // has_more (AD-404-11 / D-5): true when more verified results exist beyond the
    // current page, or when the candidate pool was capped (conservatively true).
    // Replaces the unsound `results.len() < limit` heuristic.
    let has_more = pre_page_len > page.depth() || pool_was_capped;

    // Bounded-page notice on the --ast+temporal path (AD-404-11 / D-5).
    // Fires when temporal is active, the page is non-empty (no false notice on
    // the exhausted-page path), and has_more is true — meaning there are
    // verified results beyond the current page (or the candidate pool was
    // capped), so the user should paginate. Goes to stderr (PF-006 / AD-404-8)
    // so --json stdout stays byte-identical. Mirrors run_temporal_standalone
    // (mod.rs:1151) which also gates solely on has_more.
    //
    // NOTE: the old guard `pre_page_len > temporal_window` was dead code after
    // resolved.truncate(temporal_window) (called by the caller before entry)
    // ensured pre_page_len ≤ temporal_window when temporal_active — replaced
    // here by has_more to restore parity with the text+temporal path.
    if !resolved.is_empty() && temporal_active && has_more {
        eprintln!(
            "{}",
            super::temporal::bounded_page_notice(resolved.len(), page.offset(), page.limit())
        );
    }

    // AD-397-4: Real-node patterns — line/snippet cached in the verify pool entry
    // from `find_first_strict_match`; no post-truncation re-parse needed.
    // Synthetic patterns (#394, AD-394-5) — recover_line runs post-truncation
    // (≤ limit files, AC-API3), unchanged from #394.
    if is_synthetic {
        for r in resolved.iter_mut() {
            let abs_path = root.join(&r.path);
            let stored_mtime = manifest.lookup(&r.path).and_then(|e| e.mtime);
            if let Some((ln, byte_range)) = recover_line(&abs_path, query, stored_mtime) {
                // AD-405-2 / AD-405-14: language-aware size limit keeps read_line_at
                // in sync with linearize_source and read_guarded; no drift possible.
                let max_bytes = rskim_core::Language::from_path(Path::new(&r.path))
                    .and_then(rskim_core::ast_size_limit)
                    .unwrap_or(rskim_core::AST_SIZE_LIMIT_DEFAULT);
                // Suppress snippet when byte_range is empty (synthetic recovery artifact).
                let snip = read_line_at(&abs_path, ln, max_bytes);
                r.line = Some(ln);
                r.snippet = if byte_range.is_empty() { None } else { snip };
            }
        }
    }

    // Resolve pattern metadata for display.
    let pattern_name = raw_pattern.trim();
    let (display_name, description) = pattern_description(pattern_name);

    // AD-405-10: extract scalar before consuming coverage to avoid a partial move.
    let excluded_count = coverage.size_excluded_files;
    if json {
        // AD-404-11 / D-5: has_more lets agents detect the last page without
        // relying on the unsound `len < limit` heuristic.
        // AD-405-3: ast_coverage key carries one shape on all three surfaces.
        format_ast_json(
            resolved,
            display_name,
            description,
            has_more,
            Some(coverage),
            w,
        )?;
    } else {
        // AC-404-8 / AC-404-10: page-aware empty messages are now handled
        // inside format_ast_text via the `offset` parameter, mirroring how
        // format_temporal_text (temporal.rs) threads Page for the same split.
        // AC-405-10: excluded_count appended when non-zero.
        format_ast_text(
            resolved,
            display_name,
            description,
            page.offset(),
            excluded_count,
            w,
        )?;
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Snippet extraction helpers
// ============================================================================

/// AD-405-11: byte-budget constant for the structural verify gate.
///
/// The gate loop may read at most `window * AST_VERIFY_BYTES_PER_SLOT` bytes
/// across all candidates in one query.  This is the pre-raise per-slot volume
/// (100 KiB = the old cap), deliberately DECOUPLED from `AST_SIZE_LIMIT_DEFAULT`
/// (1 MiB) so raising the cap adds files to the index without inflating query-time
/// I/O.  Pre-raise workloads cannot exceed `window * 100 KiB` anyway, so the
/// <500ms AC11 gate holds by construction.
const AST_VERIFY_BYTES_PER_SLOT: u64 = 100 * 1024;

/// Read the text of a specific 1-indexed line from a file.
///
/// Returns `None` when the file cannot be read, is non-UTF8, exceeds
/// `max_bytes`, or the line number is out of range.
///
/// ## AD-405-2 / AD-405-14: Language-aware size limit
///
/// The caller passes the LANGUAGE-AWARE limit from `rskim_core::ast_size_limit`
/// so `read_line_at` admits exactly the same files that `linearize_source` and
/// `read_guarded` admit — no silent second cap can drift to a stale value.
///
/// ## Streaming BufRead (AD-405-14)
///
/// Uses `BufRead::lines().nth(target)` instead of `fs::read` + whole-file
/// UTF-8 so fetching a single line from a 1 MiB file costs only the bytes
/// up to that line, not a full 1 MiB read.
///
/// This is the "file-reading" half of the crate split (#201): pure formatting
/// lives in `rskim_search::compound::output`; disk I/O lives here.
fn read_line_at(abs_path: &Path, line_1indexed: u32, max_bytes: u64) -> Option<String> {
    use std::io::BufRead;
    let meta = std::fs::metadata(abs_path).ok()?;
    if meta.len() > max_bytes {
        return None;
    }
    let file = std::fs::File::open(abs_path).ok()?;
    let reader = std::io::BufReader::new(file);
    let target = line_1indexed.saturating_sub(1) as usize; // 0-indexed
    reader.lines().nth(target).and_then(|r| r.ok())
}

// ============================================================================
// Pattern metadata lookup
// ============================================================================

/// AC11 / AD-374-3 (#419): returns `true` when any (at least one) resolved bigram
/// in `query` has a synthetic-marker component ID (`>= BUCKET_LABEL_BASE`).
///
/// Only [`AstQuery::Pattern`] with synthetic-marker resolved bigrams reaches
/// this path — [`AstQuery::Containment`] and [`AstQuery::SingleNode`] do not
/// reference synthetic IDs (the parser rejects them at the CLI boundary), so
/// this function returns `false` for those variants.
///
/// Used to gate the candidate pool size: synthetic patterns have a near-zero
/// verify drop rate (the early-exit `synthetic_key_present` gate is essentially
/// a membership check of what the index already indexed), so a K=5 multiplier
/// and a 100-file floor are unnecessary overhead for them.
///
/// # Design note — intentionally bigrams-only
///
/// This function inspects `resolved_bigrams()` only. It does NOT delegate to
/// `compound::reparse::query_contains_synthetic_id` (which also checks trigrams
/// and handles the Containment variant). Its correctness rests on the predicate
/// being EXACT for every shipping pattern — NOT on either misclassification
/// direction being harmless:
///
/// - It gates ROUTING as well as pool sizing (#397): real-node patterns route to
///   `find_first_strict_match`; synthetic patterns route to `pattern_occurs_in_file`
///   plus the post-truncation `recover_line`. In the routing use a misclassification
///   is NOT harmless — a genuinely-synthetic query classified 'not synthetic' would
///   route to `find_first_strict_match`, which returns `None` for synthetic IDs, and
///   the file would be DROPPED (recall loss, an ADR-007 violation).
/// - It is exact for every live pattern: all 24 real-node patterns carry no
///   synthetic ID in any n-gram (→ `false`), and all 5 synthetic patterns are
///   single-bigram/zero-trigram (OD-394-2) (→ `true`). So it agrees with
///   `query_contains_synthetic_id` for every shipping pattern.
///
/// A future synthetic-TRIGRAM pattern would defeat this bigrams-only check
/// (misclassified 'not synthetic'). Such a pattern is unsupported today and is
/// already rejected by the production guard in `pattern_occurs_in_file`'s
/// synthetic branch; adding one requires updating this predicate together with
/// that guard.
fn ast_query_is_synthetic(query: &AstQuery) -> bool {
    match query {
        AstQuery::Pattern(p) => p.resolved_bigrams().iter().any(|bg| {
            let (parent, child) = bg.decode();
            is_synthetic_id(parent) || is_synthetic_id(child)
        }),
        AstQuery::Containment(_) | AstQuery::SingleNode(_) => false,
    }
}

/// Look up human-readable metadata for a pattern name.
///
/// Returns `(name, description)`.  For named patterns, uses the catalog entry.
/// For containment queries, returns the raw query as the name and an empty description.
fn pattern_description(raw: &str) -> (&str, &str) {
    // Try catalog lookup first (for named patterns).
    if let Some(p) = all_patterns().iter().find(|p| p.name == raw) {
        return (p.name, p.description);
    }
    // Containment query — use raw string.
    (raw, "")
}

// ============================================================================
// Tests (co-located in ast_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "ast_tests.rs"]
mod tests;
