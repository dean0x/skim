//! AST-aware field classifier that maps source byte ranges to [`SearchField`] variants.
//!
//! # Algorithm
//!
//! 1. Parse `source` via [`rskim_core::Parser`] for the given language.
//! 2. Walk the tree in pre-order, collecting non-`Other` `(Range, SearchField)` tuples.
//!    Memory: O(AST nodes), not O(source bytes).
//! 3. Process ranges in reverse (innermost/children first) using interval subtraction
//!    against an "uncovered" set so that the deepest node wins each byte.
//! 4. Fill uncovered gaps with `SearchField::Other`, then merge adjacent same-field
//!    ranges.
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
///
/// # Coupling
///
/// The specific node kinds matched here (comments, strings, identifiers, body
/// blocks) are kept in sync with `rskim_core::transform::utils::node_kind_info`
/// and `rskim_core::transform::utils::find_body_child`. Any new node kind
/// categories added to those functions may need a corresponding case here.
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
        // Body/block nodes → FunctionBody.
        //
        // Without this arm, block nodes return Other from the priority match
        // below (priority 1) and are skipped by the innermost-wins rule
        // (Other is the default fill — we only overwrite non-Other). The parent
        // function node has already stamped those bytes as FunctionSignature,
        // so they remain FunctionSignature even though they are body bytes.
        // This inflates FunctionSignature field lengths by the full body size,
        // which artificially raises BM25F scores for body-content queries.
        //
        // Kinds here mirror rskim_core::transform::utils::find_body_child (the
        // union of all body kinds across supported languages).
        "block"              // Rust, Python, Go, Java, C#, Kotlin
        | "statement_block" // TypeScript, JavaScript
        | "compound_statement" // C, C++
        | "constructor_body"   // Java
        | "body_statement"     // Ruby
        | "function_body" => {
            // Kotlin, Swift
            return SearchField::FunctionBody;
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

/// Maximum source size (in bytes) accepted by [`classify_source`].
///
/// Although the classifier allocates O(AST nodes) rather than O(source bytes),
/// an unbounded source could still trigger proportional tree-sitter parsing
/// time. 100 MiB is generous for any real source file.
pub const MAX_SOURCE_BYTES: usize = 100 * 1024 * 1024; // 100 MiB

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

    // Collect non-Other AST node ranges in pre-order (parents before children).
    // Memory: O(AST_nodes) instead of the previous O(source_bytes) per-byte array.
    let mut node_ranges: Vec<(Range<usize>, SearchField)> = Vec::new();

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

            if field != SearchField::Other {
                node_ranges.push((start..end, field));
            }
        }

        // Advance cursor in pre-order.
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

/// Build the final contiguous range list from pre-order node ranges.
///
/// Implements "innermost wins": processes ranges in reverse (children first)
/// so deeper nodes claim bytes before their parents. Uncovered bytes become
/// [`SearchField::Other`]. Adjacent same-field ranges are merged.
fn build_field_ranges(
    node_ranges: Vec<(Range<usize>, SearchField)>,
    source_len: usize,
) -> Vec<(Range<usize>, SearchField)> {
    if node_ranges.is_empty() {
        return vec![(0..source_len, SearchField::Other)];
    }

    // Track which byte intervals are still unclaimed. Starts as the full source.
    // Double-buffer to reuse allocations across iterations.
    let initial_range = 0..source_len;
    let mut uncovered: Vec<Range<usize>> = vec![initial_range];
    let mut next_uncovered: Vec<Range<usize>> = Vec::new();
    let mut result: Vec<(Range<usize>, SearchField)> = Vec::with_capacity(node_ranges.len() * 2);

    // Reverse: children (later in pre-order) claim bytes before parents.
    for (range, field) in node_ranges.into_iter().rev() {
        next_uncovered.clear();

        for unc in &uncovered {
            if unc.end <= range.start || unc.start >= range.end {
                // No overlap — interval stays uncovered.
                next_uncovered.push(unc.clone());
            } else {
                // Overlap — split around the claimed region.
                if unc.start < range.start {
                    next_uncovered.push(unc.start..range.start);
                }
                result.push((unc.start.max(range.start)..unc.end.min(range.end), field));
                if unc.end > range.end {
                    next_uncovered.push(range.end..unc.end);
                }
            }
        }

        std::mem::swap(&mut uncovered, &mut next_uncovered);
    }

    // Remaining uncovered bytes → Other.
    for gap in uncovered {
        result.push((gap, SearchField::Other));
    }

    result.sort_unstable_by_key(|(r, _)| r.start);
    merge_adjacent(&mut result);
    result
}

/// Merge adjacent ranges that share the same field into a single range.
fn merge_adjacent(ranges: &mut Vec<(Range<usize>, SearchField)>) {
    if ranges.len() <= 1 {
        return;
    }
    let mut write = 0;
    for read in 1..ranges.len() {
        if ranges[read].1 == ranges[write].1 && ranges[read].0.start == ranges[write].0.end {
            ranges[write].0.end = ranges[read].0.end;
        } else {
            write += 1;
            ranges[write] = ranges[read].clone();
        }
    }
    ranges.truncate(write + 1);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod tests;
