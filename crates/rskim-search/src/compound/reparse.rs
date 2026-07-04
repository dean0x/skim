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

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use rskim_core::{AstWalkConfig, AstWalkIter, Language, Parser};

use crate::ast_index::structural::is_synthetic_id;
use crate::ast_index::{
    AstBigram, AstNgramSet, AstQuery, AstTrigram, NodeKindId, extract_ast_ngrams_with_lines,
    extract_ast_ngrams_with_metrics, linearize_source, vocab_lookup,
};

/// Maximum file size for re-parse operations.
///
/// Matches `linearize.rs::MAX_FILE_SIZE` (100 KiB) so that only files that
/// were eligible for AST indexing are re-parsed. Files above this cap degrade
/// to file-level output (`None`).
pub const MAX_REPARSE_FILE_BYTES: u64 = 100 * 1024;

/// Recover the representative line for a matched AST pattern in a source file.
///
/// For a REAL-node pattern, walks the file's CST in **pre-order** and returns
/// the 1-indexed line number and byte range of the **first** node whose kind
/// matches any of the pattern's resolved bigrams or trigrams (parent→child /
/// grandparent→parent→child relationships).
///
/// ## AD-394-5: Synthetic-marker patterns (OD-394-1 — recover now)
///
/// The 5 synthetic-marker patterns (god-function, deep-nesting, empty-function,
/// empty-catch, excessive-params) can never match the pre-order walk above —
/// `vocab_lookup` never yields a synthetic ID, so the walk's bigram/trigram
/// comparison is structurally unsatisfiable for them. These are routed
/// (`query_contains_synthetic_id`, the SAME predicate the verify gate uses) to
/// a separate branch that re-runs the index-time extraction pipeline
/// (`extract_ast_ngrams_with_lines`) and reads back the representative
/// `(line, byte)` position it recorded for the emitted marker — the marker and
/// its line come from ONE pass (ADR-006), not a second, drift-prone detection
/// re-implementation. Per-pattern representative-line rule: report the
/// 1-indexed start line of the node the marker's condition is measured on,
/// resolving up to the enclosing named construct for anonymous body/param
/// blocks — god-function → enclosing function; empty-function →
/// `function_item`; empty-catch → `catch_clause`; excessive-params →
/// parameter-list; deep-nesting → first node crossing depth ≥ 4.
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
/// The synthetic branch is equally deterministic: the MIN line per marker key
/// is recorded (topmost occurrence), so identical input always yields the same
/// result.
///
/// ## Bounded work (AC-API3)
///
/// This function re-parses ONE file. Callers apply `--limit` before iterating,
/// so at most `limit` files are re-parsed per query.
///
/// ## Deferred precision
///
/// Only the first (real-node branch) or topmost (synthetic branch) matching
/// node is returned. All-occurrences line precision is tracked in #338.
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

    // Read the file — shared by both branches below.
    let content = std::fs::read(file_path).ok()?;
    let source = std::str::from_utf8(&content).ok()?;

    // AD-394-5: synthetic-marker patterns cannot be recovered by the
    // pre-order-predecessor `MatchTable` walk below (`vocab_lookup` never
    // yields a synthetic ID, so `match_table.matches` can never fire for
    // them — they would always degrade to `None`, rendering a path-only row).
    // Route them instead through the SAME extraction pass that emits the
    // marker (`extract_ast_ngrams_with_lines`), reading back the representative
    // position it recorded (ADR-006: one pass produces both the marker and its
    // line — no second, drift-prone detection re-implementation). Routed by
    // the SAME predicate as the verify gate (`query_contains_synthetic_id`) —
    // one routing rule, no divergence.
    if query_contains_synthetic_id(query) {
        let result = linearize_source(source, lang).ok()?;
        let (_emitted, _metrics, synthetic_lines) =
            extract_ast_ngrams_with_lines(&result.nodes, lang);
        return recover_synthetic_line(query, &synthetic_lines);
    }

    // Only tree-sitter languages can be re-parsed; non-tree-sitter langs degrade.
    // We check by attempting Parser::new — if the language has no grammar, it returns Err.
    let mut parser = Parser::new(lang).ok()?;

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

/// AD-394-5: resolve `query`'s resolved synthetic bigram key(s) against the
/// `synthetic_lines` side table produced by [`extract_ast_ngrams_with_lines`],
/// and return the MIN `(line, byte)` across every key that is present.
///
/// Only [`AstQuery::Pattern`] can ever reach this function with a non-empty
/// key set: [`query_contains_synthetic_id`] is `false` for
/// [`AstQuery::Containment`] (the parser rejects synthetic-marker names in a
/// containment query — AC7) and for [`AstQuery::SingleNode`], so those
/// variants are handled defensively (empty key list → `None`) but are
/// unreachable in practice.
///
/// Deterministic (AC-F3): `synthetic_lines` already stores the MIN line per
/// key (recorded at emission time), so taking the min across the (today,
/// always single) resolved key is a deterministic function of the file.
fn recover_synthetic_line(
    query: &AstQuery,
    synthetic_lines: &HashMap<AstBigram, (u32, u32)>,
) -> Option<(u32, Range<usize>)> {
    let keys: Vec<AstBigram> = match query {
        AstQuery::Pattern(pattern) => pattern.resolved_bigrams(),
        AstQuery::Containment(ngram_set) => ngram_set.bigrams.iter().map(|e| e.ngram).collect(),
        AstQuery::SingleNode(_) => Vec::new(),
    };

    let mut best: Option<(u32, u32)> = None;
    for key in keys {
        if let Some(&(line, byte)) = synthetic_lines.get(&key) {
            best = Some(match best {
                Some((best_line, best_byte)) if best_line <= line => (best_line, best_byte),
                _ => (line, byte),
            });
        }
    }

    best.map(|(line, byte)| (line, (byte as usize)..(byte as usize + 1)))
}

/// Verify that a source file's CST contains at least one node whose **real
/// ancestor chain** matches the pattern's resolved n-grams.
///
/// This is the structural verify gate (Part B) for the AND-intersect→verify→
/// truncate-LAST architecture (AD-374-2).  Unlike [`recover_line`], which uses
/// the pre-order predecessor as a bigram approximation, this function walks the
/// CST and for each node checks its **real parent chain** via `node.parent()`:
///
/// - **Bigram** `(P, C)`: node is kind `C` and `node.parent()` is kind `P`.
/// - **Trigram** `(GP, P, C)`: node is kind `C`, `node.parent()` is kind `P`,
///   and `node.parent().parent()` is kind `GP`.
///
/// This is intentionally STRICT (AD-374-6, OD-374-3 resolved → STRICT): the
/// gate does NOT reproduce the indexer's ERROR/MISSING-node depth-jump gap-fill
/// (`extract.rs`).  The purpose of the gate is precision — files containing the
/// correct ancestor relationship, not approximations.  An ERROR-node edge that
/// the indexer accepted via gap-fill will NOT survive the strict gate; this is
/// correct behavior.  PF-004 governs the index BUILD's u16 depth arithmetic in
/// `extract.rs`; this gate only compares node KINDS (no depth values) so PF-004
/// does NOT apply here.
///
/// ## AD-394-1 / AD-394-2: Synthetic-marker patterns route through extraction-reuse
///
/// The strict walk above is structurally unsatisfiable for the 5 synthetic-marker
/// patterns (god-function, deep-nesting, empty-function, empty-catch,
/// excessive-params): their resolved n-grams contain a synthetic ID
/// (`>= BUCKET_LABEL_BASE`), and `vocab_lookup` — which resolves every real
/// `node.parent()` kind in this walk — never yields a synthetic ID (#394's
/// root cause: standalone `--ast` returned zero results for all 5).  Before
/// reaching this walk, [`AncestorMatchTable::contains_synthetic_id`] routes any
/// such pattern to a SEPARATE branch that re-runs the index-time pipeline
/// (`linearize_source` + `extract_ast_ngrams_with_metrics`) and confirms every
/// resolved n-gram KEY is present in the emitted set (reuses the indexer as the
/// single source of truth — applies ADR-006).  Real-node patterns are NOT
/// routed through extraction-reuse (AD-394-2) — collapsing the two branches
/// would loosen the 24 real patterns to the indexer's gap-fill tolerance,
/// regressing AD-374-6 precision.
///
/// ## AD-374-5: Non-tree-sitter / zero-kind files drop
///
/// Files whose language has no tree-sitter grammar (JSON/TOML/YAML), or patterns
/// that resolve to an empty match table, return `false` (never panic).  This
/// removes `Cargo.toml`/`.json` from structural results.
///
/// ## AD-374-7: `recover_line` remains line-recovery only
///
/// After the gate, surviving files still call [`recover_line`] for `:line`.  Its
/// fail-soft `None` no longer leaks false positives because non-matching files
/// were already dropped here.
///
/// ## Return value
///
/// - `true`  — at least one node in the CST matches the declared ancestor relationship.
/// - `false` — returned (never panics) for: non-tree-sitter language, empty resolved
///   match table, file > [`MAX_REPARSE_FILE_BYTES`], mtime mismatch vs
///   `manifest_mtime`, unreadable/non-UTF8 file, parse failure, no matching ancestor
///   edge.
///
/// ## AD-374-4: Relevance gate, not a #317 output cap
///
/// Dropping candidates that fail this gate is a relevance filter; it does not hide
/// output the user would otherwise legitimately see, so no `output::elision_marker`
/// is required.  Mirrors AD-355-4 on the lexical path.
pub fn pattern_occurs_in_file(
    file_path: &Path,
    query: &AstQuery,
    manifest_mtime: Option<u64>,
) -> bool {
    // Resolve the query into an ancestor-correct match table FIRST (AD-374-6).
    // Building the table is pure and file-independent, so an empty table
    // (pattern has no resolvable kinds in this process's vocabulary) short-
    // circuits before any filesystem I/O (AD-374-5).
    let ancestor_table = AncestorMatchTable::build(query);
    if ancestor_table.is_empty() {
        return false;
    }

    // Guard: file must exist and be readable as metadata.
    let Ok(meta) = std::fs::metadata(file_path) else {
        return false;
    };

    // Mtime guard: if the manifest recorded an mtime and it doesn't match,
    // the file has changed since indexing — drop (conservative; mirrors recover_line).
    if let Some(stored_mtime) = manifest_mtime {
        let current_mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        if current_mtime != Some(stored_mtime) {
            return false;
        }
    }

    // Size guard: must be within the re-parse cap (AD-374-5).
    if meta.len() > MAX_REPARSE_FILE_BYTES {
        return false;
    }

    // Detect language from extension; non-tree-sitter langs drop (AD-374-5).
    let Some(lang) = Language::from_path(file_path) else {
        return false;
    };

    // Read the file — shared by both branches below.
    let Ok(content) = std::fs::read(file_path) else {
        return false;
    };
    let Ok(source) = std::str::from_utf8(&content) else {
        return false;
    };

    // AD-394-1 / AD-394-2: route synthetic-marker patterns (any resolved
    // n-gram component >= BUCKET_LABEL_BASE) through extraction-reuse; real-
    // node patterns keep the strict ancestor walk below. Do NOT collapse the
    // two branches — see AD-394-2 for why real patterns must not be loosened.
    if ancestor_table.contains_synthetic_id() {
        // AD-394-1: verify by re-running the index-time pipeline and confirming
        // every resolved n-gram KEY is present in the emitted AstNgramSet.
        // `linearize_source` parses internally, so no separate `Parser::new`
        // is needed on this branch (AD-374-5 guards — size/non-tree-sitter —
        // are already inherited from the shared guards above; linearize_source's
        // own internal size/language guards degrade to an empty result rather
        // than ever firing here).
        let Ok(result) = linearize_source(source, lang) else {
            return false;
        };
        // CRITICAL (AD-394-1): destructure the tuple return —
        // extract_ast_ngrams_with_metrics returns (AstNgramSet, StructuralMetrics),
        // not just the ngram set.
        //
        // KNOWN PERF CHARACTERISTIC (#419, measured on skim's own repo): this
        // branch is markedly heavier per-candidate than the real-node walk
        // below (full per-file metrics recomputation vs. a bounded ancestor
        // walk), and for a low-selectivity marker like deep-nesting (common
        // in real code) the candidate pool fills to CANDIDATE_POOL_FLOOR
        // (query.rs) regardless of --limit. Dogfooding showed `--ast
        // deep-nesting` at ~6.9s vs. `--ast try-catch` at ~26ms on this
        // repo — correctness is unaffected, but this misses the design
        // plan's AC11 (<500ms, same order of magnitude as the real gate).
        // A lighter-weight early-exit presence check is tracked in #419 —
        // not fixed here to avoid an under-designed change to the shared
        // extraction path.
        let (emitted, _metrics) = extract_ast_ngrams_with_metrics(&result.nodes, lang);
        return ancestor_table.all_ngrams_present_in(&emitted);
    }

    // Real-node branch (AD-394-2, unchanged): only tree-sitter languages can be
    // re-parsed; non-tree-sitter langs drop (AD-374-5: JSON/TOML/YAML have no
    // grammar → Parser::new returns Err).
    let Ok(mut parser) = Parser::new(lang) else {
        return false;
    };
    let Ok(tree) = parser.parse(source) else {
        return false;
    };

    // Walk the CST in pre-order and check each node's REAL parent chain.
    let walk_config = AstWalkConfig::default();
    let iter = AstWalkIter::new(tree.walk(), walk_config);

    for walk_node in iter {
        let node = walk_node.node;

        // Map tree-sitter kind string → global vocabulary NodeKindId.
        let Some(child_id) = vocab_lookup(node.kind()) else {
            continue;
        };

        // Check bigrams (P, C): node is C, node.parent() is P.
        if let Some(parent_node) = node.parent() {
            let Some(parent_id) = vocab_lookup(parent_node.kind()) else {
                // Parent kind not in vocab — cannot match any bigram/trigram.
                continue;
            };

            // Bigram check: (parent_id, child_id).
            if ancestor_table.bigrams.contains(&(parent_id, child_id)) {
                return true;
            }

            // Trigram check (GP, P, C): parent.parent() is GP.
            // AD-374-6 / OD-374-3 (STRICT): require full grandparent→parent→child
            // ancestor chain via real node.parent(), not an approximation.
            if !ancestor_table.trigram_children.is_empty()
                && let Some(gp_node) = parent_node.parent()
            {
                let Some(gp_id) = vocab_lookup(gp_node.kind()) else {
                    continue;
                };
                if ancestor_table
                    .trigrams
                    .contains(&(gp_id, parent_id, child_id))
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Precomputed lookup table for ancestor-correct CST matching (AD-374-6).
///
/// Unlike [`MatchTable`] (which uses the pre-order predecessor as a bigram
/// approximation), this table is used by [`pattern_occurs_in_file`] and stores
/// the complete ancestor relationship for strict verification:
///
/// - `bigrams`: set of `(parent_id, child_id)` pairs — checked via real
///   `node.parent()`.
/// - `trigrams`: set of `(gp_id, parent_id, child_id)` triples — checked via
///   real `node.parent().parent()` (OD-374-3 resolved → STRICT).
/// - `trigram_children`: set of child-ids for any trigram — used as a fast
///   pre-check before evaluating the full triple.
///
/// **Divergence from `MatchTable` is intentional** (AD-374-6): do NOT simplify
/// these two tables into one — `MatchTable` serves line-recovery (approximate
/// pre-order context) while `AncestorMatchTable` serves the verify gate (exact
/// ancestor chain).
struct AncestorMatchTable {
    bigrams: HashSet<(NodeKindId, NodeKindId)>,
    trigrams: HashSet<(NodeKindId, NodeKindId, NodeKindId)>,
    /// Fast pre-check: set of child-ids that appear in any trigram.
    /// Avoids evaluating the full grandparent chain when `child_id` is not in
    /// any trigram at all.
    trigram_children: HashSet<NodeKindId>,
}

impl AncestorMatchTable {
    /// Resolve `query` into the strict ancestor lookup table.
    fn build(query: &AstQuery) -> Self {
        let mut bigrams = HashSet::new();
        let mut trigrams = HashSet::new();
        let mut trigram_children = HashSet::new();

        match query {
            AstQuery::Pattern(pattern) => {
                for bigram in pattern.resolved_bigrams() {
                    let (parent, child) = bigram.decode();
                    bigrams.insert((parent, child));
                }
                for trigram in pattern.resolved_trigrams() {
                    let (gp, parent, child) = trigram.decode();
                    trigrams.insert((gp, parent, child));
                    trigram_children.insert(child);
                }
            }
            AstQuery::Containment(ngram_set) => {
                for entry in &ngram_set.bigrams {
                    let (parent, child) = entry.ngram.decode();
                    bigrams.insert((parent, child));
                }
                for entry in &ngram_set.trigrams {
                    let (gp, parent, child) = entry.ngram.decode();
                    trigrams.insert((gp, parent, child));
                    trigram_children.insert(child);
                }
            }
            // SingleNode is rejected at the CLI boundary; empty table → false (AD-374-5).
            AstQuery::SingleNode(_) => {}
        }

        Self {
            bigrams,
            trigrams,
            trigram_children,
        }
    }

    /// `true` when the query resolved to no matchable kinds in this grammar.
    fn is_empty(&self) -> bool {
        self.bigrams.is_empty() && self.trigrams.is_empty()
    }

    /// AD-394-1 / AD-394-2: `true` iff any resolved bigram/trigram component is
    /// a synthetic marker ID (`is_synthetic_id`, i.e. `>= BUCKET_LABEL_BASE`).
    ///
    /// Routes the pattern to the extraction-reuse verify branch in
    /// [`pattern_occurs_in_file`] instead of the strict ancestor walk: a
    /// synthetic ID can never appear as a real `node.parent()` chain member
    /// (`vocab_lookup` never yields one), so the strict walk is structurally
    /// unsatisfiable for these patterns. Real-node patterns (this returns
    /// `false`) keep the strict walk unchanged (AD-394-2 — do not collapse the
    /// two branches; this preserves AD-374-6 precision for the 24 real
    /// patterns).
    fn contains_synthetic_id(&self) -> bool {
        self.bigrams
            .iter()
            .any(|&(p, c)| is_synthetic_id(p) || is_synthetic_id(c))
            || self
                .trigrams
                .iter()
                .any(|&(gp, p, c)| is_synthetic_id(gp) || is_synthetic_id(p) || is_synthetic_id(c))
    }

    /// AD-394-1: re-encode this table's decoded bigrams/trigrams and check
    /// that EVERY one is present as a KEY in `set` (weights/counts ignored) —
    /// the extraction-reuse verify check for synthetic-marker patterns. `set`
    /// is sorted by key ascending (guaranteed by [`AstNgramSet`]'s producer),
    /// so each lookup is `O(log n)` via binary search.
    ///
    /// An empty `self.trigrams` (true for all 5 synthetic marker patterns
    /// today — they are single-bigram/zero-trigram) makes the trigram
    /// conjunct vacuously `true`.
    fn all_ngrams_present_in(&self, set: &AstNgramSet) -> bool {
        self.bigrams.iter().all(|&(p, c)| {
            let key = AstBigram::encode(p, c);
            set.bigrams.binary_search_by_key(&key, |e| e.ngram).is_ok()
        }) && self.trigrams.iter().all(|&(gp, p, c)| {
            let key = AstTrigram::encode(gp, p, c);
            set.trigrams.binary_search_by_key(&key, |e| e.ngram).is_ok()
        })
    }
}

/// AD-394-1 / AD-394-5: shared routing predicate used by BOTH the verify gate
/// (`pattern_occurs_in_file`) and `recover_line` to decide whether `query`
/// routes through extraction-reuse — one routing rule, no divergence between
/// the two call sites. Delegates to [`AncestorMatchTable::contains_synthetic_id`]
/// so there is a single implementation of "does this query touch a synthetic
/// marker ID" (built from [`is_synthetic_id`]).
fn query_contains_synthetic_id(query: &AstQuery) -> bool {
    AncestorMatchTable::build(query).contains_synthetic_id()
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
