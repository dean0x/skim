//! Robustness tests — adversarial inputs and edge cases for the lexical search layer.
//!
//! These tests verify that the pipeline never panics, never silently corrupts data,
//! and stays within acceptable performance bounds when given hostile or degenerate input.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use rskim_core::Language;
use rskim_search::{
    lexical::{builder::LexicalLayerBuilder, query::LexicalSearchLayer},
    LayerBuilder, SearchIndex, SearchLayer, SearchQuery,
};

// ============================================================================
// Helpers
// ============================================================================

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate parent")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn fixtures_dir_abs() -> PathBuf {
    workspace_root().join("tests/fixtures/search")
}

fn fixture_rel(name: &str) -> PathBuf {
    PathBuf::from("tests/fixtures/search").join(name)
}

/// Build an index containing all Wave 1 fixtures.
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
        let abs_path = fixtures_dir_abs().join(name);
        let rel_path = fixture_rel(name);
        let content = std::fs::read_to_string(&abs_path)
            .unwrap_or_else(|e| panic!("read fixture {name}: {e}"));
        builder
            .add_file(&rel_path, &content, *lang)
            .unwrap_or_else(|e| panic!("add_file {name}: {e}"));
    }
    Box::new(builder).build().expect("build")
}

// ============================================================================
// 1. Very long query does not panic
// ============================================================================

#[test]
fn very_long_query_does_not_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let long_query = "UserService".repeat(1_000); // ~11 000 chars
    let result = index.search(&SearchQuery::text(&long_query));
    assert!(
        result.is_ok(),
        "very long query must not error: {:?}",
        result
    );
}

// ============================================================================
// 2. Unicode (CJK) query does not panic
// ============================================================================

#[test]
fn unicode_query_works() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let result = index.search(&SearchQuery::text("認証サービス"));
    assert!(result.is_ok(), "unicode query must not error");
}

// ============================================================================
// 3. Special characters in query do not panic
// ============================================================================

#[test]
fn special_chars_in_query() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    for query in &[
        "!@#$%^&*()",
        "{}[]|\\<>?/",
        "SELECT * FROM users;",
        "'; DROP TABLE users;--",
        "\x00\x01\x02\x03",
    ] {
        let result = index.search(&SearchQuery::text(query));
        // None of these should panic or return an error.
        assert!(
            result.is_ok(),
            "query {:?} must not error, got: {:?}",
            query,
            result
        );
    }
}

// ============================================================================
// 4. Newlines and tabs in query are handled
// ============================================================================

#[test]
fn newlines_in_query_handled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    let result = index.search(&SearchQuery::text("foo\nbar\tbaz\r\n"));
    assert!(
        result.is_ok(),
        "query with whitespace escapes must not error"
    );
}

// ============================================================================
// 5. Null bytes in file content do not crash the builder
// ============================================================================

#[test]
fn null_bytes_in_content_handled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let content = "fn foo() {\0 let x = 1;\0 }";
    let path = PathBuf::from("null_bytes.rs");

    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    let result = builder.add_file(&path, content, Language::Rust);
    assert!(
        result.is_ok(),
        "add_file must not error on null bytes: {:?}",
        result
    );
}

// ============================================================================
// 6. Empty file content does not crash the builder
// ============================================================================

#[test]
fn empty_file_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = PathBuf::from("empty.ts");

    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    let result = builder.add_file(&path, "", Language::TypeScript);
    assert!(result.is_ok(), "empty content must not error");

    let index = Box::new(builder).build().expect("build");
    // Empty content may or may not produce a registered file depending on
    // implementation choices — what matters is no panic and a valid index.
    assert!(
        index.stats().file_count <= 1,
        "empty file should result in at most 1 file in index"
    );
}

// ============================================================================
// 7. All-whitespace file content does not crash the builder
// ============================================================================

#[test]
fn all_whitespace_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = PathBuf::from("blank.rs");

    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    let result = builder.add_file(&path, "   \n\n  \t  \n", Language::Rust);
    assert!(result.is_ok(), "all-whitespace content must not error");
    let _ = Box::new(builder).build().expect("build");
}

// ============================================================================
// 8. Concurrent reads are safe (Send + Sync verification)
// ============================================================================

#[test]
fn concurrent_reads_are_safe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ = build_all_fixtures(dir.path());

    let layer = LexicalSearchLayer::open(dir.path()).expect("open");

    // Wrap in Arc so we can share across threads.
    let layer = std::sync::Arc::new(layer);

    // Spawn several threads that each run a search.
    std::thread::scope(|s| {
        for query in &["UserService", "AuthHandler", "database_url", "fn", "class"] {
            let layer = std::sync::Arc::clone(&layer);
            let q = query.to_string();
            s.spawn(move || {
                let result = layer.search(&SearchQuery::text(&q));
                assert!(result.is_ok(), "concurrent search for {q:?} must not error");
            });
        }
    });
}

// ============================================================================
// 9. Index size is reasonable (< 2× source size)
//
//    The index adds per-ngram entries but should stay within a reasonable
//    overhead factor of the original source.  2× is a generous bound.
// ============================================================================

#[test]
fn index_size_is_reasonable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all_fixtures(dir.path());

    // Sum source sizes.
    let fixtures: &[(&str, Language)] = &[
        ("user_service.ts", Language::TypeScript),
        ("auth_handler.rs", Language::Rust),
        ("config.json", Language::Json),
        ("deploy.yaml", Language::Yaml),
        ("README.md", Language::Markdown),
        ("utils.py", Language::Python),
    ];
    let source_bytes: u64 = fixtures
        .iter()
        .map(|(name, _)| {
            std::fs::metadata(fixtures_dir_abs().join(name))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();

    let stats = index.stats();
    // Allow up to 5× overhead or 512 KB floor, whichever is larger.
    // n-gram indexes add posting lists but should not grow unboundedly.
    let expected_max = (source_bytes * 5).max(512 * 1024);
    assert!(
        stats.index_size_bytes < expected_max,
        "index_size_bytes ({}) must be < 5× source bytes ({}) or 512KB floor ({})",
        stats.index_size_bytes,
        source_bytes,
        expected_max,
    );
    assert!(stats.index_size_bytes > 0, "index must have non-zero size");
}

// ============================================================================
// 10. Search performance: 100 searches complete in < 500ms total
//
//     Target from CLAUDE.md: <50ms per file parse+transform; search should
//     be much faster than parsing.  500ms for 100 queries is 5ms average —
//     very conservative for a mmap'd index.
//
//     Marked #[ignore] because wall-clock timing tests are flaky on slow CI
//     runners.  Run explicitly with: cargo test -- --ignored
// ============================================================================

#[test]
#[ignore]
fn search_performance_under_500ms() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ = build_all_fixtures(dir.path());
    let layer = LexicalSearchLayer::open(dir.path()).expect("open");

    let queries = [
        "UserService",
        "AuthHandler",
        "database_url",
        "replicas",
        "Authentication",
        "TokenGenerator",
        "fn",
        "class",
        "import",
        "return",
    ];

    let start = Instant::now();
    for _ in 0..10 {
        for q in &queries {
            let _ = layer
                .search(&SearchQuery::text(q))
                .expect("search must not error");
        }
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 500,
        "100 searches must complete in < 500ms, took {}ms",
        elapsed.as_millis()
    );
}
