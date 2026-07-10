//! Line-span re-parse for AST-only search results (#201, #397).
//!
//! # Problem
//!
//! The AST index is file-level: `LinearNode` stores `{kind_id, depth}` with no
//! byte offset. An AST query therefore matches at file granularity. To show the
//! user a `:line` number, we re-parse the matched file after the search, walk
//! the CST in pre-order, and return the **first** node whose ancestor chain
//! participates in the pattern's resolved bigrams/trigrams.
//!
//! # Design (post-#397)
//!
//! - **Single strict-ancestor predicate for gate and anchor (AD-397-1).**
//!   [`find_first_strict_match`] is the sole gate+anchor implementation for
//!   real-node patterns ("does this file contain the queried structure, and if
//!   so, which line?"). Both the verify
//!   gate and the line anchor read from the same function call, so they can
//!   never disagree (ADR-007 anchor-trust satisfied by construction).
//! - **Real-node-only unify (AD-397-2).** The bug (wrong anchor lines,
//!   missing lines for bigram-with-anonymous-token patterns) was entirely in
//!   the old `MatchTable` pre-order-predecessor approximation. The five
//!   synthetic-marker patterns (god-function, deep-nesting, empty-function,
//!   empty-catch, excessive-params) use a separate two-function flow
//!   ([`pattern_occurs_in_file`] + [`recover_line`]) whose AND-semantics and
//!   early-exit performance are preserved untouched.
//! - **Atomic snippet (Decision 9, DECISIONS-NEEDED.md).** [`find_first_strict_match`]
//!   returns the matched line's TEXT alongside `(line, byte_range)`, extracted
//!   from the source that is already in memory.  This eliminates the TOCTOU
//!   race between the gate parse and a separate `read_line_at` call.
//! - **Best-effort, not exact.** Returns the *first* matching node in pre-order.
//!   Exact every-occurrence line precision is deferred to #338.
//! - **Deterministic.** Same file + same pattern → same line on every run.
//!   Pre-order tree-sitter walk is deterministic for unchanged source.
//! - **Fail-soft.** Returns `None` (never panics, never errors) for:
//!   - File larger than the re-parse size guard (100 KiB).
//!   - File unreadable, deleted, or non-UTF8.
//!   - Language has no tree-sitter grammar (JSON/YAML/TOML etc.).
//!   - Pattern's node kinds are absent in the file's grammar.
//!   - File's extension does not map to a known language.
//! - **Bounded.** For real-node patterns, [`find_first_strict_match`] — the
//!   primary gate and anchor — runs over the **candidate pool** (pre-truncation;
//!   the pool size is `max(5 * limit, 100)` — AD-374-3). Only the synthetic-
//!   marker recovery path ([`recover_line`]) is bounded to ≤ `--limit` files
//!   (post-truncation, AC-API3). The `--limit` cap is applied by the caller
//!   after the gate completes; this module never applies it itself.
//!
//! ## Semantic contract (shared with #396)
//!
//! The reported `line` is the **1-indexed START line** of the match: for
//! lexical results it is the first line containing all query tokens (or the
//! rarest token for multi-token queries where no single line contains all);
//! for AST results it is the start line of the **queried structure** (the
//! child node `C` in a bigram `P > C`, or the innermost child in a trigram).
//! Where a `line_range` is present it is the single anchor line `{n, n+1}`.
//! Cross-mode field-name uniformity is tracked in #423.
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
    AstBigram, AstQuery, NodeKindId, extract_ast_ngrams_with_lines, linearize_source,
    synthetic_key_present, vocab_lookup,
};

/// Maximum file size for re-parse operations.
///
/// Matches `linearize.rs::MAX_FILE_SIZE` (100 KiB) so that only files that
/// were eligible for AST indexing are re-parsed. Files above this cap degrade
/// to file-level output (`None`).
pub const MAX_REPARSE_FILE_BYTES: u64 = 100 * 1024;

/// Find the first CST node matching the query's strict ancestor relationship.
///
/// Returns `Some((line, byte_range, snippet))` where:
/// - `line` is the **1-indexed** start line of the matched child node `C` —
///   the same semantic as #396's lexical `line_number` (see module doc).
///   Never 0.
/// - `byte_range` is the child node's byte span within the file.
/// - `snippet` is the text of the matched line, extracted from the already-
///   loaded source (Decision 9, DECISIONS-NEEDED.md: atomic read+parse, no second file access).
///
/// ## AD-397-1: Single strict-ancestor predicate
///
/// Gate and anchor share ONE call. The anchor is the node the gate admitted,
/// so they cannot disagree. Satisfies ADR-007 (anchor-trust) by construction.
///
/// ## AD-397-2: Real-node strict walk
///
/// Returns the **first** pre-order node whose *real* `node.parent()` (bigram)
/// or `node.parent().parent()` (trigram) chain matches [`AncestorMatchTable`],
/// replacing the old `MatchTable` pre-order-predecessor + any-innermost-child
/// approximation. The anchored node is the child `C`; its
/// `start_position().row` gives the 0-indexed row, converted to 1-indexed.
/// This is why `unsafe_block > block` now anchors at the correct line: the
/// intervening `unsafe` keyword is no longer confused with the parent.
///
/// ## AD-397-3: Synthetic patterns excluded
///
/// This function handles **real-node patterns only**. For synthetic-marker
/// patterns (god-function, deep-nesting, empty-function, empty-catch,
/// excessive-params), [`query_contains_synthetic_id`] returns `true` and this
/// function returns `None`. Callers route those patterns through the unchanged
/// two-function flow ([`pattern_occurs_in_file`] + [`recover_line`]), which
/// preserves the AND-semantics gate (`synthetic_key_present` with `.all()`)
/// and the early-exit #419 performance fix.
///
/// ## AD-397-6: No index-format change
///
/// Pure query-time display logic. No index artifact is read differently or
/// rewritten. FORMAT_VERSION is unchanged. ADR-006 abort-before-manifest-
/// persist build invariant is untouched. PF-004 (u16→u32 depth widening) does
/// not apply — this path compares node *kinds*, not depth arithmetic.
///
/// ## Determinism (AC7)
///
/// Pre-order tree-sitter traversal is deterministic for unchanged source.
/// Same file + same pattern → same `(line, byte_range, snippet)` on every run.
///
/// ## Returns `None` for
///
/// - Synthetic patterns (see AD-397-3).
/// - File larger than [`MAX_REPARSE_FILE_BYTES`] (100 KiB).
/// - File unreadable, deleted, or non-UTF8.
/// - Language has no tree-sitter grammar (JSON/YAML/TOML etc.).
/// - Pattern's resolved n-grams do not match any real ancestor edge.
/// - mtime mismatch between the file and `manifest_mtime`.
pub fn find_first_strict_match(
    file_path: &Path,
    query: &AstQuery,
    manifest_mtime: Option<u64>,
) -> Option<(u32, Range<usize>, String)> {
    // Resolve the ancestor table — pure, no I/O. An empty table means the
    // pattern has no resolvable kinds in the process's vocabulary.
    let ancestor_table = AncestorMatchTable::build(query);
    if ancestor_table.is_empty() {
        return None;
    }

    // AD-397-3: Synthetic-marker patterns route through the two-function flow
    // (pattern_occurs_in_file + recover_line). This function is real-node only.
    if ancestor_table.contains_synthetic_id() {
        return None;
    }

    // I/O guards: metadata → mtime → size → language → read (single owner via
    // read_guarded; cannot drift across the three re-parse entry points).
    let (lang, content) = read_guarded(file_path, manifest_mtime)?;
    // UTF-8 decode: source is shared by the tree-sitter parse below and the
    // snippet extraction at match time (atomic, no second file access —
    // Decision 9, DECISIONS-NEEDED.md).
    let source = std::str::from_utf8(&content).ok()?;

    // Tree-sitter parse; non-tree-sitter languages fail here and return None.
    let Ok(mut parser) = Parser::new(lang) else {
        return None;
    };
    let Ok(tree) = parser.parse(source) else {
        return None;
    };

    // Walk the CST in pre-order; check each node's REAL parent chain
    // (AD-374-6 / AD-397-2 strict: real node.parent(), not pre-order predecessor).
    let walk_config = AstWalkConfig::default();
    let iter = AstWalkIter::new(tree.walk(), walk_config);

    for walk_node in iter {
        let node = walk_node.node;

        // Map tree-sitter kind string → global vocabulary NodeKindId.
        let Some(child_id) = vocab_lookup(node.kind()) else {
            continue;
        };
        let Some(parent_node) = node.parent() else {
            continue;
        };
        let Some(parent_id) = vocab_lookup(parent_node.kind()) else {
            // Parent kind not in vocab — cannot match any bigram/trigram.
            continue;
        };

        // Bigram check: (parent_id, child_id).
        if ancestor_table.bigrams.contains(&(parent_id, child_id)) {
            return Some(anchor_from_node(
                node.start_position().row,
                node.byte_range(),
                source,
            ));
        }

        // Trigram check (GP, P, C): parent.parent() is GP.
        // AD-374-6 / OD-374-3 (STRICT): require full grandparent→parent→child
        // ancestor chain via real node.parent(), not an approximation.
        // `trigram_children` fast-path: only walk to the grandparent when this
        // node's kind can actually be a trigram child — every trigram's child
        // is inserted into the set in `build`, so a miss here means no trigram
        // `(_, _, child_id)` exists and the gp lookup would be wasted work.
        if ancestor_table.trigram_children.contains(&child_id) {
            let Some(gp_node) = parent_node.parent() else {
                continue;
            };
            let Some(gp_id) = vocab_lookup(gp_node.kind()) else {
                continue;
            };
            if ancestor_table
                .trigrams
                .contains(&(gp_id, parent_id, child_id))
            {
                return Some(anchor_from_node(
                    node.start_position().row,
                    node.byte_range(),
                    source,
                ));
            }
        }
    }

    None
}

/// Extract `(line, byte_range, snippet)` from a matched CST node.
///
/// - `row` is 0-indexed (`node.start_position().row`); converted to 1-indexed.
/// - `byte_range` is the node's byte span.
/// - `snippet` is the full text of the matching line, extracted atomically
///   from `source` (the same buffer used for the tree-sitter parse).
fn anchor_from_node(
    row: usize,
    byte_range: Range<usize>,
    source: &str,
) -> (u32, Range<usize>, String) {
    // Widen usize → u32 safely; line numbers beyond u32::MAX saturate (unreachable in practice).
    let line = u32::try_from(row).unwrap_or(u32::MAX).saturating_add(1); // 1-indexed, never 0
    let snippet = source.lines().nth(row).unwrap_or("").to_string();
    (line, byte_range, snippet)
}

/// Recover the representative line for a matched **synthetic-marker** AST pattern.
///
/// For REAL-NODE patterns, use [`find_first_strict_match`] instead. This
/// function now handles SYNTHETIC-ONLY patterns (god-function, deep-nesting,
/// empty-function, empty-catch, excessive-params). For any real-node pattern
/// it returns `None` immediately.
///
/// ## AD-394-5: Synthetic-marker recovery (unchanged from #394)
///
/// The 5 synthetic-marker patterns can never match the pre-order strict walk —
/// `vocab_lookup` never yields a synthetic ID. These are routed
/// (`query_contains_synthetic_id`) to a branch that re-runs the index-time
/// extraction pipeline (`extract_ast_ngrams_with_lines`) and reads back the
/// representative `(line, byte)` position it recorded for the emitted marker.
/// One pass produces both the marker and its line (ADR-006). Per-pattern
/// representative-line rule: report the 1-indexed start line of the node the
/// marker's condition is measured on, resolving up to the enclosing named
/// construct for anonymous body/param blocks.
///
/// ## Return value
///
/// - `Some((line, byte_range))` — `line` is 1-indexed and ≥ 1; `byte_range` is
///   within the file's byte length.
/// - `None` — real-node pattern (use [`find_first_strict_match`]), or degraded
///   input (file too large, unreadable, non-tree-sitter language, pattern kinds
///   absent, or parse failed).
///
/// ## Determinism (AC-F3)
///
/// The MIN line per marker key is recorded at emission time, so identical
/// input always yields the same result.
pub fn recover_line(
    file_path: &Path,
    query: &AstQuery,
    manifest_mtime: Option<u64>,
) -> Option<(u32, Range<usize>)> {
    // AD-397: Real-node patterns use find_first_strict_match (gate + anchor in
    // one strict-ancestor pass, AD-397-1/AD-397-2). This function handles
    // SYNTHETIC patterns only; return None immediately for real-node patterns
    // so callers can route correctly without a full file read.
    if !query_contains_synthetic_id(query) {
        return None;
    }

    // I/O guards: metadata → mtime → size → language → read (single owner via
    // read_guarded; cannot drift across the three re-parse entry points).
    let (lang, content) = read_guarded(file_path, manifest_mtime)?;
    let source = std::str::from_utf8(&content).ok()?;

    // AD-394-5: Synthetic-marker patterns cannot be recovered by a strict
    // ancestor walk (`vocab_lookup` never yields a synthetic ID). Route them
    // through the SAME extraction pass that emits the marker
    // (`extract_ast_ngrams_with_lines`), reading back the representative
    // position it recorded (ADR-006: one pass produces both the marker and its
    // line — no second, drift-prone detection re-implementation).
    let result = linearize_source(source, lang).ok()?;
    let (_emitted, _metrics, synthetic_lines) = extract_ast_ngrams_with_lines(&result.nodes, lang);
    recover_synthetic_line(query, &synthetic_lines)
}

/// Shared I/O guard prologue for all re-parse entry points.
///
/// Consolidates the five-step precondition that every re-parse call site must
/// satisfy before touching tree-sitter — `fs::metadata` existence, mtime
/// staleness guard, `MAX_REPARSE_FILE_BYTES` size cap, `Language::from_path`
/// detection, and `fs::read` — into a single owner so the precondition cannot
/// drift across call sites (DRY / single-source-of-truth).
///
/// Returns `None` on any guard failure. Option-returning callers short-circuit
/// with `?`; the bool-returning synthetic branch in `pattern_occurs_in_file`
/// uses `let Some(...) else { return false }`. UTF-8 decoding is left to each
/// caller because it requires borrowing the returned `Vec<u8>`.
///
/// ## Guard order (cheapest-first, avoids unnecessary I/O)
///
/// 1. `fs::metadata` — fails fast for missing / unreadable paths.
/// 2. Mtime guard — no read needed for stale files.
/// 3. Size guard — no read needed for oversized files.
/// 4. `Language::from_path` — rejects non-tree-sitter languages before reading.
/// 5. `fs::read` — only after all guards pass.
fn read_guarded(path: &Path, manifest_mtime: Option<u64>) -> Option<(Language, Vec<u8>)> {
    let meta = std::fs::metadata(path).ok()?;

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

    if meta.len() > MAX_REPARSE_FILE_BYTES {
        return None;
    }

    let lang = Language::from_path(path)?;
    let content = std::fs::read(path).ok()?;
    Some((lang, content))
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

    let (line, byte) = keys
        .into_iter()
        .filter_map(|key| synthetic_lines.get(&key).copied())
        .min_by_key(|&(line, _)| line)?;

    Some((line, (byte as usize)..(byte as usize + 1)))
}

/// Verify that a source file's CST contains at least one node whose **real
/// ancestor chain** matches the pattern's resolved n-grams.
///
/// This is the structural verify gate (Part B) for the AND-intersect→verify→
/// truncate-LAST architecture (AD-374-2). For **real-node patterns** this now
/// delegates to [`find_first_strict_match`] (AD-397-5): the strict ancestor walk
/// is the single source of truth for both gate and anchor. For **synthetic-marker
/// patterns** the existing `synthetic_key_present` AND-semantics gate is preserved
/// unchanged (AD-394-1/AD-394-2).
///
/// ## AD-397-5: pattern_occurs_in_file as a thin wrapper (real-node)
///
/// `find_first_strict_match` applies identical guards (mtime, size, language,
/// UTF-8) and the strict `node.parent()` ancestor walk. Returning `true` iff
/// `find_first_strict_match` is `Some` means gate and anchor share ONE code path —
/// no second walk can drift (ADR-007 satisfied by construction).
///
/// ## AD-394-1 / AD-394-2: Synthetic-marker patterns route through extraction-reuse
///
/// The strict walk above is structurally unsatisfiable for the 5 synthetic-marker
/// patterns (god-function, deep-nesting, empty-function, empty-catch,
/// excessive-params): their resolved n-grams contain a synthetic ID
/// (`>= BUCKET_LABEL_BASE`), and `vocab_lookup` — which resolves every real
/// `node.parent()` kind in the walk — never yields a synthetic ID. Before
/// reaching `find_first_strict_match`, [`AncestorMatchTable::contains_synthetic_id`]
/// routes any such pattern to a SEPARATE branch that re-runs the index-time pipeline
/// via `linearize_source` + [`synthetic_key_present`] (an early-exit variant of
/// `extract_ast_ngrams_with_metrics` that returns as soon as the target synthetic
/// bigram is confirmed present — AC11 / #419 fix) and confirms every resolved
/// n-gram KEY is emitted (reuses the indexer's SAME emission logic as the single
/// source of truth — applies ADR-006). Real-node patterns are NOT routed through
/// extraction-reuse (AD-394-2) — collapsing the two branches would loosen the
/// real patterns to the indexer's gap-fill tolerance, regressing AD-374-6
/// precision.
///
/// ## AD-374-5: Non-tree-sitter / zero-kind files drop
///
/// Files whose language has no tree-sitter grammar (JSON/TOML/YAML), or patterns
/// that resolve to an empty match table, return `false` (never panic).  This
/// removes `Cargo.toml`/`.json` from structural results.
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

    if ancestor_table.contains_synthetic_id() {
        // AD-394-1 / AD-394-2: Synthetic-marker branch — unchanged from #394.
        // Guards run here (not inside find_first_strict_match) because this
        // branch does NOT delegate to find_first_strict_match.

        // I/O guards: metadata → mtime → size → language → read (single owner
        // via read_guarded; cannot drift across the three re-parse entry points).
        let Some((lang, content)) = read_guarded(file_path, manifest_mtime) else {
            return false;
        };
        let Ok(source) = std::str::from_utf8(&content) else {
            return false;
        };

        // AD-394-1 / #419 (AC11 fix): verify by re-running the index-time pipeline
        // via `linearize_source` + `synthetic_key_present`. The latter uses the SAME
        // ExtractState emission logic as `extract_ast_ngrams_with_metrics` (ADR-006)
        // but exits as soon as the target bigram key is confirmed present — for
        // deep-nesting (DEEP_NODE → bucket_label(0)) this fires at the first node at
        // depth >= 4, dramatically faster than a full O(n) traversal.
        //
        // AC8 invariant: mixed (synthetic + real) patterns are rejected at the catalog
        // level, so all bigrams in ancestor_table are synthetic when we reach this
        // branch. The trigrams set is empty for all 5 current synthetic patterns.
        //
        // `linearize_source` parses internally; no separate `Parser::new` is needed.
        // AD-374-5 size/language guards are inherited from the shared guards above;
        // linearize_source's own internal guards degrade to an empty result (never
        // fire redundantly here).
        //
        // AD-394-1 load-bearing invariant: all 5 current synthetic patterns are
        // single-bigram / zero-trigram (see plan §2 item 3 and Auto-Resolved OD-394-2).
        // `contains_synthetic_id` checks BOTH bigrams and trigrams, so a future
        // synthetic-trigram-only pattern would route here while the `bigrams.iter().all()`
        // check below would miss the trigrams (silently vacuously-true if bigrams is
        // empty). This debug_assert fires loudly in that case rather than silently
        // passing every candidate — assert-invariants-in-production (reliability rule).
        debug_assert!(
            ancestor_table.trigrams.is_empty() && !ancestor_table.bigrams.is_empty(),
            "synthetic verify branch expects zero trigrams and at least one bigram; \
             got {} bigrams, {} trigrams — a future synthetic-trigram pattern \
             needs explicit trigram verification here",
            ancestor_table.bigrams.len(),
            ancestor_table.trigrams.len()
        );
        // Production guard (reliability rule — assert-invariants-in-production):
        // `debug_assert!` above is elided in release builds, so explicitly gate
        // the vacuous-true hazard here. If bigrams is somehow empty (e.g. a
        // future synthetic-trigram-only pattern routed here via
        // `contains_synthetic_id`), `.all()` over an empty iterator returns
        // `true` and every pooled candidate would silently "pass" the gate.
        // Returning false is the safe failure mode — no verified match.
        if ancestor_table.bigrams.is_empty() {
            return false;
        }
        let Ok(result) = linearize_source(source, lang) else {
            return false;
        };
        // Check every synthetic bigram via early-exit. AC8 guarantees all bigrams are
        // synthetic here (no real IDs mixed in), so no filter is needed.
        return ancestor_table
            .bigrams
            .iter()
            .all(|&(p, c)| synthetic_key_present(&result.nodes, lang, AstBigram::encode(p, c)));
        // Trigrams: all current synthetic patterns are zero-trigram; ancestor_table.trigrams
        // is empty, so the bigram check above is the complete verification.
    }

    // AD-397-5: Real-node branch — delegate to find_first_strict_match.
    //
    // find_first_strict_match applies identical guards (mtime, size, language,
    // UTF-8, parse) and the strict node.parent() ancestor walk (AD-374-6).
    // Returning .is_some() makes this a thin wrapper with a single source of
    // truth for both gate and anchor — no second walk can drift (ADR-007).
    find_first_strict_match(file_path, query, manifest_mtime).is_some()
}

/// Precomputed lookup table for ancestor-correct CST matching (AD-374-6).
///
/// Used by both [`find_first_strict_match`] and [`pattern_occurs_in_file`]
/// (synthetic branch) and stores the complete ancestor relationship for strict
/// verification:
///
/// - `bigrams`: set of `(parent_id, child_id)` pairs — checked via real
///   `node.parent()`.
/// - `trigrams`: set of `(gp_id, parent_id, child_id)` triples — checked via
///   real `node.parent().parent()` (OD-374-3 resolved → STRICT).
/// - `trigram_children`: set of child-ids for any trigram — used as a fast
///   pre-check before evaluating the full triple.
pub(super) struct AncestorMatchTable {
    bigrams: HashSet<(NodeKindId, NodeKindId)>,
    trigrams: HashSet<(NodeKindId, NodeKindId, NodeKindId)>,
    /// Fast pre-check: set of child-ids that appear in any trigram.
    /// Avoids evaluating the full grandparent chain when `child_id` is not in
    /// any trigram at all.
    trigram_children: HashSet<NodeKindId>,
}

impl AncestorMatchTable {
    /// Resolve `query` into the strict ancestor lookup table.
    pub(super) fn build(query: &AstQuery) -> Self {
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
    pub(super) fn is_empty(&self) -> bool {
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
    pub(super) fn contains_synthetic_id(&self) -> bool {
        self.bigrams
            .iter()
            .any(|&(p, c)| is_synthetic_id(p) || is_synthetic_id(c))
            || self
                .trigrams
                .iter()
                .any(|&(gp, p, c)| is_synthetic_id(gp) || is_synthetic_id(p) || is_synthetic_id(c))
    }
}

/// AD-394-1 / AD-394-5: shared routing predicate used by BOTH the verify gate
/// (`pattern_occurs_in_file`) and `recover_line` to decide whether `query`
/// routes through extraction-reuse — one routing rule, no divergence between
/// the two call sites. Delegates to [`AncestorMatchTable::contains_synthetic_id`]
/// so there is a single implementation of "does this query touch a synthetic
/// marker ID" (built from [`is_synthetic_id`]).
pub(super) fn query_contains_synthetic_id(query: &AstQuery) -> bool {
    AncestorMatchTable::build(query).contains_synthetic_id()
}

// ============================================================================
// Tests (co-located in reparse_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "reparse_tests.rs"]
mod tests;
