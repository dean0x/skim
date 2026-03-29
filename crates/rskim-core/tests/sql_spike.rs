//! SQL tree-sitter grammar spike — evaluate node types and grammar quality

#[test]
fn sql_ast_dump() {
    let source = include_str!("../../../tests/fixtures/sql/simple.sql");

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_sequel::LANGUAGE.into())
        .expect("Failed to load SQL grammar");

    let tree = parser.parse(source, None).expect("Failed to parse SQL");
    let root = tree.root_node();

    // Dump full S-expression for analysis
    println!("=== SQL AST S-expression ===");
    println!("{}", root.to_sexp());
    println!();

    // Dump top-level node kinds
    println!("=== Top-level node kinds ===");
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let kind = child.kind();
        let text = child
            .utf8_text(source.as_bytes())
            .unwrap_or("<error>")
            .lines()
            .next()
            .unwrap_or("");
        println!("  {kind}: {text}");
    }

    // Count error nodes (indicates grammar quality issues)
    println!();
    let error_count = count_errors(root);
    println!("=== Error node count: {error_count} ===");
    assert_eq!(error_count, 0, "SQL grammar should parse the fixture without errors");
}

fn count_errors(node: tree_sitter::Node) -> usize {
    let mut count = if node.is_error() || node.is_missing() {
        1
    } else {
        0
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_errors(child);
    }
    count
}
