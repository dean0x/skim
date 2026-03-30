//! Swift transformation tests — verify all modes work correctly

#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests

use rskim_core::{transform, Language, Mode};

const SIMPLE_SWIFT: &str = include_str!("../../../tests/fixtures/swift/Simple.swift");
const PROTOCOL_SWIFT: &str = include_str!("../../../tests/fixtures/swift/Protocol.swift");
const SWIFTUI_SWIFT: &str = include_str!("../../../tests/fixtures/swift/SwiftUI.swift");
const GENERICS_SWIFT: &str = include_str!("../../../tests/fixtures/swift/Generics.swift");

// ============================================================================
// Language detection
// ============================================================================

#[test]
fn test_swift_language_detection() {
    use std::path::Path;
    assert_eq!(rskim_core::detect_language("swift"), Some(Language::Swift));
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("main.swift")),
        Some(Language::Swift)
    );
}

// ============================================================================
// Structure mode
// ============================================================================

#[test]
fn test_swift_structure_strips_function_bodies() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Structure).unwrap();
    // Function bodies should be replaced with /* ... */
    assert!(
        result.contains("/* ... */"),
        "function bodies should be replaced, got:\n{result}"
    );
    // Function names should be preserved
    assert!(
        result.contains("getUser"),
        "function names should be preserved, got:\n{result}"
    );
    assert!(
        result.contains("deleteUser"),
        "function names should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_structure_preserves_struct() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Structure).unwrap();
    assert!(
        result.contains("struct User"),
        "struct declaration should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_structure_preserves_protocol() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Structure).unwrap();
    assert!(
        result.contains("protocol UserRepository"),
        "protocol declaration should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_structure_preserves_class() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Structure).unwrap();
    assert!(
        result.contains("class UserService"),
        "class declaration should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_structure_strips_init_body() {
    let source = "class Foo {\n    init(x: Int) {\n        print(x)\n    }\n\n    func bar() {\n        print(\"bar\")\n    }\n}\n";
    let result = transform(source, Language::Swift, Mode::Structure).unwrap();
    assert!(
        !result.contains("print(x)"),
        "init body should be stripped, got:\n{result}"
    );
    assert!(
        result.contains("init(x: Int)"),
        "init signature should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_structure_strips_deinit_body() {
    let source = "class Foo {\n    deinit {\n        print(\"cleanup\")\n    }\n\n    func bar() {\n        print(\"bar\")\n    }\n}\n";
    let result = transform(source, Language::Swift, Mode::Structure).unwrap();
    assert!(
        !result.contains("cleanup"),
        "deinit body should be stripped, got:\n{result}"
    );
    assert!(
        result.contains("deinit"),
        "deinit keyword should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_signatures_extracts_init() {
    let source = "class Foo {\n    init(x: Int) {\n        print(x)\n    }\n\n    func bar() {\n        print(\"bar\")\n    }\n}\n";
    let result = transform(source, Language::Swift, Mode::Signatures).unwrap();
    assert!(
        result.contains("init(x: Int)"),
        "init signature should be extracted, got:\n{result}"
    );
}

// ============================================================================
// Signatures mode
// ============================================================================

#[test]
fn test_swift_signatures_extracts_functions() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Signatures).unwrap();
    // Should extract function signatures
    assert!(
        result.contains("getUser"),
        "function signature should be extracted, got:\n{result}"
    );
    assert!(
        result.contains("func add"),
        "top-level function should be extracted, got:\n{result}"
    );
    // Should NOT contain function body content
    assert!(
        !result.contains("fatalError"),
        "function body should not be present, got:\n{result}"
    );
}

// ============================================================================
// Types mode
// ============================================================================

#[test]
fn test_swift_types_extracts_protocols() {
    let result = transform(PROTOCOL_SWIFT, Language::Swift, Mode::Types).unwrap();
    assert!(
        result.contains("protocol Drawable"),
        "protocol should be extracted, got:\n{result}"
    );
}

#[test]
fn test_swift_types_extracts_structs() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Types).unwrap();
    assert!(
        result.contains("struct User"),
        "struct should be extracted, got:\n{result}"
    );
}

#[test]
fn test_swift_types_extracts_classes() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Types).unwrap();
    assert!(
        result.contains("class UserService"),
        "class should be extracted, got:\n{result}"
    );
}

#[test]
fn test_swift_types_extracts_enums() {
    let result = transform(SWIFTUI_SWIFT, Language::Swift, Mode::Types).unwrap();
    assert!(
        result.contains("enum ViewState"),
        "enum should be extracted, got:\n{result}"
    );
}

#[test]
fn test_swift_types_extracts_typealias() {
    let result = transform(GENERICS_SWIFT, Language::Swift, Mode::Types).unwrap();
    assert!(
        result.contains("typealias StringStack"),
        "typealias should be extracted, got:\n{result}"
    );
}

// ============================================================================
// Full mode (passthrough)
// ============================================================================

#[test]
fn test_swift_full_mode_passthrough() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Full).unwrap();
    assert_eq!(
        result, SIMPLE_SWIFT,
        "full mode should return source unchanged"
    );
}

// ============================================================================
// Minimal mode
// ============================================================================

#[test]
fn test_swift_minimal_preserves_doc_comments() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Minimal).unwrap();
    assert!(
        result.contains("FIXTURE:"),
        "doc comments (/** */) should be preserved in minimal mode, got:\n{result}"
    );
    // Code should be preserved
    assert!(
        result.contains("class UserService"),
        "code should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_minimal_strips_regular_comments() {
    let source =
        "// This is a regular comment\nimport Foundation\n\nfunc main() {\n    // inside body\n}\n";
    let result = transform(source, Language::Swift, Mode::Minimal).unwrap();
    assert!(
        !result.contains("regular comment"),
        "regular comments should be stripped, got:\n{result}"
    );
    assert!(
        result.contains("inside body"),
        "in-body comments should be preserved, got:\n{result}"
    );
}

#[test]
fn test_swift_minimal_preserves_triple_slash() {
    let source = "/// This is a doc comment\nfunc documented() {\n    print(\"hello\")\n}\n";
    let result = transform(source, Language::Swift, Mode::Minimal).unwrap();
    assert!(
        result.contains("/// This is a doc comment"),
        "/// doc comments should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Pseudo mode
// ============================================================================

#[test]
fn test_swift_pseudo_strips_visibility() {
    let source = "public class Foo {\n    private var x: Int = 0\n    internal func bar() {\n        print(x)\n    }\n}\n";
    let result = transform(source, Language::Swift, Mode::Pseudo).unwrap();
    assert!(
        !result.contains("public "),
        "public modifier should be stripped, got:\n{result}"
    );
    assert!(
        !result.contains("private "),
        "private modifier should be stripped, got:\n{result}"
    );
    assert!(
        !result.contains("internal "),
        "internal modifier should be stripped, got:\n{result}"
    );
}

#[test]
fn test_swift_pseudo_preserves_async() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Pseudo).unwrap();
    assert!(
        result.contains("async"),
        "async should be preserved (changes calling semantics), got:\n{result}"
    );
}

#[test]
fn test_swift_pseudo_preserves_logic() {
    let result = transform(SIMPLE_SWIFT, Language::Swift, Mode::Pseudo).unwrap();
    assert!(
        result.contains("repository"),
        "logic should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Cross-fixture tests
// ============================================================================

#[test]
fn test_swift_all_fixtures_parse() {
    // Verify all fixtures can be parsed and transformed without errors
    for (name, source) in [
        ("Simple.swift", SIMPLE_SWIFT),
        ("Protocol.swift", PROTOCOL_SWIFT),
        ("SwiftUI.swift", SWIFTUI_SWIFT),
        ("Generics.swift", GENERICS_SWIFT),
    ] {
        for mode in [
            Mode::Structure,
            Mode::Signatures,
            Mode::Types,
            Mode::Full,
            Mode::Minimal,
            Mode::Pseudo,
        ] {
            let result = transform(source, Language::Swift, mode);
            assert!(
                result.is_ok(),
                "Failed to transform {name} in {:?} mode: {:?}",
                mode,
                result.err()
            );
        }
    }
}
