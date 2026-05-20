//! Tree-sitter-based Rust symbol extractor.
//!
//! Walks the AST for:
//! - `function_item` → function name → FunctionSignature
//! - `struct_item`   → struct name   → TypeDefinition
//! - `enum_item`     → enum name     → TypeDefinition
//! - `type_item`     → type alias name → TypeDefinition
//! - `use_declaration` → final path segment → ImportExport
//! - `impl_item`     → type name (if simple identifier) → SymbolName

use std::path::Path;

use rskim_search::SearchField;

use super::ExtractedSymbol;

/// Extract named symbols from Rust source using tree-sitter.
pub fn extract(path: &Path, content: &str) -> Vec<ExtractedSymbol> {
    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
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

    walk_node(root, &mut cursor, bytes, path, content, &mut symbols);
    symbols
}

fn walk_node(
    node: tree_sitter::Node<'_>,
    cursor: &mut tree_sitter::TreeCursor<'_>,
    bytes: &[u8],
    path: &Path,
    content: &str,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    match node.kind() {
        "function_item" => {
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
        "struct_item" | "enum_item" | "type_item" => {
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
        "use_declaration" => {
            // Extract the final segment of the use path (the imported name)
            if let Some(segment) = extract_use_last_segment(node, bytes, content) {
                symbols.push(ExtractedSymbol {
                    name: segment.0,
                    file_path: path.to_path_buf(),
                    field: SearchField::ImportExport,
                    byte_range: segment.1,
                });
            }
        }
        "impl_item" => {
            // Extract the type being implemented (if it's a simple identifier)
            if let Some(type_node) = node.child_by_field_name("type")
                && type_node.kind() == "type_identifier"
                && let Ok(name) = type_node.utf8_text(bytes)
            {
                symbols.push(ExtractedSymbol {
                    name: name.to_string(),
                    file_path: path.to_path_buf(),
                    field: SearchField::SymbolName,
                    byte_range: type_node.byte_range(),
                });
            }
        }
        _ => {}
    }

    // Recurse into children
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            walk_node(child, cursor, bytes, path, content, symbols);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Extract the last meaningful segment from a `use_declaration`.
///
/// Handles:
/// - `use foo::bar::Baz;` → "Baz"
/// - `use foo::bar::*;` → skip (glob, not a named import)
/// - `use foo::bar::{A, B};` → skip (group import handled by tree-sitter children)
fn extract_use_last_segment(
    node: tree_sitter::Node<'_>,
    bytes: &[u8],
    _content: &str,
) -> Option<(String, std::ops::Range<usize>)> {
    find_last_identifier(node, bytes)
}

fn find_last_identifier(
    node: tree_sitter::Node<'_>,
    bytes: &[u8],
) -> Option<(String, std::ops::Range<usize>)> {
    // If this node is an identifier or type_identifier, return it
    if (node.kind() == "identifier" || node.kind() == "type_identifier")
        && let Ok(name) = node.utf8_text(bytes)
    {
        return Some((name.to_string(), node.byte_range()));
    }
    // Skip glob imports
    if node.kind() == "*" {
        return None;
    }
    // Use scoped_use_list last meaningful child
    if node.kind() == "scoped_use_list" {
        return None;
    }

    // Walk children in reverse to find the last identifier
    let child_count = node.named_child_count();
    for i in (0..child_count).rev() {
        if let Some(child) = node.named_child(i)
            && let Some(result) = find_last_identifier(child, bytes)
        {
            return Some(result);
        }
    }
    None
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
        PathBuf::from("tests/fixtures/rust/simple.rs")
    }

    #[test]
    fn extracts_function_names() {
        let content = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn greet(name: &str) -> String { format!("{}", name) }
"#;
        let symbols = extract(&fixture_path(), content);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::FunctionSignature)
            .map(|s| s.name.as_str())
            .collect();
        assert!(fn_names.contains(&"add"), "should find 'add'");
        assert!(fn_names.contains(&"greet"), "should find 'greet'");
    }

    #[test]
    fn extracts_struct_names() {
        let content = r#"
pub struct Calculator { value: i32 }
pub struct Logger;
"#;
        let symbols = extract(&fixture_path(), content);
        let type_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::TypeDefinition)
            .map(|s| s.name.as_str())
            .collect();
        assert!(type_names.contains(&"Calculator"));
        assert!(type_names.contains(&"Logger"));
    }

    #[test]
    fn extracts_enum_names() {
        let content = r#"
pub enum Status { Active, Inactive }
pub enum Color { Red, Green, Blue }
"#;
        let symbols = extract(&fixture_path(), content);
        let type_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::TypeDefinition)
            .map(|s| s.name.as_str())
            .collect();
        assert!(type_names.contains(&"Status"));
        assert!(type_names.contains(&"Color"));
    }

    #[test]
    fn extracts_impl_type_names() {
        let content = r#"
pub struct Calculator { value: i32 }
impl Calculator {
    pub fn new(v: i32) -> Self { Self { value: v } }
}
"#;
        let symbols = extract(&fixture_path(), content);
        let symbol_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::SymbolName)
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            symbol_names.contains(&"Calculator"),
            "should find impl Calculator as SymbolName"
        );
    }

    #[test]
    fn fixture_file_extracts_multiple_symbols() {
        // Test with the actual fixture file content
        let content = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn greet(name: &str) -> String { format!("Hello, {}!", name) }
pub struct Calculator { value: i32 }
impl Calculator {
    pub fn new(value: i32) -> Self { Self { value } }
    pub fn add(&self, x: i32) -> i32 { self.value + x }
}
pub trait Compute { fn compute(&self, x: i32) -> i32; }
pub enum Status { Active, Inactive, Pending }
"#;
        let symbols = extract(&fixture_path(), content);
        // Should have at least: add, greet, Calculator (struct), Calculator (impl),
        // Compute (trait?), Status
        assert!(
            symbols.len() >= 5,
            "should extract at least 5 symbols, got {}",
            symbols.len()
        );
    }

    #[test]
    fn empty_content_returns_empty() {
        let symbols = extract(&fixture_path(), "");
        assert!(symbols.is_empty());
    }

    #[test]
    fn invalid_rust_returns_partial_results() {
        // tree-sitter is error-tolerant, so partial results are fine
        let content = "fn incomplete(";
        let _symbols = extract(&fixture_path(), content);
        // Just verify it doesn't panic
    }
}
