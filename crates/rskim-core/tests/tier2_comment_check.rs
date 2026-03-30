//! Quick check of comment node types in Kotlin and Swift grammars

#![allow(clippy::unwrap_used)]

#[test]
fn kotlin_comment_types() {
    let source = "// line comment\n/* block comment */\n/** doc comment */\nfun main() {}\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let text = child.utf8_text(source.as_bytes()).unwrap_or("");
        eprintln!("KOTLIN kind={:20} text={}", child.kind(), text);
    }
}

#[test]
fn swift_comment_types() {
    let source = "// line comment\n/* block comment */\n/// doc comment\nfunc main() {}\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let text = child.utf8_text(source.as_bytes()).unwrap_or("");
        eprintln!("SWIFT kind={:20} text={}", child.kind(), text);
    }
}
