//! Structure mode transformation
//!
//! ARCHITECTURE: Strip function/method bodies, keep structure.
//!
//! Token reduction target: 70-80%

use crate::{Language, Result, SkimError, TransformConfig};
use tree_sitter::{Node, Tree};
use std::collections::HashMap;

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
pub(crate) fn transform_structure(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<String> {
    // ARCHITECTURE: Markdown uses extraction, not replacement
    // Extract H1-H3 headers only (top-level document structure)
    if language == Language::Markdown {
        return extract_markdown_headers(source, tree, 1, 3);
    }

    // Get language-specific node types
    let node_types = get_node_types_for_language(language);

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

    // Build output by replacing bodies
    // Preallocate with buffer for replacement overhead
    let estimated_capacity = source.len() + (replacements.len() * 20);
    let mut result = String::with_capacity(estimated_capacity);
    let mut last_pos = 0;

    // Sort replacements by start position
    let mut sorted_replacements: Vec<_> = replacements.into_iter().collect();
    sorted_replacements.sort_unstable_by_key(|(range, _)| range.0);

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

    Ok(result)
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
            "statement_block" | "block" | "compound_statement" => return Some(child),
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
fn get_node_types_for_language(language: Language) -> NodeTypes {
    match language {
        Language::TypeScript | Language::JavaScript => NodeTypes {
            function: "function_declaration",
            method: "method_definition",
        },
        Language::Python => NodeTypes {
            function: "function_definition",
            method: "function_definition",
        },
        Language::Rust => NodeTypes {
            function: "function_item",
            method: "function_item",
        },
        Language::Go => NodeTypes {
            function: "function_declaration",
            method: "method_declaration",
        },
        Language::Java => NodeTypes {
            function: "method_declaration",
            method: "method_declaration",
        },
        Language::Markdown => NodeTypes {
            function: "atx_heading", // Not used - markdown uses special extraction
            method: "atx_heading",    // Not used - markdown uses special extraction
        },
    }
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
pub(crate) fn extract_markdown_headers(
    source: &str,
    tree: &Tree,
    min_level: u32,
    max_level: u32,
) -> Result<String> {
    let mut headers = Vec::new();
    let root = tree.root_node();

    // Traverse all nodes to find headers (depth, node)
    let mut visit_stack = vec![(0_usize, root)];

    while let Some((depth, node)) = visit_stack.pop() {
        // SECURITY: Prevent stack overflow from deeply nested AST
        if depth > MAX_MARKDOWN_DEPTH {
            return Err(SkimError::ParseError(format!(
                "Maximum markdown depth exceeded: {} (possible malicious input)",
                MAX_MARKDOWN_DEPTH
            )));
        }

        // SECURITY: Prevent memory exhaustion from excessive headers
        if headers.len() > MAX_MARKDOWN_HEADERS {
            return Err(SkimError::ParseError(format!(
                "Too many markdown headers: {} (max: {}). Possible malicious input.",
                headers.len(),
                MAX_MARKDOWN_HEADERS
            )));
        }
        let node_type = node.kind();

        // ATX headers: # Header
        if node_type == "atx_heading" {
            // Find marker child to determine level (atx_h1_marker through atx_h6_marker)
            let mut cursor = node.walk();
            let marker = node.children(&mut cursor).find(|child| {
                child.kind().starts_with("atx_h") && child.kind().ends_with("_marker")
            });

            if let Some(marker) = marker {
                // Extract level from marker node type (atx_h1_marker -> 1, atx_h2_marker -> 2, etc.)
                let marker_kind = marker.kind();
                let level = marker_kind
                    .chars()
                    .find(|c| c.is_ascii_digit())
                    .and_then(|c| c.to_digit(10))
                    .unwrap_or(1); // Default to H1 if parsing fails

                if level >= min_level && level <= max_level {
                    let header_text = node.utf8_text(source.as_bytes())
                        .map_err(|e| SkimError::ParseError(format!("UTF-8 error in header: {}", e)))?;
                    headers.push(header_text.to_string());
                }
            }
        }

        // Setext headers: underlined with === or ---
        else if node_type == "setext_heading" {
            // Setext headers are H1 (===) or H2 (---)
            // Determine level by checking child node type for underline marker
            let mut cursor = node.walk();
            let underline = node.children(&mut cursor).find(|child| {
                let kind = child.kind();
                kind == "setext_h1_underline" || kind == "setext_h2_underline"
            });

            let level = if let Some(underline_node) = underline {
                // Extract level from underline node type
                if underline_node.kind() == "setext_h1_underline" { 1 } else { 2 }
            } else {
                // Fallback: if no underline child found, default to H1
                1
            };

            if level >= min_level && level <= max_level {
                let header_text = node.utf8_text(source.as_bytes())
                    .map_err(|e| SkimError::ParseError(format!("UTF-8 error in setext header: {}", e)))?;
                headers.push(header_text.to_string());
            }
        }

        // Add children to visit stack with incremented depth (depth-first traversal)
        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            visit_stack.push((depth + 1, child));
        }
    }

    // Join headers with newlines
    Ok(headers.join("\n"))
}
