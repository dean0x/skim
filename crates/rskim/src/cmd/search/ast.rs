//! AST structural pattern query helpers for `skim search --ast`.
//!
//! # Responsibilities
//!
//! - Open the AST query engine with a clear error when the index is absent (#199).
//! - Validate the raw pattern string at the CLI boundary (before opening the index).
//! - Resolve a `--ast` pattern to a `HashSet<FileId>` for intersection filtering.
//! - Standalone AST query dispatch (`--ast` only, no text query).
//! - Output formatters: text and JSON for AST-only results.
//!
//! # Relationship to temporal.rs
//!
//! This module mirrors the structure of `temporal.rs` — one focused module per
//! search layer, with minimal hooks in `mod.rs`.  AST scores are file-level
//! (no line numbers); text+AST intersection uses lexical formatters unchanged.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use rskim_search::{AstIndexReader, AstQuery, AstQueryEngine, FileId, SearchLayer, SearchQuery};
use rskim_search::{all_patterns, parse_ast_query};
use serde::Serialize;

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

/// Validate a raw `--ast` string at the CLI boundary.
///
/// Calls [`parse_ast_query`] after trimming whitespace.  Returns a friendly
/// error for:
/// - [`AstQuery::SingleNode`] → deferred to #283 (unigram index not yet built).
/// - Unknown pattern name → surfaces the library message (lists valid patterns).
/// - Query > 4096 bytes → surfaces the library message.
/// - Empty / whitespace-only → caught before this call in `parse_flags`.
///
/// # Errors
///
/// Returns `Err` on any invalid query, with a message ready for user display.
pub(super) fn validate_ast_pattern(raw: &str) -> anyhow::Result<()> {
    match parse_ast_query(raw.trim()) {
        Ok(AstQuery::SingleNode(_)) => {
            anyhow::bail!(
                "single-node structural search is not yet supported (tracked in #283).\n\
                 Use a named pattern (e.g. `--ast try-catch`) or a containment query \
                 (e.g. `--ast \"for_statement > await_expression\"`)."
            );
        }
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

// ============================================================================
// FileId resolution
// ============================================================================

/// Resolve a `--ast` pattern to the set of matching `FileId`s.
///
/// Opens the engine, runs `SearchQuery` with `ast_pattern` set and an
/// unbounded limit so the full match set is returned.  The caller's
/// `--limit` flag then applies to the intersection.
///
/// # Errors
///
/// Returns `Err` when the index cannot be opened or the query fails.
pub(super) fn resolve_ast_file_filter(
    engine: &AstQueryEngine<AstIndexReader>,
    raw: &str,
    lang: Option<rskim_core::Language>,
) -> anyhow::Result<HashSet<FileId>> {
    // Build a SearchQuery with ast_pattern and a very high limit so we get
    // the full unfiltered set — the lexical --limit applies at intersection time.
    let mut sq = SearchQuery::new(String::new());
    sq.ast_pattern = Some(raw.to_string());
    sq.limit = Some(usize::MAX);
    sq.lang = lang;

    let results = engine
        .search(&sq)
        .map_err(|e| anyhow::anyhow!("AST query failed: {e}"))?;

    Ok(results.iter().map(|r| r.file_id).collect())
}

// ============================================================================
// Standalone AST query
// ============================================================================

/// Execute a standalone `--ast` query (no text query, only AST pattern).
///
/// Mirrors `run_temporal_standalone` in `mod.rs`:
/// - Resolves project root + cache dir.
/// - Validates the pattern before opening the index.
/// - Opens the AST engine.
/// - Executes the query and formats output.
///
/// # Errors
///
/// Returns `Err` when the index is absent, the query is invalid, or I/O fails.
/// Returns `Ok(ExitCode::SUCCESS)` for empty result sets (AC-F8).
pub(super) fn run_ast_standalone(
    raw_pattern: &str,
    limit: usize,
    json: bool,
    cache_dir: &Path,
    manifest: &super::manifest::FileManifest,
) -> anyhow::Result<std::process::ExitCode> {
    // Validate before opening the index — fail fast with good error messages.
    validate_ast_pattern(raw_pattern)?;

    let engine = open_ast_engine(cache_dir)?;

    let mut sq = SearchQuery::new(String::new());
    sq.ast_pattern = Some(raw_pattern.to_string());
    sq.limit = Some(limit);

    let results = engine
        .search(&sq)
        .map_err(|e| anyhow::anyhow!("AST query failed: {e}"))?;

    // Resolve FileIds → repo-relative paths using the manifest.
    let sorted = manifest.sorted_paths();
    let resolved: Vec<AstResult> = results
        .iter()
        .filter_map(|r| {
            sorted.get(r.file_id.0 as usize).map(|path| AstResult {
                path: path.to_string(),
                score: r.score,
            })
        })
        .collect();

    // Resolve pattern metadata for display.
    let pattern_name = raw_pattern.trim();
    let (display_name, description) = pattern_description(pattern_name);

    let mut stdout = std::io::BufWriter::new(std::io::stdout());
    if json {
        format_ast_json(&resolved, display_name, description, &mut stdout)?;
    } else {
        format_ast_text(&resolved, display_name, description, &mut stdout)?;
    }
    stdout.flush()?;

    Ok(std::process::ExitCode::SUCCESS)
}

// ============================================================================
// Output formatters
// ============================================================================

/// A resolved AST search result (file-level, no line numbers).
#[derive(Debug)]
pub(super) struct AstResult {
    /// Repo-relative path (forward slashes, no leading `.`).
    pub path: String,
    /// AST BM25 relevance score.
    pub score: f64,
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

/// Format standalone AST results as human-readable text.
///
/// Format:
/// ```text
/// AST pattern: try-catch — Try/catch error handling block
///
///   src/auth.rs  score: 2.450
///   src/db.rs    score: 1.200
///
/// 2 file(s) matched pattern "try-catch"
/// ```
///
/// Empty result → "no files match pattern" (exit 0, AC-F8).
/// File-level only — NO `:line` suffix (AC-F1 / spec §Part C).
pub(super) fn format_ast_text(
    results: &[AstResult],
    pattern_name: &str,
    description: &str,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    if description.is_empty() {
        writeln!(w, "AST pattern: {pattern_name}")?;
    } else {
        writeln!(w, "AST pattern: {pattern_name} — {description}")?;
    }
    writeln!(w)?;

    if results.is_empty() {
        writeln!(w, "no files match pattern {:?}", pattern_name)?;
        return Ok(());
    }

    for r in results {
        writeln!(w, "  {}  score: {:.3}", r.path, r.score)?;
    }

    writeln!(w)?;
    writeln!(
        w,
        "{} file(s) matched pattern {:?}",
        results.len(),
        pattern_name
    )?;
    Ok(())
}

/// JSON envelope for a standalone AST query result.
///
/// Shape (AC-A1):
/// ```json
/// {
///   "mode": "ast",
///   "pattern": "try-catch",
///   "description": "...",
///   "total": 3,
///   "results": [{"path": "src/foo.rs", "score": 2.45}, ...]
/// }
/// ```
///
/// No `line` or `snippet` keys (file-level only).
#[derive(Serialize)]
struct AstJson<'a> {
    mode: &'static str,
    pattern: &'a str,
    description: &'a str,
    total: usize,
    results: Vec<AstJsonRow<'a>>,
}

/// One file entry in the AST-only JSON output.
#[derive(Serialize)]
struct AstJsonRow<'a> {
    path: &'a str,
    score: f64,
}

/// Format standalone AST results as a JSON object.
pub(super) fn format_ast_json(
    results: &[AstResult],
    pattern_name: &str,
    description: &str,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    let envelope = AstJson {
        mode: "ast",
        pattern: pattern_name,
        description,
        total: results.len(),
        results: results
            .iter()
            .map(|r| AstJsonRow {
                path: &r.path,
                score: r.score,
            })
            .collect(),
    };
    writeln!(w, "{}", serde_json::to_string_pretty(&envelope)?)?;
    Ok(())
}

// ============================================================================
// Tests (co-located in ast_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "ast_tests.rs"]
mod tests;
