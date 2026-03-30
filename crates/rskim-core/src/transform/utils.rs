//! Shared utility functions for transformation modules
//!
//! ARCHITECTURE: Common helpers used across multiple transformation modes.

use crate::Language;
use tree_sitter::Node;

// ============================================================================
// Shared Node Type Structs
// ============================================================================

/// Node type mapping for function/method identification
///
/// Used by both structure mode (to replace function bodies) and signatures mode
/// (to extract function signatures). The struct shape is identical for both modes,
/// but factory functions produce intentionally different values per mode — e.g.,
/// signatures mode omits node kinds that have no extractable signature
/// (anonymous_initializer, deinit_declaration).
pub(crate) struct FunctionNodeTypes {
    pub(crate) function: &'static str,
    pub(crate) method: &'static str,
    /// Extra node kinds that behave like functions (e.g., Swift init/deinit, Kotlin constructors).
    /// Language-specific kinds are data-driven, not hardcoded in match logic.
    pub(crate) extra_function_kinds: &'static [&'static str],
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
        Language::C | Language::Cpp => &["compound_statement"],
        Language::CSharp => &["block"],
        Language::Ruby => &["body_statement"],
        Language::Sql => &[], // SQL has no function bodies
        Language::Kotlin => &["function_body", "block"],
        Language::Swift => &["function_body"],
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml => &[],
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

/// Find the body/block child of a function/method node
///
/// Walks immediate children looking for body-like node kinds that represent
/// a function/method body. Used by both structure mode (to replace bodies)
/// and signatures mode (to extract text before the body).
///
/// ARCHITECTURE: The matched kinds here must be the union of all body kinds
/// from `get_body_node_kinds()`. This is the single source of truth for
/// "what is a body child node" when walking DOWN from a function node.
/// `get_body_node_kinds()` is used for walking UP (checking ancestry).
pub(crate) fn find_body_child(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "statement_block" | "block" | "compound_statement" | "constructor_body"
            | "body_statement" | "function_body" => return Some(child),
            _ => continue,
        }
    }
    None
}

// ============================================================================
// Priority Scoring for AST-aware truncation
// ============================================================================

/// Single source of truth for node kind mapping and priority scoring
///
/// Returns `(static_str, priority)` for any tree-sitter node kind string.
///
/// ARCHITECTURE: tree-sitter node kind strings have static lifetime tied to the
/// grammar, but Rust can't prove this to the borrow checker. We map known kinds
/// to static strings. Unknown kinds get `("unknown", 1)` (lowest priority).
///
/// ARCHITECTURE: This is a flat mapping (no language parameter) by design. Some
/// entries like "call" and "statement" have generic names that appear in multiple
/// grammars, but tree-sitter supertypes (abstract nodes) never appear as
/// `node.kind()` — only their concrete subtypes do. Additionally, the truncation
/// scoring path only operates on top-level NodeSpans built from `root.children()`,
/// further limiting which node kinds are actually scored. See inline comments on
/// "call" and "statement" entries for cross-grammar safety analysis.
///
/// ARCHITECTURE: Linear growth with ~1 line per match arm is acceptable here.
/// Each arm is compile-time verified (typos cause "unreachable pattern" warnings),
/// and the match compiles to a jump table — faster than a HashMap at runtime.
/// Adding a new language requires ~3-5 new arms, bounded by the number of unique
/// node kinds that language introduces.
///
/// Priority levels:
/// - 5: Type definitions (type aliases, interfaces, structs, traits, enums,
///   Python class_definition — Python classes ARE the type system)
/// - 4: Function/method declarations and signatures
/// - 3: Import statements and use declarations
/// - 2: Class/module/impl containers (TS/JS class_declaration, Java class_declaration)
/// - 1: Everything else (bodies, expressions, etc.)
pub(crate) fn node_kind_info(kind: &str) -> (&'static str, u8) {
    match kind {
        // Priority 5: Type definitions
        "type_alias_declaration" => ("type_alias_declaration", 5),
        "interface_declaration" => ("interface_declaration", 5),
        "struct_item" => ("struct_item", 5),
        "trait_item" => ("trait_item", 5),
        "enum_item" => ("enum_item", 5),
        "enum_declaration" => ("enum_declaration", 5),
        "struct_specifier" => ("struct_specifier", 5),
        "enum_specifier" => ("enum_specifier", 5),
        "type_definition" => ("type_definition", 5),
        "type_item" => ("type_item", 5),
        "type_alias_statement" => ("type_alias_statement", 5),
        "type_declaration" => ("type_declaration", 5),
        "using_declaration" => ("using_declaration", 5), // C++ using type aliases
        "alias_declaration" => ("alias_declaration", 5), // C++ `using Alias = Type;`
        "class_definition" => ("class_definition", 5),   // Python: classes ARE the type system
        "struct_declaration" => ("struct_declaration", 5), // C# struct
        "create_table" => ("create_table", 5),           // SQL: tables ARE the type system
        "type_alias" => ("type_alias", 5),               // Kotlin type alias
        "object_declaration" => ("object_declaration", 5), // Kotlin object/singleton
        "typealias_declaration" => ("typealias_declaration", 5), // Swift typealias
        "protocol_declaration" => ("protocol_declaration", 5), // Swift protocol
        "atx_heading" => ("atx_heading", 5),
        "setext_heading" => ("setext_heading", 5),

        // Priority 4: Function/method declarations
        "function_declaration" => ("function_declaration", 4),
        "function_item" => ("function_item", 4),
        "method_declaration" => ("method_declaration", 4),
        "function_definition" => ("function_definition", 4),
        "method_definition" => ("method_definition", 4),
        "declaration" => ("declaration", 4),
        "template_declaration" => ("template_declaration", 4),
        "arrow_function" => ("arrow_function", 4),
        "function_expression" => ("function_expression", 4),
        "constructor_declaration" => ("constructor_declaration", 4), // C# constructor
        "method" => ("method", 4),                                   // Ruby method
        "singleton_method" => ("singleton_method", 4),               // Ruby class method
        "init_declaration" => ("init_declaration", 4),               // Swift init
        "deinit_declaration" => ("deinit_declaration", 4),           // Swift deinit
        "secondary_constructor" => ("secondary_constructor", 4),     // Kotlin constructor
        "anonymous_initializer" => ("anonymous_initializer", 4),     // Kotlin init block

        // Priority 3: Import statements
        "import_statement" => ("import_statement", 3),
        "use_declaration" => ("use_declaration", 3),
        "import_declaration" => ("import_declaration", 3),
        "preproc_include" => ("preproc_include", 3),
        "export_statement" => ("export_statement", 3),
        "use_item" => ("use_item", 3),
        "using_directive" => ("using_directive", 3), // C# using statements
        // ARCHITECTURE: "call" is a concrete node in both Ruby and Python grammars.
        // Safe at priority 3 because: (1) In Ruby, top-level `require` calls resolve
        // to "call" nodes — correct import-level priority. (2) In Python, top-level
        // calls appear as `expression_statement` > `call` — the parent
        // `expression_statement` is the top-level span, not "call" itself. The
        // truncation path only scores top-level NodeSpans from root.children().
        "call" => ("call", 3),
        "import" => ("import", 3),                   // Kotlin import
        "package_header" => ("package_header", 3),   // Kotlin package declaration

        // Priority 2: Class/module/impl containers
        "class_declaration" => ("class_declaration", 2),
        "module_declaration" => ("module_declaration", 2),
        "impl_item" => ("impl_item", 2),
        "class_specifier" => ("class_specifier", 2),
        "namespace_definition" => ("namespace_definition", 2),
        "namespace_declaration" => ("namespace_declaration", 2), // C# namespace
        "interface_type" => ("interface_type", 2),
        "struct_type" => ("struct_type", 2),
        "class" => ("class", 2),         // Ruby class
        "module" => ("module", 2),       // Ruby module
        // ARCHITECTURE: "statement" is a SUPERTYPE (abstract) in TypeScript,
        // JavaScript, C, C++, Java, and Kotlin — tree-sitter resolves these to
        // concrete subtypes (e.g., "expression_statement", "break_statement"),
        // so "statement" never appears as node.kind() for those languages.
        // Only SQL's tree-sitter-sequel grammar defines "statement" as a concrete
        // node, making this entry effectively SQL-scoped.
        "statement" => ("statement", 2),

        // Priority 1: Known but low-priority kinds
        "program" => ("program", 1),
        "source_file" => ("source_file", 1),
        "expression_statement" => ("expression_statement", 1),
        "lexical_declaration" => ("lexical_declaration", 1),
        "variable_declaration" => ("variable_declaration", 1),
        "comment" => ("comment", 1),
        "line_comment" => ("line_comment", 1),
        "block_comment" => ("block_comment", 1),

        // Unknown kinds
        _ => ("unknown", 1),
    }
}

/// Map a tree-sitter node kind string to a static &str for use in NodeSpan
///
/// Wrapper around `node_kind_info()` — returns only the static string.
pub(crate) fn to_static_node_kind(kind: &str) -> &'static str {
    node_kind_info(kind).0
}

/// Score a tree-sitter node kind for truncation priority
///
/// Wrapper around `node_kind_info()` — returns only the priority score.
/// Higher scores are kept preferentially when truncating output.
pub(crate) fn score_node_kind(kind: &str) -> u8 {
    node_kind_info(kind).1
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
        | Language::Java
        | Language::C
        | Language::Cpp
        | Language::CSharp
        | Language::Kotlin
        | Language::Swift => "//",
        Language::Python | Language::Ruby => "#",
        Language::Sql => "--",
        Language::Markdown => "<!--",
        Language::Json => "//", // JSON has no comments; // is JSONC-compatible
        Language::Yaml => "#",
        Language::Toml => "#",
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
        assert_eq!(score_node_kind("class_definition"), 5); // Python classes = type system
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
    }

    #[test]
    fn test_score_node_kind_priority_1_default() {
        assert_eq!(score_node_kind("source_file"), 1);
        assert_eq!(score_node_kind("expression_statement"), 1);
        assert_eq!(score_node_kind("unknown_node"), 1);
    }

    #[test]
    fn test_node_kind_info_consistency() {
        // For every known kind, verify:
        // 1. to_static_node_kind returns the kind itself (not "unknown")
        // 2. score_node_kind(to_static_node_kind(kind)) == score_node_kind(kind)
        //    (scoring is idempotent through the mapping)
        let known_kinds = [
            // Priority 5
            "type_alias_declaration",
            "interface_declaration",
            "struct_item",
            "trait_item",
            "enum_item",
            "enum_declaration",
            "struct_specifier",
            "enum_specifier",
            "type_definition",
            "type_item",
            "type_alias_statement",
            "type_declaration",
            "using_declaration",
            "alias_declaration",
            "class_definition",
            "struct_declaration",
            "create_table",
            "type_alias",
            "object_declaration",
            "typealias_declaration",
            "protocol_declaration",
            "atx_heading",
            "setext_heading",
            // Priority 4
            "function_declaration",
            "function_item",
            "method_declaration",
            "function_definition",
            "method_definition",
            "declaration",
            "template_declaration",
            "arrow_function",
            "function_expression",
            "constructor_declaration",
            "method",
            "singleton_method",
            "init_declaration",
            "deinit_declaration",
            "secondary_constructor",
            "anonymous_initializer",
            // Priority 3
            "import_statement",
            "use_declaration",
            "import_declaration",
            "import",
            "package_header",
            "preproc_include",
            "export_statement",
            "use_item",
            "using_directive",
            "call",
            // Priority 2
            "class_declaration",
            "module_declaration",
            "impl_item",
            "class_specifier",
            "namespace_definition",
            "namespace_declaration",
            "interface_type",
            "struct_type",
            "class",
            "module",
            "statement",
            // Priority 1
            "program",
            "source_file",
            "expression_statement",
            "lexical_declaration",
            "variable_declaration",
            "comment",
            "line_comment",
            "block_comment",
        ];

        for kind in &known_kinds {
            let static_str = to_static_node_kind(kind);
            assert_ne!(
                static_str, "unknown",
                "Known kind '{}' should not map to 'unknown'",
                kind
            );
            assert_eq!(
                static_str, *kind,
                "to_static_node_kind('{}') should return itself",
                kind
            );
            assert_eq!(
                score_node_kind(static_str),
                score_node_kind(kind),
                "Scoring should be idempotent through mapping for '{}'",
                kind
            );
        }
    }

    #[test]
    fn test_class_definition_is_priority_5() {
        // Python classes ARE the type system — verify class_definition gets Priority 5
        // in both the mapping and the scoring
        let (static_str, priority) = node_kind_info("class_definition");
        assert_eq!(static_str, "class_definition");
        assert_eq!(
            priority, 5,
            "class_definition should be Priority 5 (type-level)"
        );
    }

    #[test]
    fn test_comment_prefix() {
        assert_eq!(get_comment_prefix(Language::TypeScript), "//");
        assert_eq!(get_comment_prefix(Language::JavaScript), "//");
        assert_eq!(get_comment_prefix(Language::Rust), "//");
        assert_eq!(get_comment_prefix(Language::Go), "//");
        assert_eq!(get_comment_prefix(Language::Java), "//");
        assert_eq!(get_comment_prefix(Language::C), "//");
        assert_eq!(get_comment_prefix(Language::Cpp), "//");
        assert_eq!(get_comment_prefix(Language::CSharp), "//");
        assert_eq!(get_comment_prefix(Language::Kotlin), "//");
        assert_eq!(get_comment_prefix(Language::Swift), "//");
        assert_eq!(get_comment_prefix(Language::Python), "#");
        assert_eq!(get_comment_prefix(Language::Ruby), "#");
        assert_eq!(get_comment_prefix(Language::Sql), "--");
        assert_eq!(get_comment_prefix(Language::Markdown), "<!--");
        assert_eq!(get_comment_prefix(Language::Json), "//");
        assert_eq!(get_comment_prefix(Language::Yaml), "#");
        assert_eq!(get_comment_prefix(Language::Toml), "#");
    }

    #[test]
    fn test_comment_suffix() {
        assert_eq!(get_comment_suffix(Language::TypeScript), "");
        assert_eq!(get_comment_suffix(Language::JavaScript), "");
        assert_eq!(get_comment_suffix(Language::Rust), "");
        assert_eq!(get_comment_suffix(Language::Go), "");
        assert_eq!(get_comment_suffix(Language::Java), "");
        assert_eq!(get_comment_suffix(Language::C), "");
        assert_eq!(get_comment_suffix(Language::Cpp), "");
        assert_eq!(get_comment_suffix(Language::CSharp), "");
        assert_eq!(get_comment_suffix(Language::Kotlin), "");
        assert_eq!(get_comment_suffix(Language::Swift), "");
        assert_eq!(get_comment_suffix(Language::Python), "");
        assert_eq!(get_comment_suffix(Language::Ruby), "");
        assert_eq!(get_comment_suffix(Language::Sql), "");
        assert_eq!(get_comment_suffix(Language::Markdown), " -->");
        assert_eq!(get_comment_suffix(Language::Json), "");
        assert_eq!(get_comment_suffix(Language::Yaml), "");
        assert_eq!(get_comment_suffix(Language::Toml), "");
    }
}
