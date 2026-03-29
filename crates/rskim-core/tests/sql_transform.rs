//! SQL transformation tests — verify all modes work correctly

use rskim_core::{transform, Language, Mode};

const SIMPLE_SQL: &str = include_str!("../../../tests/fixtures/sql/simple.sql");

// ============================================================================
// Language detection
// ============================================================================

#[test]
fn test_sql_language_detection() {
    use std::path::Path;
    assert_eq!(rskim_core::detect_language("sql"), Some(Language::Sql));
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("schema.sql")),
        Some(Language::Sql)
    );
}

// ============================================================================
// Structure mode
// ============================================================================

#[test]
fn test_sql_structure_preserves_create_table() {
    let result = transform(SIMPLE_SQL, Language::Sql, Mode::Structure).unwrap();
    assert!(
        result.contains("CREATE TABLE users"),
        "CREATE TABLE should be preserved, got:\n{result}"
    );
    assert!(
        result.contains("CREATE TABLE orders"),
        "CREATE TABLE should be preserved, got:\n{result}"
    );
}

#[test]
fn test_sql_structure_preserves_select() {
    let result = transform(SIMPLE_SQL, Language::Sql, Mode::Structure).unwrap();
    assert!(
        result.contains("SELECT"),
        "SELECT should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Signatures mode
// ============================================================================

#[test]
fn test_sql_signatures_extracts_create_table() {
    let result = transform(SIMPLE_SQL, Language::Sql, Mode::Signatures).unwrap();
    assert!(
        result.contains("CREATE TABLE"),
        "CREATE TABLE should be extracted, got:\n{result}"
    );
}

// ============================================================================
// Types mode
// ============================================================================

#[test]
fn test_sql_types_extracts_create_table() {
    let result = transform(SIMPLE_SQL, Language::Sql, Mode::Types).unwrap();
    assert!(
        result.contains("CREATE TABLE"),
        "CREATE TABLE should be extracted as type, got:\n{result}"
    );
}

// ============================================================================
// Full mode (passthrough)
// ============================================================================

#[test]
fn test_sql_full_mode_passthrough() {
    let result = transform(SIMPLE_SQL, Language::Sql, Mode::Full).unwrap();
    assert_eq!(
        result, SIMPLE_SQL,
        "full mode should return source unchanged"
    );
}

// ============================================================================
// Minimal mode
// ============================================================================

#[test]
fn test_sql_minimal_strips_comments() {
    let result = transform(SIMPLE_SQL, Language::Sql, Mode::Minimal).unwrap();
    assert!(
        !result.contains("-- FIXTURE:"),
        "comments should be stripped in minimal mode, got:\n{result}"
    );
    assert!(
        result.contains("CREATE TABLE"),
        "code should be preserved, got:\n{result}"
    );
}

// ============================================================================
// Pseudo mode
// ============================================================================

#[test]
fn test_sql_pseudo_preserves_sql() {
    let result = transform(SIMPLE_SQL, Language::Sql, Mode::Pseudo).unwrap();
    assert!(
        result.contains("CREATE TABLE"),
        "SQL statements should be preserved in pseudo mode, got:\n{result}"
    );
}

// ============================================================================
// Cross-mode test
// ============================================================================

#[test]
fn test_sql_all_modes_parse() {
    for mode in [
        Mode::Structure,
        Mode::Signatures,
        Mode::Types,
        Mode::Full,
        Mode::Minimal,
        Mode::Pseudo,
    ] {
        let result = transform(SIMPLE_SQL, Language::Sql, mode);
        assert!(
            result.is_ok(),
            "Failed to transform simple.sql in {:?} mode: {:?}",
            mode,
            result.err()
        );
    }
}
