//! Compound search result row type and formatters (#201).
//!
//! # Responsibility
//!
//! This module is the **sole owner** of the enriched `AstResult` row shape:
//!
//! ```text
//! { path, score, line: Option<u32>, snippet: Option<String>,
//!   layers_matched: Vec<&'static str>, temporal: Option<TemporalAnnotation> }
//! ```
//!
//! `#202` populates `temporal`; this module defines the field so `#202` only
//! assigns it, never re-declares the type.
//!
//! ## Crate split
//!
//! - **Pure row + format logic** lives here (no I/O, no side effects).
//! - **File-reading snippet extraction** (opening files, mtime guards) stays in
//!   the `rskim` CLI crate (`cmd/search/ast.rs`), which calls these formatters
//!   after supplying already-resolved rows.
//!
//! ## `layers_matched` semantics
//!
//! `layers_matched` contains the layers that contributed **non-zero signal** for
//! a given file.  Temporal-as-sort does NOT count as a matched layer:
//! `--ast + --hot` still yields `["ast"]`, not `["ast","temporal"]`.
//! True compound dispatch (text+AST+temporal merged) is tracked in #339.
//!
//! ## Additive JSON schema
//!
//! All new keys (`line`, `snippet`, `layers_matched`) use
//! `#[serde(skip_serializing_if = "...")]` so they are **absent** (never `null`
//! or `0`) on degraded rows, preserving back-compat with existing `path`/`score`
//! consumers.

use std::io::{self, Write};

use serde::Serialize;

use crate::AstCoverage;

// ============================================================================
// TemporalAnnotation (defined here so #202 populates, never re-declares it)
// ============================================================================

/// Optional temporal metadata attached to a compound search result.
///
/// All fields use `skip_serializing_if = "Option::is_none"` so absent data
/// produces no JSON keys at all (back-compat for consumers reading only
/// `path`/`score`).
///
/// `#201` always leaves this as `None`; `#202` populates it in the
/// temporal-sort compound path (tracked in #339).
#[derive(Debug, Clone, Serialize, Default)]
pub struct TemporalAnnotation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotspot_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_density: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cochange_jaccard: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes_30d: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes_90d: Option<u32>,
}

// ============================================================================
// AstResult — enriched row type (sole definition, #201 onwards)
// ============================================================================

/// An enriched AST search result row.
///
/// This is the **canonical** row shape owned by `#201`. Every field added
/// after the initial `path`/`score` pair uses `#[serde(skip_serializing_if)]`
/// so the JSON output is additive and back-compat.
///
/// ## Field semantics
///
/// - `path` — repo-relative path (forward slashes, no leading `.`).
/// - `score` — AST BM25 relevance score.
/// - `line` — 1-indexed representative line recovered by re-parse (best-effort;
///   absent when the pattern's node kinds are not present in this file's grammar,
///   or when the file changed/was deleted since indexing). Deferred exact-every-
///   occurrence precision is tracked in #338.
/// - `snippet` — single representative source line at `line` (no context window).
///   Absent when `line` is absent.
/// - `layers_matched` — layers that contributed non-zero signal for this file.
///   For standalone `--ast` queries: always `["ast"]`. For `text + --ast`
///   intersection: `["lexical","ast"]`. Temporal-as-sort does NOT count.
/// - `temporal` — temporal metadata; always `None` in `#201`. `#202` populates
///   it in the true compound dispatch path (tracked in #339).
#[derive(Debug, Clone, Serialize)]
pub struct AstResult {
    /// Repo-relative path (forward slashes, no leading `.`).
    pub path: String,
    /// AST BM25 relevance score.
    pub score: f64,
    /// 1-indexed representative line recovered by re-parse.
    ///
    /// `None` when no matching node was found (grammar drift, file changed,
    /// language lacks the pattern's kinds). Never `0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Source text of the representative line (no surrounding context).
    ///
    /// `None` iff `line` is `None`. Uses `skip_serializing_if` so the key is
    /// absent (not `null`) on degraded rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Layers that contributed non-zero signal for this file.
    ///
    /// Stable insertion order: lexical before ast before temporal.
    pub layers_matched: Vec<&'static str>,
    /// Temporal metadata populated by `#202`; always `None` in `#201`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalAnnotation>,
}

impl AstResult {
    /// Construct a result for a standalone AST-only query (no text, no temporal).
    ///
    /// `layers_matched` is always `["ast"]` on this path (AC-F5).
    #[must_use]
    pub fn ast_only(path: String, score: f64, line: Option<u32>, snippet: Option<String>) -> Self {
        // Invariant: snippet is Some iff line is Some.
        debug_assert!(
            line.is_some() || snippet.is_none(),
            "snippet must be None when line is None (AC-F4)"
        );
        Self {
            path,
            score,
            line,
            snippet,
            layers_matched: vec!["ast"],
            temporal: None,
        }
    }

    /// Construct a result for a `text + --ast` intersection query.
    ///
    /// `layers_matched` is `["lexical","ast"]` (stable order, AC-F6).
    #[must_use]
    pub fn lexical_ast(
        path: String,
        score: f64,
        line: Option<u32>,
        snippet: Option<String>,
    ) -> Self {
        debug_assert!(
            line.is_some() || snippet.is_none(),
            "snippet must be None when line is None (AC-F4)"
        );
        Self {
            path,
            score,
            line,
            snippet,
            layers_matched: vec!["lexical", "ast"],
            temporal: None,
        }
    }
}

// ============================================================================
// JSON envelope
// ============================================================================

/// JSON envelope for a compound AST query result (AC-F4, AC-API1).
///
/// Shape (clean corpus — `ast_coverage` absent when all counts are zero):
/// ```json
/// {
///   "mode": "ast",
///   "pattern": "try-catch",
///   "description": "...",
///   "total": 3,
///   "has_more": true,
///   "results": [
///     { "path": "src/foo.rs", "score": 2.45, "line": 42,
///       "snippet": "  fn foo() {", "layers_matched": ["ast"] }
///   ],
///   "ast_coverage": {
///     "size_eligible_files": 676,
///     "size_excluded_files": 1,
///     "undetermined_files": 0,
///     "excluded_by_lang": { "rust": 1 },
///     "excluded": [
///       { "path": "crates/rskim-search/src/weights.rs", "lang": "rust",
///         "size_bytes": 2700349, "limit_bytes": 1048576,
///         "reason": "ast_size_cap" }
///     ]
///   }
/// }
/// ```
///
/// Degraded rows omit `line` and `snippet` keys entirely (never `null`/`0`).
/// `has_more` is absent when `false` (additive, back-compat; AD-404-11).
/// `ast_coverage` is absent when all counts are zero (D-5 / AD-405-9).
#[derive(Serialize)]
struct AstJsonEnvelope<'a> {
    mode: &'static str,
    pattern: &'a str,
    description: &'a str,
    total: usize,
    /// Sound pagination terminator: true when more results exist beyond the
    /// current page. Replaces the unsound `results.len() < limit` heuristic.
    /// See AD-404-11 and D-5 user sign-off 2026-07-15.
    ///
    /// Absent when false (additive, back-compat).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    has_more: bool,
    results: &'a [AstResult],
    /// AST size-coverage summary (D-5 / AD-405-9).
    ///
    /// Absent when all counts are zero (`is_clean()` == true) — never `null`,
    /// never present-with-empty-arrays.  Same shape as `--stats --json` and
    /// pure-text `--ast` [`crate::QueryOutput`] (one Serialize struct, three
    /// surfaces — AD-405-3).
    #[serde(skip_serializing_if = "Option::is_none")]
    ast_coverage: Option<AstCoverage>,
}

// ============================================================================
// Terminal formatter
// ============================================================================

/// Format compound AST results as human-readable text (AC-F1, AC-F2, AC-API1).
///
/// Format for a result WITH a recovered line:
/// ```text
/// src/auth.rs:42  [0.87]  try-catch
///   pub async fn handle_auth(req: AuthRequest) -> Result<Token> {
/// ```
///
/// Format for a degraded row (no line recovered):
/// ```text
/// src/models/user.rs  [0.72]  try-catch
/// ```
///
/// Empty result at `offset == 0` → "no files match pattern" (exit 0, AC-F8).
/// Empty result at `offset > 0` → page-exhausted message (AC-404-10): the
/// caller has paged past all results, so the message distinguishes "no data"
/// from "paged too far".
/// When `excluded_count > 0`, the empty-result message appends the count
/// so agents know the pattern matched zero INDEXED files but N files were
/// excluded from AST indexing by the size cap (AC-405-10).
///
/// **Do NOT add an `Option<AstCoverage>` param here**: the JSON coverage key
/// is in `format_ast_json`'s envelope only; the text notice goes to stderr
/// at the call site (C3 / AD-405-4).  Only the scalar `excluded_count` is
/// needed to compose the empty-result message.
///
/// Mirrors `format_temporal_text` (in `rskim`) which takes a `Page` for the
/// same page-aware empty-message split; here `offset: usize` carries the one
/// field that drives formatting without pulling the `Page` type across the
/// crate boundary.
///
/// # Errors
///
/// Returns `Err` on I/O write failures.
pub fn format_ast_text(
    results: &[AstResult],
    pattern_name: &str,
    description: &str,
    offset: usize,
    excluded_count: u64,
    w: &mut impl Write,
) -> io::Result<()> {
    if description.is_empty() {
        writeln!(w, "AST pattern: {pattern_name}")?;
    } else {
        writeln!(w, "AST pattern: {pattern_name} — {description}")?;
    }
    writeln!(w)?;

    if results.is_empty() {
        if offset > 0 {
            // AC-404-8 / AC-404-10: page-aware empty — "paged past all matches".
            // AC-405-10: append excluded count when non-zero.
            if excluded_count > 0 {
                writeln!(
                    w,
                    "No more files match pattern {pattern_name:?} beyond offset {offset} \
                     (try a smaller --offset). ({excluded_count} file(s) excluded from \
                     AST indexing by size cap — run `skim search --stats --json`.)"
                )?;
            } else {
                writeln!(
                    w,
                    "No more files match pattern {pattern_name:?} beyond offset {offset} \
                     (try a smaller --offset)."
                )?;
            }
        } else {
            // AC-405-10: append excluded count when non-zero.
            if excluded_count > 0 {
                writeln!(
                    w,
                    "no files match pattern {:?} ({excluded_count} file(s) excluded from \
                     AST indexing by size cap — run `skim search --stats --json`.)",
                    pattern_name
                )?;
            } else {
                writeln!(w, "no files match pattern {:?}", pattern_name)?;
            }
        }
        return Ok(());
    }

    for r in results {
        match r.line {
            Some(ln) => {
                writeln!(w, "  {}:{ln}  [{:.3}]  {}", r.path, r.score, pattern_name)?;
                if let Some(ref snip) = r.snippet {
                    writeln!(w, "    {snip}")?;
                }
            }
            None => {
                // Degraded row: no `:line` suffix, no snippet (AC-F2).
                writeln!(w, "  {}  [{:.3}]  {}", r.path, r.score, pattern_name)?;
            }
        }
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

// ============================================================================
// JSON formatter
// ============================================================================

/// Format compound AST results as a JSON object (AC-F4, AC-API1, AD-404-11).
///
/// Empty results → `{"mode":"ast","total":0,"results":[]}` (AC-F8).
/// Degraded rows omit `line` and `snippet` keys (AC-F4 NEGATIVE).
///
/// `has_more`: sound pagination terminator — true when more results exist beyond
/// the current page or the candidate pool was capped; false on the last page.
/// The field is ABSENT from the JSON when false (additive, back-compat).
/// Replaces the unsound `results.len() < limit` heuristic (D-5 / AD-404-11).
///
/// `ast_coverage`: when `Some`, the JSON envelope includes the `ast_coverage`
/// key with counts and a bounded excluded-file sample (AD-405-9 / D-5).
/// Pass `None` on a clean corpus (no excluded or undetermined files) to omit
/// the key entirely — never serialize an empty struct as `null` (AC-405-9).
///
/// # Errors
///
/// Returns `Err` on I/O write failures or JSON serialisation errors.
pub fn format_ast_json(
    results: &[AstResult],
    pattern_name: &str,
    description: &str,
    has_more: bool,
    ast_coverage: Option<AstCoverage>,
    w: &mut impl Write,
) -> io::Result<()> {
    let envelope = AstJsonEnvelope {
        mode: "ast",
        pattern: pattern_name,
        description,
        total: results.len(),
        has_more,
        results,
        ast_coverage,
    };
    let json = serde_json::to_string_pretty(&envelope)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writeln!(w, "{json}")
}

// ============================================================================
// Tests (co-located in output_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
