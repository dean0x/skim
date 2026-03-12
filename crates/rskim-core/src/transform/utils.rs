//! Shared utility functions for transformation modules
//!
//! ARCHITECTURE: Common helpers used across multiple transformation modes.

use crate::Language;
use tree_sitter::Node;

/// Check if a node is inside a function/method body
///
/// Walks up the AST via parent nodes looking for body/block nodes or
/// function definition nodes. Comments inside function bodies should be
/// preserved in minimal mode.
///
/// NOTE: In tree-sitter-python, comments inside function bodies are children
/// of `function_definition`, not of `block`. We must also check for function
/// definition ancestors to handle this correctly.
///
/// # Arguments
/// * `node` - The AST node to check
/// * `language` - The language (determines which node types are "body" nodes)
pub(crate) fn is_inside_function_body(node: Node, language: Language) -> bool {
    let body_kinds = get_body_node_kinds(language);
    let fn_kinds = get_function_node_kinds(language);
    let mut current = node.parent();
    let mut depth = 0;
    const MAX_PARENT_WALK: usize = 500;

    while let Some(parent) = current {
        depth += 1;
        if depth > MAX_PARENT_WALK {
            return false;
        }
        let kind = parent.kind();
        if body_kinds.contains(&kind) {
            return true;
        }
        // In some grammars (Python), comments are children of the function
        // definition itself, not of the body block node. Check if any
        // ancestor is a function definition.
        if fn_kinds.contains(&kind) {
            return true;
        }
        current = parent.parent();
    }

    false
}

/// Get the node kinds that represent function/method bodies for a language
fn get_body_node_kinds(language: Language) -> &'static [&'static str] {
    match language {
        Language::TypeScript | Language::JavaScript => &["statement_block"],
        Language::Python | Language::Rust | Language::Go => &["block"],
        Language::Java => &["block", "constructor_body"],
        Language::Markdown | Language::Json | Language::Yaml => &[],
    }
}

/// Get the node kinds that represent function/method definitions
///
/// Used to catch cases where comments are children of function definitions
/// rather than their body blocks. In tree-sitter-python, comments inside
/// function bodies are children of `function_definition`, not of `block`.
///
/// NOTE: `class_definition` is intentionally excluded — class-level
/// comments (outside methods) should still be stripped.
fn get_function_node_kinds(language: Language) -> &'static [&'static str] {
    match language {
        Language::Python => &["function_definition"],
        // Other languages correctly place comments inside body blocks,
        // so no function-level check needed.
        _ => &[],
    }
}

// ============================================================================
// Priority Scoring for AST-aware truncation
// ============================================================================

/// Score a tree-sitter node kind for truncation priority
///
/// Higher scores are kept preferentially when truncating output.
///
/// Priority levels:
/// - 5: Type definitions (type aliases, interfaces, structs, traits, enums)
/// - 4: Function/method declarations and signatures
/// - 3: Import statements and use declarations
/// - 2: Class/module/impl declarations (containers)
/// - 1: Everything else (bodies, expressions, etc.)
pub(crate) fn score_node_kind(kind: &str) -> u8 {
    match kind {
        // Priority 5: Type definitions
        "type_alias_declaration"
        | "interface_declaration"
        | "struct_item"
        | "trait_item"
        | "enum_item"
        | "enum_declaration"
        | "struct_specifier"
        | "enum_specifier"
        | "type_definition"
        | "type_item"
        | "type_alias_statement"
        | "type_declaration"
        | "atx_heading"
        | "setext_heading" => 5,

        // Priority 4: Function/method declarations
        "function_declaration"
        | "function_item"
        | "method_declaration"
        | "function_definition"
        | "method_definition"
        | "declaration"
        | "template_declaration"
        | "arrow_function"
        | "function_expression" => 4,

        // Priority 3: Import statements
        "import_statement" | "use_declaration" | "import_declaration" | "preproc_include"
        | "export_statement" | "use_item" => 3,

        // Priority 2: Class/module/impl containers
        "class_declaration"
        | "module_declaration"
        | "impl_item"
        | "class_definition"
        | "class_specifier"
        | "namespace_definition"
        | "interface_type"
        | "struct_type" => 2,

        // Priority 1: Everything else
        _ => 1,
    }
}

/// Get the single-line comment prefix for a language
///
/// Used to generate omission markers in the correct comment syntax.
pub(crate) fn get_comment_prefix(language: Language) -> &'static str {
    match language {
        Language::TypeScript
        | Language::JavaScript
        | Language::Rust
        | Language::Go
        | Language::Java => "//",
        Language::Python => "#",
        Language::Markdown => "<!--",
        Language::Json | Language::Yaml => "//",
    }
}

/// Get the comment suffix for a language (empty for most, closing tag for Markdown)
pub(crate) fn get_comment_suffix(language: Language) -> &'static str {
    match language {
        Language::Markdown => " -->",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_node_kind_priority_5() {
        assert_eq!(score_node_kind("type_alias_declaration"), 5);
        assert_eq!(score_node_kind("interface_declaration"), 5);
        assert_eq!(score_node_kind("struct_item"), 5);
        assert_eq!(score_node_kind("trait_item"), 5);
        assert_eq!(score_node_kind("enum_item"), 5);
        assert_eq!(score_node_kind("atx_heading"), 5);
    }

    #[test]
    fn test_score_node_kind_priority_4() {
        assert_eq!(score_node_kind("function_declaration"), 4);
        assert_eq!(score_node_kind("function_item"), 4);
        assert_eq!(score_node_kind("method_declaration"), 4);
        assert_eq!(score_node_kind("function_definition"), 4);
    }

    #[test]
    fn test_score_node_kind_priority_3() {
        assert_eq!(score_node_kind("import_statement"), 3);
        assert_eq!(score_node_kind("use_declaration"), 3);
        assert_eq!(score_node_kind("import_declaration"), 3);
    }

    #[test]
    fn test_score_node_kind_priority_2() {
        assert_eq!(score_node_kind("class_declaration"), 2);
        assert_eq!(score_node_kind("impl_item"), 2);
        assert_eq!(score_node_kind("class_definition"), 2);
    }

    #[test]
    fn test_score_node_kind_priority_1_default() {
        assert_eq!(score_node_kind("source_file"), 1);
        assert_eq!(score_node_kind("expression_statement"), 1);
        assert_eq!(score_node_kind("unknown_node"), 1);
    }

    #[test]
    fn test_comment_prefix() {
        assert_eq!(get_comment_prefix(Language::TypeScript), "//");
        assert_eq!(get_comment_prefix(Language::JavaScript), "//");
        assert_eq!(get_comment_prefix(Language::Rust), "//");
        assert_eq!(get_comment_prefix(Language::Go), "//");
        assert_eq!(get_comment_prefix(Language::Java), "//");
        assert_eq!(get_comment_prefix(Language::Python), "#");
        assert_eq!(get_comment_prefix(Language::Markdown), "<!--");
    }

    #[test]
    fn test_comment_suffix() {
        assert_eq!(get_comment_suffix(Language::TypeScript), "");
        assert_eq!(get_comment_suffix(Language::Python), "");
        assert_eq!(get_comment_suffix(Language::Markdown), " -->");
    }
}
