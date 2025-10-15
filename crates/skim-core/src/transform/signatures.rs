//! Signatures mode transformation
//!
//! ARCHITECTURE: Extract ONLY function/method signatures.
//!
//! Token reduction target: 85-92%

use crate::{Language, Result, SkimError};
use tree_sitter::{Node, Tree};

/// Maximum AST recursion depth to prevent stack overflow attacks
const MAX_AST_DEPTH: usize = 500;

/// Maximum number of signatures to prevent memory exhaustion
const MAX_SIGNATURES: usize = 10_000;

/// Transform to signatures-only
///
/// # What to Keep
///
/// - Function/method signatures ONLY
/// - No function bodies
/// - No class bodies
/// - No comments
///
/// # What to Remove
///
/// - ALL implementation code
/// - Class bodies (keep class name + method signatures)
/// - Type implementations
/// - Comments
///
/// # Implementation Strategy
///
/// Extract only callable signatures, one per line.
/// More aggressive than structure mode - no bodies at all.
pub(crate) fn transform_signatures(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &crate::TransformConfig,
) -> Result<String> {
    let node_types = get_signature_node_types(language);

    let mut signatures = Vec::new();
    collect_signatures(tree.root_node(), source, &node_types, &mut signatures, 0)?;

    // Check signature count limit
    if signatures.len() > MAX_SIGNATURES {
        return Err(SkimError::ParseError(format!(
            "Too many signatures: {} (max: {}). Possible malicious input.",
            signatures.len(),
            MAX_SIGNATURES
        )));
    }

    Ok(signatures.join("\n"))
}

/// Recursively collect function/method signatures
fn collect_signatures(
    node: Node,
    source: &str,
    node_types: &SignatureNodeTypes,
    signatures: &mut Vec<String>,
    depth: usize,
) -> Result<()> {
    // SECURITY: Prevent stack overflow
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input)",
            MAX_AST_DEPTH
        )));
    }

    let kind = node.kind();

    // Check if this is a function/method node
    if is_signature_node(kind, node_types) {
        if let Some(sig) = extract_signature(node, source, node_types)? {
            signatures.push(sig);
        }
    }

    // Recursively process children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_signatures(child, source, node_types, signatures, depth + 1)?;
    }

    Ok(())
}

/// Check if node is a signature-bearing node
fn is_signature_node(kind: &str, node_types: &SignatureNodeTypes) -> bool {
    kind == node_types.function
        || kind == node_types.method
        || kind == "arrow_function"
        || kind == "function_expression"
        || kind == "method_declaration"
}

/// Extract signature text from node
fn extract_signature(
    node: Node,
    source: &str,
    node_types: &SignatureNodeTypes,
) -> Result<Option<String>> {
    // Find the body node
    let body_node = find_body_for_signature(node);

    let end_pos = if let Some(body) = body_node {
        // Extract everything before the body
        body.start_byte()
    } else {
        // No body found, use entire node
        node.end_byte()
    };

    let start = node.start_byte();

    // Validate byte ranges
    if end_pos < start || end_pos > source.len() {
        return Ok(None);
    }

    // Validate UTF-8 boundaries
    if !source.is_char_boundary(start) || !source.is_char_boundary(end_pos) {
        return Err(SkimError::ParseError(format!(
            "Invalid UTF-8 boundary at signature range [{}, {})",
            start, end_pos
        )));
    }

    let signature = source[start..end_pos].trim();

    // Skip empty signatures
    if signature.is_empty() {
        return Ok(None);
    }

    Ok(Some(signature.to_string()))
}

/// Find body node for a function/method
fn find_body_for_signature(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "statement_block" | "block" | "compound_statement" | "body" => return Some(child),
            _ => continue,
        }
    }
    None
}

/// Node types for signature extraction
struct SignatureNodeTypes {
    function: &'static str,
    method: &'static str,
}

/// Get signature node types for language
fn get_signature_node_types(language: Language) -> SignatureNodeTypes {
    match language {
        Language::TypeScript | Language::JavaScript => SignatureNodeTypes {
            function: "function_declaration",
            method: "method_definition",
        },
        Language::Python => SignatureNodeTypes {
            function: "function_definition",
            method: "function_definition",
        },
        Language::Rust => SignatureNodeTypes {
            function: "function_item",
            method: "function_item",
        },
        Language::Go => SignatureNodeTypes {
            function: "function_declaration",
            method: "method_declaration",
        },
        Language::Java => SignatureNodeTypes {
            function: "method_declaration",
            method: "method_declaration",
        },
    }
}
