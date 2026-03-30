//! Kotlin transformation tests — verify all modes work correctly

#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests

use rskim_core::{transform, Language, Mode};

const SIMPLE_KT: &str = include_str!("../../../tests/fixtures/kotlin/Simple.kt");
const DATA_CLASS_KT: &str = include_str!("../../../tests/fixtures/kotlin/DataClass.kt");
const COROUTINES_KT: &str = include_str!("../../../tests/fixtures/kotlin/Coroutines.kt");
const INTERFACES_KT: &str = include_str!("../../../tests/fixtures/kotlin/Interfaces.kt");

// ============================================================================
// Language detection
// ============================================================================

#[test]
fn test_kotlin_language_detection() {
    use std::path::Path;
    assert_eq!(rskim_core::detect_language("kt"), Some(Language::Kotlin));
    assert_eq!(rskim_core::detect_language("kts"), Some(Language::Kotlin));
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("Main.kt")),
        Some(Language::Kotlin)
    );
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("build.gradle.kts")),
        Some(Language::Kotlin)
    );
}

// ============================================================================
// Structure mode
// ============================================================================

#[test]
fn test_kotlin_structure_strips_function_bodies() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Structure).unwrap();
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
fn test_kotlin_structure_preserves_class_declaration() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Structure).unwrap();
    assert!(
        result.contains("class UserService"),
        "class declaration should be preserved, got:\n{result}"
    );
}

#[test]
fn test_kotlin_structure_preserves_data_class() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Structure).unwrap();
    assert!(
        result.contains("data class User"),
        "data class should be preserved, got:\n{result}"
    );
}

#[test]
fn test_kotlin_structure_preserves_interface() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Structure).unwrap();
    assert!(
        result.contains("interface UserRepository"),
        "interface should be preserved, got:\n{result}"
    );
}

#[test]
fn test_kotlin_structure_preserves_expression_body() {
    // Expression-body functions have no block body, so they should be preserved intact
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Structure).unwrap();
    // The expression-body function `fun add(a: Int, b: Int): Int = a + b` should appear
    // It has a function_body but it's an expression, not a block — structure mode should still strip it
    assert!(
        result.contains("fun add"),
        "expression-body function should be preserved, got:\n{result}"
    );
}

#[test]
fn test_kotlin_structure_strips_init_block() {
    let source = "class Foo(val x: Int) {\n    init {\n        println(x)\n    }\n\n    fun bar() {\n        println(\"bar\")\n    }\n}\n";
    let result = transform(source, Language::Kotlin, Mode::Structure).unwrap();
    // init block body should be replaced
    assert!(
        !result.contains("println(x)"),
        "init block body should be stripped, got:\n{result}"
    );
    assert!(
        result.contains("init"),
        "init keyword should be preserved, got:\n{result}"
    );
}

#[test]
fn test_kotlin_structure_strips_secondary_constructor() {
    let source = "class Foo(val x: Int) {\n    constructor(x: Int, y: Int) : this(x) {\n        println(y)\n    }\n\n    fun bar() {\n        println(\"bar\")\n    }\n}\n";
    let result = transform(source, Language::Kotlin, Mode::Structure).unwrap();
    assert!(
        !result.contains("println(y)"),
        "secondary constructor body should be stripped, got:\n{result}"
    );
    assert!(
        result.contains("constructor"),
        "constructor keyword should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Signatures mode
// ============================================================================

#[test]
fn test_kotlin_signatures_extracts_functions() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Signatures).unwrap();
    // Should extract function signatures
    assert!(
        result.contains("getUser"),
        "function signature should be extracted, got:\n{result}"
    );
    assert!(
        result.contains("createUser"),
        "suspend function should be extracted, got:\n{result}"
    );
    // Should NOT contain function body content
    assert!(
        !result.contains("repository.findById"),
        "function body should not be present, got:\n{result}"
    );
}

#[test]
fn test_kotlin_signatures_preserves_suspend() {
    let result = transform(COROUTINES_KT, Language::Kotlin, Mode::Signatures).unwrap();
    assert!(
        result.contains("suspend"),
        "suspend modifier should be preserved in signatures, got:\n{result}"
    );
}

// ============================================================================
// Types mode
// ============================================================================

#[test]
fn test_kotlin_types_extracts_classes() {
    let result = transform(INTERFACES_KT, Language::Kotlin, Mode::Types).unwrap();
    assert!(
        result.contains("sealed class Result"),
        "sealed class should be extracted, got:\n{result}"
    );
}

#[test]
fn test_kotlin_types_extracts_interfaces() {
    let result = transform(INTERFACES_KT, Language::Kotlin, Mode::Types).unwrap();
    assert!(
        result.contains("interface Validator"),
        "interface should be extracted, got:\n{result}"
    );
}

#[test]
fn test_kotlin_types_extracts_type_aliases() {
    let result = transform(INTERFACES_KT, Language::Kotlin, Mode::Types).unwrap();
    assert!(
        result.contains("typealias UserMap"),
        "type alias should be extracted, got:\n{result}"
    );
}

// ============================================================================
// Full mode (passthrough)
// ============================================================================

#[test]
fn test_kotlin_full_mode_passthrough() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Full).unwrap();
    assert_eq!(
        result, SIMPLE_KT,
        "full mode should return source unchanged"
    );
}

// ============================================================================
// Minimal mode
// ============================================================================

#[test]
fn test_kotlin_minimal_preserves_doc_comments() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Minimal).unwrap();
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
fn test_kotlin_minimal_strips_regular_comments() {
    let source =
        "// This is a regular comment\npackage com.example\n\nfun main() {\n    // inside body\n}\n";
    let result = transform(source, Language::Kotlin, Mode::Minimal).unwrap();
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
fn test_kotlin_pseudo_strips_visibility() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Pseudo).unwrap();
    assert!(
        !result.contains("private "),
        "private modifier should be stripped, got:\n{result}"
    );
}

#[test]
fn test_kotlin_pseudo_preserves_suspend() {
    let result = transform(COROUTINES_KT, Language::Kotlin, Mode::Pseudo).unwrap();
    assert!(
        result.contains("suspend"),
        "suspend should be preserved (changes calling semantics), got:\n{result}"
    );
}

#[test]
fn test_kotlin_pseudo_preserves_logic() {
    let result = transform(SIMPLE_KT, Language::Kotlin, Mode::Pseudo).unwrap();
    assert!(
        result.contains("repository.findById"),
        "logic should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Cross-fixture tests
// ============================================================================

#[test]
fn test_kotlin_all_fixtures_parse() {
    // Verify all fixtures can be parsed and transformed without errors
    for (name, source) in [
        ("Simple.kt", SIMPLE_KT),
        ("DataClass.kt", DATA_CLASS_KT),
        ("Coroutines.kt", COROUTINES_KT),
        ("Interfaces.kt", INTERFACES_KT),
    ] {
        for mode in [
            Mode::Structure,
            Mode::Signatures,
            Mode::Types,
            Mode::Full,
            Mode::Minimal,
            Mode::Pseudo,
        ] {
            let result = transform(source, Language::Kotlin, mode);
            assert!(
                result.is_ok(),
                "Failed to transform {name} in {:?} mode: {:?}",
                mode,
                result.err()
            );
        }
    }
}
