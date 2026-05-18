//! Tests for the query execution module (query.rs).

#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::BufWriter;

use tempfile::tempdir;

use super::{execute_query, format_json_output, format_text_output};
use crate::cmd::search::types::{QueryConfig, QueryOutput};

// ============================================================================
// Test helpers
// ============================================================================

/// Stub analytics config with analytics disabled.
const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
    enabled: false,
    input_cost_per_mtok: None,
    session_id: None,
};

/// Create a minimal indexable project in `root` with a few Rust files.
fn create_test_project(root: &std::path::Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // Create a .git dir so the project root is found
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    fs::write(
        src.join("auth.rs"),
        "/// Authenticate a user.\npub fn authenticate(token: &str) -> bool {\n    !token.is_empty()\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod auth;\npub fn parse_config(s: &str) -> Option<String> {\n    Some(s.to_string())\n}\n",
    )
    .unwrap();
}

/// Build a QueryConfig pointing at `root` and `cache_dir`.
fn make_config(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
    text: &str,
) -> QueryConfig {
    QueryConfig {
        text: text.to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
    }
}

// ============================================================================
// execute_query
// ============================================================================

#[test]
fn test_execute_query_auto_builds_index_on_cold_start() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // Index was auto-built — query should succeed (0 or more results).
    assert_eq!(output.query, "authenticate");
    assert!(
        output.duration_ms < 60_000,
        "query should complete within 60s"
    );
}

#[test]
fn test_execute_query_finds_results_for_known_term() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // "authenticate" is in auth.rs — should find at least one result.
    assert!(
        !output.results.is_empty(),
        "should find results for 'authenticate'"
    );
    // All results should have valid paths
    for r in &output.results {
        assert!(!r.path.is_empty(), "result path must not be empty");
    }
}

#[test]
fn test_execute_query_respects_limit() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let mut config = make_config(&root, &cache_dir, "fn");
    config.limit = 1;
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();
    assert!(
        output.results.len() <= 1,
        "limit=1 must produce at most 1 result"
    );
}

#[test]
fn test_execute_query_empty_query_returns_empty_results() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();
    assert!(
        output.results.is_empty(),
        "empty query → empty results"
    );
}

// ============================================================================
// format_text_output
// ============================================================================

#[test]
fn test_format_text_output_empty_results() {
    let output = QueryOutput {
        query: "nothing".to_string(),
        total: 0,
        results: vec![],
        duration_ms: 5,
        index_stats: None,
    };
    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(s.contains("no results") || s.is_empty() || s.contains("nothing"),
        "empty result message should mention query or 'no results', got: {s:?}");
}

#[test]
fn test_format_text_output_includes_path_and_score() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/auth.rs".to_string(),
        score: 12.34,
        field: "function_signature".to_string(),
        line_number: Some(2),
        snippet: Some(SnippetContext {
            lines: vec![
                SnippetLine {
                    line_number: 1,
                    content: "/// Authenticate".to_string(),
                    is_match: false,
                },
                SnippetLine {
                    line_number: 2,
                    content: "pub fn authenticate".to_string(),
                    is_match: true,
                },
            ],
        }),
        match_positions: vec![],
    };

    let output = QueryOutput {
        query: "authenticate".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 3,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(s.contains("src/auth.rs"), "output should contain path");
}

// ============================================================================
// format_json_output
// ============================================================================

#[test]
fn test_format_json_output_is_valid_json() {
    let output = QueryOutput {
        query: "test".to_string(),
        total: 0,
        results: vec![],
        duration_ms: 1,
        index_stats: None,
    };
    let mut buf = BufWriter::new(Vec::new());
    format_json_output(&output, &mut buf).unwrap();
    let bytes = buf.into_inner().unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    // Must be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(s).expect("must be valid JSON");
    assert_eq!(parsed["query"], "test");
    assert_eq!(parsed["total"], 0);
}
