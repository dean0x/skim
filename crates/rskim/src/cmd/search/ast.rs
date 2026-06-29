//! AST structural pattern query helpers for `skim search --ast`.
//!
//! # Responsibilities
//!
//! - Open the AST query engine with a clear error when the index is absent (#199).
//! - Validate the raw pattern string at the CLI boundary (before opening the index).
//! - Resolve a `--ast` pattern to scored `Vec<(FileId, f64)>` for compound RRF ranking (#198).
//! - Standalone AST query dispatch (`--ast` only, no text query).
//! - Output formatters: text and JSON for AST-only results (delegates to `rskim_search::compound::output`).
//! - Line-span re-parse: after limit is applied, re-parse each matched file to
//!   recover a representative line number and snippet (AC-F1, #201).
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
use rskim_search::{all_patterns, parse_ast_query};
// #201: enriched row type + formatters from rskim-search.
// pub(super) re-exports so test module (ast_tests.rs) can call them as super::.
pub(super) use rskim_search::AstResult;
use rskim_search::{pattern_occurs_in_file, recover_line};
pub(super) use rskim_search::{format_ast_json, format_ast_text};

use super::types::TemporalSort;

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
/// # Line-span re-parse (#201)
///
/// AFTER truncation, each surviving file is re-parsed to recover a representative
/// line number. Re-parse uses `rskim_search::recover_line` (see
/// `compound/reparse.rs`) which:
/// - Is bounded to at most `limit` files (AC-API3) — re-parse runs post-truncation.
/// - Fails-soft to `None` on grammar miss, size guard, mtime mismatch (AC-F2).
/// - Returns a 1-indexed line; never 0 (AC-F4 NEGATIVE).
///
/// The snippet is extracted by reading the specific line from the file content.
///
/// # Errors
///
/// Returns `Err` when the index is absent, the query is invalid, or I/O fails.
/// Returns `Ok(ExitCode::SUCCESS)` for empty result sets (AC-F8).
// Ten cohesive runtime inputs for the `search --ast` CLI entrypoint (pattern,
// limit, json, cache dir, manifest, blast-radius filter, temporal sort, temporal
// DB, root, writer). Bundling them into a struct would be artificial — they are
// all independent caller-supplied knobs — so the argument count is allowed here
// intentionally.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_ast_standalone(
    raw_pattern: &str,
    limit: usize,
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

    // AD-374-3 / AD-355-2: verify-then-truncate-LAST with a candidate pool.
    //
    // Because the Part B gate (pattern_occurs_in_file) can DROP candidates,
    // taking exactly `limit` before the gate would under-fill results. We fetch
    // `LEXICAL_CANDIDATE_POOL_K × limit` candidates (the AST candidate pool),
    // gate them, then truncate to `limit` LAST — mirroring the lexical
    // verify-then-truncate-LAST architecture (AD-355-2).
    //
    // With AND-intersect upstream (AD-374-1) the pool is already small, so K=5
    // is adequate. The K value has no measured corpus basis and is tracked under
    // #361 per ADR-003.
    //
    // Temporal path: the temporal resort also needs a wider window so hot files
    // beyond raw rank `limit` can be promoted (AC-F4 temporal contract). We take
    // the MAX of the temporal window and the verify pool so both constraints are met.
    let temporal_active = temporal_sort.is_some() && temporal_db.is_some();
    let temporal_window = if temporal_active {
        super::temporal::resort_window(limit)
    } else {
        limit
    };
    // AD-374-3: AST candidate pool — reuse LEXICAL_CANDIDATE_POOL_K from query.rs
    // (pub(super)) as the single definition. No AST-local fork.
    let ast_pool = super::query::candidate_pool(limit, super::query::LEXICAL_CANDIDATE_POOL_K);
    let window = temporal_window.max(ast_pool);

    // Intersect AST results with blast-radius FileIds (when set), then take the
    // working window (pool). The filter runs BEFORE truncation so the window reflects
    // the filtered set size (avoids PF-006 silent feature-drop).
    let pooled: Vec<_> = raw_results
        .into_iter()
        .filter(|(fid, _)| {
            blast_file_ids
                .as_ref()
                .is_none_or(|allowed| allowed.contains(fid))
        })
        .take(window)
        .collect();

    // AD-374-2: Structural verify gate — drop candidates that do NOT contain the
    // pattern's declared ancestor relationship in their real CST.
    //
    // `pattern_occurs_in_file` re-parses each file and confirms the ancestor chain
    // (parent→child for bigrams; grandparent→parent→child for trigrams) via real
    // `node.parent()` calls — NOT the pre-order-predecessor approximation used by
    // `recover_line`. This is the correctness backstop that eliminates unrelated-
    // subtree false positives that AND-intersect alone would keep.
    //
    // AD-374-5: Non-tree-sitter files (JSON/TOML/YAML, node_count=0) return false
    // from `pattern_occurs_in_file` and are dropped here.
    //
    // AD-374-4: Dropping candidates that fail the gate is a RELEVANCE filter, not
    // a #317 output cap. No `output::elision_marker` is required (mirrors AD-355-4).
    //
    // AC-10 / AD-373 dependency note: FileId → path resolution via `sorted.get(fid.0)`
    // is sound only because #373 aligned FileId assignment order with BTreeMap
    // resolution order. The gate assumes this alignment; without it the wrong file
    // would be re-parsed (ADR-006 read-side desync).
    let verified_pool: Vec<_> = pooled
        .into_iter()
        .filter(|(fid, _)| {
            let idx = fid.0 as usize;
            match sorted.get(idx) {
                Some(rel_path) => {
                    let abs_path = root.join(rel_path);
                    let stored_mtime = manifest.lookup(rel_path).and_then(|e| e.mtime);
                    pattern_occurs_in_file(&abs_path, &query, stored_mtime)
                }
                None => {
                    // Out-of-range FileId — warn and drop (ADR-006 counterpart).
                    eprintln!(
                        "skim search: AST verify gate warning: FileId({idx}) is out of \
                         manifest range (manifest has {} files) — index may be out of sync; \
                         run `skim search --rebuild`",
                        sorted.len()
                    );
                    false
                }
            }
        })
        .collect();

    // Resolve FileIds → repo-relative paths from the verified pool.
    let mut resolved: Vec<AstResult> = Vec::with_capacity(verified_pool.len());
    for (fid, score) in &verified_pool {
        let idx = fid.0 as usize;
        // Safety: all verified_pool entries have valid sorted.get(idx) — the
        // gate above already dropped out-of-range FileIds. Use get() with a
        // defensive fallback to avoid panicking on an edge case.
        if let Some(rel_path) = sorted.get(idx) {
            resolved.push(AstResult::ast_only(
                rel_path.to_string(),
                *score,
                None,
                None,
            ));
        }
    }

    // Temporal enrichment + re-sort before the truncate-LAST step (AC-F4).
    // When absent, results stay in raw AST order (graceful degradation — AC-A3).
    if let (Some(sort), Some(db)) = (temporal_sort, temporal_db) {
        super::temporal::enrich_ast_results(&mut resolved, sort, db);
    }
    // AD-374-3 / AD-355-2: truncate-LAST — after any temporal re-sort so the
    // top-`limit` by temporal score survive; in the non-temporal path this is
    // the only truncation (min(limit, verified_count) results).
    resolved.truncate(limit);

    // Re-parse the final (≤ `limit`) set to recover a representative line + snippet.
    // Re-parse runs strictly AFTER truncation (AC-API3, AC-8 #374, #201: at most
    // `limit` files); each per-file recover_line call is bounded by the 100 KiB
    // size guard.
    //
    // AD-374-7: recover_line is LINE-RECOVERY ONLY. Emit/drop decisions are made
    // by the verify gate above. A file that passes the gate but whose representative
    // line cannot be recovered still emits as a degraded row (path present, no :line
    // or snippet). recover_line returning None MUST NOT drop a gate-passed file.
    for r in &mut resolved {
        let abs_path = root.join(&r.path);
        // Recover the stored mtime from the manifest for the stale guard.
        let stored_mtime = manifest.lookup(&r.path).and_then(|e| e.mtime);
        if let Some((ln, byte_range)) = recover_line(&abs_path, &query, stored_mtime) {
            // Extract the single representative line as snippet text; suppress when
            // the byte range is empty (parse artifact).
            let snip = read_line_at(&abs_path, ln, rskim_search::MAX_REPARSE_FILE_BYTES);
            r.line = Some(ln);
            r.snippet = if byte_range.is_empty() { None } else { snip };
        }
    }

    // Resolve pattern metadata for display.
    let pattern_name = raw_pattern.trim();
    let (display_name, description) = pattern_description(pattern_name);

    if json {
        format_ast_json(&resolved, display_name, description, w)?;
    } else {
        format_ast_text(&resolved, display_name, description, w)?;
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Snippet extraction helpers
// ============================================================================

/// Read the text of a specific 1-indexed line from a file.
///
/// Returns `None` when the file cannot be read, is non-UTF8, exceeds
/// `max_bytes`, or the line number is out of range.
///
/// This is the "file-reading" half of the crate split (#201): pure formatting
/// lives in `rskim_search::compound::output`; disk I/O lives here.
fn read_line_at(abs_path: &Path, line_1indexed: u32, max_bytes: u64) -> Option<String> {
    let meta = std::fs::metadata(abs_path).ok()?;
    if meta.len() > max_bytes {
        return None;
    }
    let content = std::fs::read(abs_path).ok()?;
    let text = std::str::from_utf8(&content).ok()?;
    let target = line_1indexed.saturating_sub(1) as usize; // → 0-indexed
    text.lines().nth(target).map(|l| l.to_string())
}

// ============================================================================
// Pattern metadata lookup
// ============================================================================

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
