//! Structure mode transformation
//!
//! ARCHITECTURE: Strip function/method bodies, keep structure.
//!
//! Token reduction target: 70-80%

use crate::{Language, Result, TransformConfig};
use tree_sitter::{Node, Tree, TreeCursor};
use std::collections::HashMap;

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
    config: &TransformConfig,
) -> Result<String> {
    // Get language-specific node types
    let node_types = get_node_types_for_language(language);

    // Find all body nodes to replace
    let mut replacements: HashMap<(usize, usize), &str> = HashMap::new();
    collect_body_replacements(tree.root_node(), &node_types, &mut replacements);

    // Build output by replacing bodies
    let mut result = String::with_capacity(source.len());
    let mut last_pos = 0;

    // Sort replacements by start position
    let mut sorted_replacements: Vec<_> = replacements.into_iter().collect();
    sorted_replacements.sort_by_key(|(range, _)| range.0);

    for ((start, end), replacement) in sorted_replacements {
        // Copy everything before this replacement
        result.push_str(&source[last_pos..start]);
        // Add replacement
        result.push_str(replacement);
        last_pos = end;
    }

    // Copy remaining source
    result.push_str(&source[last_pos..]);

    Ok(result)
}

/// Recursively collect body nodes that should be replaced
fn collect_body_replacements<'a>(
    node: Node,
    node_types: &NodeTypes,
    replacements: &mut HashMap<(usize, usize), &'a str>,
) {
    let kind = node.kind();

    // Check if this is a function/method with a body
    if matches_function_node(kind, node_types) {
        if let Some(body) = find_body_node(node) {
            let start = body.start_byte();
            let end = body.end_byte();
            replacements.insert((start, end), " { /* ... */ }");
        }
    }

    // Recursively process children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_body_replacements(child, node_types, replacements);
    }
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
    }
}
