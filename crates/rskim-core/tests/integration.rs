//! Integration tests for rskim-core
//!
//! These tests validate the full pipeline from source → transformation.

#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests
#![allow(clippy::expect_used)] // Expect is acceptable in tests

use rskim_core::{transform, transform_auto, Language, Mode};
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

    // All modes should produce same output for JSON (modes don't apply)
    let structure = transform(source, Language::Json, Mode::Structure).unwrap();
    let signatures = transform(source, Language::Json, Mode::Signatures).unwrap();
    let types = transform(source, Language::Json, Mode::Types).unwrap();
    let full = transform(source, Language::Json, Mode::Full).unwrap();

    // NOTE: JSON ignores mode parameter, always does structure extraction
    assert_eq!(structure, signatures);
    assert_eq!(structure, types);
    assert_eq!(structure, full);
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

    // All modes should produce same output for YAML (modes don't apply)
    let structure = transform(source, Language::Yaml, Mode::Structure).unwrap();
    let signatures = transform(source, Language::Yaml, Mode::Signatures).unwrap();
    let types = transform(source, Language::Yaml, Mode::Types).unwrap();
    let full = transform(source, Language::Yaml, Mode::Full).unwrap();

    // NOTE: YAML ignores mode parameter, always does structure extraction
    assert_eq!(structure, signatures);
    assert_eq!(structure, types);
    assert_eq!(structure, full);
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
