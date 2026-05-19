//! Tree-sitter-based field classifier for Markdown documents.
//!
//! Uses the `rskim_core` parser (tree-sitter-md grammar) to produce
//! byte-range classifications for headings, code blocks, prose, and link
//! reference definitions.
//!
//! # Field mapping
//!
//! | Node kind | Field |
//! |-----------|-------|
//! | `atx_heading` (H1–H3) | [`SearchField::TypeDefinition`] |
//! | `atx_heading` (H4+) | [`SearchField::Other`] |
//! | `setext_heading` (always H1 or H2) | [`SearchField::TypeDefinition`] |
//! | `fenced_code_block` / `indented_code_block` / `code_fence_content` | [`SearchField::FunctionBody`] |
//! | `paragraph` / `list` / `list_item` / `block_quote` | [`SearchField::Comment`] |
//! | `link_reference_definition` | [`SearchField::ImportExport`] |
//! | Everything else | [`SearchField::Other`] |
//!
//! # Error fallback
//!
//! If the tree-sitter parser fails to initialise (e.g., grammar not compiled),
//! the entire source is returned as a single `Other` range. This mirrors the
//! behaviour of [`crate::lexical::classifier`] for unsupported languages.

use std::ops::Range;

use rskim_core::Language;

use crate::SearchField;
use crate::lexical::classifier::build_field_ranges;

/// Classify byte ranges in a Markdown source string using tree-sitter.
///
/// Returns a sorted, non-overlapping, contiguous `Vec` of
/// `(Range<usize>, SearchField)` tuples covering every byte `0..source.len()`.
///
/// # Errors
///
/// Returns [`crate::SearchError`] only if the size guard in
/// [`crate::lexical::classifier::classify_source`] fires. The Markdown parser
/// itself is fault-tolerant — syntax errors produce error nodes rather than
/// parse failures.
pub(crate) fn classify_markdown(source: &str) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }

    let len = source.len();

    if len > crate::lexical::classifier::MAX_SOURCE_BYTES {
        return Err(crate::SearchError::FileTooLarge {
            size: len,
            limit: crate::lexical::classifier::MAX_SOURCE_BYTES,
        });
    }

    // Attempt to parse with the Markdown grammar. If the parser cannot be
    // initialised (grammar missing), return a single Other range.
    let mut parser = match rskim_core::Parser::new(Language::Markdown) {
        Ok(p) => p,
        Err(_) => {
            return Ok(vec![(0..len, SearchField::Other)]);
        }
    };

    let tree = match parser.parse(source) {
        Ok(t) => t,
        Err(_) => {
            return Ok(vec![(0..len, SearchField::Other)]);
        }
    };

    let root = tree.root_node();
    let bytes = source.as_bytes();

    // Collect non-Other node ranges in pre-order (same pattern as classify_source).
    let mut node_ranges: Vec<(Range<usize>, SearchField)> = Vec::new();
    let mut cursor = root.walk();

    loop {
        let node = cursor.node();
        let byte_range = node.byte_range();

        let start = byte_range.start.min(len);
        let end = byte_range.end.min(len);

        if start < end {
            let kind = node.kind();
            let field = map_markdown_kind(kind, bytes, start, len);
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
                return Ok(build_field_ranges(node_ranges, len));
            }
        }
    }
}

/// Map a Markdown tree-sitter node kind to a [`SearchField`].
///
/// For `atx_heading`, counts the leading `#` characters to determine heading
/// level: 1–3 → TypeDefinition, 4+ → Other.
///
/// `bytes` is the full source as bytes; `start` is the node's start byte offset.
fn map_markdown_kind(kind: &str, bytes: &[u8], start: usize, source_len: usize) -> SearchField {
    match kind {
        "atx_heading" => {
            // Count leading `#` chars to determine heading level.
            let level = count_atx_heading_level(bytes, start, source_len);
            if (1..=3).contains(&level) {
                SearchField::TypeDefinition
            } else {
                SearchField::Other
            }
        }
        "setext_heading" => {
            // Setext headings are always H1 (underline `===`) or H2 (underline `---`).
            SearchField::TypeDefinition
        }
        "fenced_code_block" | "indented_code_block" | "code_fence_content" => {
            SearchField::FunctionBody
        }
        "paragraph" | "list" | "list_item" | "block_quote" => SearchField::Comment,
        "link_reference_definition" => SearchField::ImportExport,
        // document, section, html_block, thematic_break, and everything else → Other (skip).
        _ => SearchField::Other,
    }
}

/// Count the number of leading `#` characters in an ATX heading node.
///
/// Skips the heading's start in `bytes` and counts consecutive `#` bytes.
fn count_atx_heading_level(bytes: &[u8], start: usize, len: usize) -> usize {
    let mut count = 0;
    let mut i = start;
    while i < len && bytes[i] == b'#' {
        count += 1;
        i += 1;
    }
    count
}
