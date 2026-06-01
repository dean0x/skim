//! Tree-sitter-based TypeScript symbol extractor.
//!
//! Walks the AST for:
//! - `function_declaration`        → function name    → FunctionSignature
//! - `method_definition`           → method name      → FunctionSignature
//! - `interface_declaration`       → interface name   → TypeDefinition
//! - `type_alias_declaration`      → type alias name  → TypeDefinition
//! - `enum_declaration`            → enum name        → TypeDefinition
//! - `class_declaration`           → class name       → SymbolName
//! - `import_statement`            → imported names   → ImportExport
//! - `export_statement`            → exported names   → ImportExport

use std::path::Path;
use std::sync::Arc;

use rskim_search::SearchField;

use super::ExtractedSymbol;

/// Extract named symbols from TypeScript source using tree-sitter.
pub fn extract(path: &Path, content: &str) -> Vec<ExtractedSymbol> {
    let path = Arc::new(path.to_path_buf());
    super::walk_ast(
        content,
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        move |node, bytes, symbols| match node.kind() {
            "function_declaration" | "method_definition" => {
                if let Some(name_node) = node.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(bytes)
                {
                    symbols.push(ExtractedSymbol {
                        name: name.to_string(),
                        file_path: Arc::clone(&path),
                        field: SearchField::FunctionSignature,
                        byte_range: name_node.byte_range(),
                    });
                }
            }
            "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(bytes)
                {
                    symbols.push(ExtractedSymbol {
                        name: name.to_string(),
                        file_path: Arc::clone(&path),
                        field: SearchField::TypeDefinition,
                        byte_range: name_node.byte_range(),
                    });
                }
            }
            "class_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(bytes)
                {
                    symbols.push(ExtractedSymbol {
                        name: name.to_string(),
                        file_path: Arc::clone(&path),
                        field: SearchField::SymbolName,
                        byte_range: name_node.byte_range(),
                    });
                }
            }
            "import_statement" => {
                collect_import_names(node, bytes, symbols, &path);
            }
            "export_statement" => {
                if let Some(decl) = node.child_by_field_name("declaration")
                    && let Some(name_node) = decl.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(bytes)
                {
                    symbols.push(ExtractedSymbol {
                        name: name.to_string(),
                        file_path: Arc::clone(&path),
                        field: SearchField::ImportExport,
                        byte_range: name_node.byte_range(),
                    });
                }
            }
            _ => {}
        },
    )
}

/// Collect import specifier names from a `named_imports` AST node.
///
/// Handles `{ Foo, Bar }` — pushes each specifier name as an `ImportExport` symbol.
fn collect_named_imports(
    named_imports_node: tree_sitter::Node<'_>,
    bytes: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    path: &Arc<std::path::PathBuf>,
) {
    let mut cursor = named_imports_node.walk();
    for spec in named_imports_node.children(&mut cursor) {
        if spec.kind() == "import_specifier" {
            if let Some(name_node) = spec.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(bytes)
            {
                symbols.push(ExtractedSymbol {
                    name: name.to_string(),
                    file_path: Arc::clone(path),
                    field: SearchField::ImportExport,
                    byte_range: name_node.byte_range(),
                });
            }
        }
    }
}

/// Collect named imports from an import statement.
///
/// Handles:
/// - `import { Foo, Bar } from 'module';` → "Foo", "Bar"
/// - `import Foo from 'module';` → "Foo"
/// - `import * as Foo from 'module';` → skip (namespace import)
fn collect_import_names(
    node: tree_sitter::Node<'_>,
    bytes: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    path: &Arc<std::path::PathBuf>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "import_clause" {
            continue;
        }
        let mut clause_cursor = child.walk();
        for clause_child in child.children(&mut clause_cursor) {
            match clause_child.kind() {
                "identifier" => {
                    if let Ok(name) = clause_child.utf8_text(bytes) {
                        symbols.push(ExtractedSymbol {
                            name: name.to_string(),
                            file_path: Arc::clone(path),
                            field: SearchField::ImportExport,
                            byte_range: clause_child.byte_range(),
                        });
                    }
                }
                "named_imports" => {
                    collect_named_imports(clause_child, bytes, symbols, path);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture_path() -> PathBuf {
        PathBuf::from("tests/fixtures/typescript/simple.ts")
    }

    #[test]
    fn extracts_function_declarations() {
        let content = r#"
function add(a: number, b: number): number { return a + b; }
function greet(name: string): string { return `Hello ${name}`; }
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
    fn extracts_interface_names() {
        let content = r#"
interface User { name: string; age: number; }
interface Repository { id: number; }
"#;
        let symbols = extract(&fixture_path(), content);
        let type_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::TypeDefinition)
            .map(|s| s.name.as_str())
            .collect();
        assert!(type_names.contains(&"User"));
        assert!(type_names.contains(&"Repository"));
    }

    #[test]
    fn extracts_type_aliases() {
        let content = r#"
type ID = string;
type Result<T> = T | Error;
"#;
        let symbols = extract(&fixture_path(), content);
        let type_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::TypeDefinition)
            .map(|s| s.name.as_str())
            .collect();
        assert!(type_names.contains(&"ID"));
        assert!(type_names.contains(&"Result"));
    }

    #[test]
    fn extracts_enum_names() {
        let content = r#"
enum Status { Active, Inactive }
enum Color { Red = 'red', Green = 'green' }
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
    fn extracts_class_names() {
        let content = r#"
class UserService { constructor() {} }
class Logger { log(msg: string) {} }
"#;
        let symbols = extract(&fixture_path(), content);
        let symbol_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::SymbolName)
            .map(|s| s.name.as_str())
            .collect();
        assert!(symbol_names.contains(&"UserService"));
        assert!(symbol_names.contains(&"Logger"));
    }

    #[test]
    fn extracts_named_imports() {
        let content = r#"
import { Router, Request, Response } from 'express';
"#;
        let symbols = extract(&fixture_path(), content);
        let import_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::ImportExport)
            .map(|s| s.name.as_str())
            .collect();
        assert!(import_names.contains(&"Router"));
        assert!(import_names.contains(&"Request"));
    }

    #[test]
    fn extracts_class_methods() {
        let content = r#"
class Service {
    async handle(req: Request): Promise<Response> { return new Response(); }
    validate(input: string): boolean { return true; }
}
"#;
        let symbols = extract(&fixture_path(), content);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::FunctionSignature)
            .map(|s| s.name.as_str())
            .collect();
        assert!(fn_names.contains(&"handle"));
        assert!(fn_names.contains(&"validate"));
    }

    #[test]
    fn empty_content_returns_empty() {
        let symbols = extract(&fixture_path(), "");
        assert!(symbols.is_empty());
    }
}
