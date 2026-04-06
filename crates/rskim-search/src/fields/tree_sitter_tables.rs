//! Per-language static node-kind → `SearchField` mapping tables.
//!
//! Each language exports two slices consumed by [`TreeSitterClassifier`]:
//!   `FIELD_MAP`     — container node kind → `SearchField` (direct mapping)
//!   `DECL_PARENTS`  — parent node kind → `SearchField` for identifier nodes
//!
//! Keeping the tables here (≈300 lines of data) leaves the classifier logic
//! in `tree_sitter_fields.rs` concise and easy to navigate.

use crate::SearchField;

// ============================================================================
// TypeScript / JavaScript
// ============================================================================

pub const TS_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const TS_DECL_PARENTS: &[(&str, SearchField)] = &[
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
pub const JS_FIELD_MAP: &[(&str, SearchField)] = TS_FIELD_MAP;
pub const JS_DECL_PARENTS: &[(&str, SearchField)] = TS_DECL_PARENTS;

// ============================================================================
// Python
// ============================================================================

pub const PY_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class_definition", SearchField::TypeDefinition),
    ("function_definition", SearchField::FunctionSignature),
    ("decorated_definition", SearchField::FunctionSignature),
    ("import_statement", SearchField::ImportExport),
    ("import_from_statement", SearchField::ImportExport),
    ("block", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string", SearchField::StringLiteral),
];

pub const PY_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_definition", SearchField::SymbolName),
    ("class_definition", SearchField::SymbolName),
    ("assignment", SearchField::SymbolName),
];

// ============================================================================
// Rust
// ============================================================================

pub const RS_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const RS_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_item", SearchField::SymbolName),
    ("struct_item", SearchField::SymbolName),
    ("enum_item", SearchField::SymbolName),
    ("trait_item", SearchField::SymbolName),
    ("type_item", SearchField::SymbolName),
    ("const_item", SearchField::SymbolName),
    ("static_item", SearchField::SymbolName),
    ("mod_item", SearchField::SymbolName),
];

// ============================================================================
// Go
// ============================================================================

pub const GO_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const GO_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_declaration", SearchField::SymbolName),
    ("method_declaration", SearchField::SymbolName),
    ("type_spec", SearchField::SymbolName),
    ("const_spec", SearchField::SymbolName),
    ("var_spec", SearchField::SymbolName),
];

// ============================================================================
// Java
// ============================================================================

pub const JAVA_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const JAVA_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::SymbolName),
    ("method_declaration", SearchField::SymbolName),
    ("interface_declaration", SearchField::SymbolName),
    ("enum_declaration", SearchField::SymbolName),
    ("variable_declarator", SearchField::SymbolName),
];

// ============================================================================
// C
// ============================================================================

pub const C_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const C_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_definition", SearchField::SymbolName),
    ("function_declarator", SearchField::SymbolName),
    ("struct_specifier", SearchField::SymbolName),
    ("enum_specifier", SearchField::SymbolName),
    ("type_definition", SearchField::SymbolName),
    ("init_declarator", SearchField::SymbolName),
];

// ============================================================================
// C++
// ============================================================================

pub const CPP_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const CPP_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_definition", SearchField::SymbolName),
    ("function_declarator", SearchField::SymbolName),
    ("struct_specifier", SearchField::SymbolName),
    ("enum_specifier", SearchField::SymbolName),
    ("type_definition", SearchField::SymbolName),
    ("init_declarator", SearchField::SymbolName),
    ("class_specifier", SearchField::SymbolName),
    ("namespace_definition", SearchField::SymbolName),
];

// ============================================================================
// C#
// ============================================================================

pub const CS_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const CS_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("class_declaration", SearchField::SymbolName),
    ("struct_declaration", SearchField::SymbolName),
    ("method_declaration", SearchField::SymbolName),
    ("interface_declaration", SearchField::SymbolName),
    ("variable_declarator", SearchField::SymbolName),
];

// ============================================================================
// Ruby
// ============================================================================

pub const RB_FIELD_MAP: &[(&str, SearchField)] = &[
    ("class", SearchField::TypeDefinition),
    ("module", SearchField::TypeDefinition),
    ("method", SearchField::FunctionSignature),
    ("singleton_method", SearchField::FunctionSignature),
    ("call", SearchField::ImportExport), // require / include
    ("body_statement", SearchField::FunctionBody),
    ("comment", SearchField::Comment),
    ("string", SearchField::StringLiteral),
];

pub const RB_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("class", SearchField::SymbolName),
    ("module", SearchField::SymbolName),
    ("method", SearchField::SymbolName),
    ("assignment", SearchField::SymbolName),
];

// ============================================================================
// Kotlin
// ============================================================================

pub const KT_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const KT_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_declaration", SearchField::SymbolName),
    ("class_declaration", SearchField::SymbolName),
    ("object_declaration", SearchField::SymbolName),
    ("property_declaration", SearchField::SymbolName),
];

// ============================================================================
// Swift
// ============================================================================

pub const SWIFT_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const SWIFT_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("function_declaration", SearchField::SymbolName),
    ("class_declaration", SearchField::SymbolName),
    ("struct_declaration", SearchField::SymbolName),
    ("protocol_declaration", SearchField::SymbolName),
    ("enum_declaration", SearchField::SymbolName),
];

// ============================================================================
// SQL
// ============================================================================

pub const SQL_FIELD_MAP: &[(&str, SearchField)] = &[
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

pub const SQL_DECL_PARENTS: &[(&str, SearchField)] = &[
    ("create_table", SearchField::SymbolName),
    ("create_function", SearchField::SymbolName),
    ("column_definition", SearchField::SymbolName),
];
