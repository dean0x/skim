//! Tests for the query execution module (query.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
fn make_config(root: &std::path::Path, cache_dir: &std::path::Path, text: &str) -> QueryConfig {
    QueryConfig {
        text: text.to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: None,
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
    assert!(output.results.is_empty(), "empty query → empty results");
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
    // format_text_output writes "no results for <query>" on empty results.
    assert!(
        s.contains("no results"),
        "empty result output must contain 'no results', got: {s:?}"
    );
}

#[test]
fn test_format_text_output_includes_path_and_score() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/auth.rs".to_string(),
        score: 12.34,
        field: "function_signature".to_string(),
        line_number: Some(2),
        line_range: Some(2..3),
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
        stale: false,
        match_positions: vec![],
        temporal: None,
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
// [stale] marker
// ============================================================================

#[test]
fn test_format_text_output_includes_stale_marker() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/old.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: Some(10),
        line_range: Some(10..11),
        snippet: Some(SnippetContext {
            lines: vec![SnippetLine {
                line_number: 10,
                content: "pub fn old_fn()".to_string(),
                is_match: true,
            }],
        }),
        stale: true,
        match_positions: vec![],
        temporal: None,
    };

    let output = QueryOutput {
        query: "old_fn".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 2,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("[stale]"),
        "stale result must include '[stale]' marker in output, got: {s:?}"
    );
}

// ============================================================================
// Edge cases: no .git, corrupt index
// ============================================================================

#[test]
fn test_execute_query_no_git_dir_returns_ok_or_graceful_err() {
    // Project root with no .git — should not panic, must return Ok or Err.
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // No .git directory — non-git project.
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    let config = make_config(&root, &cache_dir, "main");

    // Must not panic. Either succeeds (0 or more results) or fails gracefully.
    match execute_query(&config, &TEST_ANALYTICS) {
        Ok(output) => {
            assert_eq!(output.query, "main");
        }
        Err(e) => {
            // Acceptable: I/O or index error — but no panic.
            let msg = e.to_string();
            assert!(!msg.is_empty(), "error message must not be empty");
        }
    }
}

#[test]
fn test_execute_query_corrupt_index_returns_err_not_panic() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // Write garbage bytes into the index file.
    fs::write(
        cache_dir.join("index.skidx"),
        b"this is not a valid index\xff\x00\xde\xad",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "authenticate");

    // A corrupt index must return Err rather than panic.
    // (auto_refresh_if_stale may rebuild and succeed — both outcomes are acceptable.)
    match execute_query(&config, &TEST_ANALYTICS) {
        Ok(_) => {
            // Rebuild succeeded after detecting corruption — acceptable.
        }
        Err(e) => {
            // Graceful error — confirm non-empty message.
            assert!(!e.to_string().is_empty(), "error message must not be empty");
        }
    }
}

// ============================================================================
// ResolvedResult JSON serialization
// ============================================================================

/// line_range: Some(5..13) must serialise as {"start":5,"end":13} in JSON output.
#[test]
fn test_resolved_result_line_range_some_serializes_start_end() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/lib.rs".to_string(),
        score: 1.0,
        field: "function_signature".to_string(),
        line_number: Some(5),
        line_range: Some(5..13),
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: None,
    };

    let value = serde_json::to_value(&result).expect("ResolvedResult must serialize");
    assert_eq!(
        value["line_range"]["start"], 5,
        "line_range.start must be 5"
    );
    assert_eq!(value["line_range"]["end"], 13, "line_range.end must be 13");
}

/// line_range: None must serialise as JSON null.
#[test]
fn test_resolved_result_line_range_none_serializes_null() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/lib.rs".to_string(),
        score: 1.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: None,
    };

    let value = serde_json::to_value(&result).expect("ResolvedResult must serialize");
    assert!(
        value["line_range"].is_null(),
        "line_range must be null when None, got: {:?}",
        value["line_range"]
    );
}

// ============================================================================
// blast_radius_paths filter
// ============================================================================

/// When blast_radius_paths is set, execute_query must restrict results to
/// the allowed paths. The target file itself is included in the set (Issue fix:
/// previously only co-change *partners* were included, excluding the target).
#[test]
fn test_execute_query_blast_radius_includes_only_allowed_paths() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // Allow only src/auth.rs in the blast-radius set.
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string());

    let config = QueryConfig {
        text: "authenticate".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // All results must be from the allowed set.
    for r in &output.results {
        assert_eq!(
            r.path, "src/auth.rs",
            "blast-radius filter must restrict results to allowed paths, got: {}",
            r.path
        );
    }
}

/// When blast_radius_paths contains the target file, a query that matches
/// the target returns results for that file.
/// Regression for: combined mode was excluding the target file itself.
#[test]
fn test_execute_query_blast_radius_target_file_is_included() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // Build an allowlist that includes src/auth.rs (the "target") plus a
    // partner that has no matching content for "authenticate".
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string()); // target
    allowed.insert("src/does_not_exist.rs".to_string()); // partner (not indexed)

    let config = QueryConfig {
        text: "authenticate".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // src/auth.rs contains "authenticate" and is in the allowed set — it must appear.
    let has_auth = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_auth,
        "target file (src/auth.rs) must be in blast-radius results when it matches the query"
    );
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

// ============================================================================
// Temporal annotation in text output (Step 11)
// ============================================================================

/// format_text_output includes "hotspot: X.XXX" when temporal annotation present.
#[test]
fn test_format_text_output_includes_temporal_hotspot() {
    use crate::cmd::search::types::{ResolvedResult, TemporalAnnotation};

    let result = ResolvedResult {
        path: "src/hot.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: Some(1),
        line_range: Some(1..2),
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: Some(TemporalAnnotation {
            hotspot_score: Some(0.95),
            ..Default::default()
        }),
    };

    let output = QueryOutput {
        query: "hot".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("hotspot:"),
        "temporal hotspot annotation must appear, got: {s:?}"
    );
    assert!(
        s.contains("0.950"),
        "hotspot score must be formatted to 3dp, got: {s:?}"
    );
}

/// format_text_output shows "risk: X.XXX" when risk annotation present.
#[test]
fn test_format_text_output_includes_temporal_risk() {
    use crate::cmd::search::types::{ResolvedResult, TemporalAnnotation};

    let result = ResolvedResult {
        path: "src/risky.rs".to_string(),
        score: 3.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: Some(TemporalAnnotation {
            risk_score: Some(0.80),
            ..Default::default()
        }),
    };

    let output = QueryOutput {
        query: "risky".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("risk:"),
        "risk annotation must appear, got: {s:?}"
    );
    assert!(
        s.contains("0.800"),
        "risk score must be formatted to 3dp, got: {s:?}"
    );
}

/// format_text_output omits temporal section when annotation is None.
#[test]
fn test_format_text_output_omits_temporal_when_none() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/plain.rs".to_string(),
        score: 2.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: None,
    };

    let output = QueryOutput {
        query: "plain".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        !s.contains("hotspot:"),
        "no hotspot annotation when temporal is None, got: {s:?}"
    );
    assert!(
        !s.contains("risk:"),
        "no risk annotation when temporal is None, got: {s:?}"
    );
}

/// format_json_output includes temporal annotations inside each result object.
#[test]
fn test_format_json_output_includes_temporal_annotations() {
    use crate::cmd::search::types::{ResolvedResult, TemporalAnnotation};

    let result = ResolvedResult {
        path: "src/hot.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: Some(TemporalAnnotation {
            hotspot_score: Some(0.95),
            risk_score: Some(0.70),
            ..Default::default()
        }),
    };

    let output = QueryOutput {
        query: "hot".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_json_output(&output, &mut buf).unwrap();
    let bytes = buf.into_inner().unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(s).expect("must be valid JSON");

    let temporal = &parsed["results"][0]["temporal"];
    assert!(
        !temporal.is_null(),
        "temporal field must be present in JSON when Some"
    );
    let hs = temporal["hotspot_score"].as_f64().unwrap();
    assert!(
        (hs - 0.95).abs() < 1e-6,
        "hotspot_score must be ~0.95, got {hs}"
    );
    let rs = temporal["risk_score"].as_f64().unwrap();
    assert!(
        (rs - 0.70).abs() < 1e-6,
        "risk_score must be ~0.70, got {rs}"
    );
}
