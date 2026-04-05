//! Data-driven field classifier for tree-sitter languages.
//!
//! Uses `FxHashMap` lookup tables (O(1)) instead of match statements.
//! Each language defines static slices of `(node_kind, SearchField)` pairs.
//! Identifiers are classified only in declaration contexts to avoid
//! indexing every variable reference.

use rustc_hash::FxHashMap;

use rskim_core::Language;

use crate::{FieldClassifier, SearchField};

// ============================================================================
// Per-language static data tables
// ============================================================================

// Each language exports two slices:
//   FIELD_MAP        — container node kind → SearchField (direct mapping)
//   DECL_PARENTS     — parent node kind → SearchField for identifier nodes

// --- TypeScript / JavaScript ---

const TS_FIELD_MAP: &[(&str, SearchField)] = &[
    ("type_alias_declaration", SearchField::TypeDefinition),
    ("interface_declaration", SearchField::TypeDefinition),
    ("enum_declaration", SearchField::TypeDefinition),
    ("class_declaration", SearchField::TypeDefinition),
    ("function_declaration", SearchField::FunctionSignature),
    ("method_definition", SearchField::FunctionSignature),
    ("arrow_function", SearchField::FunctionSignature),
    ("import_statement", SearchField::ImportExport),
    ("export_statement", SearchField::ImportExport),
    ("statement_block", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string", SearchField::StringLiteral),
    ("template_string", SearchField::StringLiteral),
];

const TS_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_declaration", SearchField::SymbolName),
    ("class_declaration", SearchField::SymbolName),
    ("type_alias_declaration", SearchField::SymbolName),
    ("variable_declarator", SearchField::SymbolName),
    ("method_definition", SearchField::SymbolName),
    ("interface_declaration", SearchField::SymbolName),
    ("enum_declaration", SearchField::SymbolName),
    ("import_specifier", SearchField::ImportExport),
];

// JavaScript shares the same tables as TypeScript.
const JS_FIELD_MAP: &[(&str, SearchField)] = TS_FIELD_MAP;
const JS_DECL_PARENTS: &[(&str, SearchField)] = TS_DECL_PARENTS;

// --- Python ---

const PY_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class_definition", SearchField::TypeDefinition),
    ("function_definition", SearchField::FunctionSignature),
    ("decorated_definition", SearchField::FunctionSignature),
    ("import_statement", SearchField::ImportExport),
    ("import_from_statement", SearchField::ImportExport),
    ("block", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string", SearchField::StringLiteral),
];

const PY_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_definition", SearchField::SymbolName),
    ("class_definition", SearchField::SymbolName),
    ("assignment", SearchField::SymbolName),
];

// --- Rust ---

const RS_FIELD_MAP: &[(&str, SearchField)] = &[
    ("struct_item", SearchField::TypeDefinition),
    ("enum_item", SearchField::TypeDefinition),
    ("type_item", SearchField::TypeDefinition),
    ("trait_item", SearchField::TypeDefinition),
    ("impl_item", SearchField::TypeDefinition),
    ("function_item", SearchField::FunctionSignature),
    ("function_signature_item", SearchField::FunctionSignature),
    ("use_declaration", SearchField::ImportExport),
    ("block", SearchField::FunctionBody),
    ("line_comment", SearchField::Comment),
    ("block_comment", SearchField::Comment),
    ("string_literal", SearchField::StringLiteral),
    ("raw_string_literal", SearchField::StringLiteral),
];

const RS_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_item", SearchField::SymbolName),
    ("struct_item", SearchField::SymbolName),
    ("enum_item", SearchField::SymbolName),
    ("trait_item", SearchField::SymbolName),
    ("type_item", SearchField::SymbolName),
    ("const_item", SearchField::SymbolName),
    ("static_item", SearchField::SymbolName),
    ("mod_item", SearchField::SymbolName),
];

// --- Go ---

const GO_FIELD_MAP: &[(&str, SearchField)] = &[
    ("type_declaration", SearchField::TypeDefinition),
    ("type_spec", SearchField::TypeDefinition),
    ("function_declaration", SearchField::FunctionSignature),
    ("method_declaration", SearchField::FunctionSignature),
    ("import_declaration", SearchField::ImportExport),
    ("block", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("interpreted_string_literal", SearchField::StringLiteral),
    ("raw_string_literal", SearchField::StringLiteral),
];

const GO_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_declaration", SearchField::SymbolName),
    ("method_declaration", SearchField::SymbolName),
    ("type_spec", SearchField::SymbolName),
    ("const_spec", SearchField::SymbolName),
    ("var_spec", SearchField::SymbolName),
];

// --- Java ---

const JAVA_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::TypeDefinition),
    ("interface_declaration", SearchField::TypeDefinition),
    ("enum_declaration", SearchField::TypeDefinition),
    ("annotation_type_declaration", SearchField::TypeDefinition),
    ("method_declaration", SearchField::FunctionSignature),
    ("constructor_declaration", SearchField::FunctionSignature),
    ("import_declaration", SearchField::ImportExport),
    ("block", SearchField::FunctionBody),
    ("line_comment", SearchField::Comment),
    ("block_comment", SearchField::Comment),
    ("string_literal", SearchField::StringLiteral),
];

const JAVA_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::SymbolName),
    ("method_declaration", SearchField::SymbolName),
    ("interface_declaration", SearchField::SymbolName),
    ("enum_declaration", SearchField::SymbolName),
    ("variable_declarator", SearchField::SymbolName),
];

// --- C ---

const C_FIELD_MAP: &[(&str, SearchField)] = &[
    ("struct_specifier", SearchField::TypeDefinition),
    ("enum_specifier", SearchField::TypeDefinition),
    ("type_definition", SearchField::TypeDefinition),
    ("union_specifier", SearchField::TypeDefinition),
    ("function_definition", SearchField::FunctionSignature),
    ("declaration", SearchField::FunctionSignature),
    ("preproc_include", SearchField::ImportExport),
    ("compound_statement", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string_literal", SearchField::StringLiteral),
];

const C_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_definition", SearchField::SymbolName),
    ("function_declarator", SearchField::SymbolName),
    ("struct_specifier", SearchField::SymbolName),
    ("enum_specifier", SearchField::SymbolName),
    ("type_definition", SearchField::SymbolName),
    ("init_declarator", SearchField::SymbolName),
];

// --- C++ ---

const CPP_FIELD_MAP: &[(&str, SearchField)] = &[
    ("struct_specifier", SearchField::TypeDefinition),
    ("enum_specifier", SearchField::TypeDefinition),
    ("type_definition", SearchField::TypeDefinition),
    ("union_specifier", SearchField::TypeDefinition),
    ("class_specifier", SearchField::TypeDefinition),
    ("namespace_definition", SearchField::TypeDefinition),
    ("template_declaration", SearchField::TypeDefinition),
    ("function_definition", SearchField::FunctionSignature),
    ("declaration", SearchField::FunctionSignature),
    ("preproc_include", SearchField::ImportExport),
    ("using_declaration", SearchField::ImportExport),
    ("compound_statement", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string_literal", SearchField::StringLiteral),
];

const CPP_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_definition", SearchField::SymbolName),
    ("function_declarator", SearchField::SymbolName),
    ("struct_specifier", SearchField::SymbolName),
    ("enum_specifier", SearchField::SymbolName),
    ("type_definition", SearchField::SymbolName),
    ("init_declarator", SearchField::SymbolName),
    ("class_specifier", SearchField::SymbolName),
    ("namespace_definition", SearchField::SymbolName),
];

// --- C# ---

const CS_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::TypeDefinition),
    ("struct_declaration", SearchField::TypeDefinition),
    ("interface_declaration", SearchField::TypeDefinition),
    ("enum_declaration", SearchField::TypeDefinition),
    ("method_declaration", SearchField::FunctionSignature),
    ("constructor_declaration", SearchField::FunctionSignature),
    ("using_directive", SearchField::ImportExport),
    ("block", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string_literal", SearchField::StringLiteral),
    ("verbatim_string_literal", SearchField::StringLiteral),
];

const CS_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::SymbolName),
    ("struct_declaration", SearchField::SymbolName),
    ("method_declaration", SearchField::SymbolName),
    ("interface_declaration", SearchField::SymbolName),
    ("variable_declarator", SearchField::SymbolName),
];

// --- Ruby ---

const RB_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class", SearchField::TypeDefinition),
    ("module", SearchField::TypeDefinition),
    ("method", SearchField::FunctionSignature),
    ("singleton_method", SearchField::FunctionSignature),
    ("call", SearchField::ImportExport), // require / include
    ("body_statement", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string", SearchField::StringLiteral),
];

const RB_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("class", SearchField::SymbolName),
    ("module", SearchField::SymbolName),
    ("method", SearchField::SymbolName),
    ("assignment", SearchField::SymbolName),
];

// --- Kotlin ---

const KT_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::TypeDefinition),
    ("object_declaration", SearchField::TypeDefinition),
    ("type_alias", SearchField::TypeDefinition),
    ("function_declaration", SearchField::FunctionSignature),
    ("import_header", SearchField::ImportExport),
    ("function_body", SearchField::FunctionBody),
    ("line_comment", SearchField::Comment),
    ("multiline_comment", SearchField::Comment),
    ("string_literal", SearchField::StringLiteral),
];

const KT_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_declaration", SearchField::SymbolName),
    ("class_declaration", SearchField::SymbolName),
    ("object_declaration", SearchField::SymbolName),
    ("property_declaration", SearchField::SymbolName),
];

// --- Swift ---

const SWIFT_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::TypeDefinition),
    ("struct_declaration", SearchField::TypeDefinition),
    ("protocol_declaration", SearchField::TypeDefinition),
    ("enum_declaration", SearchField::TypeDefinition),
    ("function_declaration", SearchField::FunctionSignature),
    ("init_declaration", SearchField::FunctionSignature),
    ("import_declaration", SearchField::ImportExport),
    ("code_block", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("multiline_comment", SearchField::Comment),
    ("line_string_literal", SearchField::StringLiteral),
];

const SWIFT_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_declaration", SearchField::SymbolName),
    ("class_declaration", SearchField::SymbolName),
    ("struct_declaration", SearchField::SymbolName),
    ("protocol_declaration", SearchField::SymbolName),
    ("enum_declaration", SearchField::SymbolName),
];

// --- SQL ---

const SQL_FIELD_MAP: &[(&str, SearchField)] = &[
    ("create_table", SearchField::TypeDefinition),
    ("create_view", SearchField::TypeDefinition),
    ("create_function", SearchField::FunctionSignature),
    ("create_procedure", SearchField::FunctionSignature),
    ("select", SearchField::FunctionBody),
    ("insert", SearchField::FunctionBody),
    ("update", SearchField::FunctionBody),
    ("delete", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string", SearchField::StringLiteral),
];

const SQL_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("create_table", SearchField::SymbolName),
    ("create_function", SearchField::SymbolName),
    ("column_definition", SearchField::SymbolName),
];

// ============================================================================
// Classifier implementation
// ============================================================================

/// Data-driven field classifier using per-language lookup tables.
///
/// Two-level classification:
/// 1. Container nodes (function_declaration, struct_item, etc.) → direct field_map lookup
/// 2. Identifier nodes → parent-context lookup via declaration_parents table
pub struct TreeSitterClassifier {
    /// Direct node-kind → SearchField mapping for container nodes.
    field_map: FxHashMap<&'static str, SearchField>,
    /// Parent-kind → SearchField for identifier nodes in declaration contexts.
    declaration_parents: FxHashMap<&'static str, SearchField>,
}

impl TreeSitterClassifier {
    /// Create a classifier for the given language.
    ///
    /// Returns `None` for serde-based languages (JSON, YAML, TOML) and Markdown.
    pub fn for_language(language: Language) -> Option<Self> {
        #[allow(clippy::type_complexity)]
        let (field_pairs, parent_pairs): (&[(&str, SearchField)], &[(&str, SearchField)]) =
            match language {
                Language::Json | Language::Yaml | Language::Toml | Language::Markdown => {
                    return None;
                }
                Language::TypeScript => (TS_FIELD_MAP, TS_DECL_PARENTS),
                Language::JavaScript => (JS_FIELD_MAP, JS_DECL_PARENTS),
                Language::Python => (PY_FIELD_MAP, PY_DECL_PARENTS),
                Language::Rust => (RS_FIELD_MAP, RS_DECL_PARENTS),
                Language::Go => (GO_FIELD_MAP, GO_DECL_PARENTS),
                Language::Java => (JAVA_FIELD_MAP, JAVA_DECL_PARENTS),
                Language::C => (C_FIELD_MAP, C_DECL_PARENTS),
                Language::Cpp => (CPP_FIELD_MAP, CPP_DECL_PARENTS),
                Language::CSharp => (CS_FIELD_MAP, CS_DECL_PARENTS),
                Language::Ruby => (RB_FIELD_MAP, RB_DECL_PARENTS),
                Language::Kotlin => (KT_FIELD_MAP, KT_DECL_PARENTS),
                Language::Swift => (SWIFT_FIELD_MAP, SWIFT_DECL_PARENTS),
                Language::Sql => (SQL_FIELD_MAP, SQL_DECL_PARENTS),
            };

        let field_map = field_pairs.iter().copied().collect();
        let declaration_parents = parent_pairs.iter().copied().collect();

        Some(Self {
            field_map,
            declaration_parents,
        })
    }
}

impl FieldClassifier for TreeSitterClassifier {
    /// Classify a tree-sitter node into a search field.
    ///
    /// Classification logic:
    /// 1. Check `field_map` for a direct node kind match → return if found.
    /// 2. If node kind is `"identifier"`, `"type_identifier"`, or `"property_identifier"`:
    ///    - Check parent node's kind against `declaration_parents`.
    ///    - Only classify identifiers in declaration contexts.
    /// 3. Otherwise return `None`.
    fn classify_node(
        &self,
        node: &tree_sitter::Node<'_>,
        _source: &str,
    ) -> Option<SearchField> {
        let kind = node.kind();

        // Step 1: direct container node lookup.
        if let Some(&field) = self.field_map.get(kind) {
            return Some(field);
        }

        // Step 2: identifier in declaration context.
        if matches!(kind, "identifier" | "type_identifier" | "property_identifier") {
            if let Some(parent) = node.parent() {
                if let Some(&field) = self.declaration_parents.get(parent.kind()) {
                    return Some(field);
                }
            }
        }

        None
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_languages_return_none() {
        assert!(TreeSitterClassifier::for_language(Language::Json).is_none());
        assert!(TreeSitterClassifier::for_language(Language::Yaml).is_none());
        assert!(TreeSitterClassifier::for_language(Language::Toml).is_none());
        assert!(TreeSitterClassifier::for_language(Language::Markdown).is_none());
    }

    #[test]
    fn tree_sitter_languages_return_some() {
        let langs = [
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Rust,
            Language::Go,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::CSharp,
            Language::Ruby,
            Language::Kotlin,
            Language::Swift,
            Language::Sql,
        ];
        for lang in langs {
            assert!(
                TreeSitterClassifier::for_language(lang).is_some(),
                "Expected Some for {lang:?}"
            );
        }
    }

    #[test]
    fn rust_field_map_contains_expected_entries() {
        let classifier = TreeSitterClassifier::for_language(Language::Rust)
            .expect("Rust classifier must exist");
        assert_eq!(
            classifier.field_map.get("struct_item"),
            Some(&SearchField::TypeDefinition)
        );
        assert_eq!(
            classifier.field_map.get("function_item"),
            Some(&SearchField::FunctionSignature)
        );
        assert_eq!(
            classifier.field_map.get("use_declaration"),
            Some(&SearchField::ImportExport)
        );
    }

    #[test]
    fn typescript_declaration_parents_contains_expected_entries() {
        let classifier = TreeSitterClassifier::for_language(Language::TypeScript)
            .expect("TypeScript classifier must exist");
        assert_eq!(
            classifier.declaration_parents.get("function_declaration"),
            Some(&SearchField::SymbolName)
        );
        assert_eq!(
            classifier.declaration_parents.get("import_specifier"),
            Some(&SearchField::ImportExport)
        );
    }
}
