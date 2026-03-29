//! Ruby transformation tests — verify all modes work correctly

use rskim_core::{transform, Language, Mode};

const SIMPLE_RB: &str = include_str!("../../../tests/fixtures/ruby/simple.rb");
const CLASS_RB: &str = include_str!("../../../tests/fixtures/ruby/class.rb");
const MODULE_RB: &str = include_str!("../../../tests/fixtures/ruby/module.rb");
const BLOCKS_RB: &str = include_str!("../../../tests/fixtures/ruby/blocks.rb");

// ============================================================================
// Language detection
// ============================================================================

#[test]
fn test_ruby_language_detection() {
    use std::path::Path;
    assert_eq!(rskim_core::detect_language("rb"), Some(Language::Ruby));
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("app.rb")),
        Some(Language::Ruby)
    );
}

// ============================================================================
// Structure mode
// ============================================================================

#[test]
fn test_ruby_structure_strips_method_bodies() {
    let result = transform(SIMPLE_RB, Language::Ruby, Mode::Structure).unwrap();
    // Method bodies should be replaced with /* ... */
    assert!(
        result.contains("/* ... */"),
        "method bodies should be replaced, got:\n{result}"
    );
    // Method names should be preserved
    assert!(
        result.contains("def find_user"),
        "method name should be preserved, got:\n{result}"
    );
    assert!(
        result.contains("def delete_user"),
        "method name should be preserved, got:\n{result}"
    );
}

#[test]
fn test_ruby_structure_preserves_class() {
    let result = transform(SIMPLE_RB, Language::Ruby, Mode::Structure).unwrap();
    assert!(
        result.contains("class UserService"),
        "class declaration should be preserved, got:\n{result}"
    );
}

#[test]
fn test_ruby_structure_preserves_requires() {
    let result = transform(SIMPLE_RB, Language::Ruby, Mode::Structure).unwrap();
    assert!(
        result.contains("require 'json'"),
        "require statements should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Signatures mode
// ============================================================================

#[test]
fn test_ruby_signatures_extracts_methods() {
    let result = transform(SIMPLE_RB, Language::Ruby, Mode::Signatures).unwrap();
    assert!(
        result.contains("def find_user"),
        "method signature should be extracted, got:\n{result}"
    );
    assert!(
        result.contains("def delete_user"),
        "method signature should be extracted, got:\n{result}"
    );
    // Should NOT contain method body content
    assert!(
        !result.contains("User.find"),
        "method body should not be present, got:\n{result}"
    );
}

// ============================================================================
// Types mode
// ============================================================================

#[test]
fn test_ruby_types_extracts_classes() {
    let result = transform(CLASS_RB, Language::Ruby, Mode::Types).unwrap();
    assert!(
        result.contains("class Animal"),
        "class should be extracted, got:\n{result}"
    );
    assert!(
        result.contains("class Dog"),
        "class should be extracted, got:\n{result}"
    );
}

#[test]
fn test_ruby_types_extracts_modules() {
    let result = transform(MODULE_RB, Language::Ruby, Mode::Types).unwrap();
    assert!(
        result.contains("module Validators"),
        "module should be extracted, got:\n{result}"
    );
}

// ============================================================================
// Full mode (passthrough)
// ============================================================================

#[test]
fn test_ruby_full_mode_passthrough() {
    let result = transform(SIMPLE_RB, Language::Ruby, Mode::Full).unwrap();
    assert_eq!(
        result, SIMPLE_RB,
        "full mode should return source unchanged"
    );
}

// ============================================================================
// Minimal mode
// ============================================================================

#[test]
fn test_ruby_minimal_strips_comments() {
    let result = transform(SIMPLE_RB, Language::Ruby, Mode::Minimal).unwrap();
    assert!(
        !result.contains("# FIXTURE:"),
        "file-level comments should be stripped, got:\n{result}"
    );
    assert!(
        result.contains("class UserService"),
        "code should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Pseudo mode
// ============================================================================

#[test]
fn test_ruby_pseudo_preserves_logic() {
    let result = transform(SIMPLE_RB, Language::Ruby, Mode::Pseudo).unwrap();
    assert!(
        result.contains("def find_user"),
        "method definition should be preserved, got:\n{result}"
    );
    assert!(
        result.contains("User.find"),
        "method body should be preserved in pseudo mode, got:\n{result}"
    );
}

// ============================================================================
// Cross-fixture tests
// ============================================================================

#[test]
fn test_ruby_all_fixtures_parse() {
    for (name, source) in [
        ("simple.rb", SIMPLE_RB),
        ("class.rb", CLASS_RB),
        ("module.rb", MODULE_RB),
        ("blocks.rb", BLOCKS_RB),
    ] {
        for mode in [
            Mode::Structure,
            Mode::Signatures,
            Mode::Types,
            Mode::Full,
            Mode::Minimal,
            Mode::Pseudo,
        ] {
            let result = transform(source, Language::Ruby, mode);
            assert!(
                result.is_ok(),
                "Failed to transform {name} in {:?} mode: {:?}",
                mode,
                result.err()
            );
        }
    }
}
