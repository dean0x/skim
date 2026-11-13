//! AST-aware field classifier that maps source byte ranges to [`SearchField`] variants.
//!
//! # Algorithm
//!
//! 1. The caller parses `source` once with [`rskim_core::Parser`] and passes the
//!    tree to [`classify_tree`].
//! 2. Walk the tree in pre-order, collecting non-`Other` `(Range, SearchField)` tuples.
//!    Memory: O(AST nodes), not O(source bytes).
//! 3. Sweep the ranges once with a stack of the open nodes so that the deepest
//!    node wins each byte ([`build_field_ranges`]).
//! 4. Fill uncovered gaps with `SearchField::Other`, then merge adjacent same-field
//!    ranges.
//!
//! The classifier never parses and never fails. The caller bounds the source
//! size before it parses.
//!
//! # Format-specific dispatchers
//!
//! Markdown and the data formats have their own classifiers:
//!
//! - Markdown: [`classify_tree`] routes a Markdown tree to its own
//!   tree-sitter classifier.
//! - JSON/YAML/TOML: lightweight std-only scanners. These formats have no
//!   tree-sitter grammar, so they have no tree: [`classify_data_format`] takes
//!   the source alone.
//!
//! Every other tree-sitter language takes the generic path in [`classify_tree`].
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
use tree_sitter::Tree;

use super::field::SearchField;

/// Returns `true` if `kind` is a name token of some grammar.
///
/// A name inside a string or a comment (the `${name}` of a template string)
/// is code, not part of the text around it.
#[inline]
fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "simple_identifier"
            | "variable_name"
            | "attribute_name"
    )
}

/// Returns `true` if `kind` is a comment node of some grammar.
///
/// Every kind is a node type of a grammar that `rskim_core` parses, checked
/// against its `node-types.json` at the pinned version:
///
/// | Kind | Grammars |
/// |------|----------|
/// | `comment` | TypeScript, JavaScript, Python, Go, C, C++, C#, Ruby, SQL (`--`), Swift, Bash |
/// | `line_comment`, `block_comment` | Rust, Java, Kotlin |
/// | `doc_comment` | Rust (inside a line or block comment) |
/// | `multiline_comment` | Swift |
/// | `marginalia` | SQL (`/* … */`) |
/// | `html_comment` | TypeScript, JavaScript (`<!-- … -->`) |
///
/// The role test `a_word_in_a_comment_or_a_string_gets_that_role_in_every_language`
/// holds one source per comment form of each language.
#[inline]
fn is_comment_kind(kind: &str) -> bool {
    matches!(
        kind,
        "comment"
            | "line_comment"
            | "block_comment"
            | "doc_comment"
            | "multiline_comment"
            | "marginalia"
            | "html_comment"
    )
}

/// Returns `true` if `kind` is a string node of some grammar, checked like
/// [`is_comment_kind`].
///
/// | Kind | Grammars |
/// |------|----------|
/// | `string` | TypeScript, JavaScript, Python, Ruby, Bash |
/// | `string_literal` | Rust, Java, C, C++, C#, Kotlin |
/// | `interpreted_string_literal` | Go |
/// | `raw_string_literal` | Go, Rust, C++, C#, Swift |
/// | `string_content` | Python, Rust, C, C++, C#, Ruby, Kotlin, Bash |
/// | `template_string` | TypeScript, JavaScript |
/// | `line_string_literal`, `multi_line_string_literal` | Swift |
/// | `verbatim_string_literal` | C# |
/// | `raw_string`, `ansi_c_string`, `heredoc_body` | Bash |
/// | `heredoc_body` | Ruby |
///
/// The SQL `literal` kind is not here: it is a string, a number, a boolean or
/// null, and [`is_sql_string`] decides.
#[inline]
fn is_string_kind(kind: &str) -> bool {
    matches!(
        kind,
        "string"
            | "string_literal"
            | "interpreted_string_literal"
            | "raw_string_literal"
            | "string_content"
            | "template_string"
            | "line_string_literal"
            | "multi_line_string_literal"
            | "verbatim_string_literal"
            | "raw_string"
            | "ansi_c_string"
            | "heredoc_body"
    )
}

/// Returns `true` if the text of a SQL `literal` node is a string. A string
/// has a quote (`'…'`, `"…"`) or a dollar quote (`$$…$$`); a number, a boolean
/// and null have neither.
fn is_sql_string(text: &str) -> bool {
    text.bytes().any(|b| matches!(b, b'\'' | b'"' | b'$'))
}

/// Returns `true` if `kind` is a function or block body node.
///
/// The kinds mirror `rskim_core::transform::utils::find_body_child`, the union
/// of all body kinds across the supported languages.
#[inline]
fn is_body_kind(kind: &str) -> bool {
    matches!(
        kind,
        "block"                    // Rust, Python, Go, Java, C#, Kotlin
            | "statement_block"    // TypeScript, JavaScript
            | "compound_statement" // C, C++
            | "constructor_body"   // Java
            | "body_statement"     // Ruby
            | "function_body" // Kotlin, Swift
    )
}

/// The field of one syntax node: the comment and string kinds, `Code` for a
/// name, a body and a declaration or import node (priority 3 and up in
/// `rskim_core::node_kind_priority`), and `Other` for the rest.
///
/// `text` is the source text of the node. Only a SQL `literal` reads it.
fn node_field(kind: &str, lang: Language, text: &str) -> SearchField {
    if is_comment_kind(kind) {
        SearchField::Comment
    } else if is_string_kind(kind)
        || (lang == Language::Sql && kind == "literal" && is_sql_string(text))
    {
        SearchField::StringLiteral
    } else if is_identifier_kind(kind)
        || is_body_kind(kind)
        || rskim_core::node_kind_priority(kind) >= 3
    {
        SearchField::Code
    } else {
        SearchField::Other
    }
}

/// Classify the bytes of the data formats (JSON, YAML, TOML), which have no
/// tree-sitter grammar and so no tree.
///
/// Returns `None` for every other language: those take [`classify_tree`].
/// Otherwise returns a sorted, non-overlapping, contiguous list of
/// `(Range<usize>, SearchField)` tuples covering every byte from `0` to
/// `source.len()`, or an empty vector for empty source.
pub(super) fn classify_data_format(
    source: &str,
    lang: Language,
) -> Option<Vec<(Range<usize>, SearchField)>> {
    match lang {
        Language::Json => Some(super::fields::serde_fields::classify_json(source)),
        Language::Yaml => Some(super::fields::serde_fields::classify_yaml(source)),
        Language::Toml => Some(super::fields::serde_fields::classify_toml(source)),
        _ => None,
    }
}

/// Classify all bytes in `source` according to their AST-derived field.
///
/// `tree` must be the parse of `source` for `lang` (the TSX grammar for a
/// `.tsx` file). Returns a sorted, non-overlapping, contiguous list of
/// `(Range<usize>, SearchField)` tuples covering every byte from `0` to
/// `source.len()`, or an empty vector for empty source.
pub(super) fn classify_tree(
    tree: &Tree,
    source: &str,
    lang: Language,
) -> Vec<(Range<usize>, SearchField)> {
    if source.is_empty() {
        return Vec::new();
    }

    let len = source.len();

    // Markdown needs custom heading-level logic.
    if lang == Language::Markdown {
        return super::fields::markdown::classify_markdown(tree, source);
    }

    // Generic tree-sitter path for all other languages.
    let root = tree.root_node();

    // Collect non-Other AST node ranges in pre-order (parents before children).
    // Memory: O(AST_nodes), not O(source_bytes).
    let mut node_ranges: Vec<(Range<usize>, SearchField)> = Vec::new();

    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let byte_range = node.byte_range();

        // Clamp range to source bounds (safety against malformed AST).
        let start = byte_range.start.min(len);
        let end = byte_range.end.min(len);

        if start < end {
            let field = node_field(node.kind(), lang, source.get(start..end).unwrap_or(""));

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
                return build_field_ranges(node_ranges, len);
            }
        }
    }
}

/// Build the final contiguous range list from pre-order node ranges.
///
/// Implements "innermost wins": a byte takes the field of the innermost node
/// that covers it, and a byte no node covers is [`SearchField::Other`]. One
/// sweep over the ranges with a stack of the open nodes does it in time linear
/// in the number of ranges. Adjacent same-field ranges are merged.
///
/// # Preconditions
///
/// - `node_ranges` must be in **pre-order** (parents before children) and
///   properly nested, as the nodes of a syntax tree are. A range that ends
///   after its parent is cut at the parent's end.
/// - `source_len` must equal the byte length of the source that was parsed.
///
/// # Output guarantees
///
/// The returned `Vec` is:
/// - Sorted ascending by `range.start`.
/// - Non-overlapping (no two ranges share a byte).
/// - Contiguous (covers every byte `0..source_len`).
/// - `sum(range.end - range.start) == source_len`.
/// - For empty input (`node_ranges.is_empty()`), returns a single `Other`
///   range covering `0..source_len`.
pub(super) fn build_field_ranges(
    node_ranges: Vec<(Range<usize>, SearchField)>,
    source_len: usize,
) -> Vec<(Range<usize>, SearchField)> {
    if node_ranges.is_empty() {
        return vec![(0..source_len, SearchField::Other)];
    }

    let mut result: Vec<(Range<usize>, SearchField)> = Vec::with_capacity(node_ranges.len() * 2);
    // The open nodes, innermost last: (end of the node, its field).
    let mut open: Vec<(usize, SearchField)> = Vec::new();
    // Every byte before `cursor` is already in `result`.
    let mut cursor = 0usize;
    let emit = |result: &mut Vec<(Range<usize>, SearchField)>,
                cursor: &mut usize,
                to: usize,
                field: SearchField| {
        if *cursor < to {
            result.push((*cursor..to, field));
            *cursor = to;
        }
    };

    for (range, field) in node_ranges {
        // A node that ends before this one starts is closed: its bytes up to
        // its end are its own.
        while let Some(&(end, open_field)) = open.last() {
            if end > range.start {
                break;
            }
            emit(&mut result, &mut cursor, end, open_field);
            open.pop();
        }
        // The bytes between the cursor and this node belong to the node that
        // is open around it, or to no node.
        let parent = open.last().copied();
        emit(
            &mut result,
            &mut cursor,
            range.start,
            parent.map_or(SearchField::Other, |(_, f)| f),
        );
        let end = parent.map_or(range.end, |(parent_end, _)| range.end.min(parent_end));
        open.push((end, field));
    }
    while let Some((end, open_field)) = open.pop() {
        emit(&mut result, &mut cursor, end, open_field);
    }
    emit(&mut result, &mut cursor, source_len, SearchField::Other);

    merge_adjacent(&mut result);
    result
}

/// Merge adjacent ranges that share the same field into a single range.
pub(super) fn merge_adjacent(ranges: &mut Vec<(Range<usize>, SearchField)>) {
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

/// Parse `source` as `lang` and classify it, for any language: the data
/// formats take [`classify_data_format`], every other language is parsed and
/// takes [`classify_tree`].
#[cfg(test)]
pub(super) fn classify_source(
    source: &str,
    lang: Language,
) -> rskim_core::Result<Vec<(Range<usize>, SearchField)>> {
    if let Some(ranges) = classify_data_format(source, lang) {
        return Ok(ranges);
    }
    let mut parser = rskim_core::Parser::new(lang)?;
    let tree = parser.parse(source)?;
    Ok(classify_tree(&tree, source, lang))
}

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod tests;
