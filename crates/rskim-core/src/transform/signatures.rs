//! Signatures mode transformation
//!
//! ARCHITECTURE: Extract ONLY function/method signatures.
//!
//! Token reduction target: 85-92%

use crate::transform::structure::extract_markdown_headers_with_spans;
use crate::transform::truncate::NodeSpan;
use crate::transform::utils::{to_static_node_kind, FunctionNodeTypes};
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
#[cfg(test)]
#[allow(dead_code)] // Convenience wrapper available for tests
pub(crate) fn transform_signatures(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &crate::TransformConfig,
) -> Result<String> {
    let (text, _spans) = transform_signatures_with_spans(source, tree, language, config)?;
    Ok(text)
}

/// Transform to signatures-only and return NodeSpan metadata for truncation
pub(crate) fn transform_signatures_with_spans(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &crate::TransformConfig,
) -> Result<(String, Vec<NodeSpan>)> {
    let (text, spans, _line_map) =
        transform_signatures_with_spans_and_line_map(source, tree, language)?;
    Ok((text, spans))
}

/// Transform to signatures-only, returning NodeSpan metadata AND a source line map.
///
/// The source line map maps each output line index to the 1-indexed source line
/// where that signature begins. Multi-line signatures map all their output lines
/// to consecutive source lines starting from the signature's start line.
///
/// # Design Decision (AC-18)
/// Signatures mode uses `node.start_position().row + 1` to annotate each
/// signature with its source line. Multi-line signatures get consecutive
/// source line numbers from their start line.
pub(crate) fn transform_signatures_with_spans_and_line_map(
    source: &str,
    tree: &Tree,
    language: Language,
) -> Result<(String, Vec<NodeSpan>, Vec<usize>)> {
    // ARCHITECTURE: Markdown signatures mode extracts ALL headers (H1-H6)
    if language == Language::Markdown {
        let (text, spans) = extract_markdown_headers_with_spans(source, tree, 1, 6)?;
        let line_count = text.lines().count();
        // For markdown, use identity map (headers are extracted with their source structure)
        let line_map = (1..=line_count).collect();
        return Ok((text, spans, line_map));
    }

    // ARCHITECTURE: JSON is handled by Strategy Pattern in Language::transform_source()
    // and never reaches this code path.
    let node_types = get_signature_node_types(language).ok_or_else(|| {
        SkimError::ParseError(format!(
            "Language {:?} does not support tree-sitter signature transformation",
            language
        ))
    })?;

    let mut signatures: Vec<(String, &'static str, usize)> = Vec::new();
    collect_signatures_with_kinds_and_lines(
        tree.root_node(),
        source,
        &node_types,
        &mut signatures,
        0,
    )?;

    // Check signature count limit
    if signatures.len() > MAX_SIGNATURES {
        return Err(SkimError::ParseError(format!(
            "Too many signatures: {} (max: {}). Possible malicious input.",
            signatures.len(),
            MAX_SIGNATURES
        )));
    }

    // Build text, spans, and source line map
    let mut spans = Vec::with_capacity(signatures.len());
    let mut source_line_map: Vec<usize> = Vec::new();
    let mut current_output_line = 0;

    let texts: Vec<String> = signatures
        .into_iter()
        .map(|(sig, kind, source_start_line)| {
            let line_count = sig.lines().count().max(1);
            spans.push(NodeSpan::new(
                current_output_line..current_output_line + line_count,
                kind,
            ));
            // Map each output line to consecutive source lines from source_start_line
            for i in 0..line_count {
                source_line_map.push(source_start_line + i);
            }
            current_output_line += line_count;
            sig
        })
        .collect();

    Ok((texts.join("\n"), spans, source_line_map))
}

/// Recursively collect function/method signatures with node kind AND source start line.
///
/// The source start line is `node.start_position().row + 1` (1-indexed).
fn collect_signatures_with_kinds_and_lines(
    node: Node,
    source: &str,
    node_types: &SignatureNodeTypes,
    signatures: &mut Vec<(String, &'static str, usize)>,
    depth: usize,
) -> Result<()> {
    // SECURITY: Prevent stack overflow from deeply nested or malicious input
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input)",
            MAX_AST_DEPTH
        )));
    }

    let kind = node.kind();

    if is_signature_node(kind, node_types) {
        if let Some(sig) = extract_signature(node, source, node_types)? {
            let static_kind = to_static_node_kind(kind);
            // 1-indexed source line where this signature starts
            let source_start_line = node.start_position().row + 1;
            signatures.push((sig, static_kind, source_start_line));
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_signatures_with_kinds_and_lines(
            child,
            source,
            node_types,
            signatures,
            depth + 1,
        )?;
    }

    Ok(())
}

/// Check if node is a signature-bearing node
fn is_signature_node(kind: &str, node_types: &SignatureNodeTypes) -> bool {
    kind == node_types.function
        || kind == node_types.method
        || kind == "arrow_function"
        || kind == "function_expression"
        || node_types.extra_function_kinds.contains(&kind)
}

/// Extract signature text from node
fn extract_signature(
    node: Node,
    source: &str,
    _node_types: &SignatureNodeTypes,
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
///
/// Delegates to shared `find_body_child` in utils.rs.
fn find_body_for_signature(node: Node) -> Option<Node> {
    crate::transform::utils::find_body_child(node)
}

/// Type alias: signatures mode reuses the shared FunctionNodeTypes struct from utils.
/// The factory function (get_signature_node_types) produces intentionally different
/// values than structure mode — e.g., omitting node kinds with no extractable signature.
type SignatureNodeTypes = FunctionNodeTypes;

/// Get signature node types for language
///
/// Returns None for languages that don't use tree-sitter (e.g., JSON).
/// ARCHITECTURE: JSON is handled by the Strategy Pattern in Language::transform_source().
fn get_signature_node_types(language: Language) -> Option<SignatureNodeTypes> {
    match language {
        Language::TypeScript | Language::JavaScript => Some(SignatureNodeTypes {
            function: "function_declaration",
            method: "method_definition",
            extra_function_kinds: &[],
        }),
        Language::Python => Some(SignatureNodeTypes {
            function: "function_definition",
            method: "function_definition",
            extra_function_kinds: &[],
        }),
        Language::Rust => Some(SignatureNodeTypes {
            function: "function_item",
            method: "function_item",
            extra_function_kinds: &[],
        }),
        Language::Go => Some(SignatureNodeTypes {
            function: "function_declaration",
            method: "method_declaration",
            extra_function_kinds: &[],
        }),
        Language::Java => Some(SignatureNodeTypes {
            function: "method_declaration",
            method: "method_declaration",
            extra_function_kinds: &[],
        }),
        // Unreachable: Markdown returns early via extract_markdown_headers_with_spans
        Language::Markdown => Some(SignatureNodeTypes {
            function: "atx_heading",
            method: "atx_heading",
            extra_function_kinds: &[],
        }),
        Language::C | Language::Cpp => Some(SignatureNodeTypes {
            function: "function_definition",
            method: "function_definition",
            extra_function_kinds: &[],
        }),
        Language::CSharp => Some(SignatureNodeTypes {
            function: "method_declaration",
            method: "constructor_declaration",
            extra_function_kinds: &[],
        }),
        Language::Ruby => Some(SignatureNodeTypes {
            function: "method",
            method: "singleton_method",
            extra_function_kinds: &[],
        }),
        Language::Sql => Some(SignatureNodeTypes {
            function: "create_table",
            method: "create_index",
            extra_function_kinds: &[],
        }),
        Language::Kotlin => Some(SignatureNodeTypes {
            function: "function_declaration",
            method: "function_declaration",
            // anonymous_initializer (init {}) omitted: has no parameters/signature to extract
            extra_function_kinds: &["secondary_constructor"],
        }),
        Language::Swift => Some(SignatureNodeTypes {
            function: "function_declaration",
            method: "function_declaration",
            // deinit_declaration omitted: has no parameters/signature to extract
            extra_function_kinds: &["init_declaration"],
        }),
        Language::Json | Language::Yaml | Language::Toml => None,
    }
}
