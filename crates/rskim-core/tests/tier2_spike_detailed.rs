//! Detailed AST dump — one level deep, to clearly see node types

#![allow(clippy::unwrap_used)]

fn dump_node_types(source: &str, lang: tree_sitter::Language, label: &str) {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();

    eprintln!("\n=== {} TOP-LEVEL CHILDREN ===", label);
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let text = child
            .utf8_text(source.as_bytes())
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        eprintln!(
            "  kind={:30} named={} text_preview={:50}",
            child.kind(),
            child.is_named(),
            text
        );

        // One more level deep
        let mut cursor2 = child.walk();
        for grandchild in child.children(&mut cursor2) {
            let gc_text = grandchild
                .utf8_text(source.as_bytes())
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("");
            eprintln!(
                "    kind={:28} named={} text_preview={:50}",
                grandchild.kind(),
                grandchild.is_named(),
                gc_text
            );
        }
    }
    eprintln!("=== END ===");
}

#[test]
fn kotlin_detailed_ast() {
    let source = include_str!("../../../tests/fixtures/kotlin/Simple.kt");
    dump_node_types(
        source,
        tree_sitter_kotlin_ng::LANGUAGE.into(),
        "KOTLIN",
    );
}

#[test]
fn swift_detailed_ast() {
    let source = include_str!("../../../tests/fixtures/swift/Simple.swift");
    dump_node_types(
        source,
        tree_sitter_swift::LANGUAGE.into(),
        "SWIFT",
    );
}

#[test]
fn swift_struct_vs_class() {
    // Test specifically to see how struct and class differ
    let source = r#"
struct Point {
    var x: Int
    var y: Int
}

class Animal {
    var name: String
    init(name: String) {
        self.name = name
    }
}

protocol Drawable {
    func draw()
}

enum Direction {
    case north
    case south
    case east
    case west
}

typealias StringList = [String]
"#;
    dump_node_types(
        source,
        tree_sitter_swift::LANGUAGE.into(),
        "SWIFT-TYPES",
    );
}

#[test]
fn kotlin_types_ast() {
    let source = r#"
data class Point(val x: Int, val y: Int)

sealed class Result<out T> {
    data class Success<T>(val value: T) : Result<T>()
    data class Error(val message: String) : Result<Nothing>()
}

interface Drawable {
    fun draw()
}

enum class Direction {
    NORTH, SOUTH, EAST, WEST
}

typealias StringList = List<String>

object Singleton {
    val instance = "hello"
}
"#;
    dump_node_types(
        source,
        tree_sitter_kotlin_ng::LANGUAGE.into(),
        "KOTLIN-TYPES",
    );
}
