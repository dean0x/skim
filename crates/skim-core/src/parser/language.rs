//! Language-specific parsing logic
//!
//! ARCHITECTURE: Language detection and grammar loading

use crate::Language;

/// Get tree-sitter node types for a language
///
/// Different languages have different AST node types:
/// - TypeScript: "function_declaration", "class_declaration"
/// - Python: "function_definition", "class_definition"
/// - Rust: "function_item", "struct_item"
///
/// This function returns language-specific node type mappings.
pub(crate) fn get_node_types(language: Language) -> LanguageNodeTypes {
    match language {
        Language::TypeScript | Language::JavaScript => LanguageNodeTypes {
            function: "function_declaration",
            class: "class_declaration",
            interface: "interface_declaration",
            type_alias: "type_alias_declaration",
        },
        Language::Python => LanguageNodeTypes {
            function: "function_definition",
            class: "class_definition",
            interface: "", // Python has no interfaces
            type_alias: "type_alias_statement",
        },
        Language::Rust => LanguageNodeTypes {
            function: "function_item",
            class: "struct_item", // Closest equivalent
            interface: "trait_item",
            type_alias: "type_item",
        },
        Language::Go => LanguageNodeTypes {
            function: "function_declaration",
            class: "type_declaration", // Go has no classes
            interface: "interface_type",
            type_alias: "type_alias",
        },
        Language::Java => LanguageNodeTypes {
            function: "method_declaration",
            class: "class_declaration",
            interface: "interface_declaration",
            type_alias: "", // Java has no type aliases (pre-generics)
        },
    }
}

/// Node type mappings for a language
#[derive(Debug)]
pub(crate) struct LanguageNodeTypes {
    pub function: &'static str,
    pub class: &'static str,
    pub interface: &'static str,
    pub type_alias: &'static str,
}
