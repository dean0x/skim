//! Integration tests for rskim-core
//!
//! These tests validate the full pipeline from source → transformation.

#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests

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
}

#[test]
fn test_unsupported_language() {
    use rskim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("unknown.xyz")),
        None
    );
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
