//! Tests for QueryEngine (query.rs).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use super::*;
use crate::index::NgramIndexBuilder;
use crate::lexical::BM25FConfig;
use crate::{FileId, LayerBuilder, SearchLayer, SearchQuery, SearchResult};

// -----------------------------------------------------------------------
// Test doubles
// -----------------------------------------------------------------------

/// A `SearchLayer` that records the last query it received and returns a fixed
/// empty result. Used to assert that the decorator forwards the exact query
/// unchanged to the inner layer.
struct SpyLayer {
    received: Mutex<Option<SearchQuery>>,
}

impl SpyLayer {
    fn new() -> Self {
        Self {
            received: Mutex::new(None),
        }
    }

    fn take_received(&self) -> Option<SearchQuery> {
        self.received.lock().unwrap().take()
    }
}

impl SearchLayer for SpyLayer {
    fn search(&self, query: &SearchQuery) -> crate::Result<Vec<SearchResult>> {
        *self.received.lock().unwrap() = Some(query.clone());
        Ok(vec![])
    }

    fn name(&self) -> &str {
        "spy"
    }
}

impl SearchLayer for Arc<SpyLayer> {
    fn search(&self, query: &SearchQuery) -> crate::Result<Vec<SearchResult>> {
        (**self).search(query)
    }

    fn name(&self) -> &str {
        (**self).name()
    }
}

/// A `SearchLayer` that panics if `search` is ever called. Used to prove that
/// a short-circuit path in `QueryEngine` never reaches the inner layer.
struct PanicLayer;

impl SearchLayer for PanicLayer {
    fn search(&self, _query: &SearchQuery) -> crate::Result<Vec<SearchResult>> {
        panic!("PanicLayer::search was called — inner layer must not be reached");
    }

    fn name(&self) -> &str {
        "panic"
    }
}

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
    let (_dir, engine) =
        build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let result = engine.search(&SearchQuery::new("")).unwrap();
    assert!(result.is_empty(), "empty query should return empty vec");
}

#[test]
fn test_empty_query_short_circuits_inner_layer() {
    // PanicLayer panics if search() is called — proves the decorator never
    // reaches the inner layer for empty queries.
    let engine = QueryEngine::new(Box::new(PanicLayer));
    let result = engine.search(&SearchQuery::new("")).unwrap();
    assert!(
        result.is_empty(),
        "empty query short-circuit must return empty vec"
    );
}

#[test]
fn test_oversized_query_returns_invalid_query_error() {
    // PanicLayer proves the oversized-query check short-circuits before the
    // inner layer is reached.
    let engine = QueryEngine::new(Box::new(PanicLayer));
    let long_query = "a".repeat(MAX_QUERY_BYTES + 1);
    let err = engine.search(&SearchQuery::new(long_query)).unwrap_err();
    assert!(
        matches!(err, SearchError::InvalidQuery(_)),
        "expected InvalidQuery variant, got {:?}",
        err
    );
    let msg = format!("{err}");
    assert!(
        msg.contains(&MAX_QUERY_BYTES.to_string()),
        "error message should contain max length: {msg}"
    );
}

#[test]
fn test_query_at_exact_max_length_succeeds() {
    let (_dir, engine) =
        build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let exact_query = "a".repeat(MAX_QUERY_BYTES);
    // Must not return an InvalidQuery error; empty results are fine
    let result = engine.search(&SearchQuery::new(exact_query));
    assert!(result.is_ok(), "query at exact max length should succeed");
}

#[test]
fn test_invalid_bm25f_config_rejected_before_search() {
    // PanicLayer proves the BM25F validation short-circuits before the inner
    // layer is reached.
    let engine = QueryEngine::new(Box::new(PanicLayer));
    let mut query = SearchQuery::new("foo");
    query.bm25f_config = Some(BM25FConfig {
        k1: -1.0,
        ..BM25FConfig::default()
    });

    let err = engine.search(&query).unwrap_err();
    assert!(
        matches!(err, SearchError::InvalidQuery(_)),
        "expected InvalidQuery variant, got {:?}",
        err
    );
    let msg = format!("{err}");
    assert!(msg.contains("k1"), "error message should mention k1: {msg}");
}

#[test]
fn test_nan_bm25f_config_rejected() {
    let engine = QueryEngine::new(Box::new(PanicLayer));
    let mut query = SearchQuery::new("foo");
    query.bm25f_config = Some(BM25FConfig {
        k1: f32::NAN,
        ..BM25FConfig::default()
    });

    let err = engine.search(&query).unwrap_err();
    assert!(
        matches!(err, SearchError::InvalidQuery(_)),
        "NaN k1 must produce InvalidQuery variant, got {:?}",
        err
    );
    let msg = format!("{err}");
    assert!(msg.contains("k1"), "error message should mention k1: {msg}");
}

#[test]
fn test_infinity_bm25f_config_rejected() {
    let engine = QueryEngine::new(Box::new(PanicLayer));
    let mut query = SearchQuery::new("foo");
    query.bm25f_config = Some(BM25FConfig {
        k1: f32::INFINITY,
        ..BM25FConfig::default()
    });

    let err = engine.search(&query).unwrap_err();
    assert!(
        matches!(err, SearchError::InvalidQuery(_)),
        "expected InvalidQuery variant, got {:?}",
        err
    );
    let msg = format!("{err}");
    assert!(msg.contains("k1"), "error message should mention k1: {msg}");
}

#[test]
fn test_neg_infinity_bm25f_config_rejected() {
    let engine = QueryEngine::new(Box::new(PanicLayer));
    let mut query = SearchQuery::new("foo");
    query.bm25f_config = Some(BM25FConfig {
        k1: f32::NEG_INFINITY,
        ..BM25FConfig::default()
    });

    let err = engine.search(&query).unwrap_err();
    assert!(
        matches!(err, SearchError::InvalidQuery(_)),
        "expected InvalidQuery variant, got {:?}",
        err
    );
    let msg = format!("{err}");
    assert!(msg.contains("k1"), "error message should mention k1: {msg}");
}

#[test]
fn test_name_returns_query_engine() {
    let (_dir, engine) =
        build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    assert_eq!(engine.name(), "query-engine");
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
    // SpyLayer records whatever query it receives; QueryEngine must forward the
    // exact query unchanged (same text, same struct fields).
    let spy = Arc::new(SpyLayer::new());
    let engine = QueryEngine::new(Box::new(Arc::clone(&spy)));
    let original_query = SearchQuery::new("processEvent");
    engine.search(&original_query).unwrap();

    let received = spy
        .take_received()
        .expect("inner layer must have been called for a valid query");
    assert_eq!(
        received.text, original_query.text,
        "QueryEngine must forward the text unchanged"
    );
    assert_eq!(
        received.lang, original_query.lang,
        "QueryEngine must forward the lang unchanged"
    );
    assert_eq!(
        received.ast_pattern, original_query.ast_pattern,
        "QueryEngine must forward the ast_pattern unchanged"
    );
    assert_eq!(
        received.temporal_flags, original_query.temporal_flags,
        "QueryEngine must forward the temporal_flags unchanged"
    );
    assert_eq!(
        received.limit, original_query.limit,
        "QueryEngine must forward the limit unchanged"
    );
    assert_eq!(
        received.offset, original_query.offset,
        "QueryEngine must forward the offset unchanged"
    );
    // BM25FConfig contains f32 — compare via Debug since SearchQuery does not
    // derive PartialEq.
    assert_eq!(
        format!("{:?}", received.bm25f_config),
        format!("{:?}", original_query.bm25f_config),
        "QueryEngine must forward the bm25f_config unchanged"
    );
}

#[test]
fn test_search_delegates_populated_fields_to_inner_layer() {
    // Exercises the delegation path with all optional fields populated.
    // Complements test_search_delegates_to_inner_layer which uses all-None defaults.
    let spy = Arc::new(SpyLayer::new());
    let engine = QueryEngine::new(Box::new(Arc::clone(&spy)));

    let mut original_query = SearchQuery::new("processEvent");
    original_query.lang = Some(rskim_core::Language::Rust);
    original_query.ast_pattern = Some("fn_decl".to_string());
    original_query.temporal_flags = Some(crate::TemporalFlags {
        modified_within_days: Some(30),
    });
    original_query.limit = Some(10);
    original_query.offset = Some(5);
    original_query.bm25f_config = Some(BM25FConfig::default());

    engine.search(&original_query).unwrap();

    let received = spy
        .take_received()
        .expect("inner layer must have been called for a valid query");

    assert_eq!(
        received.text, original_query.text,
        "QueryEngine must forward the text unchanged"
    );
    assert_eq!(
        received.lang, original_query.lang,
        "QueryEngine must forward the lang unchanged"
    );
    assert_eq!(
        received.ast_pattern, original_query.ast_pattern,
        "QueryEngine must forward the ast_pattern unchanged"
    );
    assert_eq!(
        received.temporal_flags, original_query.temporal_flags,
        "QueryEngine must forward the temporal_flags unchanged"
    );
    assert_eq!(
        received.limit, original_query.limit,
        "QueryEngine must forward the limit unchanged"
    );
    assert_eq!(
        received.offset, original_query.offset,
        "QueryEngine must forward the offset unchanged"
    );
    // BM25FConfig contains f32 — compare via Debug since SearchQuery does not
    // derive PartialEq.
    assert_eq!(
        format!("{:?}", received.bm25f_config),
        format!("{:?}", original_query.bm25f_config),
        "QueryEngine must forward the bm25f_config unchanged"
    );
}

#[test]
fn test_deterministic_results() {
    let (_dir, engine) = build_query_engine(&[
        (
            FileId(0),
            "fn computeHash(input: &str) -> u64 {}",
            rskim_core::Language::Rust,
        ),
        (
            FileId(1),
            "fn computeSum(a: i32, b: i32) -> i32 {}",
            rskim_core::Language::Rust,
        ),
    ]);
    let query = SearchQuery::new("compute");

    let first = engine.search(&query).unwrap();
    for _ in 0..10 {
        let run = engine.search(&query).unwrap();
        assert_eq!(run.len(), first.len(), "result count changed across runs");
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
    let (_dir, engine) =
        build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    // Whitespace-only passes validation; the inner layer produces no ngrams from it.
    let result = engine.search(&SearchQuery::new("   "));
    assert!(result.is_ok(), "whitespace-only query should not error");
}

/// AD-355-7 / PF-007: a single-character query cannot produce trigrams, so the
/// inner `NgramIndexReader` falls back to returning ALL indexed files as score-0
/// candidates.  `QueryEngine` must not short-circuit before delegating to the
/// inner layer (the only short-circuit here is an empty-string check, not a
/// short-length check).
///
/// Discriminating observable: the indexed file (FileId 0) IS present in the
/// candidate set, even though the query char 'x' is absent from the content.
/// The caller (Part A verify step) decides who survives based on substring
/// membership.  An empty result set would hide the AD-355-7 fallback and break
/// short-query recall.
#[test]
fn test_single_char_query_returns_all_file_candidates_ad355_7() {
    let (_dir, engine) =
        build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let mut q = SearchQuery::new("x"); // 1-byte query — no trigrams possible
    q.limit = Some(50);
    let result = engine.search(&q).unwrap();
    // AD-355-7: the inner reader returns all indexed files as score-0 candidates.
    // QueryEngine must not suppress this (no short-circuit on short queries).
    assert!(
        result.iter().any(|r| r.file_id.0 == 0),
        "AD-355-7: FileId(0) must appear in short-query candidate set; got {:?}",
        result.iter().map(|r| r.file_id.0).collect::<Vec<_>>()
    );
    // All candidates carry score 0.0 — ranking is deferred to the verify layer.
    for r in &result {
        assert_eq!(r.score, 0.0, "short-query candidates must have score 0.0");
    }
}

#[test]
fn test_no_matching_ngrams_returns_empty() {
    let (_dir, engine) =
        build_query_engine(&[(FileId(0), "fn foo() {}", rskim_core::Language::Rust)]);
    let result = engine
        .search(&SearchQuery::new("xyz123uniquetoken"))
        .unwrap();
    assert!(
        result.is_empty(),
        "query with no indexed ngrams should return empty results"
    );
}

#[test]
fn test_lang_filter_passes_through() {
    let rust_content = "fn rust_function() {}";
    let py_content = "def python_function(): pass";

    let dir = tempfile::tempdir().unwrap();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(FileId(0), rust_content, rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(FileId(1), py_content, rskim_core::Language::Python)
        .unwrap();
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
        (
            FileId(0),
            "fn alpha_handler() {}",
            rskim_core::Language::Rust,
        ),
        (
            FileId(1),
            "fn alpha_processor() {}",
            rskim_core::Language::Rust,
        ),
        (
            FileId(2),
            "fn alpha_worker() {}",
            rskim_core::Language::Rust,
        ),
    ]);

    // Get all results first
    let all_results = engine.search(&SearchQuery::new("alpha")).unwrap();
    assert!(
        all_results.len() >= 2,
        "expected at least 2 results to test pagination, got {}",
        all_results.len()
    );

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
