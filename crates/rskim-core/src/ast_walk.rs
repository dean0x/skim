//! Shared iterative pre-order DFS traversal for tree-sitter parse trees.
//!
//! # Design
//!
//! `AstWalkIter` provides a reusable, bounds-guarded iterator over every node
//! in a tree-sitter `Tree`. It encapsulates the `TreeCursor`-based DFS loop
//! that was previously duplicated in `rskim-search/linearize.rs` and
//! `rskim-research/ast_extract.rs`.
//!
//! # Traversal order
//!
//! Nodes are yielded in pre-order: a parent is yielded before any of its
//! children. The root node is always the first item yielded (at depth 0).
//!
//! # Bounds guards
//!
//! - `max_depth`: Once the cursor reaches this depth, the subtree at that node
//!   is skipped. Sibling subtrees at shallower depths continue to be visited.
//! - `max_nodes`: Once the total yield count reaches this limit, the subtree at
//!   the current node is skipped. Sibling subtrees continue.
//!
//! When both limits are 0, the iterator yields nothing.
//!
//! # Error and MISSING nodes
//!
//! `AstWalkNode::is_error` is `true` when `node.is_error() || node.is_missing()`.
//! Children of error nodes are still traversed; callers decide whether to use
//! or skip error items.
//!
//! # Invariant
//!
//! After the iterator is exhausted:
//! `node_count() == (non-error yields) + error_count()`
//!
//! # Example
//!
//! ```no_run
//! use rskim_core::{AstWalkConfig, AstWalkIter, Parser, Language};
//!
//! let mut parser = Parser::new(Language::Rust).unwrap();
//! let tree = parser.parse("fn main() {}").unwrap();
//! let config = AstWalkConfig::default();
//! let mut iter = AstWalkIter::new(tree.walk(), config);
//!
//! while let Some(item) = iter.next() {
//!     println!("depth={} kind={} error={}", item.depth, item.node.kind(), item.is_error);
//! }
//!
//! println!("total={} errors={}", iter.node_count(), iter.error_count());
//! ```

/// Configuration for `AstWalkIter` bounds guards.
///
/// Both limits guard against pathological inputs (deeply nested code, generated
/// files with hundreds of thousands of nodes). Matching the defaults used in
/// `rskim-search` and `rskim-research` (500 / 100 000).
#[derive(Debug, Clone, Copy)]
pub struct AstWalkConfig {
    /// Maximum traversal depth. Nodes at this depth or deeper have their
    /// subtrees skipped. A value of 0 causes the iterator to yield nothing.
    pub max_depth: u32,
    /// Maximum total nodes yielded. Once reached, remaining subtrees are
    /// skipped. A value of 0 causes the iterator to yield nothing.
    pub max_nodes: u32,
}

impl AstWalkConfig {
    /// Default maximum traversal depth (500).
    ///
    /// Canonical source used by `AstWalkConfig::default()`, `linearize.rs`, and
    /// `ast_extract.rs`. Update here to change the limit everywhere.
    pub const DEFAULT_MAX_DEPTH: u32 = 500;

    /// Default maximum nodes yielded per traversal (100 000).
    ///
    /// Canonical source used by `AstWalkConfig::default()`, `linearize.rs`, and
    /// `ast_extract.rs`. Update here to change the limit everywhere.
    pub const DEFAULT_MAX_NODES: u32 = 100_000;
}

impl Default for AstWalkConfig {
    fn default() -> Self {
        Self {
            max_depth: Self::DEFAULT_MAX_DEPTH,
            max_nodes: Self::DEFAULT_MAX_NODES,
        }
    }
}

/// A single node yielded by `AstWalkIter`.
///
/// The node borrows from the `Tree` that was passed to `AstWalkIter::new`.
/// The `depth` is the 0-indexed pre-order traversal depth (root = 0).
pub struct AstWalkNode<'a> {
    /// The tree-sitter node at this position in the traversal.
    pub node: tree_sitter::Node<'a>,
    /// 0-indexed depth from the root. Root node is depth 0.
    pub depth: u32,
    /// `true` when `node.is_error() || node.is_missing()`.
    pub is_error: bool,
}

/// Iterative pre-order DFS iterator over a tree-sitter `Tree`.
///
/// Created via `AstWalkIter::new`. Implements `Iterator<Item = AstWalkNode<'_>>`.
///
/// After the iterator is exhausted, call `node_count()` and `error_count()` to
/// retrieve traversal statistics.
pub struct AstWalkIter<'a> {
    cursor: tree_sitter::TreeCursor<'a>,
    /// Stack of depths at each descent level, used to restore depth on ascent.
    level_stack: Vec<u32>,
    depth: u32,
    node_count: u32,
    error_count: u32,
    config: AstWalkConfig,
    /// Set to true when there are no more nodes to visit.
    done: bool,
    /// Set to false after the first call to `next()`.
    first: bool,
}

impl<'a> AstWalkIter<'a> {
    /// Create a new iterator.
    ///
    /// `cursor` must be positioned at the root of the tree (i.e., obtained
    /// directly from `tree.walk()`).
    #[must_use]
    pub fn new(cursor: tree_sitter::TreeCursor<'a>, config: AstWalkConfig) -> Self {
        Self {
            cursor,
            level_stack: Vec::with_capacity((config.max_depth as usize).min(64)),
            depth: 0,
            node_count: 0,
            error_count: 0,
            config,
            done: false,
            first: true,
        }
    }

    /// Total nodes yielded so far (or after exhaustion: total nodes visited).
    ///
    /// Satisfies the invariant: `node_count() == non_error_yields + error_count()`.
    #[must_use]
    pub fn node_count(&self) -> u32 {
        self.node_count
    }

    /// Number of ERROR or MISSING nodes encountered.
    #[must_use]
    pub fn error_count(&self) -> u32 {
        self.error_count
    }

    /// Attempt to skip the current subtree due to a bounds guard being hit.
    ///
    /// Moves the cursor to the next sibling or ascends until a sibling is found.
    /// Returns `true` if a sibling was found (traversal continues), `false` if
    /// the traversal is exhausted.
    fn skip_subtree(&mut self) -> bool {
        loop {
            if self.cursor.goto_next_sibling() {
                // Depth is unchanged — we moved to a sibling, not a child.
                return true;
            }
            match self.level_stack.pop() {
                Some(parent_depth) => {
                    self.cursor.goto_parent();
                    self.depth = parent_depth;
                }
                None => {
                    self.done = true;
                    return false;
                }
            }
        }
    }

    /// Advance past the current node to the next one in pre-order.
    ///
    /// Tries to descend into the first child. If there are no children, moves to
    /// the next sibling or ascends. Returns `false` when the traversal is done.
    fn advance(&mut self) -> bool {
        if self.cursor.goto_first_child() {
            self.level_stack.push(self.depth);
            self.depth = self.depth.saturating_add(1);
            return true;
        }
        // No children — move to sibling or ascend.
        loop {
            if self.cursor.goto_next_sibling() {
                return true;
            }
            match self.level_stack.pop() {
                Some(parent_depth) => {
                    self.cursor.goto_parent();
                    self.depth = parent_depth;
                }
                None => {
                    self.done = true;
                    return false;
                }
            }
        }
    }
}

impl<'a> Iterator for AstWalkIter<'a> {
    type Item = AstWalkNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        // On the very first call, the cursor is already at the root — do NOT
        // advance before yielding the root. On subsequent calls we advance first.
        if self.first {
            self.first = false;
        } else if !self.advance() {
            return None;
        }

        // Inner loop: skip subtrees that hit bounds, then yield.
        loop {
            // ── Bounds guards ─────────────────────────────────────────────────
            if self.depth >= self.config.max_depth || self.node_count >= self.config.max_nodes {
                if !self.skip_subtree() {
                    return None;
                }
                continue; // Re-check bounds at the new position.
            }

            // ── Yield current node ────────────────────────────────────────────
            let node = self.cursor.node();
            let is_error = node.is_error() || node.is_missing();

            self.node_count = self.node_count.saturating_add(1);
            if is_error {
                self.error_count = self.error_count.saturating_add(1);
            }

            return Some(AstWalkNode {
                node,
                depth: self.depth,
                is_error,
            });
        }
    }
}

/// `AstWalkIter` is fused: once `next()` returns `None` it always returns `None`.
///
/// The `done` flag is set to `true` on exhaustion and is never cleared, so the
/// stdlib optimization (`take_while`, `chain`, etc.) is safe to rely on.
impl<'a> std::iter::FusedIterator for AstWalkIter<'a> {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Parse Rust source and return a tree. Panics on failure (acceptable in tests).
    fn parse_rust(source: &str) -> tree_sitter::Tree {
        let mut ts_parser = tree_sitter::Parser::new();
        ts_parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("Rust grammar should load");
        ts_parser.parse(source, None).expect("parse should succeed")
    }

    // ── AC-F1: Pre-order traversal order ──────────────────────────────────────

    #[test]
    fn pre_order_traversal_order() {
        // The root node (source_file) must be the first item yielded.
        let tree = parse_rust("fn hello() {}");
        let config = AstWalkConfig::default();
        let mut iter = AstWalkIter::new(tree.walk(), config);

        let first = iter.next().expect("should yield at least one node");
        assert_eq!(first.node.kind(), "source_file");
        assert_eq!(first.depth, 0);
    }

    // ── AC-F2: Depth increments correctly for nested structures ───────────────

    #[test]
    fn depth_correct_for_nested_source() {
        let tree = parse_rust("fn hello() {}");
        let config = AstWalkConfig::default();
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

        // Root is depth 0. At least one child must be at depth 1.
        assert!(
            items.iter().any(|n| n.depth == 1),
            "expected at least one node at depth 1"
        );
        // Depth must be strictly monotone at the level boundary.
        for window in items.windows(2) {
            let d0 = window[0].depth;
            let d1 = window[1].depth;
            // Pre-order: next depth can be parent+1, same, or any ancestor.
            // It can never exceed parent depth by more than 1.
            assert!(
                d1 <= d0 + 1,
                "depth jumped from {d0} to {d1} — invalid pre-order transition"
            );
        }
    }

    // ── AC-F3: Error nodes are flagged ────────────────────────────────────────

    #[test]
    fn error_nodes_flagged() {
        // Deliberately broken input.
        let tree = parse_rust("fn broken(((( {}");
        let config = AstWalkConfig::default();
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

        let has_error = items.iter().any(|n| n.is_error);
        assert!(has_error, "broken syntax should produce is_error nodes");
    }

    // ── AC-F4: MISSING nodes are flagged ─────────────────────────────────────

    #[test]
    fn missing_nodes_flagged() {
        // Broken syntax that causes tree-sitter-rust to insert MISSING nodes.
        // `fn;` is missing a name and body, which forces the parser to insert
        // MISSING nodes to complete the grammar.
        let tree = parse_rust("fn;");
        let config = AstWalkConfig::default();
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

        // At minimum we expect some error or missing markers from the malformed input.
        // tree-sitter is error-tolerant — it either inserts MISSING nodes or an
        // ERROR node. Either counts as `is_error=true` in our iterator.
        let has_error_or_missing = items.iter().any(|n| n.is_error);
        assert!(
            has_error_or_missing,
            "malformed input 'fn;' should produce at least one error/missing node"
        );
    }

    // ── AC-F5: Children of ERROR nodes are still yielded ─────────────────────

    #[test]
    fn error_children_still_yielded() {
        // `fn broken(((( {}` — the ERROR node wraps children; we prove the walker
        // descended *into* ERROR subtrees by finding non-error nodes deeper than
        // the shallowest error node.
        let tree = parse_rust("fn broken(((( {}");
        let config = AstWalkConfig::default();
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

        // 1. At least one ERROR/MISSING node must appear.
        let error_nodes: Vec<_> = items.iter().filter(|n| n.is_error).collect();
        assert!(
            !error_nodes.is_empty(),
            "broken syntax should produce is_error nodes"
        );

        // 2. The shallowest error depth.
        let min_error_depth = error_nodes.iter().map(|n| n.depth).min().unwrap();

        // 3. At least one non-error node must exist at a depth strictly greater
        //    than the shallowest error depth, proving the walker descended into
        //    the ERROR subtree rather than skipping it.
        let has_deeper_non_error = items
            .iter()
            .any(|n| !n.is_error && n.depth > min_error_depth);
        assert!(
            has_deeper_non_error,
            "expected non-error nodes at depth > {} (inside ERROR subtree), \
             but only found items at depths: {:?}",
            min_error_depth,
            items.iter().map(|n| n.depth).collect::<Vec<_>>()
        );
    }

    // ── AC-F6: max_depth guard ────────────────────────────────────────────────

    #[test]
    fn max_depth_guard() {
        let tree = parse_rust("fn hello() {}");
        let config = AstWalkConfig {
            max_depth: 3,
            max_nodes: 100_000,
        };
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

        // No item should have depth >= max_depth.
        for item in &items {
            assert!(item.depth < 3, "depth {} >= max_depth 3", item.depth);
        }
    }

    // ── AC-F7: max_nodes guard ────────────────────────────────────────────────

    #[test]
    fn max_nodes_guard() {
        let tree = parse_rust("fn hello() { let x = 1; let y = 2; }");
        let config = AstWalkConfig {
            max_depth: 500,
            max_nodes: 5,
        };
        let mut iter = AstWalkIter::new(tree.walk(), config);
        let items: Vec<_> = iter.by_ref().collect();

        // node_count() must never exceed max_nodes.
        assert!(
            iter.node_count() <= 5,
            "node_count {} exceeded max_nodes 5",
            iter.node_count()
        );
        assert!(
            items.len() <= 5,
            "yielded {} items but max_nodes was 5",
            items.len()
        );
    }

    // ── AC-F8: Bounds skip subtree but not traversal ──────────────────────────

    #[test]
    fn bounds_skip_subtree_not_traversal() {
        // A two-function file: the first function creates deep nesting,
        // the second function must still be visited even if max_depth is hit in the first.
        // We use a small max_depth to trigger the skip on nested nodes in the first fn,
        // then verify that the root and some nodes from later in the tree appear.
        let tree = parse_rust("fn a() { let x = 1; } fn b() {}");
        let config = AstWalkConfig {
            max_depth: 2, // forces some nodes in fn bodies to be skipped
            max_nodes: 100_000,
        };
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

        // Both function_item nodes should appear at depth 1 (children of source_file).
        let fn_items: Vec<_> = items
            .iter()
            .filter(|n| n.node.kind() == "function_item")
            .collect();
        assert!(
            fn_items.len() >= 2,
            "both functions should be yielded; got {} function_item nodes",
            fn_items.len()
        );
    }

    // ── AC-F9: Empty source yields root ──────────────────────────────────────

    #[test]
    fn empty_source_yields_root() {
        let tree = parse_rust("");
        let config = AstWalkConfig::default();
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();

        // tree-sitter always produces a root `source_file` node, even for empty input.
        assert_eq!(items.len(), 1, "empty source should yield exactly the root");
        assert_eq!(items[0].node.kind(), "source_file");
        assert_eq!(items[0].depth, 0);
    }

    // ── AC-F10: Exhausted iterator returns None ───────────────────────────────

    #[test]
    fn exhausted_returns_none() {
        let tree = parse_rust("fn hello() {}");
        let config = AstWalkConfig::default();
        let mut iter = AstWalkIter::new(tree.walk(), config);

        // Exhaust the iterator.
        for _ in iter.by_ref() {}

        // Further calls must return None.
        assert!(iter.next().is_none());
        assert!(iter.next().is_none());
    }

    // ── AC-A5: node_count() matches total items yielded ──────────────────────

    #[test]
    fn node_count_matches_yields() {
        let tree = parse_rust("fn hello() { let x = 1; }");
        let config = AstWalkConfig::default();
        let mut iter = AstWalkIter::new(tree.walk(), config);
        let mut manual_count: u32 = 0;

        for _item in iter.by_ref() {
            manual_count += 1;
        }

        assert_eq!(
            iter.node_count(),
            manual_count,
            "node_count() must equal number of items yielded"
        );
    }

    // ── Invariant: node_count == non_error + error_count ─────────────────────

    #[test]
    fn node_count_invariant_holds() {
        let tree = parse_rust("fn broken(((( {}");
        let config = AstWalkConfig::default();
        let mut iter = AstWalkIter::new(tree.walk(), config);
        let mut non_error: u32 = 0;

        for item in iter.by_ref() {
            if !item.is_error {
                non_error += 1;
            }
        }

        assert_eq!(
            iter.node_count(),
            non_error + iter.error_count(),
            "invariant: node_count == non_error + error_count"
        );
    }

    // ── Zero limits yield nothing ─────────────────────────────────────────────

    #[test]
    fn zero_max_depth_yields_nothing() {
        let tree = parse_rust("fn hello() {}");
        let config = AstWalkConfig {
            max_depth: 0,
            max_nodes: 100_000,
        };
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();
        assert!(items.is_empty(), "max_depth=0 should yield nothing");
    }

    #[test]
    fn zero_max_nodes_yields_nothing() {
        let tree = parse_rust("fn hello() {}");
        let config = AstWalkConfig {
            max_depth: 500,
            max_nodes: 0,
        };
        let items: Vec<_> = AstWalkIter::new(tree.walk(), config).collect();
        assert!(items.is_empty(), "max_nodes=0 should yield nothing");
    }
}
