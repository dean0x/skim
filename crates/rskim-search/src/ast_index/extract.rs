//! AST sparse n-gram extraction from linearized CST node sequences.
//!
//! This module converts a `Vec<LinearNode>` (pre-order depth-encoded CST)
//! into weighted, deduplicated `AstBigram`/`AstTrigram` sets. The extraction
//! uses structural parent→child relationships (depth-indexed ancestor table)
//! rather than sequential adjacency.
//!
//! # Design
//!
//! - Depth-jump gap-fill: a jump `> +1` in pre-order depth means a node was
//!   likely dropped (ERROR/MISSING in the original CST). The ancestor slots
//!   for the skipped depths are nulled to approximately break the parent–child
//!   chain. This is a depth-jump heuristic: it cannot detect a dropped ERROR
//!   node that had a same-depth preceding sibling (no gap is left), so one
//!   class of spurious edges remains — see the documented residual edge case.
//! - Sentinel `kind_id == 0` nodes are recorded in the ancestor table (to
//!   maintain correct depth positions) but never emitted in any n-gram key.
//! - Output carries `(ngram, weight, count)` — `count` is the term frequency
//!   (how many times the edge was emitted in the file).
//! - All n-grams are emitted (lossless): weight `1.0` when not in the
//!   selective IDF table.
//! - Both output vecs are sorted by key ascending and contain unique keys.

use std::collections::HashMap;

use rskim_core::Language;

use super::{AstBigram, AstTrigram, NodeKindId, ast_bigram_idf, ast_trigram_idf};
use crate::ast_index::linearize::LinearNode;

// ============================================================================
// Public types
// ============================================================================

/// One extracted structural bigram: the key, its IDF weight, and its term
/// frequency (emitted occurrences in the file).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AstBigramEntry {
    /// The packed parent→child node-kind pair.
    pub ngram: AstBigram,
    /// IDF weight from the per-language weight table, or `DEFAULT_AST_WEIGHT`
    /// when the n-gram is not in the table.
    pub weight: f32,
    /// Number of times this n-gram was emitted in the file (term frequency).
    /// Bounded by the total node count (≤ `DEFAULT_MAX_NODES` = 100K).
    pub count: u32,
}

/// One extracted structural trigram: the key, its IDF weight, and its term
/// frequency (emitted occurrences in the file).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AstTrigramEntry {
    /// The packed grandparent→parent→child node-kind triple.
    pub ngram: AstTrigram,
    /// IDF weight from the per-language weight table, or `DEFAULT_AST_WEIGHT`
    /// when the n-gram is not in the table.
    pub weight: f32,
    /// Number of times this n-gram was emitted in the file (term frequency).
    /// Bounded by the total node count (≤ `DEFAULT_MAX_NODES` = 100K).
    pub count: u32,
}

/// Deduplicated structural n-grams extracted from a linearized CST.
///
/// Both vecs are sorted by key ascending, contain unique keys, and carry
/// per-file term frequency in `count`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AstNgramSet {
    /// Deduplicated bigrams (parent→child pairs), sorted by key ascending.
    pub bigrams: Vec<AstBigramEntry>,
    /// Deduplicated trigrams (grandparent→parent→child triples), sorted by
    /// key ascending.
    pub trigrams: Vec<AstTrigramEntry>,
}

// ============================================================================
// Public API
// ============================================================================

/// Extract structural n-grams using injected weight lookups.
///
/// This is the testable core: both weight functions are caller-supplied,
/// making the function pure and easy to unit-test with synthetic weights.
///
/// # Algorithm
///
/// 1. Early-return empty set for empty input.
/// 2. Single pass to find `max_depth`, then allocate an ancestor table of
///    size `max_depth + 1` (one allocation, no per-iteration growth).
/// 3. For each node in pre-order:
///    - **Gap-fill**: if depth jumped by more than one from the previous
///      node, null the skipped ancestor slots (dropped ERROR/MISSING nodes
///      approximately broke the chain).
///    - Read `parent = ancestors[depth - 1]` and `gp = ancestors[depth - 2]`.
///    - **Emit bigram** when `parent` is `Some(p)` AND `p != 0` AND
///      `node.kind_id != 0` (sentinel suppression on both sides).
///    - **Emit trigram** when both `gp` and `parent` are `Some` AND all
///      three kind IDs are `!= 0`.
///    - Record `ancestors[depth] = Some(node.kind_id)`.
/// 4. Convert accumulation maps → sorted entry vecs, return.
///
/// # Allocation
///
/// Allocates O(`max_depth`) for the ancestor table and O(`nodes.len()`) for
/// the accumulation maps. Callers are responsible for bounding inputs.
/// Production callers route through `linearize_source`, which caps depth at
/// 500 and node count at 100K (`AstWalkConfig::DEFAULT_MAX_DEPTH/NODES`).
/// When calling this function directly with synthetic nodes, ensure `depth`
/// values and slice length stay within acceptable bounds.
///
/// # Parameters
///
/// - `nodes` — pre-order depth-encoded CST, not mutated.
/// - `bigram_weight` — pure weight function for a given bigram key.
/// - `trigram_weight` — pure weight function for a given trigram key.
#[must_use]
pub fn extract_ast_ngrams_with_weights(
    nodes: &[LinearNode],
    bigram_weight: impl Fn(AstBigram) -> f32,
    trigram_weight: impl Fn(AstTrigram) -> f32,
) -> AstNgramSet {
    if nodes.is_empty() {
        return AstNgramSet::default();
    }

    // Single bounded pass to find the maximum depth. This determines the
    // minimum ancestor table size — one allocation, no per-iteration growth.
    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    debug_assert!(
        usize::from(max_depth) < 65536,
        "max_depth {max_depth} overflows ancestor table index"
    );

    // Ancestor table: `ancestors[d]` = `Some(kind_id)` of the node at depth d,
    // or `None` if that slot was nulled by gap-fill or never filled.
    // Sized for depths 0..=max_depth.
    let mut ancestors: Vec<Option<NodeKindId>> = vec![None; usize::from(max_depth) + 1];

    // Accumulation maps: key → (weight, count).
    // Weight is a pure function of the key so it's constant per unique key;
    // count is the term frequency (number of emitted occurrences).
    // Cap initial capacity at 1024: unique n-grams are typically an order of
    // magnitude smaller than the total node count (most edges repeat across
    // a file), so pre-sizing to nodes.len() wastes memory.
    let cap = nodes.len().min(1024);
    let mut bigram_map: HashMap<AstBigram, (f32, u32)> = HashMap::with_capacity(cap);
    let mut trigram_map: HashMap<AstTrigram, (f32, u32)> = HashMap::with_capacity(cap);

    let mut prev_depth: Option<u16> = None;

    for node in nodes {
        let d = node.depth as usize;

        // ── Gap-fill ──────────────────────────────────────────────────────
        // A jump of more than +1 in pre-order depth means nodes were dropped
        // (ERROR/MISSING in the original CST). Null the skipped ancestor slots
        // to approximately break the parent–child chain.
        // Widen to u32 before adding 1 to avoid u16 overflow when p == u16::MAX.
        if let Some(p) = prev_depth
            && u32::from(node.depth) > u32::from(p) + 1
        {
            let fill_start = usize::from(p) + 1;
            debug_assert!(fill_start < d, "gap-fill range [{fill_start}..{d}) must be non-empty");
            for slot in &mut ancestors[fill_start..d] {
                *slot = None;
            }
        }

        // ── Resolve parent and grandparent from the ancestor table ────────
        debug_assert!(d < ancestors.len(), "depth index {d} out of ancestor table (len={})", ancestors.len());

        let parent: Option<NodeKindId> = node
            .depth
            .checked_sub(1)
            .and_then(|pd| ancestors.get(usize::from(pd)).copied().flatten());

        let grandparent: Option<NodeKindId> = node
            .depth
            .checked_sub(2)
            .and_then(|gd| ancestors.get(usize::from(gd)).copied().flatten());

        // ── Emit bigram ───────────────────────────────────────────────────
        // Suppress sentinel kind_id == 0 on both sides.
        if let Some(p) = parent
            && p != 0
            && node.kind_id != 0
        {
            let key = AstBigram::encode(p, node.kind_id);
            let w = bigram_weight(key);
            let entry = bigram_map.entry(key).or_insert((w, 0));
            entry.1 += 1;
        }

        // ── Emit trigram ──────────────────────────────────────────────────
        // Suppress when any of the three kind IDs is 0 or an ancestor is None.
        // No explicit cap: input is already bounded upstream by DEFAULT_MAX_NODES
        // (100K), so the total edge count is naturally bounded without a separate
        // per-file trigram limit.
        if let (Some(gp), Some(p)) = (grandparent, parent)
            && gp != 0
            && p != 0
            && node.kind_id != 0
        {
            let key = AstTrigram::encode(gp, p, node.kind_id);
            let w = trigram_weight(key);
            let entry = trigram_map.entry(key).or_insert((w, 0));
            entry.1 += 1;
        }

        // ── Record this node in the ancestor table ────────────────────────
        // Sentinel (kind_id == 0) nodes ARE recorded so that depth positions
        // remain correct for deeper descendants. The sentinel check happens at
        // emit time above, not here.
        ancestors[d] = Some(node.kind_id);
        prev_depth = Some(node.depth);
    }

    // ── Collect and sort ──────────────────────────────────────────────────

    let mut bigrams: Vec<AstBigramEntry> = bigram_map
        .into_iter()
        .map(|(ngram, (weight, count))| AstBigramEntry {
            ngram,
            weight,
            count,
        })
        .collect();
    bigrams.sort_unstable_by_key(|e| e.ngram.key());

    let mut trigrams: Vec<AstTrigramEntry> = trigram_map
        .into_iter()
        .map(|(ngram, (weight, count))| AstTrigramEntry {
            ngram,
            weight,
            count,
        })
        .collect();
    trigrams.sort_unstable_by_key(|e| e.ngram.key());

    AstNgramSet { bigrams, trigrams }
}

/// Extract structural n-grams using the production per-language IDF tables.
///
/// Convenience wrapper over [`extract_ast_ngrams_with_weights`]. Falls back to
/// [`crate::ast_index::DEFAULT_AST_WEIGHT`] for n-grams not in the table and
/// for non-tree-sitter languages (JSON, YAML, TOML).
#[must_use]
pub fn extract_ast_ngrams(nodes: &[LinearNode], lang: Language) -> AstNgramSet {
    extract_ast_ngrams_with_weights(
        nodes,
        |b| ast_bigram_idf(lang, b),
        |t| ast_trigram_idf(lang, t),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "extract_tests.rs"]
mod tests;
