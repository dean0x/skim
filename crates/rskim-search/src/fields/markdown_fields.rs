//! Field classification for Markdown files.
//!
//! Uses line-by-line regex matching (headings, code fences, links)
//! rather than tree-sitter or serde parsing.

use std::ops::Range;

use crate::SearchField;

/// Classify regions in Markdown content into `SearchField` spans.
pub fn classify_markdown_fields(
    _source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    // Phase 1d: implement Markdown field classification
    Ok(vec![])
}
