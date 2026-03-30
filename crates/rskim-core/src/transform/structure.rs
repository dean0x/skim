//! Structure mode transformation
//!
//! ARCHITECTURE: Strip function/method bodies, keep structure.
//!
//! Token reduction target: 70-80%

use crate::transform::truncate::NodeSpan;
use crate::transform::utils::to_static_node_kind;
use crate::{Language, Result, SkimError, TransformConfig};
use std::collections::HashMap;
use tree_sitter::{Node, Tree};

/// Maximum AST recursion depth to prevent stack overflow attacks
const MAX_AST_DEPTH: usize = 500;

/// Maximum number of AST nodes to prevent memory exhaustion
const MAX_AST_NODES: usize = 100_000;

/// Maximum markdown traversal depth to prevent stack overflow
const MAX_MARKDOWN_DEPTH: usize = 500;

/// Maximum number of markdown headers to prevent memory exhaustion
const MAX_MARKDOWN_HEADERS: usize = 10_000;

/// Transform to structure-only (strip implementations)
///
/// # What to Keep
///
/// - Function/method signatures
/// - Class declarations
/// - Type definitions
/// - Imports/exports
/// - Structural comments (if config.preserve_comments)
///
/// # What to Remove
///
/// - Function bodies → `/* ... */`
/// - Implementation details
/// - Non-structural comments
///
/// # Implementation Notes (Week 2)
///
/// 1. Traverse AST with TreeCursor
/// 2. For each function/method node:
///    - Extract signature (everything before `{`)
///    - Replace body with `/* ... */`
/// 3. For classes: keep structure, strip method bodies
/// 4. Preserve indentation
#[cfg(test)]
#[allow(dead_code)] // Convenience wrapper available for tests
pub(crate) fn transform_structure(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<String> {
    let (text, _spans) = transform_structure_with_spans(source, tree, language, config)?;
    Ok(text)
}

/// Transform to structure-only and return NodeSpan metadata for truncation
pub(crate) fn transform_structure_with_spans(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<(String, Vec<NodeSpan>)> {
    // ARCHITECTURE: Markdown uses extraction, not replacement
    // Extract H1-H3 headers only (top-level document structure)
    if language == Language::Markdown {
        let (text, spans) = extract_markdown_headers_with_spans(source, tree, 1, 3)?;
        return Ok((text, spans));
    }

    // Get language-specific node types
    // ARCHITECTURE: JSON is handled by Strategy Pattern in Language::transform_source()
    // and never reaches this code path. This unwrap is safe due to early return above.
    let node_types = get_node_types_for_language(language).ok_or_else(|| {
        SkimError::ParseError(format!(
            "Language {:?} does not support tree-sitter structure transformation",
            language
        ))
    })?;

    // Find all body nodes to replace
    let mut replacements: HashMap<(usize, usize), &'static str> = HashMap::new();
    collect_body_replacements(tree.root_node(), &node_types, &mut replacements, 0)?;

    // Check node count limit to prevent memory exhaustion
    if replacements.len() > MAX_AST_NODES {
        return Err(SkimError::ParseError(format!(
            "Too many AST nodes: {} (max: {}). Possible malicious input.",
            replacements.len(),
            MAX_AST_NODES
        )));
    }

    // Build output by replacing bodies, tracking byte offset changes
    let estimated_capacity = source.len() + (replacements.len() * 20);
    let mut result = String::with_capacity(estimated_capacity);
    let mut last_pos = 0;

    // Sort replacements by start position
    let mut sorted_replacements: Vec<_> = replacements.into_iter().collect();
    sorted_replacements.sort_unstable_by_key(|(range, _)| range.0);

    // Track cumulative byte offset delta (output_pos - source_pos)
    let mut offset_delta: i64 = 0;
    let mut offset_map: Vec<(usize, i64)> = Vec::new(); // (source_byte, delta)

    for ((start, end), replacement) in sorted_replacements {
        // Validate byte ranges
        if end < start {
            return Err(SkimError::ParseError(format!(
                "Invalid AST range: start={} end={}",
                start, end
            )));
        }
        if end > source.len() {
            return Err(SkimError::ParseError(format!(
                "AST range exceeds source length: end={} len={}",
                end,
                source.len()
            )));
        }

        // Skip overlapping replacements (nested functions already handled by parent)
        if start < last_pos {
            continue;
        }

        // Validate UTF-8 boundaries before slicing
        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(SkimError::ParseError(format!(
                "Invalid UTF-8 boundary at range [{}, {})",
                start, end
            )));
        }

        // Copy everything before this replacement
        result.push_str(&source[last_pos..start]);
        // Add replacement
        result.push_str(replacement);

        // Track the offset change at this replacement point
        let replaced_len = end - start;
        let replacement_len = replacement.len();
        // SAFETY: Both values bounded by source file size (far below i64::MAX).
        offset_delta += replacement_len as i64 - replaced_len as i64;
        offset_map.push((end, offset_delta));

        last_pos = end;
    }

    // Validate final position
    if !source.is_char_boundary(last_pos) {
        return Err(SkimError::ParseError(format!(
            "Invalid UTF-8 boundary at position {}",
            last_pos
        )));
    }

    // Copy remaining source
    result.push_str(&source[last_pos..]);

    // Build NodeSpans from top-level AST children
    let spans = build_spans_from_top_level_nodes(tree, &result, &offset_map);

    Ok((result, spans))
}

/// Recursively collect body nodes that should be replaced
///
/// # Security
/// - Enforces MAX_AST_DEPTH to prevent stack overflow
/// - Returns error if depth limit exceeded
fn collect_body_replacements(
    node: Node,
    node_types: &NodeTypes,
    replacements: &mut HashMap<(usize, usize), &'static str>,
    depth: usize,
) -> Result<()> {
    // SECURITY: Prevent stack overflow from deeply nested AST
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input with deeply nested functions)",
            MAX_AST_DEPTH
        )));
    }

    let kind = node.kind();

    // Check if this is a function/method with a body
    if matches_function_node(kind, node_types) {
        if let Some(body) = find_body_node(node) {
            let start = body.start_byte();
            let end = body.end_byte();
            replacements.insert((start, end), " { /* ... */ }");
        }
    }

    // Recursively process children with incremented depth
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_body_replacements(child, node_types, replacements, depth + 1)?;
    }

    Ok(())
}

/// Check if node kind matches a function/method/class
fn matches_function_node(kind: &str, node_types: &NodeTypes) -> bool {
    kind == node_types.function
        || kind == node_types.method
        || kind == "arrow_function"
        || kind == "function_expression"
}

/// Find the body node of a function/method
fn find_body_node(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "statement_block" | "block" | "compound_statement" | "body" | "body_statement"
            | "function_body" => return Some(child),
            _ => continue,
        }
    }
    None
}

/// Node types for different languages
struct NodeTypes {
    function: &'static str,
    method: &'static str,
}

/// Get node types based on language
///
/// Returns None for languages that don't use tree-sitter node types (e.g., JSON).
/// ARCHITECTURE: JSON is handled by the Strategy Pattern in Language::transform_source(),
/// which calls json::transform_json() directly instead of using tree-sitter parsing.
fn get_node_types_for_language(language: Language) -> Option<NodeTypes> {
    match language {
        Language::TypeScript | Language::JavaScript => Some(NodeTypes {
            function: "function_declaration",
            method: "method_definition",
        }),
        Language::Python => Some(NodeTypes {
            function: "function_definition",
            method: "function_definition",
        }),
        Language::Rust => Some(NodeTypes {
            function: "function_item",
            method: "function_item",
        }),
        Language::Go => Some(NodeTypes {
            function: "function_declaration",
            method: "method_declaration",
        }),
        Language::Java => Some(NodeTypes {
            function: "method_declaration",
            method: "method_declaration",
        }),
        // Unreachable: Markdown returns early via extract_markdown_headers_with_spans
        Language::Markdown => Some(NodeTypes {
            function: "atx_heading",
            method: "atx_heading",
        }),
        Language::C | Language::Cpp => Some(NodeTypes {
            function: "function_definition",
            method: "function_definition",
        }),
        Language::CSharp => Some(NodeTypes {
            function: "method_declaration",
            method: "constructor_declaration",
        }),
        Language::Ruby => Some(NodeTypes {
            function: "method",
            method: "singleton_method",
        }),
        Language::Sql => Some(NodeTypes {
            function: "statement",
            method: "statement",
        }),
        Language::Kotlin => Some(NodeTypes {
            function: "function_declaration",
            method: "function_declaration", // Kotlin doesn't distinguish methods from functions
        }),
        Language::Json | Language::Yaml | Language::Toml => None,
    }
}

/// Build NodeSpans from top-level AST children, mapping source byte positions
/// to output line ranges using the offset map
fn build_spans_from_top_level_nodes(
    tree: &Tree,
    output: &str,
    offset_map: &[(usize, i64)],
) -> Vec<NodeSpan> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut spans = Vec::new();

    // Pre-compute line starts in the output for byte-to-line conversion
    let line_starts: Vec<usize> =
        std::iter::once(0)
            .chain(output.bytes().enumerate().filter_map(|(i, b)| {
                if b == b'\n' {
                    Some(i + 1)
                } else {
                    None
                }
            }))
            .collect();

    let byte_to_line = |byte_pos: usize| -> usize {
        match line_starts.binary_search(&byte_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        }
    };

    // Map source byte position to output byte position using offset map
    let source_to_output_byte = |source_byte: usize| -> usize {
        let delta = match offset_map.binary_search_by_key(&source_byte, |&(pos, _)| pos) {
            Ok(idx) => offset_map[idx].1,
            Err(0) => 0,
            Err(idx) => offset_map[idx - 1].1,
        };
        // SAFETY: source_byte bounded by source file size (far below i64::MAX).
        (source_byte as i64 + delta).max(0) as usize
    };

    for child in root.children(&mut cursor) {
        let kind = child.kind();
        let source_start = child.start_byte();
        let source_end = child.end_byte();

        let output_start = source_to_output_byte(source_start).min(output.len());
        let output_end = source_to_output_byte(source_end).min(output.len());

        let start_line = byte_to_line(output_start);
        let end_line = byte_to_line(output_end.saturating_sub(1)) + 1;

        // Map tree-sitter node kinds to static str for priority scoring
        let static_kind = to_static_node_kind(kind);

        if start_line < end_line {
            spans.push(NodeSpan::new(start_line..end_line, static_kind));
        }
    }

    spans
}

/// Extract markdown headers within a level range
///
/// # Arguments
/// * `source` - Original markdown source
/// * `tree` - Parsed tree-sitter AST
/// * `min_level` - Minimum header level (1 = H1)
/// * `max_level` - Maximum header level (6 = H6)
///
/// # Returns
/// Only the header lines within the specified range
///
/// # Security
/// - Enforces MAX_MARKDOWN_DEPTH to prevent stack overflow
/// - Enforces MAX_MARKDOWN_HEADERS to prevent memory exhaustion
#[cfg(test)]
#[allow(dead_code)] // Convenience wrapper available for tests
pub(crate) fn extract_markdown_headers(
    source: &str,
    tree: &Tree,
    min_level: u32,
    max_level: u32,
) -> Result<String> {
    let (text, _spans) = extract_markdown_headers_with_spans(source, tree, min_level, max_level)?;
    Ok(text)
}

/// Extract markdown headers with NodeSpan metadata for truncation
///
/// Each header gets its own span with "atx_heading" or "setext_heading" kind.
pub(crate) fn extract_markdown_headers_with_spans(
    source: &str,
    tree: &Tree,
    min_level: u32,
    max_level: u32,
) -> Result<(String, Vec<NodeSpan>)> {
    let mut headers: Vec<(String, &'static str)> = Vec::new();
    let root = tree.root_node();

    let mut visit_stack = vec![(0_usize, root)];

    while let Some((depth, node)) = visit_stack.pop() {
        if depth > MAX_MARKDOWN_DEPTH {
            return Err(SkimError::ParseError(format!(
                "Maximum markdown depth exceeded: {} (possible malicious input)",
                MAX_MARKDOWN_DEPTH
            )));
        }

        if headers.len() > MAX_MARKDOWN_HEADERS {
            return Err(SkimError::ParseError(format!(
                "Too many markdown headers: {} (max: {}). Possible malicious input.",
                headers.len(),
                MAX_MARKDOWN_HEADERS
            )));
        }

        let node_type = node.kind();

        if node_type == "atx_heading" {
            let mut cursor = node.walk();
            let marker = node.children(&mut cursor).find(|child| {
                child.kind().starts_with("atx_h") && child.kind().ends_with("_marker")
            });

            if let Some(marker) = marker {
                let marker_kind = marker.kind();
                let level = marker_kind
                    .chars()
                    .find(|c| c.is_ascii_digit())
                    .and_then(|c| c.to_digit(10))
                    .unwrap_or(1);

                if level >= min_level && level <= max_level {
                    let header_text = node.utf8_text(source.as_bytes()).map_err(|e| {
                        SkimError::ParseError(format!("UTF-8 error in header: {}", e))
                    })?;
                    headers.push((header_text.to_string(), "atx_heading"));
                }
            }
        } else if node_type == "setext_heading" {
            let mut cursor = node.walk();
            let underline = node.children(&mut cursor).find(|child| {
                let kind = child.kind();
                kind == "setext_h1_underline" || kind == "setext_h2_underline"
            });

            let level = if let Some(underline_node) = underline {
                if underline_node.kind() == "setext_h1_underline" {
                    1
                } else {
                    2
                }
            } else {
                1
            };

            if level >= min_level && level <= max_level {
                let header_text = node.utf8_text(source.as_bytes()).map_err(|e| {
                    SkimError::ParseError(format!("UTF-8 error in setext header: {}", e))
                })?;
                headers.push((header_text.to_string(), "setext_heading"));
            }
        }

        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            visit_stack.push((depth + 1, child));
        }
    }

    // Build text and spans
    let mut spans = Vec::with_capacity(headers.len());
    let mut current_line = 0;

    let texts: Vec<String> = headers
        .into_iter()
        .map(|(text, kind)| {
            let line_count = text.lines().count().max(1);
            spans.push(NodeSpan::new(current_line..current_line + line_count, kind));
            current_line += line_count;
            text
        })
        .collect();

    Ok((texts.join("\n"), spans))
}
