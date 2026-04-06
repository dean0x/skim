//! Integration tests for tree-sitter field classification.
//!
//! Tests [`TreeSitterClassifier`] using real tree-sitter parsing via
//! [`rskim_core::Parser`] (which wraps the grammar crates available in rskim-core).
//!
//! Validates:
//! - `for_language` returns None for serde/Markdown languages
//! - `for_language` returns Some for all tree-sitter languages
//! - `classify_node` correctly classifies function declarations, identifiers,
//!   and punctuation nodes

use rskim_core::{Language, Parser};
use rskim_search::fields::for_language;
use rskim_search::SearchField;

// ============================================================================
// for_language — language coverage
// ============================================================================

#[test]
fn for_language_returns_none_for_json() {
    assert!(for_language(Language::Json).is_none());
}

#[test]
fn for_language_returns_none_for_yaml() {
    assert!(for_language(Language::Yaml).is_none());
}

#[test]
fn for_language_returns_none_for_toml() {
    assert!(for_language(Language::Toml).is_none());
}

#[test]
fn for_language_returns_none_for_markdown() {
    assert!(for_language(Language::Markdown).is_none());
}

#[test]
fn for_language_returns_some_for_all_tree_sitter_languages() {
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
            for_language(lang).is_some(),
            "Expected Some classifier for {lang:?}"
        );
    }
}

// ============================================================================
// classify_node — TypeScript fixture
// ============================================================================

/// Walk all nodes in a tree and collect those classified by the classifier.
fn walk_and_classify(
    tree: &tree_sitter::Tree,
    source: &str,
    classifier: &dyn rskim_search::FieldClassifier,
) -> Vec<(String, SearchField)> {
    let mut results = Vec::new();
    let mut cursor = tree.walk();

    // Iterative DFS over the entire tree.
    loop {
        let node = cursor.node();
        if let Some(field) = classifier.classify_node(&node, source) {
            results.push((node.kind().to_string(), field));
        }

        // Descend or advance.
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return results;
            }
        }
    }
}

#[test]
fn typescript_function_declaration_is_function_signature() {
    let source = "function greet(name: string): string { return name; }";
    let mut parser = Parser::new(Language::TypeScript).expect("TypeScript parser must build");
    let tree = parser.parse(source).expect("TypeScript parse must succeed");
    let classifier = for_language(Language::TypeScript).expect("TypeScript classifier must exist");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());
    let has_sig = classified.iter().any(|(kind, field)| {
        kind == "function_declaration" && *field == SearchField::FunctionSignature
    });
    assert!(
        has_sig,
        "function_declaration should map to FunctionSignature; got: {classified:?}"
    );
}

#[test]
fn typescript_interface_declaration_is_type_definition() {
    let source = "interface UserProfile { id: string; name: string; }";
    let mut parser = Parser::new(Language::TypeScript).expect("TypeScript parser");
    let tree = parser.parse(source).expect("parse succeeds");
    let classifier = for_language(Language::TypeScript).expect("classifier exists");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());
    let has_type_def = classified.iter().any(|(kind, field)| {
        kind == "interface_declaration" && *field == SearchField::TypeDefinition
    });
    assert!(
        has_type_def,
        "interface_declaration → TypeDefinition; got: {classified:?}"
    );
}

#[test]
fn typescript_identifier_in_function_declaration_is_symbol_name() {
    // The identifier `greet` inside function_declaration → SymbolName
    let source = "function greet(name: string): void {}";
    let mut parser = Parser::new(Language::TypeScript).expect("TypeScript parser");
    let tree = parser.parse(source).expect("parse succeeds");
    let classifier = for_language(Language::TypeScript).expect("classifier exists");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());
    let has_symbol_name = classified
        .iter()
        .any(|(kind, field)| kind == "identifier" && *field == SearchField::SymbolName);
    assert!(
        has_symbol_name,
        "identifier in function_declaration should be SymbolName; got: {classified:?}"
    );
}

#[test]
fn typescript_class_declaration_is_type_definition() {
    let source = "class UserService { constructor() {} }";
    let mut parser = Parser::new(Language::TypeScript).expect("TypeScript parser");
    let tree = parser.parse(source).expect("parse succeeds");
    let classifier = for_language(Language::TypeScript).expect("classifier exists");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());
    let has_class = classified
        .iter()
        .any(|(kind, field)| kind == "class_declaration" && *field == SearchField::TypeDefinition);
    assert!(
        has_class,
        "class_declaration → TypeDefinition; got: {classified:?}"
    );
}

#[test]
fn typescript_import_is_import_export() {
    let source = "import { Foo } from './foo';";
    let mut parser = Parser::new(Language::TypeScript).expect("TypeScript parser");
    let tree = parser.parse(source).expect("parse succeeds");
    let classifier = for_language(Language::TypeScript).expect("classifier exists");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());
    let has_import = classified
        .iter()
        .any(|(kind, field)| kind == "import_statement" && *field == SearchField::ImportExport);
    assert!(
        has_import,
        "import_statement → ImportExport; got: {classified:?}"
    );
}

// ============================================================================
// classify_node — Rust fixture
// ============================================================================

#[test]
fn rust_struct_item_is_type_definition() {
    let source = "struct Config { host: String, port: u16 }";
    let mut parser = Parser::new(Language::Rust).expect("Rust parser");
    let tree = parser.parse(source).expect("parse succeeds");
    let classifier = for_language(Language::Rust).expect("classifier exists");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());
    let has_struct = classified
        .iter()
        .any(|(kind, field)| kind == "struct_item" && *field == SearchField::TypeDefinition);
    assert!(
        has_struct,
        "struct_item → TypeDefinition; got: {classified:?}"
    );
}

#[test]
fn rust_function_item_is_function_signature() {
    let source = "fn main() {}";
    let mut parser = Parser::new(Language::Rust).expect("Rust parser");
    let tree = parser.parse(source).expect("parse succeeds");
    let classifier = for_language(Language::Rust).expect("classifier exists");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());
    let has_fn = classified
        .iter()
        .any(|(kind, field)| kind == "function_item" && *field == SearchField::FunctionSignature);
    assert!(
        has_fn,
        "function_item → FunctionSignature; got: {classified:?}"
    );
}

// ============================================================================
// classify_node — punctuation / whitespace nodes return None
// ============================================================================

#[test]
fn typescript_fixture_from_file() {
    // Tests against the real user_service.ts fixture.
    let source = include_str!("../../../tests/fixtures/search/user_service.ts");
    let mut parser = Parser::new(Language::TypeScript).expect("TypeScript parser");
    let tree = parser.parse(source).expect("parse succeeds");
    let classifier = for_language(Language::TypeScript).expect("classifier exists");

    let classified = walk_and_classify(&tree, source, classifier.as_ref());

    // The fixture has a class declaration.
    let has_class = classified
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    // The fixture has function declarations (methods).
    let has_fn = classified
        .iter()
        .any(|(_, f)| *f == SearchField::FunctionSignature);
    // The fixture has import statements.
    let has_import = classified
        .iter()
        .any(|(_, f)| *f == SearchField::ImportExport);

    assert!(
        has_class,
        "fixture should contain at least one TypeDefinition"
    );
    assert!(
        has_fn,
        "fixture should contain at least one FunctionSignature"
    );
    assert!(
        has_import,
        "fixture should contain at least one ImportExport"
    );
}
