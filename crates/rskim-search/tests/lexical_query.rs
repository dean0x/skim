//! Integration tests for `LexicalSearchLayer::search()` — the BM25F query engine.
//!
//! Each test builds a real on-disk index from fixture files, then queries it.
//! No internal state is probed; all assertions are over the public `SearchLayer`
//! and `SearchIndex` traits.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use rskim_core::Language;
use rskim_search::{
    lexical::builder::LexicalLayerBuilder, LayerBuilder, SearchIndex, SearchLayer, SearchQuery,
};

// ============================================================================
// Helpers
// ============================================================================

/// Workspace root directory (absolute).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate parent")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// Absolute path to `tests/fixtures/search/<name>` (for file reads).
fn fixture_abs(name: &str) -> PathBuf {
    workspace_root().join("tests/fixtures/search").join(name)
}

/// Relative path to `tests/fixtures/search/<name>` (for register_within).
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from("tests/fixtures/search").join(name)
}

/// Read a fixture file. Panics if the file does not exist.
fn read_fixture(name: &str) -> String {
    let path = fixture_abs(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"))
}

/// Build a `SearchIndex` from all fixture files.
///
/// The builder indexes each file under its real on-disk path so that `FileId`
/// values are stable and `FileTable` lookups work correctly in tests.
fn build_all_fixtures(dir: &Path) -> Box<dyn SearchIndex> {
    let fixtures: &[(&str, Language)] = &[
        ("user_service.ts", Language::TypeScript),
        ("auth_handler.rs", Language::Rust),
        ("config.json", Language::Json),
        ("deploy.yaml", Language::Yaml),
        ("README.md", Language::Markdown),
        ("utils.py", Language::Python),
    ];

    let mut builder = LexicalLayerBuilder::new(dir.to_path_buf(), workspace_root());

    for (name, lang) in fixtures {
        let path = fixture_path(name);
        let content = read_fixture(name);
        builder
            .add_file(&path, &content, *lang)
            .unwrap_or_else(|e| panic!("add_file {name} failed: {e}"));
    }

    Box::new(builder).build().expect("build failed")
}

/// Resolve a `FileId` to its file name (last path component), or return `None`.
fn file_name_of(index: &dyn SearchIndex, file_id: rskim_search::FileId) -> Option<String> {
    index
        .file_table()
        .lookup(file_id)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
}

// ============================================================================
// 1. Empty text query → empty results
// ============================================================================

#[test]
fn empty_text_query_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer.search(&SearchQuery::new()).expect("search");
    assert!(
        results.is_empty(),
        "no text query should return empty results"
    );
}

// ============================================================================
// 2. Empty string text query → empty results
// ============================================================================

#[test]
fn blank_text_query_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer.search(&SearchQuery::text("")).expect("search");
    assert!(
        results.is_empty(),
        "empty string query should return empty results"
    );
}

// ============================================================================
// 3. Search "UserService" → user_service.ts is the top result
// ============================================================================

#[test]
fn search_userservice_finds_user_service_ts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("UserService"))
        .expect("search");

    assert!(
        !results.is_empty(),
        "search for 'UserService' should return at least one result"
    );

    let top_name = file_name_of(index.as_ref(), results[0].0);
    assert_eq!(
        top_name.as_deref(),
        Some("user_service.ts"),
        "top result for 'UserService' should be user_service.ts, got {:?}",
        top_name
    );

    assert!(
        results[0].1 > 0.0,
        "top result score should be positive, got {}",
        results[0].1
    );
}

// ============================================================================
// 4. Search "AuthHandler" → auth_handler.rs is the top result
// ============================================================================

#[test]
fn search_authhandler_finds_auth_handler_rs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("AuthHandler"))
        .expect("search");

    assert!(
        !results.is_empty(),
        "search for 'AuthHandler' should return at least one result"
    );

    let top_name = file_name_of(index.as_ref(), results[0].0);
    assert_eq!(
        top_name.as_deref(),
        Some("auth_handler.rs"),
        "top result for 'AuthHandler' should be auth_handler.rs, got {:?}",
        top_name
    );
}

// ============================================================================
// 5. Search "database_url" → config.json is in results
// ============================================================================

#[test]
fn search_database_url_finds_config_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("database_url"))
        .expect("search");

    assert!(
        !results.is_empty(),
        "search for 'database_url' should return results"
    );

    let has_config = results
        .iter()
        .any(|(fid, _)| file_name_of(index.as_ref(), *fid).as_deref() == Some("config.json"));
    assert!(
        has_config,
        "config.json should appear in results for 'database_url'"
    );
}

// ============================================================================
// 6. Search "replicas" → deploy.yaml is in results
// ============================================================================

#[test]
fn search_replicas_finds_deploy_yaml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("replicas"))
        .expect("search");

    assert!(
        !results.is_empty(),
        "search for 'replicas' should return results"
    );

    let has_deploy = results
        .iter()
        .any(|(fid, _)| file_name_of(index.as_ref(), *fid).as_deref() == Some("deploy.yaml"));
    assert!(
        has_deploy,
        "deploy.yaml should appear in results for 'replicas'"
    );
}

// ============================================================================
// 7. Single-character query (no bigrams extractable) → empty results
//
// `extract_query_ngrams` requires at least 2 bytes to produce any bigram.
// A single-char query yields no n-grams, so search must return empty.
// ============================================================================

#[test]
fn single_char_query_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    // A single printable character produces no bigrams → must return empty.
    let results = layer.search(&SearchQuery::text("z")).expect("search");

    assert!(
        results.is_empty(),
        "single-char query produces no bigrams so must return empty results, got {} results",
        results.len()
    );
}

// ============================================================================
// 8. limit=1 → at most 1 result
// ============================================================================

#[test]
fn limit_one_returns_at_most_one_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("fn").with_limit(1))
        .expect("search");

    assert!(
        results.len() <= 1,
        "limit=1 should return at most 1 result, got {}",
        results.len()
    );
}

// ============================================================================
// 9. offset past end → empty results
// ============================================================================

#[test]
fn offset_past_end_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("fn").with_offset(100_000))
        .expect("search");

    assert!(
        results.is_empty(),
        "offset past end should return empty results, got {} results",
        results.len()
    );
}

// ============================================================================
// 10. Score ordering is descending
// ============================================================================

#[test]
fn results_are_sorted_by_score_descending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    // Use a generic term likely to match multiple files.
    let results = layer.search(&SearchQuery::text("fn")).expect("search");

    // Verify descending score order.
    for window in results.windows(2) {
        let (_, score_a) = window[0];
        let (_, score_b) = window[1];
        assert!(
            score_a >= score_b,
            "results must be sorted descending: {score_a} >= {score_b}"
        );
    }
}

// ============================================================================
// 11. All returned scores are positive
// ============================================================================

#[test]
fn all_scores_are_positive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("UserService"))
        .expect("search");

    for (fid, score) in &results {
        assert!(
            *score > 0.0,
            "all returned scores must be positive, got {score} for FileId {:?}",
            fid
        );
    }
}

// ============================================================================
// 12. TypeDefinition context outscores FunctionBody-only context
//     (TypeDefinition boost = 5.0, FunctionBody boost = 1.0)
//
//     "UserService" appears as a class name in user_service.ts (TypeDefinition)
//     and only incidentally in other files (FunctionBody fallback).
//     user_service.ts must rank first.
// ============================================================================

#[test]
fn type_definition_context_ranks_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let layer =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer");

    let results = layer
        .search(&SearchQuery::text("UserService"))
        .expect("search");

    assert!(
        !results.is_empty(),
        "expected at least one result for 'UserService'"
    );

    let top_name = file_name_of(index.as_ref(), results[0].0);
    assert_eq!(
        top_name.as_deref(),
        Some("user_service.ts"),
        "file with TypeDefinition context should rank first"
    );
}

// ============================================================================
// 13. Re-opened index produces identical results to the freshly-built one
// ============================================================================

#[test]
fn reopened_index_produces_same_results() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _index = build_all_fixtures(dir.path());

    let layer_a =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer a");
    let layer_b =
        rskim_search::lexical::query::LexicalSearchLayer::open(dir.path()).expect("open layer b");

    let query = SearchQuery::text("UserService");

    let results_a = layer_a.search(&query).expect("search a");
    let results_b = layer_b.search(&query).expect("search b");

    assert_eq!(
        results_a.len(),
        results_b.len(),
        "reopened index must return same number of results"
    );

    for ((fid_a, score_a), (fid_b, score_b)) in results_a.iter().zip(results_b.iter()) {
        assert_eq!(fid_a, fid_b, "FileIds must match");
        assert!(
            (score_a - score_b).abs() < f32::EPSILON * 16.0,
            "scores must be identical: {score_a} vs {score_b}"
        );
    }
}
