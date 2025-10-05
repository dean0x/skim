//! Integration tests for skim-core
//!
//! These tests validate the full pipeline from source → transformation.
//! They use `todo!()` implementations initially, and will be enabled
//! once the core logic is implemented.

use skim_core::{transform, transform_auto, Language, Mode};
use std::path::Path;

#[test]
fn test_transform_typescript_structure() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let result = transform(source, Language::TypeScript, Mode::Structure);

    // Should succeed once implemented
    assert!(result.is_ok());

    let output = result.unwrap();

    // Should contain signatures
    assert!(output.contains("function add"));
    assert!(output.contains("number"));

    // Should NOT contain implementation
    assert!(!output.contains("return a + b"));
}

#[test]
fn test_transform_auto_detection() {
    let source = include_str!("../../../tests/fixtures/typescript/simple.ts");
    let path = Path::new("simple.ts");

    let result = transform_auto(source, path, Mode::Structure);

    assert!(result.is_ok());
}

#[test]
fn test_detect_language_from_path() {
    use skim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("test.ts")),
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
}

#[test]
fn test_unsupported_language() {
    use skim_core::detect_language_from_path;

    assert_eq!(
        detect_language_from_path(Path::new("unknown.xyz")),
        None
    );
}
