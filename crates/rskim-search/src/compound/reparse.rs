//! Line-span re-parse for AST-only search results (#201).
//!
//! # Problem
//!
//! The AST index is file-level: `LinearNode` stores `{kind_id, depth}` with no
//! byte offset. An AST query therefore matches at file granularity. To show the
//! user a `:line` number, we re-parse the matched file after the search, walk
//! the CST in pre-order, and return the **first** node whose kind participates
//! in the pattern's resolved bigrams/trigrams.
//!
//! # Design
//!
//! - **Best-effort, not exact.** The re-parse returns a *representative* line —
//!   the first matching node in pre-order. Files with multiple occurrences of
//!   the pattern show only one. Exact every-occurrence line precision is deferred
//!   to #338.
//! - **Deterministic.** Same file + same pattern → same line on every run.
//!   Pre-order tree-sitter walk is deterministic for unchanged source.
//! - **Fail-soft.** Returns `None` (never panics, never errors) for:
//!   - File larger than the re-parse size guard (100 KiB, matching the AST index
//!     linearisation cap so only files that COULD have been indexed are re-parsed).
//!   - File unreadable, deleted, or non-UTF8.
//!   - Language has no tree-sitter grammar (JSON/YAML/TOML etc.).
//!   - Pattern's node kinds are absent in the file's grammar.
//!   - File's extension does not map to a known language.
//! - **Bounded.** Callers must apply `--limit` BEFORE calling this function.
//!   This file itself is a pure function with no knowledge of limit; the bound
//!   is enforced by the caller (AC-API3).
//!
//! ## Re-parse size guard
//!
//! 100 KiB — the same cap used by `linearize.rs::MAX_FILE_SIZE`. Files larger
//! than 100 KiB are not in the AST index (they were never linearised), so
//! attempting to re-parse them would be dead range. If a file grew beyond 100 KiB
//! since indexing, the mtime will differ and the caller's stale guard will degrade
//! to file-level output before this function is called.

use std::collections::HashSet;
use std::ops::Range;
use std::path::Path;

use rskim_core::{AstWalkConfig, AstWalkIter, Language, Parser};

use crate::ast_index::{AstQuery, NodeKindId, vocab_lookup};

/// Maximum file size for re-parse operations.
///
/// Matches `linearize.rs::MAX_FILE_SIZE` (100 KiB) so that only files that
/// were eligible for AST indexing are re-parsed. Files above this cap degrade
/// to file-level output (`None`).
pub const MAX_REPARSE_FILE_BYTES: u64 = 100 * 1024;

/// Recover the representative line for a matched AST pattern in a source file.
///
/// Walks the file's CST in **pre-order** and returns the 1-indexed line number
/// and byte range of the **first** node whose kind matches any of the pattern's
/// resolved bigrams or trigrams (parent→child / grandparent→parent→child
/// relationships).
///
/// ## Return value
///
/// - `Some((line, byte_range))` — `line` is 1-indexed and ≥ 1; `byte_range` is
///   within the file's byte length.
/// - `None` — degraded (file too large, unreadable, non-tree-sitter language,
///   pattern kinds absent, or parse failed). The command still exits 0.
///
/// ## Determinism (AC-F3)
///
/// Pre-order tree-sitter traversal is deterministic for unchanged source. The
/// same file + same pattern always yields the same `(line, byte_range)` tuple.
///
/// ## Bounded work (AC-API3)
///
/// This function re-parses ONE file. Callers apply `--limit` before iterating,
/// so at most `limit` files are re-parsed per query.
///
/// ## Deferred precision
///
/// Only the first matching node is returned. All-occurrences line precision is
/// tracked in #338.
pub fn recover_line(
    file_path: &Path,
    query: &AstQuery,
    manifest_mtime: Option<u64>,
) -> Option<(u32, Range<usize>)> {
    // Guard: file must exist and be readable as metadata.
    let meta = std::fs::metadata(file_path).ok()?;

    // Mtime guard: if the manifest recorded an mtime and it doesn't match,
    // the file has changed since indexing — positions may be stale → degrade.
    if let Some(stored_mtime) = manifest_mtime {
        let current_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        if current_mtime != Some(stored_mtime) {
            return None;
        }
    }

    // Size guard: must be within the re-parse cap.
    if meta.len() > MAX_REPARSE_FILE_BYTES {
        return None;
    }

    // Detect language from extension.
    let lang = Language::from_path(file_path)?;

    // Only tree-sitter languages can be re-parsed; non-tree-sitter langs degrade.
    // We check by attempting Parser::new — if the language has no grammar, it returns Err.
    let mut parser = Parser::new(lang).ok()?;

    // Read the file.
    let content = std::fs::read(file_path).ok()?;
    let source = std::str::from_utf8(&content).ok()?;

    // Parse.
    let tree = parser.parse(source).ok()?;

    // Resolve the query ONCE into an O(1) lookup table for the CST walk.
    // The resolved bigram/trigram sets are loop-invariant (they depend only on
    // `query`, not on any node), so resolving them per node would re-allocate and
    // re-run `resolve_kind_name` for every node in the tree.
    let match_table = MatchTable::build(query);

    if match_table.is_empty() {
        // Pattern has no resolvable kinds in this grammar → degrade.
        return None;
    }

    // Walk the CST in pre-order.
    let walk_config = AstWalkConfig::default();
    let iter = AstWalkIter::new(tree.walk(), walk_config);
    let mut prev_kind: Option<NodeKindId> = None;

    // The AstWalkIter visits nodes in pre-order; we inspect each consecutive
    // (prev, current) pair against the precomputed table. For a bigram (P, C) we
    // report the C node's location when we observe prev_kind == P followed by
    // current kind == C. Trigrams are approximated by their innermost child
    // (exact trigram re-match tracked in #338).
    //
    // Implementation note: tree-sitter node kinds use per-grammar numeric IDs,
    // not global vocabulary IDs. We map via `vocab_lookup(node.kind())`.
    for walk_node in iter {
        let node = walk_node.node;
        let kind_str = node.kind();

        // Map tree-sitter kind string → global vocabulary NodeKindId.
        let Some(kind_id) = vocab_lookup(kind_str) else {
            prev_kind = None; // Unknown kind breaks the bigram chain.
            continue;
        };

        // O(1) match check against the precomputed table, given the bigram context.
        if match_table.matches(prev_kind, kind_id) {
            // Found! Recover 1-indexed line and byte range.
            let row = node.start_position().row; // 0-indexed
            // Widen usize → u32 safely; line numbers beyond u32::MAX are
            // treated as a match at u32::MAX (extremely unlikely in practice).
            let line = u32::try_from(row).unwrap_or(u32::MAX).saturating_add(1); // → 1-indexed
            let byte_range = node.byte_range();
            return Some((line, byte_range));
        }

        prev_kind = Some(kind_id);
    }

    // No matching node found.
    None
}

/// Precomputed O(1) lookup table for the CST walk.
///
/// Resolving a query's bigrams/trigrams is loop-invariant, so we resolve once
/// and store the result as hash sets the per-node walk can probe in O(1):
/// - `bigrams`: the `(parent, child)` kind pairs of every resolved bigram.
/// - `trigram_children`: the innermost-child kind of every resolved trigram.
///   Parent/grandparent context is approximated — exact trigram re-match is
///   tracked in #338.
///
/// [`AstQuery::Pattern`] and [`AstQuery::Containment`] share identical match
/// logic and differ only in their source of bigrams/trigrams, so both collapse
/// into one table.
struct MatchTable {
    bigrams: HashSet<(NodeKindId, NodeKindId)>,
    trigram_children: HashSet<NodeKindId>,
}

impl MatchTable {
    /// Resolve `query` into the lookup table once, before the walk begins.
    fn build(query: &AstQuery) -> Self {
        let mut bigrams = HashSet::new();
        let mut trigram_children = HashSet::new();
        match query {
            AstQuery::Pattern(pattern) => {
                for bigram in pattern.resolved_bigrams() {
                    let (parent, child) = bigram.decode();
                    bigrams.insert((parent, child));
                }
                for trigram in pattern.resolved_trigrams() {
                    let (_, _, child) = trigram.decode();
                    trigram_children.insert(child);
                }
            }
            AstQuery::Containment(ngram_set) => {
                for entry in &ngram_set.bigrams {
                    let (parent, child) = entry.ngram.decode();
                    bigrams.insert((parent, child));
                }
                for entry in &ngram_set.trigrams {
                    let (_, _, child) = entry.ngram.decode();
                    trigram_children.insert(child);
                }
            }
            // SingleNode is rejected at the CLI boundary (validate_ast_pattern);
            // an empty table degrades recover_line to None.
            AstQuery::SingleNode(_) => {}
        }
        Self {
            bigrams,
            trigram_children,
        }
    }

    /// `true` when the query resolved to no matchable kinds in this grammar.
    fn is_empty(&self) -> bool {
        self.bigrams.is_empty() && self.trigram_children.is_empty()
    }

    /// Whether the `current` node kind matches, given the preceding kind `prev`.
    ///
    /// Preserves the original per-node semantics exactly: a `None` predecessor
    /// never matches (the bigram parent context is unavailable, and this gates
    /// the trigram check too), and otherwise a match is either a resolved
    /// `(prev, current)` bigram pair or a resolved trigram whose innermost child
    /// is `current`.
    fn matches(&self, prev: Option<NodeKindId>, current: NodeKindId) -> bool {
        let Some(prev_kind) = prev else {
            return false;
        };
        self.bigrams.contains(&(prev_kind, current)) || self.trigram_children.contains(&current)
    }
}

// ============================================================================
// Tests (co-located in reparse_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "reparse_tests.rs"]
mod tests;
