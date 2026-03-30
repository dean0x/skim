//! Tier 2 language spike — AST dump for Kotlin and Swift grammars
//!
//! This test verifies the grammar crates compile and dumps the AST
//! so we can verify actual node type names before implementing support.

#![allow(clippy::unwrap_used)]

#[test]
fn kotlin_ast_dump() {
    let source = include_str!("../../../tests/fixtures/kotlin/Simple.kt");
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let sexp = tree.root_node().to_sexp();
    eprintln!("=== KOTLIN AST ===\n{sexp}\n=== END ===");

    // Verify we got a valid parse tree
    assert!(
        !tree.root_node().has_error(),
        "Kotlin parse should not have errors"
    );
}

#[test]
fn swift_ast_dump() {
    let source = include_str!("../../../tests/fixtures/swift/Simple.swift");
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let sexp = tree.root_node().to_sexp();
    eprintln!("=== SWIFT AST ===\n{sexp}\n=== END ===");

    // Verify we got a valid parse tree
    assert!(
        !tree.root_node().has_error(),
        "Swift parse should not have errors"
    );
}
