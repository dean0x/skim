//! Types mode transformation
//!
//! ARCHITECTURE: Extract ONLY type definitions.
//!
//! Token reduction target: 90-95%

use crate::transform::structure::extract_markdown_headers_with_spans;
use crate::transform::truncate::NodeSpan;
use crate::transform::utils::to_static_node_kind;
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
#[cfg(test)]
#[allow(dead_code)] // Convenience wrapper available for tests
pub(crate) fn transform_types(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &crate::TransformConfig,
) -> Result<String> {
    let (text, _spans) = transform_types_with_spans(source, tree, language, config)?;
    Ok(text)
}

/// Transform to types-only and return NodeSpan metadata for truncation
pub(crate) fn transform_types_with_spans(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &crate::TransformConfig,
) -> Result<(String, Vec<NodeSpan>)> {
    // ARCHITECTURE: Markdown types mode extracts ALL headers (H1-H6)
    if language == Language::Markdown {
        return extract_markdown_headers_with_spans(source, tree, 1, 6);
    }

    // ARCHITECTURE: JSON is handled by Strategy Pattern in Language::transform_source()
    // and never reaches this code path.
    let node_types = get_type_node_types(language).ok_or_else(|| {
        SkimError::ParseError(format!(
            "Language {:?} does not support tree-sitter type transformation",
            language
        ))
    })?;

    let mut type_defs: Vec<(String, &'static str)> = Vec::new();
    collect_type_definitions_with_kinds(tree.root_node(), source, &node_types, &mut type_defs, 0)?;

    // Check type definition count limit
    if type_defs.len() > MAX_TYPE_DEFS {
        return Err(SkimError::ParseError(format!(
            "Too many type definitions: {} (max: {}). Possible malicious input.",
            type_defs.len(),
            MAX_TYPE_DEFS
        )));
    }

    // Build text and spans, tracking line offsets
    // Types mode joins with \n\n (two newlines between defs)
    let type_defs_count = type_defs.len();
    let mut spans = Vec::with_capacity(type_defs_count);
    let mut current_line = 0;

    let texts: Vec<String> = type_defs
        .into_iter()
        .enumerate()
        .map(|(idx, (def, kind))| {
            let line_count = def.lines().count().max(1);
            spans.push(NodeSpan::new(current_line..current_line + line_count, kind));
            current_line += line_count;
            // Account for the blank line separator between defs
            if idx < type_defs_count - 1 {
                current_line += 1; // \n\n adds one extra line
            }
            def
        })
        .collect();

    Ok((texts.join("\n\n"), spans))
}

/// Recursively collect type definitions with their node kind
fn collect_type_definitions_with_kinds(
    node: Node,
    source: &str,
    node_types: &TypeNodeTypes,
    type_defs: &mut Vec<(String, &'static str)>,
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

    if is_type_node(kind, node_types) {
        // For C/C++ struct_specifier and enum_specifier, only extract actual definitions
        // (nodes with a body), not bare type references like `struct Point` in return types.
        if is_type_reference(kind, &node) {
            return Ok(());
        }
        if let Some(type_def) = extract_type_definition(node, source, node_types)? {
            let static_kind = to_static_node_kind(kind);
            type_defs.push((type_def, static_kind));
        }
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_definitions_with_kinds(child, source, node_types, type_defs, depth + 1)?;
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

/// Check if a C/C++ struct_specifier or enum_specifier is just a type
/// reference (no body), not an actual definition. Only applies to these
/// specific node kinds since they represent both definitions and references
/// in C/C++ grammars. Other languages (Rust `enum_item`, TS `enum_declaration`)
/// don't have this ambiguity.
fn is_type_reference(kind: &str, node: &Node) -> bool {
    if kind != "struct_specifier" && kind != "enum_specifier" {
        return false;
    }
    // A definition has a body child (field_declaration_list, enumerator_list, etc.)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "field_declaration_list" | "enumerator_list" => return false,
            _ => {}
        }
    }
    true
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
            "class_body"
            | "declaration_list"
            | "block"
            | "field_declaration_list"
            | "body_statement"
            | "enum_class_body"
            | "protocol_body" => return Some(child),
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
        // Unreachable: Markdown returns early via extract_markdown_headers_with_spans
        Language::Markdown => Some(TypeNodeTypes {
            type_alias: "",
            interface: "",
            enum_def: "",
            class_decl: "",
            struct_def: "",
        }),
        Language::C => Some(TypeNodeTypes {
            type_alias: "type_definition",
            interface: "",
            enum_def: "enum_specifier",
            class_decl: "",
            struct_def: "struct_specifier",
        }),
        Language::Cpp => Some(TypeNodeTypes {
            type_alias: "type_definition",
            interface: "",
            enum_def: "enum_specifier",
            class_decl: "class_specifier",
            struct_def: "struct_specifier",
        }),
        Language::CSharp => Some(TypeNodeTypes {
            type_alias: "",
            interface: "interface_declaration",
            enum_def: "enum_declaration",
            class_decl: "class_declaration",
            struct_def: "struct_declaration",
        }),
        Language::Ruby => Some(TypeNodeTypes {
            type_alias: "",
            interface: "module",
            enum_def: "",
            class_decl: "class",
            struct_def: "",
        }),
        Language::Sql => Some(TypeNodeTypes {
            type_alias: "",
            interface: "",
            enum_def: "",
            class_decl: "",
            struct_def: "create_table", // CREATE TABLE defines the type structure in SQL
        }),
        Language::Kotlin => Some(TypeNodeTypes {
            type_alias: "type_alias",
            interface: "class_declaration", // Kotlin interfaces use class_declaration
            enum_def: "",                   // Kotlin enum class uses class_declaration
            class_decl: "class_declaration",
            struct_def: "", // Kotlin has no structs
        }),
        Language::Swift => Some(TypeNodeTypes {
            type_alias: "typealias_declaration",
            interface: "protocol_declaration",
            enum_def: "class_declaration", // Swift enums use class_declaration with "enum" keyword
            class_decl: "class_declaration",
            struct_def: "", // Swift structs also use class_declaration
        }),
        Language::Json | Language::Yaml | Language::Toml => None,
    }
}
