//! Language-specific parsing logic
//!
//! ARCHITECTURE: Language detection and grammar loading

use crate::Language;

/// Get tree-sitter node types for a language
#[allow(dead_code)]
///
/// Different languages have different AST node types:
/// - TypeScript: "function_declaration", "class_declaration"
/// - Python: "function_definition", "class_definition"
/// - Rust: "function_item", "struct_item"
/// - Markdown: "atx_heading", "setext_heading" (headers instead of functions)
///
/// This function returns language-specific node type mappings.
/// Returns None for languages that don't use tree-sitter (e.g., JSON).
pub(crate) fn get_node_types(language: Language) -> Option<LanguageNodeTypes> {
    match language {
        Language::TypeScript | Language::JavaScript => Some(LanguageNodeTypes {
            function: "function_declaration",
            class: "class_declaration",
            interface: "interface_declaration",
            type_alias: "type_alias_declaration",
        }),
        Language::Python => Some(LanguageNodeTypes {
            function: "function_definition",
            class: "class_definition",
            interface: "", // Python has no interfaces
            type_alias: "type_alias_statement",
        }),
        Language::Rust => Some(LanguageNodeTypes {
            function: "function_item",
            class: "struct_item", // Closest equivalent
            interface: "trait_item",
            type_alias: "type_item",
        }),
        Language::Go => Some(LanguageNodeTypes {
            function: "function_declaration",
            class: "type_declaration", // Go has no classes
            interface: "interface_type",
            type_alias: "type_alias",
        }),
        Language::Java => Some(LanguageNodeTypes {
            function: "method_declaration",
            class: "class_declaration",
            interface: "interface_declaration",
            type_alias: "", // Java has no type aliases (pre-generics)
        }),
        Language::Markdown => Some(LanguageNodeTypes {
            function: "atx_heading", // Headers are document structure (like functions in code)
            class: "setext_heading", // Alternative header syntax
            interface: "",           // N/A for markdown
            type_alias: "",          // N/A for markdown
        }),
        // ARCHITECTURE: JSON uses serde_json parser, not tree-sitter.
        // This is enforced by the Strategy Pattern in Language::transform_source().
        Language::Json => None,
    }
}

/// Node type mappings for a language
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct LanguageNodeTypes {
    pub function: &'static str,
    pub class: &'static str,
    pub interface: &'static str,
    pub type_alias: &'static str,
}
