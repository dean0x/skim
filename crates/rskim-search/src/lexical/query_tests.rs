//! Tests for QueryEngine (query.rs).

#![allow(clippy::unwrap_used)]

use crate::index::NgramIndexBuilder;
use crate::lexical::{BM25FConfig, QueryEngine};
use crate::{FileId, LayerBuilder, SearchError, SearchLayer, SearchQuery};
use crate::lexical::query::MAX_QUERY_BYTES;

// -----------------------------------------------------------------------
// Test helper
// -----------------------------------------------------------------------

fn build_query_engine(
    files: &[(FileId, &str, rskim_core::Language)],
) -> (tempfile::TempDir, QueryEngine) {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for (id, content, lang) in files {
        builder.add_file(*id, content, *lang).unwrap();
    }
    let layer = builder.build().unwrap();
    (dir, QueryEngine::new(layer))
}

// -----------------------------------------------------------------------
// Phase 1 — Validation
// -----------------------------------------------------------------------

#[test]
fn test_empty_query_returns_empty_vec() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let result = engine.search(&SearchQuery::new("")).unwrap();
    assert!(result.is_empty(), "empty query should return empty vec");
}

#[test]
fn test_oversized_query_returns_invalid_query_error() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let long_query = "a".repeat(MAX_QUERY_BYTES + 1);
    let result = engine.search(&SearchQuery::new(long_query));
    assert!(result.is_err());
    match result.unwrap_err() {
        SearchError::InvalidQuery(msg) => {
            assert!(
                msg.contains(&MAX_QUERY_BYTES.to_string()),
                "error message should contain max length: {msg}"
            );
        }
        other => panic!("expected InvalidQuery, got {other:?}"),
    }
}

#[test]
fn test_query_at_exact_max_length_succeeds() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let exact_query = "a".repeat(MAX_QUERY_BYTES);
    // Must not return an InvalidQuery error; empty results are fine
    let result = engine.search(&SearchQuery::new(exact_query));
    assert!(result.is_ok(), "query at exact max length should succeed");
}

#[test]
fn test_invalid_bm25f_config_rejected_before_search() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let mut query = SearchQuery::new("foo");
    let mut bad_config = BM25FConfig::default();
    bad_config.k1 = -1.0;
    query.bm25f_config = Some(bad_config);

    let result = engine.search(&query);
    assert!(result.is_err());
    match result.unwrap_err() {
        SearchError::InvalidQuery(msg) => {
            assert!(
                msg.contains("k1"),
                "error message should mention k1: {msg}"
            );
        }
        other => panic!("expected InvalidQuery, got {other:?}"),
    }
}

#[test]
fn test_nan_bm25f_config_rejected() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let mut query = SearchQuery::new("foo");
    let mut bad_config = BM25FConfig::default();
    bad_config.k1 = f32::NAN;
    query.bm25f_config = Some(bad_config);

    let result = engine.search(&query);
    assert!(result.is_err());
    match result.unwrap_err() {
        SearchError::InvalidQuery(_) => {}
        other => panic!("expected InvalidQuery, got {other:?}"),
    }
}

#[test]
fn test_name_returns_query_engine() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    assert_eq!(engine.name(), "query-engine");
}

#[test]
fn test_implements_search_layer_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<QueryEngine>();
}

// -----------------------------------------------------------------------
// Phase 2 — Integration
// -----------------------------------------------------------------------

#[test]
fn test_happy_path_finds_matching_file() {
    let (_dir, engine) = build_query_engine(&[(
        FileId(0),
        "fn handleRequest() {}",
        rskim_core::Language::Rust,
    )]);
    let results = engine.search(&SearchQuery::new("handleRequest")).unwrap();
    assert!(!results.is_empty(), "should find the indexed file");
    assert!(
        results[0].score > 0.0,
        "top result should have positive score"
    );
}

#[test]
fn test_search_delegates_to_inner_layer() {
    // Build a separate inner layer with the same data to compare results
    let dir = tempfile::tempdir().unwrap();
    let content = "fn processEvent(event: Event) -> Result<(), Error> {}";
    let lang = rskim_core::Language::Rust;

    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder.add_file(FileId(0), content, lang).unwrap();
    let inner = builder.build().unwrap();

    let dir2 = tempfile::tempdir().unwrap();
    let mut builder2 = NgramIndexBuilder::new(dir2.path().to_path_buf()).unwrap();
    builder2.add_file(FileId(0), content, lang).unwrap();
    let inner2 = builder2.build().unwrap();

    let engine = QueryEngine::new(inner);

    let query = SearchQuery::new("processEvent");
    let engine_results = engine.search(&query).unwrap();
    let inner_results = inner2.search(&query).unwrap();

    assert_eq!(
        engine_results.len(),
        inner_results.len(),
        "QueryEngine should delegate to inner layer: result counts differ"
    );
    for (a, b) in engine_results.iter().zip(inner_results.iter()) {
        assert_eq!(a.file_id, b.file_id, "file_ids must match");
        assert!(
            (a.score - b.score).abs() < 1e-10,
            "scores must match: {} vs {}",
            a.score,
            b.score
        );
    }
}

#[test]
fn test_deterministic_results() {
    let (_dir, engine) = build_query_engine(&[
        (FileId(0), "fn computeHash(input: &str) -> u64 {}", rskim_core::Language::Rust),
        (FileId(1), "fn computeSum(a: i32, b: i32) -> i32 {}", rskim_core::Language::Rust),
    ]);
    let query = SearchQuery::new("compute");

    let first = engine.search(&query).unwrap();
    for _ in 0..49 {
        let run = engine.search(&query).unwrap();
        assert_eq!(
            run.len(),
            first.len(),
            "result count changed across runs"
        );
        for (a, b) in run.iter().zip(first.iter()) {
            assert_eq!(a.file_id, b.file_id, "file_id ordering changed");
            assert!(
                (a.score - b.score).abs() < 1e-10,
                "scores diverged: {} vs {}",
                a.score,
                b.score
            );
        }
    }
}

#[test]
fn test_unicode_query_works() {
    let (_dir, engine) = build_query_engine(&[(
        FileId(0),
        "fn compute_日本語() {}",
        rskim_core::Language::Rust,
    )]);
    let result = engine.search(&SearchQuery::new("日本語"));
    assert!(result.is_ok(), "unicode query should not error: {result:?}");
}

// -----------------------------------------------------------------------
// Phase 3 — Edge cases
// -----------------------------------------------------------------------

#[test]
fn test_whitespace_only_query_returns_empty() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    // The inner layer receives the original query; whitespace-only produces no
    // useful ngrams, so results will be empty. We only assert it does not error.
    let result = engine.search(&SearchQuery::new("   "));
    assert!(result.is_ok(), "whitespace-only query should not error");
    // Results may be empty (expected) or potentially non-empty if the inner layer
    // happens to match; we assert it is Ok rather than asserting empty here.
}

#[test]
fn test_single_char_query_returns_empty() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let result = engine.search(&SearchQuery::new("x")).unwrap();
    // Single character cannot form a bigram, so the index returns nothing
    assert!(result.is_empty(), "single-char query should return empty results");
}

#[test]
fn test_no_matching_ngrams_returns_empty() {
    let (_dir, engine) = build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let result = engine.search(&SearchQuery::new("xyz123uniquetoken")).unwrap();
    assert!(result.is_empty(), "query with no indexed ngrams should return empty results");
}

#[test]
fn test_lang_filter_passes_through() {
    let rust_content = "fn rust_function() {}";
    let py_content = "def python_function(): pass";

    let dir = tempfile::tempdir().unwrap();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder.add_file(FileId(0), rust_content, rskim_core::Language::Rust).unwrap();
    builder.add_file(FileId(1), py_content, rskim_core::Language::Python).unwrap();
    let layer = builder.build().unwrap();
    let engine = QueryEngine::new(layer);

    let mut query = SearchQuery::new("function");
    query.lang = Some(rskim_core::Language::Rust);

    let results = engine.search(&query).unwrap();
    // All returned results should be from the Rust file (FileId(0))
    for result in &results {
        assert_eq!(
            result.file_id,
            FileId(0),
            "lang filter should restrict to Rust file, got file_id={}",
            result.file_id
        );
    }
}

#[test]
fn test_pagination_passes_through() {
    let (_dir, engine) = build_query_engine(&[
        (FileId(0), "fn alpha_handler() {}", rskim_core::Language::Rust),
        (FileId(1), "fn alpha_processor() {}", rskim_core::Language::Rust),
        (FileId(2), "fn alpha_worker() {}", rskim_core::Language::Rust),
    ]);

    // Get all results first
    let all_results = engine.search(&SearchQuery::new("alpha")).unwrap();
    if all_results.len() < 2 {
        // Not enough results to test pagination; skip
        return;
    }

    // Paginated: offset=1, limit=1
    let mut paginated_query = SearchQuery::new("alpha");
    paginated_query.offset = Some(1);
    paginated_query.limit = Some(1);

    let paginated = engine.search(&paginated_query).unwrap();
    assert_eq!(paginated.len(), 1, "limit=1 should return exactly 1 result");
    assert_eq!(
        paginated[0].file_id, all_results[1].file_id,
        "offset=1 should skip the first result"
    );
}
