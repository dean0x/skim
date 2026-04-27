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
    let (text, spans, _line_map) = transform_types_with_spans_and_line_map(source, tree, language)?;
    Ok((text, spans))
}

/// Transform to types-only, returning NodeSpan metadata AND a source line map.
///
/// The source line map maps each output line index to the 1-indexed source line
/// where that type definition begins. Separator blank lines between definitions
/// get source line 0 (no annotation).
///
/// # Design Decision (AC-18)
/// Types mode uses `node.start_position().row + 1` to annotate each type
/// definition with its source line. Multi-line definitions get consecutive
/// source line numbers from their start line. Blank separator lines get 0.
pub(crate) fn transform_types_with_spans_and_line_map(
    source: &str,
    tree: &Tree,
    language: Language,
) -> Result<(String, Vec<NodeSpan>, Vec<usize>)> {
    // ARCHITECTURE: Markdown types mode extracts ALL headers (H1-H6)
    if language == Language::Markdown {
        let (text, spans) = extract_markdown_headers_with_spans(source, tree, 1, 6)?;
        let line_count = text.lines().count();
        let line_map = (1..=line_count).collect();
        return Ok((text, spans, line_map));
    }

    // ARCHITECTURE: JSON is handled by Strategy Pattern in Language::transform_source()
    // and never reaches this code path.
    let node_types = get_type_node_types(language).ok_or_else(|| {
        SkimError::ParseError(format!(
            "Language {:?} does not support tree-sitter type transformation",
            language
        ))
    })?;

    let mut type_defs: Vec<(String, &'static str, usize)> = Vec::new();
    collect_type_definitions_with_kinds_and_lines(
        tree.root_node(),
        source,
        &node_types,
        &mut type_defs,
        0,
    )?;

    // Check type definition count limit
    if type_defs.len() > MAX_TYPE_DEFS {
        return Err(SkimError::ParseError(format!(
            "Too many type definitions: {} (max: {}). Possible malicious input.",
            type_defs.len(),
            MAX_TYPE_DEFS
        )));
    }

    // Build text, spans, and source line map
    // Types mode joins with \n\n (two newlines between defs)
    let type_defs_count = type_defs.len();
    let mut spans = Vec::with_capacity(type_defs_count);
    let mut source_line_map: Vec<usize> = Vec::new();
    let mut current_output_line = 0;

    let texts: Vec<String> = type_defs
        .into_iter()
        .enumerate()
        .map(|(idx, (def, kind, source_start_line))| {
            let line_count = def.lines().count().max(1);
            spans.push(NodeSpan::new(
                current_output_line..current_output_line + line_count,
                kind,
            ));
            // Map each output line to consecutive source lines from source_start_line
            for i in 0..line_count {
                source_line_map.push(source_start_line + i);
            }
            current_output_line += line_count;
            // Account for the blank line separator between defs (\n\n → 1 extra blank line)
            if idx < type_defs_count - 1 {
                source_line_map.push(0); // blank separator line → no annotation
                current_output_line += 1;
            }
            def
        })
        .collect();

    Ok((texts.join("\n\n"), spans, source_line_map))
}

/// Recursively collect type definitions with node kind AND source start line.
///
/// The source start line is `node.start_position().row + 1` (1-indexed).
fn collect_type_definitions_with_kinds_and_lines(
    node: Node,
    source: &str,
    node_types: &TypeNodeTypes,
    type_defs: &mut Vec<(String, &'static str, usize)>,
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
            // 1-indexed source line where this type definition starts
            let source_start_line = node.start_position().row + 1;
            type_defs.push((type_def, static_kind, source_start_line));
        }
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_type_definitions_with_kinds_and_lines(
            child,
            source,
            node_types,
            type_defs,
            depth + 1,
        )?;
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
        // ARCHITECTURE: tree-sitter-kotlin uses class_declaration for all class-like
        // constructs (class, interface, data class, sealed class, enum class). There is
        // no grammar-level distinction, so interface and class_decl map to the same kind.
        Language::Kotlin => Some(TypeNodeTypes {
            type_alias: "type_alias",
            interface: "class_declaration",
            enum_def: "",
            class_decl: "class_declaration",
            struct_def: "",
        }),
        // ARCHITECTURE: tree-sitter-swift uses class_declaration for struct, class, and
        // enum declarations. Only protocol_declaration is a distinct grammar node.
        // This means enum_def and struct_def overlap with class_decl — callers should
        // expect duplicate matches when querying multiple fields.
        Language::Swift => Some(TypeNodeTypes {
            type_alias: "typealias_declaration",
            interface: "protocol_declaration",
            enum_def: "class_declaration",
            class_decl: "class_declaration",
            struct_def: "",
        }),
        Language::Json | Language::Yaml | Language::Toml => None,
    }
}
