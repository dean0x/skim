//! Tree-sitter-based field classifier for Markdown documents.
//!
//! Reads a tree of the tree-sitter-md grammar (parsed by the caller with
//! `rskim_core`) to produce byte-range classifications for code blocks and
//! prose.
//!
//! # Field mapping
//!
//! | Node kind | Field |
//! |-----------|-------|
//! | `fenced_code_block` / `indented_code_block` / `code_fence_content` | [`SearchField::Code`] |
//! | `paragraph` / `list` / `list_item` / `block_quote` | [`SearchField::Comment`] |
//! | Everything else | [`SearchField::Other`] |
//!
//! A code block inside a list item or a block quote takes the code field and
//! not the prose field around it.
use std::ops::Range;

use tree_sitter::Tree;

use super::super::classifier::build_field_ranges;
use super::super::field::SearchField;

/// Classify byte ranges in a Markdown source string from its tree.
///
/// `tree` must be the Markdown parse of `source`. Returns a sorted,
/// non-overlapping, contiguous `Vec` of `(Range<usize>, SearchField)` tuples
/// covering every byte `0..source.len()`, or an empty vector for empty source.
/// The Markdown parser is fault-tolerant: syntax errors produce error nodes, so
/// every source has a tree.
pub(crate) fn classify_markdown(tree: &Tree, source: &str) -> Vec<(Range<usize>, SearchField)> {
    if source.is_empty() {
        return Vec::new();
    }

    let len = source.len();

    let root = tree.root_node();

    // Collect non-Other node ranges in pre-order (same pattern as classify_tree).
    let mut node_ranges: Vec<(Range<usize>, SearchField)> = Vec::new();
    let mut cursor = root.walk();

    loop {
        let node = cursor.node();
        let byte_range = node.byte_range();

        let start = byte_range.start.min(len);
        let end = byte_range.end.min(len);

        if start < end {
            let field = map_markdown_kind(node.kind());
            if field != SearchField::Other {
                node_ranges.push((start..end, field));
            }
        }

        // Advance in pre-order.
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return build_field_ranges(node_ranges, len);
            }
        }
    }
}

/// Map a Markdown tree-sitter node kind to a [`SearchField`].
fn map_markdown_kind(kind: &str) -> SearchField {
    match kind {
        "fenced_code_block" | "indented_code_block" | "code_fence_content" => SearchField::Code,
        "paragraph" | "list" | "list_item" | "block_quote" => SearchField::Comment,
        // document, section, headings, html_block, thematic_break, link
        // reference definitions and everything else → Other (skip).
        _ => SearchField::Other,
    }
}
