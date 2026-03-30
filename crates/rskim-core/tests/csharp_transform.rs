//! C# transformation tests — verify all modes work correctly

#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests

use rskim_core::{transform, Language, Mode};

const SIMPLE_CS: &str = include_str!("../../../tests/fixtures/csharp/simple.cs");
const TYPES_CS: &str = include_str!("../../../tests/fixtures/csharp/types.cs");
const INTERFACES_CS: &str = include_str!("../../../tests/fixtures/csharp/interfaces.cs");
const GENERICS_CS: &str = include_str!("../../../tests/fixtures/csharp/generics.cs");

// ============================================================================
// Language detection
// ============================================================================

#[test]
fn test_csharp_language_detection() {
    use std::path::Path;
    assert_eq!(rskim_core::detect_language("cs"), Some(Language::CSharp));
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("Program.cs")),
        Some(Language::CSharp)
    );
}

// ============================================================================
// Structure mode
// ============================================================================

#[test]
fn test_csharp_structure_strips_method_bodies() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Structure).unwrap();
    // Method bodies should be replaced with /* ... */
    assert!(
        result.contains("/* ... */"),
        "method bodies should be replaced, got:\n{result}"
    );
    // Method signatures should be preserved
    assert!(
        result.contains("GetUser"),
        "method names should be preserved, got:\n{result}"
    );
    assert!(
        result.contains("DeleteUser"),
        "method names should be preserved, got:\n{result}"
    );
    // Body content should NOT be present
    assert!(
        !result.contains("_repository.FindById"),
        "method body content should be stripped, got:\n{result}"
    );
    assert!(
        !result.contains("_repository.Delete"),
        "method body content should be stripped, got:\n{result}"
    );
}

#[test]
fn test_csharp_structure_preserves_class_declaration() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Structure).unwrap();
    assert!(
        result.contains("class UserService"),
        "class declaration should be preserved, got:\n{result}"
    );
}

#[test]
fn test_csharp_structure_preserves_using_directives() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Structure).unwrap();
    assert!(
        result.contains("using System"),
        "using directives should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Signatures mode
// ============================================================================

#[test]
fn test_csharp_signatures_extracts_methods() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Signatures).unwrap();
    // Should extract method signatures
    assert!(
        result.contains("GetUser"),
        "method signature should be extracted, got:\n{result}"
    );
    assert!(
        result.contains("DeleteUser"),
        "method signature should be extracted, got:\n{result}"
    );
    // Should NOT contain method body content
    assert!(
        !result.contains("_repository.FindById"),
        "method body should not be present, got:\n{result}"
    );
}

// ============================================================================
// Types mode
// ============================================================================

#[test]
fn test_csharp_types_extracts_interfaces() {
    let result = transform(TYPES_CS, Language::CSharp, Mode::Types).unwrap();
    assert!(
        result.contains("interface IRepository"),
        "interface should be extracted, got:\n{result}"
    );
}

#[test]
fn test_csharp_types_extracts_enums() {
    let result = transform(TYPES_CS, Language::CSharp, Mode::Types).unwrap();
    assert!(
        result.contains("enum Status"),
        "enum should be extracted, got:\n{result}"
    );
}

#[test]
fn test_csharp_types_extracts_structs() {
    let result = transform(TYPES_CS, Language::CSharp, Mode::Types).unwrap();
    assert!(
        result.contains("struct Point"),
        "struct should be extracted, got:\n{result}"
    );
}

#[test]
fn test_csharp_types_extracts_classes() {
    let result = transform(TYPES_CS, Language::CSharp, Mode::Types).unwrap();
    assert!(
        result.contains("class User"),
        "class should be extracted, got:\n{result}"
    );
}

// ============================================================================
// Full mode (passthrough)
// ============================================================================

#[test]
fn test_csharp_full_mode_passthrough() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Full).unwrap();
    assert_eq!(
        result, SIMPLE_CS,
        "full mode should return source unchanged"
    );
}

// ============================================================================
// Minimal mode
// ============================================================================

#[test]
fn test_csharp_minimal_preserves_doc_comments() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Minimal).unwrap();
    // The file-level /** ... */ doc comment should be preserved (it's a doc comment)
    assert!(
        result.contains("FIXTURE:"),
        "doc comments (/**) should be preserved in minimal mode, got:\n{result}"
    );
    // Code should be preserved
    assert!(
        result.contains("class UserService"),
        "code should be preserved, got:\n{result}"
    );
}

#[test]
fn test_csharp_minimal_strips_regular_comments() {
    // Regular // comments at module level should be stripped
    let source = "// This is a regular comment\nusing System;\npublic class Foo {\n    public void Bar() {\n        // inside body\n    }\n}\n";
    let result = transform(source, Language::CSharp, Mode::Minimal).unwrap();
    assert!(
        !result.contains("regular comment"),
        "regular comments should be stripped, got:\n{result}"
    );
    assert!(
        result.contains("inside body"),
        "in-body comments should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Pseudo mode
// ============================================================================

#[test]
fn test_csharp_pseudo_strips_visibility() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Pseudo).unwrap();
    assert!(
        !result.contains("public "),
        "public modifier should be stripped, got:\n{result}"
    );
    assert!(
        !result.contains("private "),
        "private modifier should be stripped, got:\n{result}"
    );
}

#[test]
fn test_csharp_pseudo_preserves_async() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Pseudo).unwrap();
    assert!(
        result.contains("async"),
        "async modifier should be preserved (changes calling semantics), got:\n{result}"
    );
}

#[test]
fn test_csharp_pseudo_preserves_logic() {
    let result = transform(SIMPLE_CS, Language::CSharp, Mode::Pseudo).unwrap();
    assert!(
        result.contains("_logger = logger"),
        "logic should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Cross-fixture tests
// ============================================================================

#[test]
fn test_csharp_all_fixtures_parse() {
    // Verify all fixtures can be parsed and transformed without errors
    for (name, source) in [
        ("simple.cs", SIMPLE_CS),
        ("types.cs", TYPES_CS),
        ("interfaces.cs", INTERFACES_CS),
        ("generics.cs", GENERICS_CS),
    ] {
        for mode in [
            Mode::Structure,
            Mode::Signatures,
            Mode::Types,
            Mode::Full,
            Mode::Minimal,
            Mode::Pseudo,
        ] {
            let result = transform(source, Language::CSharp, mode);
            assert!(
                result.is_ok(),
                "Failed to transform {name} in {:?} mode: {:?}",
                mode,
                result.err()
            );
        }
    }
}
