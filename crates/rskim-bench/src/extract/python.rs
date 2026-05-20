//! Tree-sitter-based Python symbol extractor.
//!
//! Walks the AST for:
//! - `function_definition` → function name → FunctionSignature
//! - `class_definition`   → class name    → TypeDefinition
//! - `import_from_statement` → imported names → ImportExport

use std::path::Path;

use rskim_search::SearchField;

use super::ExtractedSymbol;

/// Extract named symbols from Python source using tree-sitter.
pub fn extract(path: &Path, content: &str) -> Vec<ExtractedSymbol> {
    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return vec![];
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return vec![],
    };

    let bytes = content.as_bytes();
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut symbols = Vec::new();

    walk_node(root, &mut cursor, bytes, path, &mut symbols);
    symbols
}

fn walk_node(
    node: tree_sitter::Node<'_>,
    cursor: &mut tree_sitter::TreeCursor<'_>,
    bytes: &[u8],
    path: &Path,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(bytes)
            {
                symbols.push(ExtractedSymbol {
                    name: name.to_string(),
                    file_path: path.to_path_buf(),
                    field: SearchField::FunctionSignature,
                    byte_range: name_node.byte_range(),
                });
            }
        }
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(bytes)
            {
                symbols.push(ExtractedSymbol {
                    name: name.to_string(),
                    file_path: path.to_path_buf(),
                    field: SearchField::TypeDefinition,
                    byte_range: name_node.byte_range(),
                });
            }
        }
        "import_from_statement" => {
            // Extract all `name` child nodes (the imported names)
            extract_import_names(node, bytes, path, symbols);
        }
        _ => {}
    }

    // Recurse into children
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            walk_node(child, cursor, bytes, path, symbols);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Extract imported names from an `import_from_statement` node.
///
/// Handles: `from foo import Bar, baz` → ["Bar", "baz"]
fn extract_import_names(
    node: tree_sitter::Node<'_>,
    bytes: &[u8],
    path: &Path,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            // Look for identifier nodes that represent import names
            // (after the 'import' keyword)
            if child.kind() == "dotted_name" {
                // Last segment of dotted name is the import target
                if let Some(last) = last_named_child(child)
                    && let Ok(name) = last.utf8_text(bytes)
                {
                    symbols.push(ExtractedSymbol {
                        name: name.to_string(),
                        file_path: path.to_path_buf(),
                        field: SearchField::ImportExport,
                        byte_range: last.byte_range(),
                    });
                }
            } else if child.kind() == "aliased_import" {
                // `from x import Foo as F` → "F"
                if let Some(alias) = child.child_by_field_name("alias")
                    && let Ok(name) = alias.utf8_text(bytes)
                {
                    symbols.push(ExtractedSymbol {
                        name: name.to_string(),
                        file_path: path.to_path_buf(),
                        field: SearchField::ImportExport,
                        byte_range: alias.byte_range(),
                    });
                }
            }
            // wildcard_import (`from x import *`) is silently skipped
        }
    }
}

fn last_named_child(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let count = node.named_child_count();
    if count == 0 {
        None
    } else {
        node.named_child(count - 1)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture_path() -> PathBuf {
        PathBuf::from("tests/fixtures/python/simple.py")
    }

    #[test]
    fn extracts_function_names() {
        let content = r#"
def calculate_sum(a: int, b: int) -> int:
    return a + b

def greet_user(name: str) -> str:
    return f"Hello, {name}!"
"#;
        let symbols = extract(&fixture_path(), content);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::FunctionSignature)
            .map(|s| s.name.as_str())
            .collect();
        assert!(fn_names.contains(&"calculate_sum"));
        assert!(fn_names.contains(&"greet_user"));
    }

    #[test]
    fn extracts_class_names() {
        let content = r#"
class Calculator:
    def add(self, x: int, y: int) -> int:
        return x + y

class Logger:
    pass
"#;
        let symbols = extract(&fixture_path(), content);
        let class_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::TypeDefinition)
            .map(|s| s.name.as_str())
            .collect();
        assert!(class_names.contains(&"Calculator"));
        assert!(class_names.contains(&"Logger"));
    }

    #[test]
    fn extracts_import_names() {
        let content = r#"
from os.path import join, exists
from typing import Optional, List
"#;
        let symbols = extract(&fixture_path(), content);
        let import_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::ImportExport)
            .map(|s| s.name.as_str())
            .collect();
        // Should find at least some import names
        assert!(
            !import_names.is_empty(),
            "should extract some import names, got: {import_names:?}"
        );
    }

    #[test]
    fn methods_inside_class_are_extracted() {
        let content = r#"
class Calculator:
    def add(self, x: int, y: int) -> int:
        return x + y
    def multiply(self, x: int, y: int) -> int:
        return x * y
"#;
        let symbols = extract(&fixture_path(), content);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::FunctionSignature)
            .map(|s| s.name.as_str())
            .collect();
        assert!(fn_names.contains(&"add"), "method 'add' should be extracted");
        assert!(
            fn_names.contains(&"multiply"),
            "method 'multiply' should be extracted"
        );
    }

    #[test]
    fn empty_content_returns_empty() {
        let symbols = extract(&fixture_path(), "");
        assert!(symbols.is_empty());
    }

    #[test]
    fn fixture_extracts_multiple_symbol_types() {
        let content = r#"
from os.path import join

def calculate_sum(a: int, b: int) -> int:
    return a + b

class Calculator:
    def add(self, x: int, y: int) -> int:
        return x + y
"#;
        let symbols = extract(&fixture_path(), content);
        let has_fn = symbols.iter().any(|s| s.field == SearchField::FunctionSignature);
        let has_class = symbols.iter().any(|s| s.field == SearchField::TypeDefinition);
        assert!(has_fn, "should extract function symbols");
        assert!(has_class, "should extract class symbols");
    }
}
