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
use rskim_search::recover_line;
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

    let raw_results = engine
        .search_ast(&query)
        .map_err(|e| anyhow::anyhow!("AST query failed: {e}"))?;

    // Hoist sorted_paths() once — reused for the blast-radius membership check
    // (fid.0 as usize) and the path-resolution step below.
    let sorted = manifest.sorted_paths();

    // A temporal sort defers truncation: take a bounded window (limit*5 ≥ 100) so
    // the re-sort can promote a temporally-hot file that ranks beyond `limit` in
    // raw FileId/AST order (AC-F4). Without a sort, take exactly `limit` — the
    // non-temporal base case is byte-identical to before (GAP-2 carve-out, AC-A1).
    let temporal_active = temporal_sort.is_some() && temporal_db.is_some();
    let window = if temporal_active {
        super::temporal::resort_window(limit)
    } else {
        limit
    };

    // Intersect AST results with blast-radius FileIds (when set), then take the
    // working window. The filter runs BEFORE truncation so the window reflects the
    // filtered set size (avoids PF-006 silent feature-drop).
    let windowed: Vec<_> = raw_results
        .into_iter()
        .filter(|(fid, _)| {
            blast_file_ids
                .as_ref()
                .is_none_or(|allowed| allowed.contains(fid))
        })
        .take(window)
        .collect();

    // Resolve FileIds → repo-relative paths (no re-parse yet). Warn on out-of-range
    // FileIds (avoids PF-002 silent-drop; applies ADR-006 counterpart on the read side).
    let mut resolved: Vec<AstResult> = Vec::with_capacity(windowed.len());
    for (fid, score) in &windowed {
        let idx = fid.0 as usize;
        match sorted.get(idx) {
            Some(rel_path) => {
                resolved.push(AstResult::ast_only(
                    rel_path.to_string(),
                    *score,
                    None,
                    None,
                ));
            }
            None => {
                eprintln!(
                    "skim search: AST result warning: FileId({idx}) is out of manifest range \
                     (manifest has {} files) — index may be out of sync; run `skim search --rebuild`",
                    sorted.len()
                );
            }
        }
    }

    // Temporal enrichment + re-sort, THEN truncate to `limit` (truncate after the
    // sort so the top-`limit` by temporal score survive — AC-F4). Enrichment
    // performs at most `window` per-file DB lookups (AC-P1). When the temporal
    // DB is absent (temporal_db == None) results stay in raw AST order, unannotated
    // (graceful degradation — AC-A3; the warning is emitted by the caller).
    if let (Some(sort), Some(db)) = (temporal_sort, temporal_db) {
        super::temporal::enrich_ast_results(&mut resolved, sort, db);
        resolved.truncate(limit);
    }

    // Re-parse the final (≤ `limit`) set to recover a representative line + snippet.
    // Re-parse runs strictly AFTER truncation (AC-API3, #201: at most `limit` files);
    // each per-file recover_line call is bounded by the 100 KiB size guard.
    // fail-soft: recover_line returns None on any error → degraded row (AC-F2).
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
