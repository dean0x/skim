//! Shared utility functions for transformation modules
//!
//! ARCHITECTURE: Common helpers used across multiple transformation modes.

use crate::Language;
use tree_sitter::Node;

/// Get the single-line comment prefix for a language
///
/// Returns the character(s) that start a single-line comment in the given language.
///
/// # Examples
/// ```
/// use rskim_core::get_comment_prefix;
/// use rskim_core::Language;
///
/// assert_eq!(get_comment_prefix(Language::TypeScript), "//");
/// assert_eq!(get_comment_prefix(Language::Python), "#");
/// ```
pub fn get_comment_prefix(language: Language) -> &'static str {
    match language {
        Language::Python => "#",
        Language::Markdown => "<!--",
        _ => "//",
    }
}

/// Get the comment suffix for a language (non-empty only for Markdown)
///
/// # Examples
/// ```
/// use rskim_core::get_comment_suffix;
/// use rskim_core::Language;
///
/// assert_eq!(get_comment_suffix(Language::Markdown), " -->");
/// assert_eq!(get_comment_suffix(Language::TypeScript), "");
/// ```
pub fn get_comment_suffix(language: Language) -> &'static str {
    match language {
        Language::Markdown => " -->",
        _ => "",
    }
}

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

    while let Some(parent) = current {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comment_prefix() {
        assert_eq!(get_comment_prefix(Language::TypeScript), "//");
        assert_eq!(get_comment_prefix(Language::JavaScript), "//");
        assert_eq!(get_comment_prefix(Language::Python), "#");
        assert_eq!(get_comment_prefix(Language::Rust), "//");
        assert_eq!(get_comment_prefix(Language::Go), "//");
        assert_eq!(get_comment_prefix(Language::Java), "//");
        assert_eq!(get_comment_prefix(Language::Markdown), "<!--");
    }

    #[test]
    fn test_comment_suffix() {
        assert_eq!(get_comment_suffix(Language::Markdown), " -->");
        assert_eq!(get_comment_suffix(Language::TypeScript), "");
        assert_eq!(get_comment_suffix(Language::Python), "");
    }
}
