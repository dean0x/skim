//! Tree-sitter-based Go symbol extractor.
//!
//! Walks the AST for:
//! - `function_declaration`  → function name → FunctionSignature
//! - `method_declaration`    → method name   → FunctionSignature
//! - `type_declaration`      → type spec name → TypeDefinition
//! - `import_spec`           → last path segment → ImportExport

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rskim_search::SearchField;

use super::ExtractedSymbol;

/// Extract named symbols from Go source using tree-sitter.
pub fn extract(path: &Path, content: &str) -> Vec<ExtractedSymbol> {
    let path = Arc::new(path.to_path_buf());
    super::walk_ast(
        content,
        tree_sitter_go::LANGUAGE.into(),
        move |node, bytes, symbols| {
            match node.kind() {
                "function_declaration" | "method_declaration" => {
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
                "type_declaration" => {
                    // type_declaration contains one or more type_spec children
                    extract_type_specs(node, bytes, &path, symbols);
                }
                "import_spec" => {
                    // Extract last segment of the import path string
                    if let Some(seg) = extract_import_path_last_segment(node, bytes) {
                        symbols.push(ExtractedSymbol {
                            name: seg.0,
                            file_path: Arc::clone(&path),
                            field: SearchField::ImportExport,
                            byte_range: seg.1,
                        });
                    }
                }
                _ => {}
            }
        },
    )
}

/// Extract type names from `type_declaration` → `type_spec` children.
fn extract_type_specs(
    node: tree_sitter::Node<'_>,
    bytes: &[u8],
    path: &Arc<PathBuf>,
    symbols: &mut Vec<ExtractedSymbol>,
) {
    let child_count = node.named_child_count();
    for i in 0..child_count {
        if let Some(child) = node.named_child(i)
            && child.kind() == "type_spec"
            && let Some(name_node) = child.child_by_field_name("name")
            && let Ok(name) = name_node.utf8_text(bytes)
        {
            symbols.push(ExtractedSymbol {
                name: name.to_string(),
                file_path: Arc::clone(path),
                field: SearchField::TypeDefinition,
                byte_range: name_node.byte_range(),
            });
        }
    }
}

/// Extract the last segment of a Go import path.
///
/// For `import "fmt"` → "fmt"
/// For `import "os/path"` → "path"
/// Strips the surrounding quotes from the string literal.
fn extract_import_path_last_segment(
    node: tree_sitter::Node<'_>,
    bytes: &[u8],
) -> Option<(String, std::ops::Range<usize>)> {
    // import_spec has a `path` field (interpreted_string_literal)
    if let Some(path_node) = node.child_by_field_name("path")
        && let Ok(raw) = path_node.utf8_text(bytes)
    {
        // Strip surrounding quotes
        let stripped = raw.trim_matches('"');
        // Take last path segment
        let last_seg = stripped.rsplit('/').next().unwrap_or(stripped);
        if !last_seg.is_empty() {
            return Some((last_seg.to_string(), path_node.byte_range()));
        }
    }
    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code -- unwrap/expect acceptable
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture_path() -> PathBuf {
        PathBuf::from("tests/fixtures/go/simple.go")
    }

    #[test]
    fn extracts_function_names() {
        let content = r#"
package main

func Add(a int, b int) int {
    return a + b
}

func Greet(name string) string {
    return "Hello, " + name
}
"#;
        let symbols = extract(&fixture_path(), content);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::FunctionSignature)
            .map(|s| s.name.as_str())
            .collect();
        assert!(fn_names.contains(&"Add"), "should find 'Add'");
        assert!(fn_names.contains(&"Greet"), "should find 'Greet'");
    }

    #[test]
    fn extracts_method_names() {
        let content = r#"
package main

type Calculator struct { value int }

func (c *Calculator) Add(x int) int {
    return c.value + x
}
"#;
        let symbols = extract(&fixture_path(), content);
        let fn_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::FunctionSignature)
            .map(|s| s.name.as_str())
            .collect();
        assert!(fn_names.contains(&"Add"), "should find method 'Add'");
    }

    #[test]
    fn extracts_type_names() {
        let content = r#"
package main

type Calculator struct { value int }
type Status int
type Computer interface { Compute(x int) int }
"#;
        let symbols = extract(&fixture_path(), content);
        let type_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::TypeDefinition)
            .map(|s| s.name.as_str())
            .collect();
        assert!(type_names.contains(&"Calculator"));
        assert!(type_names.contains(&"Status"));
        assert!(type_names.contains(&"Computer"));
    }

    #[test]
    fn extracts_import_names() {
        let content = r#"
package main

import (
    "fmt"
    "os/path"
)

func main() {}
"#;
        let symbols = extract(&fixture_path(), content);
        let import_names: Vec<&str> = symbols
            .iter()
            .filter(|s| s.field == SearchField::ImportExport)
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            import_names.contains(&"fmt"),
            "should find 'fmt', got: {import_names:?}"
        );
        assert!(
            import_names.contains(&"path"),
            "should find 'path' (from os/path), got: {import_names:?}"
        );
    }

    #[test]
    fn empty_content_returns_empty() {
        let symbols = extract(&fixture_path(), "");
        assert!(symbols.is_empty());
    }

    #[test]
    fn fixture_file_multi_symbol_types() {
        let content = r#"
package main

import "fmt"

func Add(a int, b int) int { return a + b }

type Calculator struct { value int }

func (c *Calculator) Add(x int) int { return c.value + x }

type Computer interface { Compute(x int) int }
"#;
        let symbols = extract(&fixture_path(), content);
        let has_fn = symbols
            .iter()
            .any(|s| s.field == SearchField::FunctionSignature);
        let has_type = symbols
            .iter()
            .any(|s| s.field == SearchField::TypeDefinition);
        let has_import = symbols.iter().any(|s| s.field == SearchField::ImportExport);
        assert!(has_fn, "should extract function symbols");
        assert!(has_type, "should extract type symbols");
        assert!(has_import, "should extract import symbols");
    }
}
