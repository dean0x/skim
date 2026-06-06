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
    if nodes.is_empty() {
        return (AstNgramSet::default(), StructuralMetrics::default());
    }

    let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
    let mut state = ExtractState::new(max_depth, nodes.len());
    let mut prev_depth: Option<u16> = None;

    for node in nodes {
        let d = usize::from(node.depth);

        state.update_max_depth(node.depth);
        state.emit_depth_buckets(node.depth);

        // ── Gap-fill (PF-004 safe: widen to u32) ────────────────────────────
        // When a depth jump > +1 occurs, null ancestor slots AND child_counts
        // for the skipped depths so that subtree-close logic is not triggered
        // for nonexistent parents.
        if let Some(p) = prev_depth
            && u32::from(node.depth) > u32::from(p) + 1
        {
            state.fill_depth_gap(p, d);
        }

        // ── Increment parent's counted-child count ────────────────────────────
        // The "parent" of the current node is at depth d-1. We increment that
        // slot's counted-child count if the current node is a counted child.
        state.update_child_count(d, node.kind_id);

        // ── Close subtrees of the previous node ──────────────────────────────
        // In pre-order DFS a node's subtree is fully exhausted when the next
        // node is at the same depth OR shallower. Close depths [node.depth..=prev]
        // in reverse (deepest first) so parent metrics include correctly-computed
        // child metrics before the slot is overwritten by the incoming sibling.
        if let Some(p) = prev_depth {
            state.close_open_subtrees(node.depth, p);
        }

        state.update_branch_count(node.kind_id);
        state.emit_real_ngrams(node, lang);
        state.record_node(d, node.kind_id);

        prev_depth = Some(node.depth);
    }

    // ── Close any still-open depths at the end of the stream ─────────────────
    // After processing all nodes, depths that were never "closed" (because no
    // later node had a smaller depth) must be closed now. close_open_subtrees(0, p)
    // closes [0..=p] in reverse, which matches the original end-of-stream loop.
    if let Some(p) = prev_depth {
        state.close_open_subtrees(0, p);
    }

    state.into_ngram_set()
}

// ============================================================================
// ExtractState — mutable traversal state for extract_ast_ngrams_with_metrics
// ============================================================================

/// Mutable state threaded through the single-pass extraction loop.
///
/// Grouping the three parallel depth-indexed arrays (`ancestors`, `child_counts`,
/// `depth_kind`) together with the accumulation maps and metrics makes the
/// per-node operations expressible as focused methods, each touching only
/// the fields it needs. The struct is local to this module and carries no heap
/// allocations beyond the initial setup.
///
/// # Invariants (maintained by the methods below)
///
/// - `ancestors`, `child_counts`, and `depth_kind` are all allocated to length
///   `max_depth + 1` at construction and never resized.
/// - `child_counts[d]` is meaningful only when `ancestors[d].is_some()`.
/// - `depth_kind[d]` intentionally diverges from `ancestors[d]` at gap-fill
///   and subtree-close boundaries: `ancestors[d]` becomes `None` when the slot
///   is invalidated, but `depth_kind[d]` retains the last known kind so that
///   `close_depth` can read the enclosing parent at subtree-close time.
struct ExtractState {
    ancestors: Vec<Option<NodeKindId>>,
    child_counts: Vec<u32>,
    /// Mirrors `ancestors` kind values but is NOT nulled by gap-fill, so
    /// `close_depth` can read the enclosing-parent kind after a sibling
    /// overwrites the `ancestors` slot to `None`.
    depth_kind: Vec<NodeKindId>,
    bigram_map: HashMap<AstBigram, (f32, u32)>,
    trigram_map: HashMap<AstTrigram, (f32, u32)>,
    metrics: StructuralMetrics,
}

impl ExtractState {
    fn new(max_depth: u16, node_count: usize) -> Self {
        let table_len = usize::from(max_depth) + 1;
        let cap = node_count.min(1024);
        Self {
            ancestors: vec![None; table_len],
            child_counts: vec![0u32; table_len],
            depth_kind: vec![0u16; table_len],
            bigram_map: HashMap::with_capacity(cap),
            trigram_map: HashMap::with_capacity(cap),
            metrics: StructuralMetrics::default(),
        }
    }

    /// Update `metrics.max_depth` when `depth` exceeds the current maximum.
    #[inline]
    fn update_max_depth(&mut self, depth: u16) {
        self.metrics.max_depth = self.metrics.max_depth.max(depth);
    }

    /// Emit cumulative `DEEP_NODE → bucket_label(i)` synthetics for every
    /// depth bucket edge crossed by `depth`.
    #[inline]
    fn emit_depth_buckets(&mut self, depth: u16) {
        emit_bucket_crossings(
            &mut self.bigram_map,
            DEEP_NODE,
            &DEPTH_EDGES,
            u32::from(depth),
        );
    }

    /// Null ancestor slots and reset child counts for the depth range
    /// `(prev_depth..node_depth)` — the depths that were skipped over by
    /// a jump of more than +1.
    ///
    /// # PF-004
    ///
    /// The caller must widen `prev_depth` to u32 before the `> prev + 1`
    /// guard to avoid u16 overflow at `p == u16::MAX`. This function itself
    /// only uses `usize` indices and is safe regardless.
    #[inline]
    fn fill_depth_gap(&mut self, prev_depth: u16, node_d: usize) {
        let fill_start = usize::from(prev_depth) + 1;
        debug_assert!(
            fill_start < node_d,
            "gap-fill range [{fill_start}..{node_d}) must be non-empty"
        );
        for slot in &mut self.ancestors[fill_start..node_d] {
            *slot = None;
        }
        for cc in &mut self.child_counts[fill_start..node_d] {
            *cc = 0;
        }
    }

    /// Increment the counted-child count of the parent (depth `d - 1`) when
    /// the current node at depth `d` is a counted child.
    ///
    /// Sentinel nodes (`kind_id == 0`) are NOT counted (they are not real
    /// constructs), but they ARE still recorded in the ancestor table by
    /// `record_node` to preserve correct depth positions.
    #[inline]
    fn update_child_count(&mut self, d: usize, kind_id: NodeKindId) {
        if d > 0 {
            let parent_d = d - 1;
            if self.ancestors[parent_d].is_some() && is_counted_child(kind_id) {
                self.child_counts[parent_d] = self.child_counts[parent_d].saturating_add(1);
            }
        }
    }

    /// Close all depth slots in `[node_depth..=prev_depth]` (deepest first).
    ///
    /// In pre-order DFS a node's subtree is fully exhausted when the next node
    /// is at the same depth OR shallower. Closing in reverse order (deepest
    /// first) ensures that parent structural metrics (e.g. `max_block_stmts`)
    /// are computed from fully-accumulated child counts before the slot is
    /// overwritten by the incoming sibling or uncle.
    #[inline]
    fn close_open_subtrees(&mut self, node_depth: u16, prev_depth: u16) {
        let close_start = usize::from(node_depth);
        let close_end = usize::from(prev_depth);
        if close_end >= close_start {
            for depth_to_close in (close_start..=close_end).rev() {
                if self.ancestors[depth_to_close].is_some() {
                    close_depth(
                        depth_to_close,
                        &self.child_counts,
                        &self.depth_kind,
                        &mut self.metrics,
                        &mut self.bigram_map,
                    );
                }
            }
        }
    }

    /// Increment `metrics.branch_count` (saturating) when `kind_id` is a
    /// branch construct (if, match, switch, etc.).
    #[inline]
    fn update_branch_count(&mut self, kind_id: NodeKindId) {
        if BRANCH_KIND_IDS.contains(&kind_id) {
            self.metrics.branch_count = self.metrics.branch_count.saturating_add(1);
        }
    }

    /// Emit real bigram and trigram n-grams for `node` using the production
    /// per-language IDF tables. Sentinel `kind_id == 0` is suppressed on both
    /// sides of every n-gram.
    #[inline]
    fn emit_real_ngrams(&mut self, node: &LinearNode, lang: Language) {
        let d = usize::from(node.depth);
        let table_len = self.ancestors.len();
        debug_assert!(
            d < table_len,
            "depth {d} out of ancestor table (len={table_len})"
        );

        let parent: Option<NodeKindId> = node
            .depth
            .checked_sub(1)
            .and_then(|pd| self.ancestors.get(usize::from(pd)).copied().flatten());

        let grandparent: Option<NodeKindId> = node
            .depth
            .checked_sub(2)
            .and_then(|gd| self.ancestors.get(usize::from(gd)).copied().flatten());

        if let Some(p) = parent
            && p != 0
            && node.kind_id != 0
        {
            let key = AstBigram::encode(p, node.kind_id);
            let entry = self
                .bigram_map
                .entry(key)
                .or_insert_with(|| (ast_bigram_idf(lang, key), 0));
            entry.1 = entry.1.saturating_add(1);
        }

        if let (Some(gp), Some(p)) = (grandparent, parent)
            && gp != 0
            && p != 0
            && node.kind_id != 0
        {
            let key = AstTrigram::encode(gp, p, node.kind_id);
            let entry = self
                .trigram_map
                .entry(key)
                .or_insert_with(|| (ast_trigram_idf(lang, key), 0));
            entry.1 = entry.1.saturating_add(1);
        }
    }

    /// Record `node.kind_id` in the ancestor table at `depth d` and reset
    /// `child_counts[d]` to zero (this node's children have not been seen yet).
    ///
    /// Sentinel nodes (`kind_id == 0`) ARE recorded here to preserve correct
    /// depth positions for their descendants. Suppression happens at emit time.
    #[inline]
    fn record_node(&mut self, d: usize, kind_id: NodeKindId) {
        self.ancestors[d] = Some(kind_id);
        self.depth_kind[d] = kind_id;
        self.child_counts[d] = 0;
    }

    /// Consume the state and return the sorted `(AstNgramSet, StructuralMetrics)`.
    fn into_ngram_set(self) -> (AstNgramSet, StructuralMetrics) {
        let mut bigrams: Vec<AstBigramEntry> = self
            .bigram_map
            .into_iter()
            .map(|(ngram, (weight, count))| AstBigramEntry {
                ngram,
                weight,
                count,
            })
            .collect();
        bigrams.sort_unstable_by_key(|e| e.ngram.key());

        let mut trigrams: Vec<AstTrigramEntry> = self
            .trigram_map
            .into_iter()
            .map(|(ngram, (weight, count))| AstTrigramEntry {
                ngram,
                weight,
                count,
            })
            .collect();
        trigrams.sort_unstable_by_key(|e| e.ngram.key());

        (AstNgramSet { bigrams, trigrams }, self.metrics)
    }
}

// ============================================================================
// Shared synthetic-emission helpers
// ============================================================================

/// Emit a synthetic bigram `(parent → child)` with `DEFAULT_AST_WEIGHT` and
/// `count = 1` into `bigram_map`, incrementing the count when the key is
/// already present (saturating).
#[inline]
fn emit_synthetic(
    bigram_map: &mut HashMap<AstBigram, (f32, u32)>,
    parent: NodeKindId,
    child: NodeKindId,
) {
    use crate::ast_index::DEFAULT_AST_WEIGHT;
    let key = AstBigram::encode(parent, child);
    let entry = bigram_map.entry(key).or_insert((DEFAULT_AST_WEIGHT, 0));
    entry.1 = entry.1.saturating_add(1);
}

/// Emit cumulative bucket crossings: for each `(i, edge)` in `edges` where
/// `value >= edge`, emit `parent → bucket_label(i)` into `bigram_map`.
///
/// Used by depth-bucket, large-body, and many-params emission — all three share
/// the same cumulative threshold pattern.
#[inline]
fn emit_bucket_crossings(
    bigram_map: &mut HashMap<AstBigram, (f32, u32)>,
    parent: NodeKindId,
    edges: &[u32],
    value: u32,
) {
    for (i, &edge) in edges.iter().enumerate() {
        if value >= edge {
            emit_synthetic(bigram_map, parent, bucket_label(i));
        }
    }
}

// ============================================================================
// close_depth — emit synthetic markers when a depth slot's subtree closes
// ============================================================================

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
    let kind_id = depth_kind[depth_idx];
    if kind_id == 0 {
        return; // sentinel — not a real construct, skip
    }

    // Compute enclosing_id once: the parent of this slot (depth_idx - 1).
    // Both BODY and PARAM_LIST branches need it; computing it here removes
    // the duplicated `depth_idx > 0` guard and `depth_kind[depth_idx - 1]`
    // lookup that previously appeared in two separate inner blocks.
    let enclosing_id: Option<NodeKindId> = depth_idx
        .checked_sub(1)
        .map(|pd| depth_kind[pd])
        .filter(|&id| id != 0);

    let count = child_counts[depth_idx];

    if BODY_KIND_IDS.contains(&kind_id) {
        emit_empty_or_large_body(count, enclosing_id, metrics, bigram_map);
    }

    // ── MANY_PARAMS emission ──────────────────────────────────────────────────
    if PARAM_LIST_KIND_IDS.contains(&kind_id) {
        // PF-004: saturating cast — count can be up to DEFAULT_MAX_NODES (100K) > u16::MAX
        let count_u16 = count.min(u32::from(u16::MAX)) as u16;
        if count_u16 > metrics.max_params {
            metrics.max_params = count_u16;
        }
        emit_bucket_crossings(bigram_map, MANY_PARAMS, &PARAM_EDGES, count);
    }
}

/// Emit `EMPTY_BODY` and `LARGE_BODY` markers for a body/block node whose
/// subtree has just closed.
///
/// - `EMPTY_BODY → enclosing_id` fires when `count == 0` and `enclosing_id`
///   is `Some` (i.e. there is a named enclosing construct).
/// - `LARGE_BODY → bucket_label(i)` fires cumulatively when the enclosing
///   construct is a function/method kind and `count` crosses a bucket edge.
///   Updates `metrics.max_block_stmts` (PF-004 saturating cast).
fn emit_empty_or_large_body(
    count: u32,
    enclosing_id: Option<NodeKindId>,
    metrics: &mut StructuralMetrics,
    bigram_map: &mut HashMap<AstBigram, (f32, u32)>,
) {
    if count == 0
        && let Some(enc) = enclosing_id
    {
        emit_synthetic(bigram_map, EMPTY_BODY, enc);
    }

    if let Some(enc) = enclosing_id
        && FUNCTION_KIND_IDS.contains(&enc)
    {
        // PF-004: saturating cast — count can be up to DEFAULT_MAX_NODES (100K) > u16::MAX
        let count_u16 = count.min(u32::from(u16::MAX)) as u16;
        if count_u16 > metrics.max_block_stmts {
            metrics.max_block_stmts = count_u16;
        }
        emit_bucket_crossings(bigram_map, LARGE_BODY, &BODY_STMT_EDGES, count);
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
