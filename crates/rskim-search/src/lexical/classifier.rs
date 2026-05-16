//! AST-aware field classifier that maps source byte ranges to [`SearchField`] variants.
//!
//! # Algorithm
//!
//! 1. Parse `source` via [`rskim_core::Parser`] for the given language.
//! 2. Walk the tree in pre-order, mapping each node kind to a [`SearchField`] via
//!    [`rskim_core::node_kind_priority`] and [`map_priority_to_field`].
//! 3. Fill a per-byte `field_at` array: children overwrite parents (innermost wins).
//! 4. Run-length encode the result into a sorted, non-overlapping, contiguous range
//!    list.
//!
//! # Non-tree-sitter languages
//!
//! For languages where [`rskim_core::Language::to_tree_sitter`] returns `None`
//! (JSON, YAML, TOML), the entire source is classified as
//! [`SearchField::Other`] — a single range `0..source.len()`.
//!
//! # Invariants
//!
//! The returned `Vec` satisfies:
//! - Sorted ascending by `range.start`.
//! - Non-overlapping (no two ranges share a byte).
//! - Contiguous (covers every byte from 0 to `source.len()`).
//! - `sum(range.end - range.start) == source.len()`.
//! - For empty source, an empty `Vec` is returned.

use std::ops::Range;

use rskim_core::Language;

use crate::SearchField;

/// Map a node_kind_priority value (1–5) to a [`SearchField`] for indexing.
///
/// Priority 5 (type definitions) → TypeDefinition
/// Priority 4 (function declarations) → FunctionSignature
/// Priority 3 (imports/exports) → ImportExport
/// Priority 2 (class/module containers) → FunctionBody (treated as body-level)
/// Priority 1 (everything else) → Other
///
/// Comments and string literals get their own fields via dedicated node kinds
/// handled in [`classify_node_kind`].
fn map_priority_to_field(kind: &str, priority: u8) -> SearchField {
    // First check for specific comment/string kinds regardless of priority.
    match kind {
        "comment" | "line_comment" | "block_comment" | "doc_comment" => {
            return SearchField::Comment;
        }
        "string_literal"
        | "string"
        | "interpreted_string_literal"
        | "raw_string_literal"
        | "string_content"
        | "raw_str_literal"
        | "template_string"
        | "template_literal"
        | "quoted_string" => {
            return SearchField::StringLiteral;
        }
        // Identifier / name nodes → SymbolName
        "identifier"
        | "type_identifier"
        | "field_identifier"
        | "property_identifier"
        | "variable_name"
        | "attribute_name" => {
            return SearchField::SymbolName;
        }
        _ => {}
    }

    match priority {
        5 => SearchField::TypeDefinition,
        4 => SearchField::FunctionSignature,
        3 => SearchField::ImportExport,
        _ => SearchField::Other,
    }
}

/// Classify all bytes in `source` according to their AST-derived field.
///
/// Returns a sorted, non-overlapping, contiguous list of `(Range<usize>, SearchField)`
/// tuples covering every byte from `0` to `source.len()`.
///
/// For empty source, returns an empty vector.
/// For languages without tree-sitter support, returns a single `Other` range.
///
/// # Errors
///
/// Returns [`crate::SearchError`] if the tree-sitter parser fails to initialise
/// (grammar loading failure, which should not happen in practice for supported
/// languages).
/// Maximum source size (in bytes) accepted by [`classify_source`].
///
/// The classifier allocates a per-byte `Vec<SearchField>`, so accepting
/// unbounded input would allow a caller to trigger proportional memory
/// allocation. 100 MiB is generous for any real source file while keeping
/// peak RSS bounded.
pub const MAX_SOURCE_BYTES: usize = 100 * 1024 * 1024; // 100 MiB

pub fn classify_source(
    source: &str,
    lang: Language,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }

    let len = source.len();

    if len > MAX_SOURCE_BYTES {
        return Err(crate::SearchError::FileTooLarge {
            size: len,
            limit: MAX_SOURCE_BYTES,
        });
    }

    // For non-tree-sitter languages (JSON, YAML, TOML), classify all bytes as Other.
    let mut parser = match rskim_core::Parser::new(lang) {
        Ok(p) => p,
        Err(_) => {
            // Language does not use tree-sitter — return single Other range.
            return Ok(vec![(0..len, SearchField::Other)]);
        }
    };

    let tree = parser.parse(source)?;
    let root = tree.root_node();

    // Allocate per-byte field array, initialised to Other.
    let mut field_at: Vec<SearchField> = vec![SearchField::Other; len];

    // Pre-order walk: children are processed after parents, so they overwrite
    // (innermost wins).
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let byte_range = node.byte_range();

        // Clamp range to source bounds (safety against malformed AST).
        let start = byte_range.start.min(len);
        let end = byte_range.end.min(len);

        if start < end {
            let kind = node.kind();
            let priority = rskim_core::node_kind_priority(kind);
            let field = map_priority_to_field(kind, priority);

            // Only overwrite if this field is more specific than Other.
            // Other is the default; we only stamp non-Other fields so that
            // an unrecognised parent doesn't clobber a specific child.
            if field != SearchField::Other {
                for byte in &mut field_at[start..end] {
                    *byte = field;
                }
            }
        }

        // Advance cursor in pre-order.
        if cursor.goto_first_child() {
            continue;
        }
        // No children — try next sibling.
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            // No sibling — go up.
            if !cursor.goto_parent() {
                // Reached root — traversal complete.
                return Ok(run_length_encode(field_at, len));
            }
        }
    }
}

/// Run-length encode a per-byte field array into a sorted range list.
///
/// Adjacent bytes with the same field are merged into one `Range<usize>`.
/// The output is sorted, non-overlapping, and covers `[0..len)`.
fn run_length_encode(field_at: Vec<SearchField>, len: usize) -> Vec<(Range<usize>, SearchField)> {
    if len == 0 {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut start = 0usize;
    let mut current = field_at[0];

    for (i, &f) in field_at.iter().enumerate().skip(1) {
        if f != current {
            result.push((start..i, current));
            start = i;
            current = f;
        }
    }
    // Push the final segment.
    result.push((start..len, current));
    result
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod tests;
