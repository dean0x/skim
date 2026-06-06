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
use crate::ast_index::structural::{
    BODY_KIND_IDS, BODY_STMT_EDGES, BRANCH_KIND_IDS, DEEP_NODE, DEPTH_EDGES, EMPTY_BODY,
    FUNCTION_KIND_IDS, LARGE_BODY, MANY_PARAMS, PARAM_EDGES, PARAM_LIST_KIND_IDS,
    StructuralMetrics, bucket_label, is_counted_child,
};

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
        let d = usize::from(node.depth);

        // ── Gap-fill ──────────────────────────────────────────────────────
        // A jump of more than +1 in pre-order depth means nodes were dropped
        // (ERROR/MISSING in the original CST). Null the skipped ancestor slots
        // to approximately break the parent–child chain.
        // Widen to u32 before adding 1 to avoid u16 overflow when p == u16::MAX.
        if let Some(p) = prev_depth
            && u32::from(node.depth) > u32::from(p) + 1
        {
            let fill_start = usize::from(p) + 1;
            debug_assert!(
                fill_start < d,
                "gap-fill range [{fill_start}..{d}) must be non-empty"
            );
            for slot in &mut ancestors[fill_start..d] {
                *slot = None;
            }
        }

        // ── Resolve parent and grandparent from the ancestor table ────────
        let table_len = ancestors.len();
        debug_assert!(
            d < table_len,
            "depth {d} out of ancestor table (len={table_len})"
        );

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
            let entry = bigram_map
                .entry(key)
                .or_insert_with(|| (bigram_weight(key), 0));
            entry.1 = entry.1.saturating_add(1);
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
            let entry = trigram_map
                .entry(key)
                .or_insert_with(|| (trigram_weight(key), 0));
            entry.1 = entry.1.saturating_add(1);
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

/// Extract structural n-grams AND per-file structural metrics in a single pass.
///
/// This function extends [`extract_ast_ngrams_with_weights`] by folding the
/// structural computation (body-statement counting, parameter counting, depth
/// tracking, branch counting) and synthetic n-gram emission into the SAME
/// traversal loop — no second pass, no additional allocations beyond the
/// ancestor tracking already performed.
///
/// # Synthetic markers emitted
///
/// - `EMPTY_BODY → enclosing_kind` — body/block with zero counted children,
///   keyed on the enclosing construct (parent of the body kind).
/// - `DEEP_NODE → bucket_label(i)` — cumulative, for each depth edge crossed.
/// - `LARGE_BODY → bucket_label(i)` — cumulative, for function/method bodies
///   only, for each body-statement-count edge crossed.
/// - `MANY_PARAMS → bucket_label(i)` — cumulative, for each param-count edge
///   crossed.
///
/// All synthetic n-grams use `DEFAULT_AST_WEIGHT` and `count = 1`.
/// Synthetic parent IDs (>= 65000) are guaranteed to not be in the vocabulary,
/// so no real containment bigram can collide.
///
/// # PF-004 compliance
///
/// Saturating casts are used for all u16/u32 accumulations:
/// - `max_block_stmts` and `max_params` use `min(u16::MAX)` before cast.
/// - `branch_count` saturates at `u32::MAX`.
///
/// # Returns
///
/// A pair `(AstNgramSet, StructuralMetrics)` where:
/// - `AstNgramSet` contains all real n-grams plus synthetic marker bigrams.
/// - `StructuralMetrics` contains per-file complexity metrics.
#[must_use]
pub fn extract_ast_ngrams_with_metrics(
    nodes: &[LinearNode],
    lang: Language,
) -> (AstNgramSet, StructuralMetrics) {
    use crate::ast_index::DEFAULT_AST_WEIGHT;

    if nodes.is_empty() {
        return (AstNgramSet::default(), StructuralMetrics::default());
    }

    // ── Metrics state ─────────────────────────────────────────────────────────
    let mut metrics = StructuralMetrics::default();

    // ── Ancestor table (same as extract_ast_ngrams_with_weights) ─────────────
    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    let mut ancestors: Vec<Option<NodeKindId>> = vec![None; usize::from(max_depth) + 1];

    // ── Accumulation maps for real n-grams ────────────────────────────────────
    let cap = nodes.len().min(1024);
    let mut bigram_map: HashMap<AstBigram, (f32, u32)> = HashMap::with_capacity(cap);
    let mut trigram_map: HashMap<AstTrigram, (f32, u32)> = HashMap::with_capacity(cap);

    // ── Per-ancestor structural tracking ─────────────────────────────────────
    //
    // For each depth slot in the ancestor table we track:
    //   - How many "counted children" the node at that depth has seen so far.
    //   - The kind_id of the node at that depth (mirrors `ancestors[d]` but
    //     kept separately so we can access it at subtree-close time even after
    //     the slot may have been overwritten by a sibling at the same depth).
    //
    // Invariant: `child_counts[d]` is valid only when `ancestors[d].is_some()`.
    // When gap-fill nulls `ancestors[d]`, we also reset `child_counts[d]`.
    let table_len = usize::from(max_depth) + 1;
    let mut child_counts: Vec<u32> = vec![0u32; table_len];
    // Track the kind_id stored at each depth slot so we can access it during
    // the "previous node's subtree close" detection below.
    let mut depth_kind: Vec<NodeKindId> = vec![0u16; table_len];

    let mut prev_depth: Option<u16> = None;

    // Helper: emit a synthetic bigram with count=1, DEFAULT_AST_WEIGHT
    // into `bigram_map`.
    let emit_synthetic = |bm: &mut HashMap<AstBigram, (f32, u32)>,
                          parent: NodeKindId,
                          child: NodeKindId| {
        let key = AstBigram::encode(parent, child);
        let entry = bm.entry(key).or_insert((DEFAULT_AST_WEIGHT, 0));
        entry.1 = entry.1.saturating_add(1);
    };

    for node in nodes {
        let d = usize::from(node.depth);

        // ── Update max_depth ──────────────────────────────────────────────────
        if node.depth > metrics.max_depth {
            metrics.max_depth = node.depth;
        }

        // ── Depth bucket emission ─────────────────────────────────────────────
        // For EVERY node, check all depth edges. Cumulative: emit all crossed edges.
        for (i, &edge) in DEPTH_EDGES.iter().enumerate() {
            if u32::from(node.depth) >= edge {
                emit_synthetic(&mut bigram_map, DEEP_NODE, bucket_label(i));
            }
        }

        // ── Gap-fill (PF-004 safe: widen to u32) ────────────────────────────
        // When a depth jump > +1 occurs, null ancestor slots AND child_counts
        // for the skipped depths so that subtree-close logic is not triggered
        // for nonexistent parents.
        if let Some(p) = prev_depth
            && u32::from(node.depth) > u32::from(p) + 1
        {
            let fill_start = usize::from(p) + 1;
            debug_assert!(
                fill_start < d,
                "gap-fill range [{fill_start}..{d}) must be non-empty"
            );
            for slot in &mut ancestors[fill_start..d] {
                *slot = None;
            }
            for cc in &mut child_counts[fill_start..d] {
                *cc = 0;
            }
        }

        // ── Detect subtree close: update child_counts for the parent ──────────
        // The "parent" of the current node is at depth d-1. We increment that
        // slot's counted-child count if the current node is a counted child.
        if d > 0 {
            let parent_d = d - 1;
            if ancestors[parent_d].is_some() && is_counted_child(node.kind_id) {
                child_counts[parent_d] = child_counts[parent_d].saturating_add(1);
            }
        }

        // ── Detect if PREVIOUS node's subtree is closing ──────────────────────
        // When depth decreases (or stays equal — a sibling), the node at
        // `prev_depth` has had all its children visited. We can now emit
        // structural markers for that node.
        //
        // Specifically: when `node.depth <= prev_depth`, the node at
        // `prev_depth` will no longer accumulate children (all children at
        // depth `prev_depth + 1` have been visited). We close out all depths
        // from `prev_depth` down to `node.depth + 1` (exclusive).
        //
        // We close depths in reverse (deepest first) so that parent metrics
        // include the correctly-computed child metrics.
        if let Some(p) = prev_depth {
            // Close all depths from prev_depth down to node.depth (inclusive).
            //
            // When depth decreases (e.g. prev=3 → cur=2), we must close:
            // - depth 3: the last leaf that had no more children,
            // - depth 2: the ancestor that was occupying depth 2 BEFORE the
            //   current node replaces it (e.g. `parameters` before `->`).
            //
            // In pre-order DFS a node's subtree is fully exhausted when the
            // next node is at the same depth OR shallower. Closing depths
            // [close_end..=close_start] (deepest first) ensures that each
            // structural container (parameters, block, etc.) is closed with
            // its final `child_counts` value intact before the slot is
            // overwritten by the incoming sibling or uncle.
            let close_start = usize::from(node.depth);
            let close_end = usize::from(p);
            if close_end >= close_start {
                for depth_to_close in (close_start..=close_end).rev() {
                    if ancestors[depth_to_close].is_some() {
                        close_depth(
                            depth_to_close,
                            &child_counts,
                            &depth_kind,
                            &mut metrics,
                            &mut bigram_map,
                        );
                    }
                }
            }
        }

        // ── Branch count ──────────────────────────────────────────────────────
        if BRANCH_KIND_IDS.contains(&node.kind_id) {
            metrics.branch_count = metrics.branch_count.saturating_add(1);
        }

        // ── Resolve parent and grandparent (real n-gram emission) ─────────────
        let table_len = ancestors.len();
        debug_assert!(d < table_len, "depth {d} out of ancestor table (len={table_len})");

        let parent: Option<NodeKindId> = node
            .depth
            .checked_sub(1)
            .and_then(|pd| ancestors.get(usize::from(pd)).copied().flatten());

        let grandparent: Option<NodeKindId> = node
            .depth
            .checked_sub(2)
            .and_then(|gd| ancestors.get(usize::from(gd)).copied().flatten());

        // Emit real bigram
        if let Some(p) = parent
            && p != 0
            && node.kind_id != 0
        {
            let key = AstBigram::encode(p, node.kind_id);
            let entry = bigram_map
                .entry(key)
                .or_insert_with(|| (ast_bigram_idf(lang, key), 0));
            entry.1 = entry.1.saturating_add(1);
        }

        // Emit real trigram
        if let (Some(gp), Some(p)) = (grandparent, parent)
            && gp != 0
            && p != 0
            && node.kind_id != 0
        {
            let key = AstTrigram::encode(gp, p, node.kind_id);
            let entry = trigram_map
                .entry(key)
                .or_insert_with(|| (ast_trigram_idf(lang, key), 0));
            entry.1 = entry.1.saturating_add(1);
        }

        // ── Record node in ancestor table + reset child_count for this depth ──
        ancestors[d] = Some(node.kind_id);
        depth_kind[d] = node.kind_id;
        // Reset child_count for this depth (this is a new node at depth d,
        // its children haven't started yet).
        child_counts[d] = 0;
        prev_depth = Some(node.depth);
    }

    // ── Close any still-open depths at the end of the stream ─────────────────
    // After processing all nodes, depths that were never "closed" (because no
    // later node had a smaller depth) must be closed now.
    if let Some(p) = prev_depth {
        for d in (0..=usize::from(p)).rev() {
            if ancestors[d].is_some() {
                close_depth(
                    d,
                    &child_counts,
                    &depth_kind,
                    &mut metrics,
                    &mut bigram_map,
                );
            }
        }
    }

    // ── Collect and sort ──────────────────────────────────────────────────────
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

    (AstNgramSet { bigrams, trigrams }, metrics)
}

/// Close a depth slot: emit synthetic markers for the node at `depth_idx`
/// based on its accumulated `child_counts` and kind.
///
/// Called when a node at `depth_idx` is known to have had all its children
/// visited (either because a subsequent node at a shallower or equal depth
/// was encountered, or because we're at end-of-stream).
fn close_depth(
    depth_idx: usize,
    child_counts: &[u32],
    depth_kind: &[NodeKindId],
    metrics: &mut StructuralMetrics,
    bigram_map: &mut HashMap<AstBigram, (f32, u32)>,
) {
    use crate::ast_index::DEFAULT_AST_WEIGHT;

    let kind_id = depth_kind[depth_idx];
    if kind_id == 0 {
        return; // sentinel — not a real construct, skip
    }

    let count = child_counts[depth_idx];

    // ── EMPTY_BODY emission ───────────────────────────────────────────────────
    // If this node is a body/block kind AND has zero counted children,
    // emit EMPTY_BODY → enclosing_kind (the parent of this body kind).
    if BODY_KIND_IDS.contains(&kind_id) {
        if count == 0 {
            // Enclosing kind = parent of this body = ancestor at depth_idx - 1
            if depth_idx > 0 {
                let enclosing_id = depth_kind[depth_idx - 1];
                if enclosing_id != 0 {
                    let key = AstBigram::encode(EMPTY_BODY, enclosing_id);
                    let entry = bigram_map.entry(key).or_insert((DEFAULT_AST_WEIGHT, 0));
                    entry.1 = entry.1.saturating_add(1);
                }
            }
        }

        // ── LARGE_BODY emission (function/method bodies only) ─────────────────
        // Check if the enclosing node is a function/method kind.
        if depth_idx > 0 {
            let enclosing_id = depth_kind[depth_idx - 1];
            if FUNCTION_KIND_IDS.contains(&enclosing_id) {
                // PF-004: saturating cast — count can be up to DEFAULT_MAX_NODES (100K) > u16::MAX
                let count_u16 = count.min(u32::from(u16::MAX)) as u16;
                if count_u16 > metrics.max_block_stmts {
                    metrics.max_block_stmts = count_u16;
                }
                // Cumulative bucket emission
                for (i, &edge) in BODY_STMT_EDGES.iter().enumerate() {
                    if count >= edge {
                        let key = AstBigram::encode(LARGE_BODY, bucket_label(i));
                        let entry = bigram_map.entry(key).or_insert((DEFAULT_AST_WEIGHT, 0));
                        entry.1 = entry.1.saturating_add(1);
                    }
                }
            }
        }
    }

    // ── MANY_PARAMS emission ──────────────────────────────────────────────────
    if PARAM_LIST_KIND_IDS.contains(&kind_id) {
        // PF-004: saturating cast
        let count_u16 = count.min(u32::from(u16::MAX)) as u16;
        if count_u16 > metrics.max_params {
            metrics.max_params = count_u16;
        }
        for (i, &edge) in PARAM_EDGES.iter().enumerate() {
            if count >= edge {
                let key = AstBigram::encode(MANY_PARAMS, bucket_label(i));
                let entry = bigram_map.entry(key).or_insert((DEFAULT_AST_WEIGHT, 0));
                entry.1 = entry.1.saturating_add(1);
            }
        }
    }

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
