//! Integration tests for rskim-core
//!
//! These tests validate the full pipeline from source → transformation.

#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests
#![allow(clippy::expect_used)] // Expect is acceptable in tests

use rskim_core::{
    transform, transform_auto, transform_with_config, truncate_to_token_budget, Language, Mode,
    TransformConfig,
};
use std::path::Path;

// ============================================================================
// TypeScript Tests
// ============================================================================

#[test]
fn test_typescript_structure() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let result = transform(source, Language::TypeScript, Mode::Structure).unwrap();

    // Should contain signatures
    assert!(result.contains("function add"));
    assert!(result.contains("number"));

    // Should NOT contain implementation
    assert!(!result.contains("return a + b"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_typescript_signatures() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let result = transform(source, Language::TypeScript, Mode::Signatures).unwrap();

    // Should contain only signatures
    assert!(result.contains("function add(a: number, b: number): number"));
    assert!(result.contains("function greet(name: string): string"));

    // Should NOT contain bodies
    assert!(!result.contains("return"));
    assert!(!result.contains("{"));
}

#[test]
fn test_typescript_types() {
    let source = include_str!("../../../tests/fixtures/typescript/types.ts");
    let result = transform(source, Language::TypeScript, Mode::Types).unwrap();

    // Should contain type definitions
    assert!(result.contains("type UserId"));
    assert!(result.contains("interface User"));
    assert!(result.contains("enum Status"));
    assert!(result.contains("class UserService"));

    // Should NOT contain function implementations
    assert!(!result.contains("findUser(id: UserId): User | null {"));
}

#[test]
fn test_typescript_full() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let result = transform(source, Language::TypeScript, Mode::Full).unwrap();

    // Should be identical to input
    assert_eq!(result, source);
}

// ============================================================================
// Python Tests
// ============================================================================

#[test]
fn test_python_structure() {
    let source = include_str!("../../../tests/fixtures/python/simple.py");
    let result = transform(source, Language::Python, Mode::Structure).unwrap();

    // Should contain function signatures
    assert!(result.contains("def calculate_sum"));
    assert!(result.contains("def greet_user"));

    // Should NOT contain implementation
    assert!(!result.contains("result = a + b"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_python_signatures() {
    let source = include_str!("../../../tests/fixtures/python/simple.py");
    let result = transform(source, Language::Python, Mode::Signatures).unwrap();

    // Should contain only signatures
    assert!(result.contains("def calculate_sum(a: int, b: int) -> int:"));
    assert!(result.contains("def greet_user(name: str) -> str:"));

    // Should NOT contain bodies
    assert!(!result.contains("result = "));
}

// ============================================================================
// Rust Tests
// ============================================================================

#[test]
fn test_rust_structure() {
    let source = include_str!("../../../tests/fixtures/rust/simple.rs");
    let result = transform(source, Language::Rust, Mode::Structure).unwrap();

    // Should contain function signatures
    assert!(result.contains("pub fn add"));
    assert!(result.contains("pub fn greet"));

    // Should NOT contain implementation
    assert!(!result.contains("a + b"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_rust_signatures() {
    let source = include_str!("../../../tests/fixtures/rust/simple.rs");
    let result = transform(source, Language::Rust, Mode::Signatures).unwrap();

    // Should contain function signatures
    assert!(result.contains("pub fn add(a: i32, b: i32) -> i32"));
    assert!(result.contains("pub fn greet(name: &str) -> String"));
}

#[test]
fn test_rust_types() {
    let source = include_str!("../../../tests/fixtures/rust/simple.rs");
    let result = transform(source, Language::Rust, Mode::Types).unwrap();

    // Should contain type definitions
    assert!(result.contains("pub struct Calculator"));
    assert!(result.contains("pub trait Compute"));
    assert!(result.contains("pub enum Status"));
}

// ============================================================================
// Go Tests
// ============================================================================

#[test]
fn test_go_structure() {
    let source = include_str!("../../../tests/fixtures/go/simple.go");
    let result = transform(source, Language::Go, Mode::Structure).unwrap();

    // Should contain function signatures
    assert!(result.contains("func Add"));
    assert!(result.contains("func Greet"));

    // Should NOT contain implementation
    assert!(!result.contains("return a + b"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_go_signatures() {
    let source = include_str!("../../../tests/fixtures/go/simple.go");
    let result = transform(source, Language::Go, Mode::Signatures).unwrap();

    // Should contain function signatures
    assert!(result.contains("func Add(a int, b int) int"));
    assert!(result.contains("func (c *Calculator) Add(x int) int"));
}

#[test]
fn test_go_types() {
    let source = include_str!("../../../tests/fixtures/go/simple.go");
    let result = transform(source, Language::Go, Mode::Types).unwrap();

    // Should contain type definitions
    assert!(result.contains("type Calculator struct"));
    assert!(result.contains("type Computer interface"));
}

// ============================================================================
// Java Tests
// ============================================================================

#[test]
fn test_java_structure() {
    let source = include_str!("../../../tests/fixtures/java/Simple.java");
    let result = transform(source, Language::Java, Mode::Structure).unwrap();

    // Should contain method signatures
    assert!(result.contains("public int add"));
    assert!(result.contains("public String greet"));

    // Should NOT contain implementation
    assert!(!result.contains("return a + b"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_java_signatures() {
    let source = include_str!("../../../tests/fixtures/java/Simple.java");
    let result = transform(source, Language::Java, Mode::Signatures).unwrap();

    // Should contain method signatures
    assert!(result.contains("public int add(int a, int b)"));
    assert!(result.contains("public String greet(String name)"));
}

#[test]
fn test_java_types() {
    let source = include_str!("../../../tests/fixtures/java/Simple.java");
    let result = transform(source, Language::Java, Mode::Types).unwrap();

    // Should contain type definitions
    assert!(result.contains("public class Simple"));
    assert!(result.contains("interface Computer"));
    assert!(result.contains("enum Status"));
}

// ============================================================================
// Markdown Tests
// ============================================================================

#[test]
fn test_markdown_structure() {
    let source = include_str!("../../../tests/fixtures/markdown/simple.md");
    let result = transform(source, Language::Markdown, Mode::Structure).unwrap();

    // Should contain H1-H3 headers
    assert!(result.contains("# Main Title"));
    assert!(result.contains("## Section One"));
    assert!(result.contains("### Subsection 1.1"));
    assert!(result.contains("Setext Style H1"));
    assert!(result.contains("Setext Style H2"));

    // Should NOT contain H4-H6
    assert!(!result.contains("#### Deep Header"));
    assert!(!result.contains("##### Even Deeper"));
    assert!(!result.contains("###### Deepest"));

    // Should NOT contain body content
    assert!(!result.contains("introductory content"));
    assert!(!result.contains("implementation details"));
}

#[test]
fn test_markdown_signatures() {
    let source = include_str!("../../../tests/fixtures/markdown/simple.md");
    let result = transform(source, Language::Markdown, Mode::Signatures).unwrap();

    // Should contain ALL headers (H1-H6)
    assert!(result.contains("# Main Title"));
    assert!(result.contains("## Section One"));
    assert!(result.contains("### Subsection 1.1"));
    assert!(result.contains("#### Deep Header"));
    assert!(result.contains("##### Even Deeper"));
    assert!(result.contains("###### Deepest"));

    // Should NOT contain body content
    assert!(!result.contains("introductory content"));
}

#[test]
fn test_markdown_types() {
    let source = include_str!("../../../tests/fixtures/markdown/simple.md");
    let result = transform(source, Language::Markdown, Mode::Types).unwrap();

    // Types mode should be identical to signatures for markdown (no type system)
    let signatures = transform(source, Language::Markdown, Mode::Signatures).unwrap();
    assert_eq!(result, signatures);

    // Should contain ALL headers
    assert!(result.contains("# Main Title"));
    assert!(result.contains("###### Deepest"));
}

#[test]
fn test_markdown_full() {
    let source = include_str!("../../../tests/fixtures/markdown/simple.md");
    let result = transform(source, Language::Markdown, Mode::Full).unwrap();

    // Should be identical to input
    assert_eq!(result, source);
}

// ============================================================================
// Language Detection Tests
// ============================================================================

#[test]
fn test_transform_auto_detection() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let path = Path::new("simple.ts");

    let result = transform_auto(source, path, Mode::Structure);
    assert!(result.is_ok());
}

#[test]
fn test_detect_language_from_path() {
    use rskim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("test.ts")),
        Some(Language::TypeScript)
    );
    assert_eq!(
        detect_language_from_path(Path::new("test.tsx")),
        Some(Language::TypeScript)
    );
    assert_eq!(
        detect_language_from_path(Path::new("script.py")),
        Some(Language::Python)
    );
    assert_eq!(
        detect_language_from_path(Path::new("main.rs")),
        Some(Language::Rust)
    );
    assert_eq!(
        detect_language_from_path(Path::new("main.go")),
        Some(Language::Go)
    );
    assert_eq!(
        detect_language_from_path(Path::new("Main.java")),
        Some(Language::Java)
    );
    assert_eq!(
        detect_language_from_path(Path::new("README.md")),
        Some(Language::Markdown)
    );
    assert_eq!(
        detect_language_from_path(Path::new("doc.markdown")),
        Some(Language::Markdown)
    );
    assert_eq!(
        detect_language_from_path(Path::new("data.json")),
        Some(Language::Json)
    );
    // C extensions
    assert_eq!(
        detect_language_from_path(Path::new("main.c")),
        Some(Language::C)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.h")),
        Some(Language::C)
    );
    // C++ extensions
    assert_eq!(
        detect_language_from_path(Path::new("main.cpp")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("main.cc")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("main.cxx")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.hpp")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.hxx")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.hh")),
        Some(Language::Cpp)
    );
    // TOML extension
    assert_eq!(
        detect_language_from_path(Path::new("Cargo.toml")),
        Some(Language::Toml)
    );
}

#[test]
fn test_unsupported_language() {
    use rskim_core::detect_language_from_path;

    assert_eq!(detect_language_from_path(Path::new("unknown.xyz")), None);
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_empty_file() {
    let source = "";
    let result = transform(source, Language::TypeScript, Mode::Structure).unwrap();
    assert_eq!(result, "");
}

#[test]
fn test_unicode_support() {
    let source = "function greet(name: string) { return \"你好 🎉 \" + name; }";
    let result = transform(source, Language::TypeScript, Mode::Structure).unwrap();

    // Should handle UTF-8 correctly
    assert!(result.contains("function greet"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_malformed_syntax() {
    // tree-sitter is error-tolerant, should not crash
    let source = "function broken(() { { { {";
    let result = transform(source, Language::TypeScript, Mode::Structure);

    // Should succeed (tree-sitter handles broken syntax)
    assert!(result.is_ok());
}

#[test]
fn test_nested_functions() {
    let source = r#"
function outer() {
    function inner() {
        return 42;
    }
    return inner();
}
"#;
    let result = transform(source, Language::TypeScript, Mode::Structure).unwrap();

    // Should handle nested functions without panic
    assert!(result.contains("function outer"));
    assert!(result.contains("{ /* ... */ }"));
}

// ============================================================================
// Token Reduction Tests
// ============================================================================

#[test]
fn test_structure_reduces_tokens() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let result = transform(source, Language::TypeScript, Mode::Structure).unwrap();

    // Structure mode should be significantly smaller
    assert!(result.len() < source.len());
}

#[test]
fn test_signatures_more_aggressive() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let structure = transform(source, Language::TypeScript, Mode::Structure).unwrap();
    let signatures = transform(source, Language::TypeScript, Mode::Signatures).unwrap();

    // Signatures should be more aggressive than structure
    assert!(signatures.len() < structure.len());
}

// ============================================================================
// JSON Tests
// ============================================================================

#[test]
fn test_json_simple_structure() {
    let source = include_str!("../../../tests/fixtures/json/simple.json");
    let result = transform(source, Language::Json, Mode::Structure).unwrap();

    // Should contain keys
    assert!(result.contains("name"));
    assert!(result.contains("age"));
    assert!(result.contains("email"));
    assert!(result.contains("active"));

    // Should NOT contain values
    assert!(!result.contains("John Doe"));
    assert!(!result.contains("30"));
    assert!(!result.contains("john@example.com"));
    assert!(!result.contains("true"));
}

#[test]
fn test_json_nested_structure() {
    let source = include_str!("../../../tests/fixtures/json/nested.json");
    let result = transform(source, Language::Json, Mode::Structure).unwrap();

    // Should contain nested keys
    assert!(result.contains("user"));
    assert!(result.contains("profile"));
    assert!(result.contains("name"));
    assert!(result.contains("address"));
    assert!(result.contains("street"));
    assert!(result.contains("settings"));
    assert!(result.contains("metadata"));

    // Should NOT contain values
    assert!(!result.contains("Jane Smith"));
    assert!(!result.contains("123 Main St"));
    assert!(!result.contains("dark"));
}

#[test]
fn test_json_arrays() {
    let source = include_str!("../../../tests/fixtures/json/array.json");
    let result = transform(source, Language::Json, Mode::Structure).unwrap();

    // Should contain array key
    assert!(result.contains("users"));

    // For arrays of objects, should show structure of first item
    assert!(result.contains("id"));
    assert!(result.contains("name"));
    assert!(result.contains("role"));

    // Should NOT contain actual values
    assert!(!result.contains("Alice"));
    assert!(!result.contains("admin"));

    // For arrays of primitives, just show the key
    assert!(result.contains("tags"));
    assert!(result.contains("counts"));
    assert!(!result.contains("important"));
    assert!(!result.contains("10"));
}

#[test]
fn test_json_edge_cases() {
    let source = include_str!("../../../tests/fixtures/json/edge.json");
    let result = transform(source, Language::Json, Mode::Structure).unwrap();

    // Should handle empty objects/arrays
    assert!(result.contains("empty_object"));
    assert!(result.contains("empty_array"));

    // Should handle null, booleans, numbers
    assert!(result.contains("null_value"));
    assert!(result.contains("boolean_true"));
    assert!(result.contains("number_int"));
    assert!(result.contains("string_empty"));

    // Should handle unicode
    assert!(result.contains("unicode"));

    // Should handle mixed arrays (just shows key, not nested object structure)
    assert!(result.contains("mixed_array"));
    // For mixed arrays, we just show the key, not the nested object
}

#[test]
fn test_json_modes_identical() {
    let source = include_str!("../../../tests/fixtures/json/simple.json");

    // Serde-based modes (Structure/Signatures/Types) all produce the same
    // key-only structure extraction since there's no tree-sitter AST to
    // differentiate modes.
    let structure = transform(source, Language::Json, Mode::Structure).unwrap();
    let signatures = transform(source, Language::Json, Mode::Signatures).unwrap();
    let types = transform(source, Language::Json, Mode::Types).unwrap();

    assert_eq!(structure, signatures);
    assert_eq!(structure, types);

    // Full mode returns original source unchanged (documented contract)
    let full = transform(source, Language::Json, Mode::Full).unwrap();
    assert_eq!(full, source);
    assert_ne!(
        full, structure,
        "Full mode should differ from structure extraction"
    );
}

#[test]
fn test_json_auto_detection() {
    let source = include_str!("../../../tests/fixtures/json/simple.json");
    let path = Path::new("data.json");

    let result = transform_auto(source, path, Mode::Structure);
    assert!(result.is_ok());

    let content = result.unwrap();
    assert!(content.contains("name"));
    assert!(!content.contains("John Doe"));
}

#[test]
fn test_json_invalid() {
    let source = r#"{"invalid": "#;
    let result = transform(source, Language::Json, Mode::Structure);

    // Should return error for invalid JSON
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid JSON"));
}

#[test]
fn test_json_deeply_nested_security() {
    // SECURITY TEST: Ensure deeply nested JSON is rejected (depth > 500)
    let mut json = String::from("{\"level1\":");
    for i in 2..=550 {
        json.push_str(&format!("{{\"level{}\":", i));
    }
    json.push_str("\"value\"");
    for _ in 0..550 {
        json.push('}');
    }

    let result = transform(&json, Language::Json, Mode::Structure);

    // Should reject with depth/recursion limit error
    // Note: serde_json has its own recursion limit of 128, our validate_json_depth provides additional protection at 500
    assert!(result.is_err(), "Expected error for deeply nested JSON");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("recursion limit")
            || err_msg.contains("nesting depth")
            || err_msg.contains("depth exceeded"),
        "Error message should mention recursion/depth limit, got: {}",
        err_msg
    );
}

#[test]
fn test_json_token_reduction() {
    let source = include_str!("../../../tests/fixtures/json/nested.json");
    let result = transform(source, Language::Json, Mode::Structure).unwrap();

    // JSON structure should be significantly smaller than original
    assert!(result.len() < source.len());

    // Should achieve reasonable reduction (30-60% depending on structure)
    let reduction_ratio = (source.len() - result.len()) as f64 / source.len() as f64;
    assert!(
        reduction_ratio > 0.30,
        "Expected >30% reduction, got {:.1}%",
        reduction_ratio * 100.0
    );
}

#[test]
fn test_json_large_keys_security() {
    // SECURITY TEST: Ensure JSON with >10,000 keys is rejected
    let mut json = String::from("{");
    for i in 0..10_001 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!("\"key_{}\":{}", i, i));
    }
    json.push('}');

    let result = transform(&json, Language::Json, Mode::Structure);

    assert!(result.is_err(), "Expected error for excessive keys");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("key count exceeded"),
        "Error message should mention key count limit, got: {}",
        err_msg
    );
}

#[test]
fn test_json_top_level_array_of_objects() {
    // Top-level array containing objects should extract first object's structure
    let json = r#"[{"id": 1, "name": "First"}, {"id": 2, "name": "Second"}]"#;
    let result = transform(json, Language::Json, Mode::Structure).unwrap();

    // Should show structure of first object
    assert!(result.contains("id"), "Should contain 'id' key");
    assert!(result.contains("name"), "Should contain 'name' key");
    assert!(!result.contains("First"), "Should not contain values");
    assert!(!result.contains("Second"), "Should not contain values");
}

#[test]
fn test_json_top_level_array_of_primitives() {
    // Top-level array of primitives should return empty array
    let json = r#"[1, 2, 3, "four", true, null]"#;
    let result = transform(json, Language::Json, Mode::Structure).unwrap();

    // Should return empty array representation
    assert_eq!(result.trim(), "[]", "Array of primitives should return []");
}

#[test]
fn test_json_top_level_empty_array() {
    // Empty top-level array
    let json = r#"[]"#;
    let result = transform(json, Language::Json, Mode::Structure).unwrap();

    assert_eq!(result.trim(), "[]", "Empty array should return []");
}

#[test]
fn test_json_primitive_root() {
    // Standalone primitive values at root level
    // These are valid JSON but unusual - handle gracefully
    let test_cases = vec![
        ("42", ""),       // number
        ("\"text\"", ""), // string
        ("true", ""),     // boolean
        ("null", ""),     // null
        ("3.14", ""),     // float
    ];

    for (input, expected) in test_cases {
        let result = transform(input, Language::Json, Mode::Structure).unwrap();
        assert_eq!(
            result.trim(),
            expected,
            "Primitive root '{}' should return empty string, got: '{}'",
            input,
            result
        );
    }
}

#[test]
fn test_json_nested_arrays() {
    // Arrays of arrays (matrix-like structures)
    let json = r#"{"matrix": [[1, 2], [3, 4]], "data": [{"id": 1}]}"#;
    let result = transform(json, Language::Json, Mode::Structure).unwrap();

    // matrix is array of arrays (primitives) - just show key
    assert!(result.contains("matrix"), "Should contain 'matrix' key");
    // data is array of objects - show structure
    assert!(result.contains("data"), "Should contain 'data' key");
    assert!(
        result.contains("id"),
        "Should contain nested 'id' key from data"
    );
    // Should not contain actual values
    assert!(!result.contains("[["), "Should not contain array syntax");
}

// ============================================================================
// YAML Tests
// ============================================================================

#[test]
fn test_yaml_simple_structure() {
    let source = include_str!("../../../tests/fixtures/yaml/simple.yaml");
    let result = transform(source, Language::Yaml, Mode::Structure).unwrap();

    // Should contain keys
    assert!(result.contains("name"));
    assert!(result.contains("age"));
    assert!(result.contains("email"));
    assert!(result.contains("active"));

    // Should NOT contain values
    assert!(!result.contains("John Doe"));
    assert!(!result.contains("30"));
    assert!(!result.contains("john@example.com"));
    assert!(!result.contains("true"));
}

#[test]
fn test_yaml_nested_structure() {
    let source = include_str!("../../../tests/fixtures/yaml/nested.yaml");
    let result = transform(source, Language::Yaml, Mode::Structure).unwrap();

    // Should contain nested keys
    assert!(result.contains("user"));
    assert!(result.contains("name"));
    assert!(result.contains("address"));
    assert!(result.contains("street"));
    assert!(result.contains("city"));
    assert!(result.contains("preferences"));
    assert!(result.contains("theme"));
    assert!(result.contains("notifications"));

    // Should NOT contain values
    assert!(!result.contains("John Doe"));
    assert!(!result.contains("123 Main St"));
    assert!(!result.contains("dark"));
    assert!(!result.contains("Springfield"));
}

#[test]
fn test_yaml_multi_document() {
    let source = include_str!("../../../tests/fixtures/yaml/multi-doc.yaml");
    let result = transform(source, Language::Yaml, Mode::Structure).unwrap();

    // Should contain document separator
    assert!(
        result.contains("---"),
        "Multi-document output should contain ---"
    );

    // Should contain keys from all documents
    assert!(result.contains("apiVersion"));
    assert!(result.contains("kind"));
    assert!(result.contains("metadata"));
    assert!(result.contains("name"));
    assert!(result.contains("data"));
    assert!(result.contains("spec"));
    assert!(result.contains("replicas"));

    // Should NOT contain values
    assert!(!result.contains("ConfigMap"));
    assert!(!result.contains("Secret"));
    assert!(!result.contains("Deployment"));
    assert!(!result.contains("app-config"));
    assert!(!result.contains("value1"));
}

#[test]
fn test_yaml_anchors() {
    let source = include_str!("../../../tests/fixtures/yaml/anchors.yaml");
    let result = transform(source, Language::Yaml, Mode::Structure).unwrap();

    // Should contain keys
    assert!(result.contains("defaults"));
    assert!(result.contains("development"));
    assert!(result.contains("production"));
    assert!(result.contains("adapter"));
    assert!(result.contains("host"));
    assert!(result.contains("database"));

    // Should NOT contain values (anchors are resolved)
    assert!(!result.contains("postgres"));
    assert!(!result.contains("localhost"));
    assert!(!result.contains("dev_db"));
    assert!(!result.contains("prod_db"));
}

#[test]
fn test_yaml_modes_identical() {
    let source = include_str!("../../../tests/fixtures/yaml/simple.yaml");

    // Serde-based modes (Structure/Signatures/Types) all produce the same
    // key-only structure extraction since there's no tree-sitter AST to
    // differentiate modes.
    let structure = transform(source, Language::Yaml, Mode::Structure).unwrap();
    let signatures = transform(source, Language::Yaml, Mode::Signatures).unwrap();
    let types = transform(source, Language::Yaml, Mode::Types).unwrap();

    assert_eq!(structure, signatures);
    assert_eq!(structure, types);

    // Full mode returns original source unchanged (documented contract)
    let full = transform(source, Language::Yaml, Mode::Full).unwrap();
    assert_eq!(full, source);
    assert_ne!(
        full, structure,
        "Full mode should differ from structure extraction"
    );
}

#[test]
fn test_yaml_auto_detection() {
    let source = include_str!("../../../tests/fixtures/yaml/simple.yaml");
    let path = Path::new("config.yaml");

    let result = transform_auto(source, path, Mode::Structure);
    assert!(result.is_ok());

    let content = result.unwrap();
    assert!(content.contains("name"));
    assert!(!content.contains("John Doe"));
}

#[test]
fn test_yaml_auto_detection_yml() {
    let source = include_str!("../../../tests/fixtures/yaml/simple.yaml");
    let path = Path::new("config.yml");

    let result = transform_auto(source, path, Mode::Structure);
    assert!(result.is_ok());

    let content = result.unwrap();
    assert!(content.contains("name"));
}

#[test]
fn test_yaml_invalid() {
    let source = r#"invalid: [unclosed"#;
    let result = transform(source, Language::Yaml, Mode::Structure);

    // Should return error for invalid YAML
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid YAML"));
}

#[test]
fn test_yaml_deeply_nested_security() {
    // SECURITY TEST: Ensure deeply nested YAML is rejected
    // Note: serde_yaml_ng has internal recursion limit (128) which triggers first
    let mut yaml = String::new();
    for i in 0..=200 {
        yaml.push_str(&"  ".repeat(i));
        yaml.push_str(&format!("level{}: \n", i));
    }
    yaml.push_str(&"  ".repeat(201));
    yaml.push_str("value: end");

    let result = transform(&yaml, Language::Yaml, Mode::Structure);

    // Should reject with depth/recursion limit error
    assert!(result.is_err(), "Expected error for deeply nested YAML");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("recursion limit")
            || err_msg.contains("depth exceeded")
            || err_msg.contains("Invalid YAML"),
        "Error message should mention recursion/depth limit, got: {}",
        err_msg
    );
}

#[test]
fn test_yaml_token_reduction() {
    let source = include_str!("../../../tests/fixtures/yaml/nested.yaml");
    let result = transform(source, Language::Yaml, Mode::Structure).unwrap();

    // YAML structure should be significantly smaller than original
    assert!(result.len() < source.len());

    // Should achieve reasonable reduction
    let reduction_ratio = (source.len() - result.len()) as f64 / source.len() as f64;
    assert!(
        reduction_ratio > 0.30,
        "Expected >30% reduction, got {:.1}%",
        reduction_ratio * 100.0
    );
}

#[test]
fn test_yaml_kubernetes_fixture() {
    let source = include_str!("../../../tests/fixtures/yaml/kubernetes.yaml");
    let result = transform(source, Language::Yaml, Mode::Structure).unwrap();

    // Kubernetes manifests should be properly processed
    assert!(result.contains("apiVersion"));
    assert!(result.contains("kind"));
    assert!(result.contains("metadata"));
    assert!(result.contains("spec"));

    // Values should be stripped
    assert!(!result.contains("apps/v1"));
    assert!(!result.contains("Deployment"));
}

#[test]
fn test_yaml_github_actions_fixture() {
    let source = include_str!("../../../tests/fixtures/yaml/github-actions.yaml");
    let result = transform(source, Language::Yaml, Mode::Structure).unwrap();

    // GitHub Actions workflow should be properly processed
    assert!(result.contains("name"));
    assert!(result.contains("on"));
    assert!(result.contains("jobs"));
    assert!(result.contains("steps"));

    // Values should be stripped
    assert!(!result.contains("ubuntu-latest"));
    assert!(!result.contains("actions/checkout"));
}

#[test]
fn test_detect_language_yaml() {
    use rskim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("config.yaml")),
        Some(Language::Yaml)
    );
    assert_eq!(
        detect_language_from_path(Path::new("config.yml")),
        Some(Language::Yaml)
    );
    assert_eq!(
        detect_language_from_path(Path::new("kubernetes/deployment.yaml")),
        Some(Language::Yaml)
    );
}

#[test]
fn test_yaml_large_keys_security() {
    // SECURITY TEST: Ensure YAML with >10,000 keys is rejected
    let mut yaml = String::new();
    for i in 0..10_001 {
        yaml.push_str(&format!("key_{}: {}\n", i, i));
    }

    let result = transform(&yaml, Language::Yaml, Mode::Structure);

    assert!(result.is_err(), "Expected error for excessive keys");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("key count exceeded"),
        "Error message should mention key count limit, got: {}",
        err_msg
    );
}

// ============================================================================
// Minimal Mode Tests
// ============================================================================

// --- TypeScript Minimal ---

#[test]
fn test_typescript_minimal_strips_regular_comments() {
    let source = include_str!("../../../tests/fixtures/typescript/comments.ts");
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // Should NOT contain regular comments
    assert!(
        !result.contains("// FIXTURE:"),
        "Regular single-line comment should be stripped"
    );
    assert!(
        !result.contains("// TESTS:"),
        "Regular single-line comment should be stripped"
    );
    assert!(
        !result.contains("// This is a regular single-line comment"),
        "Regular comment should be stripped"
    );
    assert!(
        !result.contains("/* This is a regular block comment"),
        "Regular block comment should be stripped"
    );
    assert!(
        !result.contains("// Another regular comment"),
        "Regular comment between declarations should be stripped"
    );
    assert!(
        !result.contains("/* Regular block comment at module level"),
        "Module-level block comment should be stripped"
    );
}

#[test]
fn test_typescript_minimal_preserves_jsdoc() {
    let source = include_str!("../../../tests/fixtures/typescript/comments.ts");
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // Should preserve JSDoc comments
    assert!(result.contains("/**"), "JSDoc comments should be preserved");
    assert!(
        result.contains("* This is a JSDoc comment (KEEP)"),
        "JSDoc content should be preserved"
    );
    assert!(
        result.contains("* @param x"),
        "JSDoc @param tags should be preserved"
    );
    assert!(
        result.contains("* A documented class (KEEP)"),
        "Class JSDoc should be preserved"
    );
    assert!(
        result.contains("/** Constructor doc (KEEP) */"),
        "Constructor JSDoc should be preserved"
    );
}

#[test]
fn test_typescript_minimal_preserves_body_comments() {
    let source = include_str!("../../../tests/fixtures/typescript/comments.ts");
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // Should preserve comments inside function bodies
    assert!(
        result.contains("// This comment is inside a function body (KEEP)"),
        "Body comment should be preserved"
    );
    assert!(
        result.contains("// inline comment in body (KEEP)"),
        "Inline body comment should be preserved"
    );
    assert!(
        result.contains("// body comment (KEEP)"),
        "Method body comment should be preserved"
    );
}

#[test]
fn test_typescript_minimal_preserves_all_code() {
    let source = include_str!("../../../tests/fixtures/typescript/comments.ts");
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // Should keep all code intact
    assert!(result.contains("export function add(x: number, y: number): number"));
    assert!(result.contains("const result = x + y;"));
    assert!(result.contains("return result;"));
    assert!(result.contains("export class Calculator"));
    assert!(result.contains("export interface Config"));
    assert!(result.contains("export const VERSION"));
}

#[test]
fn test_typescript_minimal_normalizes_blank_lines() {
    let source = include_str!("../../../tests/fixtures/typescript/comments.ts");
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // The fixture has 4+ blank lines that should be normalized to max 2
    // Check that there are no 3+ consecutive blank lines
    assert!(
        !result.contains("\n\n\n\n"),
        "Should normalize 4+ consecutive blank lines"
    );
}

// --- JavaScript Minimal ---

#[test]
fn test_javascript_minimal_strips_regular_comments() {
    let source = include_str!("../../../tests/fixtures/javascript/comments.js");
    let result = transform(source, Language::JavaScript, Mode::Minimal).unwrap();

    assert!(
        !result.contains("// FIXTURE:"),
        "Regular comment should be stripped"
    );
    assert!(
        !result.contains("/* This is a regular block comment"),
        "Regular block comment should be stripped"
    );
}

#[test]
fn test_javascript_minimal_preserves_jsdoc() {
    let source = include_str!("../../../tests/fixtures/javascript/comments.js");
    let result = transform(source, Language::JavaScript, Mode::Minimal).unwrap();

    assert!(
        result.contains("* This is a JSDoc comment (KEEP)"),
        "JSDoc should be preserved"
    );
    assert!(
        result.contains("* @param {number} x"),
        "JSDoc @param should be preserved"
    );
}

#[test]
fn test_javascript_minimal_preserves_body_comments() {
    let source = include_str!("../../../tests/fixtures/javascript/comments.js");
    let result = transform(source, Language::JavaScript, Mode::Minimal).unwrap();

    assert!(
        result.contains("// This comment is inside a function body (KEEP)"),
        "Body comment should be preserved"
    );
}

// --- Python Minimal ---

#[test]
fn test_python_minimal_strips_regular_comments() {
    let source = include_str!("../../../tests/fixtures/python/comments.py");
    let result = transform(source, Language::Python, Mode::Minimal).unwrap();

    assert!(
        !result.contains("# This is a regular comment at module level"),
        "Regular Python comment should be stripped"
    );
    assert!(
        !result.contains("# Another regular comment"),
        "Regular Python comment should be stripped"
    );
    assert!(
        !result.contains("# Regular comment between functions"),
        "Regular between-function comment should be stripped"
    );
    assert!(
        !result.contains("# Regular module-level comment"),
        "Regular module-level comment should be stripped"
    );
}

#[test]
fn test_python_minimal_preserves_docstrings() {
    let source = include_str!("../../../tests/fixtures/python/comments.py");
    let result = transform(source, Language::Python, Mode::Minimal).unwrap();

    // Python docstrings (triple-quoted strings) should be preserved
    assert!(
        result.contains("\"\"\"Add two numbers together."),
        "Function docstring should be preserved"
    );
    assert!(
        result.contains("\"\"\"A simple calculator class (KEEP).\"\"\""),
        "Class docstring should be preserved"
    );
    assert!(
        result.contains("\"\"\"Initialize calculator (KEEP).\"\"\""),
        "Method docstring should be preserved"
    );
    assert!(
        result.contains("\"\"\"Add to stored value (KEEP).\"\"\""),
        "Method docstring should be preserved"
    );
}

#[test]
fn test_python_minimal_preserves_shebang() {
    let source = include_str!("../../../tests/fixtures/python/comments.py");
    let result = transform(source, Language::Python, Mode::Minimal).unwrap();

    assert!(
        result.starts_with("#!/usr/bin/env python3"),
        "Shebang should be preserved"
    );
}

#[test]
fn test_python_minimal_preserves_body_comments() {
    let source = include_str!("../../../tests/fixtures/python/comments.py");
    let result = transform(source, Language::Python, Mode::Minimal).unwrap();

    assert!(
        result.contains("# This comment is inside a function body (KEEP)"),
        "Body comment should be preserved"
    );
    assert!(
        result.contains("# inline comment in body (KEEP)"),
        "Inline body comment should be preserved"
    );
    assert!(
        result.contains("# body comment (KEEP)"),
        "Method body comment should be preserved"
    );
}

// --- Rust Minimal ---

#[test]
fn test_rust_minimal_strips_regular_comments() {
    let source = include_str!("../../../tests/fixtures/rust/comments.rs");
    let result = transform(source, Language::Rust, Mode::Minimal).unwrap();

    assert!(
        !result.contains("// FIXTURE:"),
        "Regular line comment should be stripped"
    );
    assert!(
        !result.contains("// This is a regular line comment"),
        "Regular comment should be stripped"
    );
    assert!(
        !result.contains("/* This is a regular block comment"),
        "Regular block comment should be stripped"
    );
    assert!(
        !result.contains("// Regular comment between items"),
        "Comment between items should be stripped"
    );
    assert!(
        !result.contains("/* Regular block comment at module level"),
        "Module-level block comment should be stripped"
    );
    assert!(
        !result.contains("// Regular comment (STRIP)"),
        "Regular comment should be stripped"
    );
}

#[test]
fn test_rust_minimal_preserves_doc_comments() {
    let source = include_str!("../../../tests/fixtures/rust/comments.rs");
    let result = transform(source, Language::Rust, Mode::Minimal).unwrap();

    // /// doc comments
    assert!(
        result.contains("/// Function doc comment (KEEP)"),
        "/// doc comment should be preserved"
    );
    assert!(
        result.contains("/// Multi-line doc comment (KEEP)"),
        "/// multi-line doc comment should be preserved"
    );
    assert!(
        result.contains("/// Struct doc comment (KEEP)"),
        "Struct doc comment should be preserved"
    );
    assert!(
        result.contains("/// Field doc comment (KEEP)"),
        "Field doc comment should be preserved"
    );

    // //! inner doc comments
    assert!(
        result.contains("//! Module-level doc comment (KEEP)"),
        "//! inner doc comment should be preserved"
    );
    assert!(
        result.contains("//! Another inner doc comment (KEEP)"),
        "//! inner doc comment should be preserved"
    );
}

#[test]
fn test_rust_minimal_preserves_body_comments() {
    let source = include_str!("../../../tests/fixtures/rust/comments.rs");
    let result = transform(source, Language::Rust, Mode::Minimal).unwrap();

    assert!(
        result.contains("// This comment is inside a function body (KEEP)"),
        "Body comment should be preserved"
    );
    assert!(
        result.contains("// inline comment in body (KEEP)"),
        "Inline body comment should be preserved"
    );
    assert!(
        result.contains("// body comment (KEEP)"),
        "Method body comment should be preserved"
    );
}

// --- Go Minimal ---

#[test]
fn test_go_minimal_strips_standalone_comments() {
    let source = include_str!("../../../tests/fixtures/go/comments.go");
    let result = transform(source, Language::Go, Mode::Minimal).unwrap();

    assert!(
        !result.contains("// FIXTURE:"),
        "Standalone comment should be stripped"
    );
    assert!(
        !result.contains("// TESTS:"),
        "Standalone comment should be stripped"
    );
    assert!(
        !result.contains("// This is a standalone comment"),
        "Standalone comment should be stripped"
    );
    assert!(
        !result.contains("/* This is a standalone block comment"),
        "Standalone block comment should be stripped"
    );
    assert!(
        !result.contains("// Regular comment not adjacent"),
        "Non-adjacent comment should be stripped"
    );
    assert!(
        !result.contains("/* Block comment at module level"),
        "Non-adjacent block comment should be stripped"
    );
    assert!(
        !result.contains("// Standalone comment (STRIP)"),
        "Standalone comment should be stripped"
    );
}

#[test]
fn test_go_minimal_preserves_doc_comments() {
    let source = include_str!("../../../tests/fixtures/go/comments.go");
    let result = transform(source, Language::Go, Mode::Minimal).unwrap();

    // Go doc comments are adjacent to declarations
    assert!(
        result.contains("// Add adds two numbers together."),
        "Doc comment adjacent to func should be preserved"
    );
    assert!(
        result.contains("// This is a Go doc comment adjacent to a declaration (KEEP)."),
        "Multi-line Go doc comment should be preserved"
    );
    assert!(
        result.contains("// Calculator is a simple calculator."),
        "Doc comment adjacent to type should be preserved"
    );
    assert!(
        result.contains("// NewCalculator creates a new Calculator."),
        "Doc comment adjacent to func should be preserved"
    );
    assert!(
        result.contains("// Add adds to the calculator value."),
        "Doc comment adjacent to method should be preserved"
    );
}

#[test]
fn test_go_minimal_preserves_body_comments() {
    let source = include_str!("../../../tests/fixtures/go/comments.go");
    let result = transform(source, Language::Go, Mode::Minimal).unwrap();

    assert!(
        result.contains("// This comment is inside a function body (KEEP)"),
        "Body comment should be preserved"
    );
    assert!(
        result.contains("// inline comment in body (KEEP)"),
        "Inline body comment should be preserved"
    );
    assert!(
        result.contains("// body comment (KEEP)"),
        "Method body comment should be preserved"
    );
}

// --- Java Minimal ---

#[test]
fn test_java_minimal_strips_regular_comments() {
    let source = include_str!("../../../tests/fixtures/java/Comments.java");
    let result = transform(source, Language::Java, Mode::Minimal).unwrap();

    assert!(
        !result.contains("// FIXTURE:"),
        "Regular comment should be stripped"
    );
    assert!(
        !result.contains("// This is a regular single-line comment"),
        "Regular single-line comment should be stripped"
    );
    assert!(
        !result.contains("/* This is a regular block comment"),
        "Regular block comment should be stripped"
    );
    assert!(
        !result.contains("// Regular comment inside class but outside method"),
        "Class-level comment should be stripped"
    );
    assert!(
        !result.contains("/* Regular block comment at top level"),
        "Top-level block comment should be stripped"
    );
    assert!(
        !result.contains("// Regular comment (STRIP)"),
        "Regular comment should be stripped"
    );
}

#[test]
fn test_java_minimal_preserves_javadoc() {
    let source = include_str!("../../../tests/fixtures/java/Comments.java");
    let result = transform(source, Language::Java, Mode::Minimal).unwrap();

    assert!(
        result.contains("* This is a Javadoc comment (KEEP)"),
        "Javadoc should be preserved"
    );
    assert!(
        result.contains("* @author test"),
        "Javadoc @author should be preserved"
    );
    assert!(
        result.contains("* Constructor Javadoc (KEEP)"),
        "Constructor Javadoc should be preserved"
    );
    assert!(
        result.contains("* Add method Javadoc (KEEP)"),
        "Method Javadoc should be preserved"
    );
}

#[test]
fn test_java_minimal_preserves_body_comments() {
    let source = include_str!("../../../tests/fixtures/java/Comments.java");
    let result = transform(source, Language::Java, Mode::Minimal).unwrap();

    assert!(
        result.contains("// This comment is inside a method body (KEEP)"),
        "Constructor body comment should be preserved"
    );
    assert!(
        result.contains("// inline comment in body (KEEP)"),
        "Inline body comment should be preserved"
    );
    assert!(
        result.contains("// body comment (KEEP)"),
        "Method body comment should be preserved"
    );
}

// --- Passthrough Tests ---

#[test]
fn test_json_minimal_passthrough() {
    let source = include_str!("../../../tests/fixtures/json/simple.json");
    let result = transform(source, Language::Json, Mode::Minimal).unwrap();

    // JSON should be returned unchanged
    assert_eq!(
        result, source,
        "JSON should pass through unchanged in minimal mode"
    );
}

#[test]
fn test_yaml_minimal_passthrough() {
    let source = include_str!("../../../tests/fixtures/yaml/simple.yaml");
    let result = transform(source, Language::Yaml, Mode::Minimal).unwrap();

    // YAML should be returned unchanged
    assert_eq!(
        result, source,
        "YAML should pass through unchanged in minimal mode"
    );
}

#[test]
fn test_markdown_minimal_passthrough() {
    let source = include_str!("../../../tests/fixtures/markdown/simple.md");
    let result = transform(source, Language::Markdown, Mode::Minimal).unwrap();

    // Markdown should be returned unchanged
    assert_eq!(
        result, source,
        "Markdown should pass through unchanged in minimal mode"
    );
}

// --- Edge Cases ---

#[test]
fn test_minimal_empty_file() {
    let source = "";
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();
    assert_eq!(result, "");
}

#[test]
fn test_minimal_no_comments() {
    let source = "export function add(a: number, b: number): number {\n    return a + b;\n}\n";
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // No comments to strip, should be essentially unchanged
    assert!(result.contains("export function add"));
    assert!(result.contains("return a + b;"));
}

#[test]
fn test_minimal_only_comments() {
    let source = "// comment 1\n// comment 2\n// comment 3\n";
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // All comments stripped, result should be mostly empty (just blank lines from normalization)
    assert!(
        !result.contains("// comment"),
        "All comments should be stripped"
    );
}

#[test]
fn test_minimal_blank_line_normalization() {
    let source = "const a = 1;\n\n\n\n\nconst b = 2;\n";
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // 5 blank lines should be normalized to 2
    assert!(
        !result.contains("\n\n\n\n"),
        "4+ consecutive blank lines should be normalized"
    );
    assert!(result.contains("const a = 1;"));
    assert!(result.contains("const b = 2;"));
}

#[test]
fn test_minimal_preserves_inline_body_comment_with_code() {
    let source = "function test() {\n    const x = 1; // important note\n}\n";
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    assert!(
        result.contains("// important note"),
        "Inline comment in function body should be preserved"
    );
}

#[test]
fn test_minimal_token_reduction() {
    let source = include_str!("../../../tests/fixtures/typescript/comments.ts");
    let result = transform(source, Language::TypeScript, Mode::Minimal).unwrap();

    // Minimal mode should reduce tokens (by removing comments)
    assert!(
        result.len() < source.len(),
        "Minimal mode should reduce file size: original={}, result={}",
        source.len(),
        result.len()
    );
}

#[test]
fn test_python_minimal_nested_function_body_comments() {
    let source = "# top-level comment\ndef outer():\n    # comment in outer body\n    def inner():\n        # comment in inner body\n        return 1\n    return inner()\n";
    let result = transform(source, Language::Python, Mode::Minimal).unwrap();

    assert!(
        !result.contains("top-level comment"),
        "top-level should be stripped"
    );
    assert!(
        result.contains("comment in outer body"),
        "outer body comment should be preserved"
    );
    assert!(
        result.contains("comment in inner body"),
        "inner body comment should be preserved"
    );
}

#[test]
fn test_python_minimal_class_level_comments_stripped() {
    let source = "class Foo:\n    # class-level comment\n    def bar(self):\n        # body comment\n        pass\n";
    let result = transform(source, Language::Python, Mode::Minimal).unwrap();

    assert!(
        !result.contains("class-level comment"),
        "class-level should be stripped"
    );
    assert!(
        result.contains("body comment"),
        "method body comment should be preserved"
    );
}

// ============================================================================
// Max Lines (AST-aware truncation) Tests
// ============================================================================

#[test]
fn test_max_lines_output_never_exceeds_limit() {
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(5);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 5,
        "Output should be at most 5 lines, got {}: {:?}",
        line_count,
        result,
    );
}

#[test]
fn test_max_lines_types_preferred_over_functions() {
    // With structure mode and small max_lines, types should be prioritized
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(10);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    // Types and interfaces should be present (priority 5)
    let has_type = result.contains("type UserId")
        || result.contains("type ApiResponse")
        || result.contains("interface User")
        || result.contains("interface UserService");

    assert!(
        has_type,
        "Should contain type definitions with max_lines=10: {:?}",
        result,
    );

    let line_count = result.lines().count();
    assert!(
        line_count <= 10,
        "Output should be at most 10 lines, got {}",
        line_count,
    );
}

#[test]
fn test_max_lines_none_returns_full_output() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");

    let result_full = transform(source, Language::TypeScript, Mode::Structure).unwrap();
    let config = TransformConfig::with_mode(Mode::Structure);
    let result_none = transform_with_config(source, Language::TypeScript, &config).unwrap();

    assert_eq!(
        result_full, result_none,
        "No max_lines should return identical output to transform()"
    );
}

#[test]
fn test_max_lines_short_file_passes_through() {
    let source = "function add(a: number, b: number): number { return a + b; }\n";
    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(100);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    // File shorter than max_lines should pass through unchanged (minus body replacement)
    assert!(
        result.contains("function add"),
        "Short file should pass through: {:?}",
        result,
    );
}

#[test]
fn test_max_lines_with_signatures_mode() {
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Signatures).with_max_lines(3);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 3,
        "Signatures mode with max_lines=3 should produce at most 3 lines, got {}: {:?}",
        line_count,
        result,
    );
}

#[test]
fn test_max_lines_with_types_mode() {
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Types).with_max_lines(5);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 5,
        "Types mode with max_lines=5 should produce at most 5 lines, got {}",
        line_count,
    );
}

#[test]
fn test_max_lines_with_full_mode() {
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Full).with_max_lines(5);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 5,
        "Full mode with max_lines=5 should produce at most 5 lines, got {}",
        line_count,
    );
}

#[test]
fn test_max_lines_with_minimal_mode() {
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Minimal).with_max_lines(5);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 5,
        "Minimal mode with max_lines=5 should produce at most 5 lines, got {}",
        line_count,
    );
}

#[test]
fn test_max_lines_python() {
    let source = include_str!("../../../tests/fixtures/python/mixed_priority.py");
    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(5);
    let result = transform_with_config(source, Language::Python, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 5,
        "Python max_lines=5 should produce at most 5 lines, got {}: {:?}",
        line_count,
        result,
    );

    // Python omission markers should use # syntax
    if result.contains("(truncated)") {
        assert!(
            result.contains("# ..."),
            "Python omission markers should use # syntax: {:?}",
            result,
        );
    }
}

#[test]
fn test_max_lines_rust() {
    let source = include_str!("../../../tests/fixtures/rust/mixed_priority.rs");
    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(5);
    let result = transform_with_config(source, Language::Rust, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 5,
        "Rust max_lines=5 should produce at most 5 lines, got {}: {:?}",
        line_count,
        result,
    );

    // Rust omission markers should use // syntax
    if result.contains("(truncated)") {
        assert!(
            result.contains("// ..."),
            "Rust omission markers should use // syntax: {:?}",
            result,
        );
    }
}

#[test]
fn test_max_lines_json_simple_truncation() {
    let source = r#"{
  "users": [
    {"id": 1, "name": "Alice"},
    {"id": 2, "name": "Bob"}
  ],
  "config": {
    "host": "localhost",
    "port": 3000,
    "debug": true
  }
}"#;

    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(3);
    let result = transform_with_config(source, Language::Json, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 3,
        "JSON max_lines=3 should produce at most 3 lines, got {}: {:?}",
        line_count,
        result,
    );
}

#[test]
fn test_max_lines_yaml_simple_truncation() {
    let source =
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: myservice\n  labels:\n    app: myapp\n";

    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(3);
    let result = transform_with_config(source, Language::Yaml, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 3,
        "YAML max_lines=3 should produce at most 3 lines, got {}: {:?}",
        line_count,
        result,
    );
}

#[test]
fn test_max_lines_1_returns_at_least_one_meaningful_line() {
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(1);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    let line_count = result.lines().count();
    assert!(
        line_count <= 1,
        "max_lines=1 should produce at most 1 line, got {}: {:?}",
        line_count,
        result,
    );
    assert!(
        !result.trim().is_empty(),
        "max_lines=1 should produce at least one meaningful line"
    );
}

#[test]
fn test_max_lines_omission_markers_present() {
    let source = include_str!("../../../tests/fixtures/typescript/mixed_priority.ts");
    let config = TransformConfig::with_mode(Mode::Structure).with_max_lines(5);
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();

    // The file is large enough that truncation should produce omission markers
    assert!(
        result.contains("// ... (truncated)"),
        "Should contain TypeScript omission markers: {:?}",
        result,
    );
}

// ============================================================================
// Token Budget Truncation (public API wrapper)
// ============================================================================

// Exercises the public `rskim_core::truncate_to_token_budget` wrapper to
// ensure the delegation to the internal implementation is not broken.

fn count_words(s: &str) -> usize {
    s.split_whitespace().count()
}

#[test]
fn test_truncate_to_token_budget_public_api_no_truncation() {
    let text = "line one\nline two\nline three\n";
    let result = truncate_to_token_budget(text, Language::TypeScript, 100, count_words, None)
        .expect("truncate_to_token_budget should succeed");
    assert_eq!(
        result, text,
        "Text within budget should be returned unchanged"
    );
}

#[test]
fn test_truncate_to_token_budget_public_api_truncates_over_budget() {
    let text = "word1 word2\nword3 word4\nword5 word6\nword7 word8\n";
    let result = truncate_to_token_budget(text, Language::TypeScript, 6, count_words, None)
        .expect("truncate_to_token_budget should succeed");
    let token_count = count_words(&result);
    assert!(
        token_count <= 6,
        "Output should have at most 6 word-tokens, got {}: {:?}",
        token_count,
        result,
    );
    assert!(
        result.contains("truncated"),
        "Truncated output should contain omission marker: {:?}",
        result,
    );
}

// ============================================================================
// C Tests
// ============================================================================

#[test]
fn test_c_structure() {
    let source = include_str!("../../../tests/fixtures/c/simple.c");
    let result = transform(source, Language::C, Mode::Structure).unwrap();

    // Should contain function signatures
    assert!(result.contains("int add"));
    assert!(result.contains("void greet"));

    // Should NOT contain implementation
    assert!(!result.contains("return a + b"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_c_signatures() {
    let source = include_str!("../../../tests/fixtures/c/simple.c");
    let result = transform(source, Language::C, Mode::Signatures).unwrap();

    // Should contain function signatures
    assert!(result.contains("int add(int a, int b)"));
    assert!(result.contains("void greet(const char* name)"));

    // Should NOT contain function bodies
    assert!(!result.contains("return a + b"));
    assert!(!result.contains("printf"));
}

#[test]
fn test_c_types() {
    let source = include_str!("../../../tests/fixtures/c/types.c");
    let result = transform(source, Language::C, Mode::Types).unwrap();

    // Should contain type definitions
    assert!(result.contains("struct Person"));
    assert!(result.contains("enum Status"));
    assert!(result.contains("typedef"));

    // Should contain anonymous struct typedef (typedef struct { ... } Vector3)
    assert!(
        result.contains("Vector3"),
        "Should extract anonymous struct typedef, got: {}",
        result
    );

    // Should contain typedef enum (typedef enum { ... } LogLevel)
    assert!(
        result.contains("LogLevel"),
        "Should extract typedef enum, got: {}",
        result
    );

    // NOTE: union Value is not extracted because TypeNodeTypes does not include
    // union_specifier. This is tracked as a known gap, not a test failure.
}

#[test]
fn test_c_full() {
    let source = include_str!("../../../tests/fixtures/c/simple.c");
    let result = transform(source, Language::C, Mode::Full).unwrap();

    // Should be identical to input
    assert_eq!(result, source);
}

#[test]
fn test_c_minimal_strips_comments() {
    let source = include_str!("../../../tests/fixtures/c/comments.c");
    let result = transform(source, Language::C, Mode::Minimal).unwrap();

    // Should strip standalone comments
    assert!(
        !result.contains("This is a standalone comment"),
        "Standalone comments should be stripped"
    );
    assert!(
        !result.contains("Block comment at module level"),
        "Block comments at module level should be stripped"
    );
    assert!(
        !result.contains("Regular comment between functions"),
        "Regular comments should be stripped"
    );

    // Should keep all code
    assert!(result.contains("int add(int a, int b)"));
    assert!(result.contains("void greet(const char* name)"));

    // Should keep body comments
    assert!(result.contains("inside a function body"));
}

#[test]
fn test_c_minimal_preserves_doxygen() {
    let source = include_str!("../../../tests/fixtures/c/comments.c");
    let result = transform(source, Language::C, Mode::Minimal).unwrap();

    // Should preserve Doxygen /** comments
    assert!(
        result.contains("Adds two integers together"),
        "Doxygen comments should be preserved"
    );

    // Should preserve /// doc comments
    assert!(
        result.contains("Doxygen single-line doc comment"),
        "Doxygen /// comments should be preserved"
    );
}

#[test]
fn test_c_auto_detection() {
    let source = include_str!("../../../tests/fixtures/c/simple.c");

    // .c extension
    let result = transform_auto(source, Path::new("main.c"), Mode::Structure);
    assert!(result.is_ok());
    assert!(result.unwrap().contains("int add"));

    // .h extension
    let result = transform_auto(source, Path::new("main.h"), Mode::Structure);
    assert!(result.is_ok());
    assert!(result.unwrap().contains("int add"));
}

// ============================================================================
// C++ Tests
// ============================================================================

#[test]
fn test_cpp_structure() {
    let source = include_str!("../../../tests/fixtures/cpp/simple.cpp");
    let result = transform(source, Language::Cpp, Mode::Structure).unwrap();

    // Should contain function signatures
    assert!(result.contains("int add"));
    assert!(result.contains("greet"));

    // Should NOT contain implementation
    assert!(!result.contains("return a + b"));
    assert!(result.contains("{ /* ... */ }"));
}

#[test]
fn test_cpp_signatures() {
    let source = include_str!("../../../tests/fixtures/cpp/simple.cpp");
    let result = transform(source, Language::Cpp, Mode::Signatures).unwrap();

    // Should contain function signatures
    assert!(result.contains("int add(int a, int b)"));
    assert!(result.contains("greet"));

    // Should NOT contain function bodies
    assert!(!result.contains("return a + b"));
}

#[test]
fn test_cpp_types() {
    let source = include_str!("../../../tests/fixtures/cpp/types.cpp");
    let result = transform(source, Language::Cpp, Mode::Types).unwrap();

    // Should contain type definitions
    assert!(result.contains("struct Point"));
    assert!(result.contains("enum class Status"));
    assert!(result.contains("class Animal"));

    // Should contain template class (template<typename T> class Container)
    assert!(
        result.contains("class Container"),
        "Should extract template class, got: {}",
        result
    );

    // Should contain namespace-scoped types (shapes::Circle, shapes::Rectangle)
    assert!(
        result.contains("struct Circle"),
        "Should extract namespace-scoped Circle struct, got: {}",
        result
    );
    assert!(
        result.contains("struct Rectangle"),
        "Should extract namespace-scoped Rectangle struct, got: {}",
        result
    );
}

#[test]
fn test_cpp_full() {
    let source = include_str!("../../../tests/fixtures/cpp/simple.cpp");
    let result = transform(source, Language::Cpp, Mode::Full).unwrap();

    // Should be identical to input
    assert_eq!(result, source);
}

#[test]
fn test_cpp_minimal_strips_comments() {
    let source = include_str!("../../../tests/fixtures/cpp/comments.cpp");
    let result = transform(source, Language::Cpp, Mode::Minimal).unwrap();

    // Should strip standalone comments
    assert!(
        !result.contains("This is a standalone comment"),
        "Standalone comments should be stripped"
    );
    assert!(
        !result.contains("Block comment at module level"),
        "Block comments at module level should be stripped"
    );
    assert!(
        !result.contains("Regular comment between declarations"),
        "Regular comments should be stripped"
    );

    // Should keep all code
    assert!(result.contains("class Calculator"));
    assert!(result.contains("greet"));

    // Should keep body comments
    assert!(result.contains("body comment"));
}

#[test]
fn test_cpp_minimal_preserves_doxygen() {
    let source = include_str!("../../../tests/fixtures/cpp/comments.cpp");
    let result = transform(source, Language::Cpp, Mode::Minimal).unwrap();

    // Should preserve Doxygen /** comments
    assert!(
        result.contains("A simple calculator class"),
        "Doxygen class comments should be preserved"
    );

    // Should preserve /// doc comments
    assert!(
        result.contains("Add a value"),
        "Doxygen /// comments should be preserved"
    );
    assert!(
        result.contains("Greet a person by name"),
        "Doxygen /// comments should be preserved"
    );
}

#[test]
fn test_cpp_auto_detection() {
    let source = include_str!("../../../tests/fixtures/cpp/simple.cpp");

    // All C++ extensions should detect as C++ and produce valid output
    let extensions = [
        "main.cpp", "main.cc", "main.cxx", "main.hpp", "main.hxx", "main.hh",
    ];
    for ext in extensions {
        let result = transform_auto(source, Path::new(ext), Mode::Structure).unwrap();
        assert!(
            result.contains("int add"),
            "Extension '{}' should produce valid C++ output",
            ext
        );
    }
}

// ============================================================================
// C / C++ Malformed Syntax Tests (tree-sitter error tolerance)
// ============================================================================

#[test]
fn test_c_malformed_syntax() {
    // tree-sitter is error-tolerant, should not crash on broken C code
    let sources = [
        "int main( { { {",                 // Unclosed braces/parens
        "struct Foo { int x",              // Incomplete struct
        "void func(int a, int",            // Incomplete parameter list
        "#include <missing\nint broken(;", // Incomplete include + bad declaration
        "typedef struct {",                // Incomplete typedef
    ];
    for source in sources {
        let result = transform(source, Language::C, Mode::Structure);
        assert!(
            result.is_ok(),
            "C malformed syntax should not crash, input: '{}', error: {:?}",
            source,
            result.err()
        );
    }
}

#[test]
fn test_cpp_malformed_syntax() {
    // tree-sitter is error-tolerant, should not crash on broken C++ code
    let sources = [
        "class Foo { public:",             // Incomplete class
        "template<typename T",             // Incomplete template
        "namespace ns { struct S { int x", // Unclosed namespace + struct
        "void func() override {",          // Incomplete override function
        "enum class Status { A, B,",       // Incomplete enum class
    ];
    for source in sources {
        let result = transform(source, Language::Cpp, Mode::Structure);
        assert!(
            result.is_ok(),
            "C++ malformed syntax should not crash, input: '{}', error: {:?}",
            source,
            result.err()
        );
    }
}

// ============================================================================
// TOML Tests
// ============================================================================

#[test]
fn test_toml_simple_structure() {
    let source = include_str!("../../../tests/fixtures/toml/simple.toml");
    let result = transform(source, Language::Toml, Mode::Structure).unwrap();

    // Should contain keys
    assert!(result.contains("name"));
    assert!(result.contains("version"));
    assert!(result.contains("description"));
    assert!(result.contains("active"));

    // Should NOT contain values
    assert!(!result.contains("my-project"));
    assert!(!result.contains("1.0.0"));
    assert!(!result.contains("A sample project"));
}

#[test]
fn test_toml_nested() {
    let source = include_str!("../../../tests/fixtures/toml/nested.toml");
    let result = transform(source, Language::Toml, Mode::Structure).unwrap();

    // Should contain nested keys
    assert!(result.contains("package"));
    assert!(result.contains("server"));
    assert!(result.contains("host"));
    assert!(result.contains("port"));
    assert!(result.contains("database"));
    assert!(result.contains("url"));
    assert!(result.contains("pool_size"));

    // Should NOT contain values
    assert!(!result.contains("rskim"));
    assert!(!result.contains("localhost"));
    assert!(!result.contains("8080"));
    assert!(!result.contains("postgres://"));
}

#[test]
fn test_toml_arrays() {
    let source = include_str!("../../../tests/fixtures/toml/array.toml");
    let result = transform(source, Language::Toml, Mode::Structure).unwrap();

    // Should contain keys
    assert!(result.contains("tags"));
    assert!(result.contains("users"));
    assert!(result.contains("servers"));

    // Should NOT contain values
    assert!(!result.contains("rust"));
    assert!(!result.contains("Alice"));
    assert!(!result.contains("Bob"));
}

#[test]
fn test_toml_edge_cases() {
    let source = include_str!("../../../tests/fixtures/toml/edge.toml");
    let result = transform(source, Language::Toml, Mode::Structure).unwrap();

    // Should contain section keys
    assert!(result.contains("dates"));
    assert!(result.contains("inline"));
    assert!(result.contains("unicode"));
    assert!(result.contains("deeply_nested"));
    assert!(result.contains("multiline"));

    // Should contain inline table keys
    assert!(result.contains("point"));
    assert!(result.contains("color"));
}

#[test]
fn test_toml_modes_identical() {
    let source = include_str!("../../../tests/fixtures/toml/simple.toml");

    // TOML serde-based modes (Structure/Signatures/Types) all produce the same
    // key-only structure extraction since there's no tree-sitter AST to
    // differentiate modes.
    let structure = transform(source, Language::Toml, Mode::Structure).unwrap();
    let signatures = transform(source, Language::Toml, Mode::Signatures).unwrap();
    let types = transform(source, Language::Toml, Mode::Types).unwrap();

    assert_eq!(structure, signatures);
    assert_eq!(structure, types);

    // Full mode returns original source unchanged (documented contract)
    let full = transform(source, Language::Toml, Mode::Full).unwrap();
    assert_eq!(full, source);
    assert_ne!(
        full, structure,
        "Full mode should differ from structure extraction"
    );
}

#[test]
fn test_toml_invalid() {
    let source = "[invalid";
    let result = transform(source, Language::Toml, Mode::Structure);

    // Should return error for invalid TOML
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid TOML"));
}

#[test]
fn test_toml_minimal_passthrough() {
    let source = include_str!("../../../tests/fixtures/toml/simple.toml");

    // Minimal mode should passthrough TOML unchanged (like JSON/YAML)
    let result = transform(source, Language::Toml, Mode::Minimal).unwrap();
    assert_eq!(result, source);
}

#[test]
fn test_toml_auto_detection() {
    let source = include_str!("../../../tests/fixtures/toml/simple.toml");
    let path = Path::new("config.toml");

    let result = transform_auto(source, path, Mode::Structure);
    assert!(result.is_ok());

    let content = result.unwrap();
    assert!(content.contains("name"));
    assert!(!content.contains("my-project"));
}

#[test]
fn test_toml_token_reduction() {
    let source = include_str!("../../../tests/fixtures/toml/nested.toml");
    let result = transform(source, Language::Toml, Mode::Structure).unwrap();

    // TOML structure should be smaller than original
    assert!(result.len() < source.len());
}

#[test]
fn test_toml_deeply_nested_security() {
    // SECURITY TEST: Ensure deeply nested TOML is rejected (depth > 500)
    // Build nested inline tables: key = { nested = { nested = { ... } } }
    // Note: the toml crate may have its own recursion limit that fires
    // before our MAX_TOML_DEPTH of 500. Either error is acceptable.
    let mut toml_str = String::from("key = ");
    for _ in 0..550 {
        toml_str.push_str("{ nested = ");
    }
    toml_str.push_str("\"value\"");
    for _ in 0..550 {
        toml_str.push_str(" }");
    }

    let result = transform(&toml_str, Language::Toml, Mode::Structure);

    // Should reject with depth or parse error
    assert!(result.is_err(), "Expected error for deeply nested TOML");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("depth exceeded")
            || err_msg.contains("recursion limit")
            || err_msg.contains("Invalid TOML"),
        "Error message should mention depth/recursion limit or parse error, got: {}",
        err_msg
    );
}

// ============================================================================
// Language Detection Tests - C, C++, TOML
// ============================================================================

#[test]
fn test_detect_c_extensions() {
    use rskim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("main.c")),
        Some(Language::C)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.h")),
        Some(Language::C)
    );
}

#[test]
fn test_detect_cpp_extensions() {
    use rskim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("main.cpp")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("main.cc")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("main.cxx")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.hpp")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.hxx")),
        Some(Language::Cpp)
    );
    assert_eq!(
        detect_language_from_path(Path::new("header.hh")),
        Some(Language::Cpp)
    );
}

#[test]
fn test_detect_toml_extension() {
    use rskim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("Cargo.toml")),
        Some(Language::Toml)
    );
}

// ============================================================================
// Pseudo Mode Integration Tests
// ============================================================================

#[test]
fn test_typescript_pseudo() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let result = transform(source, Language::TypeScript, Mode::Pseudo).unwrap();

    // Should strip type annotations
    assert!(
        !result.contains(": number"),
        "type annotations should be stripped"
    );
    assert!(
        !result.contains(": string"),
        "type annotations should be stripped"
    );

    // Should strip export keyword
    assert!(
        !result.contains("export "),
        "export keyword should be stripped"
    );

    // Should preserve function names and logic
    assert!(
        result.contains("function add"),
        "function names should be preserved"
    );
    assert!(
        result.contains("function greet"),
        "function names should be preserved"
    );
    assert!(
        result.contains("return"),
        "return statements should be preserved"
    );
}

#[test]
fn test_python_pseudo() {
    let source = include_str!("../../../tests/fixtures/python/simple.py");
    let result = transform(source, Language::Python, Mode::Pseudo).unwrap();

    // Should strip type annotations
    assert!(
        !result.contains(": int"),
        "type annotations should be stripped"
    );
    assert!(
        !result.contains(": str"),
        "type annotations should be stripped"
    );
    assert!(
        !result.contains("-> int"),
        "return types should be stripped"
    );
    assert!(
        !result.contains("-> str"),
        "return types should be stripped"
    );

    // Should strip self parameter
    assert!(
        !result.contains("self,"),
        "self parameter should be stripped"
    );

    // Should preserve function names and logic
    assert!(
        result.contains("def calculate_sum"),
        "function names preserved"
    );
    assert!(
        result.contains("def greet_user"),
        "function names preserved"
    );
    assert!(result.contains("class Calculator"), "class preserved");
}

#[test]
fn test_rust_pseudo() {
    let source = include_str!("../../../tests/fixtures/rust/simple.rs");
    let result = transform(source, Language::Rust, Mode::Pseudo).unwrap();

    // Should strip visibility modifiers
    assert!(
        !result.contains("pub fn"),
        "pub modifier should be stripped from functions"
    );

    // Should preserve function names and logic
    assert!(result.contains("fn add"), "function names preserved");
    assert!(result.contains("fn greet"), "function names preserved");
    assert!(result.contains("struct Calculator"), "struct preserved");
    assert!(result.contains("impl Calculator"), "impl preserved");
}

#[test]
fn test_java_pseudo() {
    let source = include_str!("../../../tests/fixtures/java/Simple.java");
    let result = transform(source, Language::Java, Mode::Pseudo).unwrap();

    // Should strip visibility modifiers
    assert!(
        !result.contains("public class"),
        "public modifier should be stripped"
    );
    assert!(
        !result.contains("private int"),
        "private modifier should be stripped"
    );

    // Should preserve class name and methods
    assert!(result.contains("class Simple"), "class name preserved");
    assert!(result.contains("int add(int a, int b)"), "method preserved");
}

#[test]
fn test_c_pseudo() {
    let source = include_str!("../../../tests/fixtures/c/simple.c");
    let result = transform(source, Language::C, Mode::Pseudo).unwrap();

    // Should preserve function names
    assert!(result.contains("int add"), "function preserved");
    assert!(result.contains("void greet"), "function preserved");

    // Should preserve struct
    assert!(result.contains("struct Point"), "struct preserved");
}

#[test]
fn test_cpp_pseudo() {
    let source = include_str!("../../../tests/fixtures/cpp/simple.cpp");
    let result = transform(source, Language::Cpp, Mode::Pseudo).unwrap();

    // Should strip access specifiers
    assert!(
        !result.contains("public:"),
        "access specifier should be stripped"
    );
    assert!(
        !result.contains("private:"),
        "access specifier should be stripped"
    );

    // Should preserve class structure
    assert!(result.contains("class Calculator"), "class preserved");
    assert!(result.contains("class Shape"), "class preserved");
}

#[test]
fn test_go_pseudo() {
    let source = include_str!("../../../tests/fixtures/go/simple.go");
    let result = transform(source, Language::Go, Mode::Pseudo).unwrap();

    // Go pseudo is conservative — should preserve most code
    assert!(result.contains("func Add"), "function preserved");
    assert!(result.contains("func Greet"), "function preserved");
    assert!(
        result.contains("type Calculator struct"),
        "struct preserved"
    );
}

#[test]
fn test_pseudo_passthrough_for_json() {
    // JSON should pass through unchanged in pseudo mode
    let source = "{\"name\": \"test\", \"value\": 42}";
    let result = transform(source, Language::Json, Mode::Pseudo).unwrap();
    assert_eq!(
        result, source,
        "JSON should pass through unchanged in pseudo mode"
    );
}

#[test]
fn test_pseudo_passthrough_for_yaml() {
    let source = "name: test\nvalue: 42\n";
    let result = transform(source, Language::Yaml, Mode::Pseudo).unwrap();
    assert_eq!(
        result, source,
        "YAML should pass through unchanged in pseudo mode"
    );
}

#[test]
fn test_pseudo_passthrough_for_toml() {
    let source = "name = \"test\"\nvalue = 42\n";
    let result = transform(source, Language::Toml, Mode::Pseudo).unwrap();
    assert_eq!(
        result, source,
        "TOML should pass through unchanged in pseudo mode"
    );
}

#[test]
fn test_pseudo_with_config() {
    let config = TransformConfig::with_mode(Mode::Pseudo);
    let source = "export function add(a: number, b: number): number { return a + b; }";
    let result = transform_with_config(source, Language::TypeScript, &config).unwrap();
    assert!(!result.contains("export"), "export stripped via config API");
    assert!(
        !result.contains(": number"),
        "type annotations stripped via config API"
    );
}

#[test]
fn test_pseudo_auto_detection() {
    let source = "pub fn hello() -> String { \"world\".to_string() }\n";
    let result = transform_auto(source, Path::new("test.rs"), Mode::Pseudo).unwrap();
    assert!(
        !result.contains("pub "),
        "visibility stripped via auto-detection"
    );
    assert!(result.contains("fn hello"), "function preserved");
}
