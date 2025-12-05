//! Types mode transformation
//!
//! ARCHITECTURE: Extract ONLY type definitions.
//!
//! Token reduction target: 90-95%

use crate::{Language, Result, SkimError};
use tree_sitter::{Node, Tree};

/// Maximum AST recursion depth to prevent stack overflow attacks
const MAX_AST_DEPTH: usize = 500;

/// Maximum number of type definitions to prevent memory exhaustion
const MAX_TYPE_DEFS: usize = 10_000;

/// Transform to types-only
///
/// # What to Keep
///
/// - Type aliases
/// - Interface declarations
/// - Enum definitions
/// - Struct definitions (Rust/Go)
/// - Class declarations (name only, no methods)
///
/// # What to Remove
///
/// - ALL implementation code
/// - Function bodies
/// - Method implementations
/// - Function declarations
/// - Comments
///
/// # Implementation Strategy
///
/// Most aggressive mode. Extract only type system information.
pub(crate) fn transform_types(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &crate::TransformConfig,
) -> Result<String> {
    // ARCHITECTURE: Markdown types mode extracts ALL headers (H1-H6)
    // (same as signatures mode - no type system in markdown)
    if language == Language::Markdown {
        return crate::transform::structure::extract_markdown_headers(source, tree, 1, 6);
    }

    // ARCHITECTURE: JSON is handled by Strategy Pattern in Language::transform_source()
    // and never reaches this code path.
    let node_types = get_type_node_types(language).ok_or_else(|| {
        SkimError::ParseError(format!(
            "Language {:?} does not support tree-sitter type transformation",
            language
        ))
    })?;

    let mut type_defs = Vec::new();
    collect_type_definitions(tree.root_node(), source, &node_types, &mut type_defs, 0)?;

    // Check type definition count limit
    if type_defs.len() > MAX_TYPE_DEFS {
        return Err(SkimError::ParseError(format!(
            "Too many type definitions: {} (max: {}). Possible malicious input.",
            type_defs.len(),
            MAX_TYPE_DEFS
        )));
    }

    Ok(type_defs.join("\n\n"))
}

/// Recursively collect type definitions
fn collect_type_definitions(
    node: Node,
    source: &str,
    node_types: &TypeNodeTypes,
    type_defs: &mut Vec<String>,
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

    // Check if this is a type definition node
    if is_type_node(kind, node_types) {
        if let Some(type_def) = extract_type_definition(node, source, node_types)? {
            type_defs.push(type_def);
        }
        // Don't recurse into type definitions to avoid extracting nested content
        return Ok(());
    }

    // Recursively process children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_definitions(child, source, node_types, type_defs, depth + 1)?;
    }

    Ok(())
}

/// Check if node is a type definition
fn is_type_node(kind: &str, node_types: &TypeNodeTypes) -> bool {
    kind == node_types.type_alias
        || kind == node_types.interface
        || kind == node_types.enum_def
        || kind == node_types.class_decl
        || kind == node_types.struct_def
}

/// Extract type definition text from node
fn extract_type_definition(
    node: Node,
    source: &str,
    node_types: &TypeNodeTypes,
) -> Result<Option<String>> {
    let start = node.start_byte();
    let mut end = node.end_byte();

    // For classes, extract only the declaration (strip method bodies)
    if node.kind() == node_types.class_decl {
        // Find class body and strip it
        if let Some(body_node) = find_class_body(node) {
            end = body_node.start_byte();
        }
    }

    // Validate byte ranges
    if end < start || end > source.len() {
        return Ok(None);
    }

    // Validate UTF-8 boundaries
    if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return Err(SkimError::ParseError(format!(
            "Invalid UTF-8 boundary at type definition range [{}, {})",
            start, end
        )));
    }

    let type_def = source[start..end].trim();

    // Skip empty definitions
    if type_def.is_empty() {
        return Ok(None);
    }

    Ok(Some(type_def.to_string()))
}

/// Find class body node
fn find_class_body(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_body" | "declaration_list" | "block" => return Some(child),
            _ => continue,
        }
    }
    None
}

/// Node types for type extraction
struct TypeNodeTypes {
    type_alias: &'static str,
    interface: &'static str,
    enum_def: &'static str,
    class_decl: &'static str,
    struct_def: &'static str,
}

/// Get type node types for language
///
/// Returns None for languages that don't use tree-sitter (e.g., JSON).
/// ARCHITECTURE: JSON is handled by the Strategy Pattern in Language::transform_source().
fn get_type_node_types(language: Language) -> Option<TypeNodeTypes> {
    match language {
        Language::TypeScript => Some(TypeNodeTypes {
            type_alias: "type_alias_declaration",
            interface: "interface_declaration",
            enum_def: "enum_declaration",
            class_decl: "class_declaration",
            struct_def: "", // Not applicable
        }),
        Language::JavaScript => Some(TypeNodeTypes {
            type_alias: "",
            interface: "",
            enum_def: "",
            class_decl: "class_declaration",
            struct_def: "",
        }),
        Language::Python => Some(TypeNodeTypes {
            type_alias: "type_alias_statement",
            interface: "",
            enum_def: "",
            class_decl: "class_definition",
            struct_def: "",
        }),
        Language::Rust => Some(TypeNodeTypes {
            type_alias: "type_item",
            interface: "trait_item",
            enum_def: "enum_item",
            class_decl: "",
            struct_def: "struct_item",
        }),
        Language::Go => Some(TypeNodeTypes {
            type_alias: "type_declaration",
            interface: "interface_type",
            enum_def: "",
            class_decl: "",
            struct_def: "struct_type",
        }),
        Language::Java => Some(TypeNodeTypes {
            type_alias: "",
            interface: "interface_declaration",
            enum_def: "enum_declaration",
            class_decl: "class_declaration",
            struct_def: "",
        }),
        Language::Markdown => Some(TypeNodeTypes {
            type_alias: "", // Not applicable
            interface: "",  // Not applicable
            enum_def: "",   // Not applicable
            class_decl: "", // Not applicable
            struct_def: "", // Not applicable
        }),
        Language::Json => None,
        Language::Yaml => None,
    }
}
