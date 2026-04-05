//! Wave 1 acceptance tests — end-to-end verification of the lexical search layer.
//!
//! These tests build real indexes from fixture files and verify search results
//! match expectations. They exercise the complete pipeline: file walking →
//! field classification → n-gram extraction → index build → query → ranking.
//!
//! All assertions go through the public `SearchIndex` / `SearchLayer` traits.
//! No internal module state is probed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use rskim_core::Language;
use rskim_search::{
    lexical::{builder::LexicalLayerBuilder, query::LexicalSearchLayer},
    LayerBuilder, SearchIndex, SearchLayer, SearchQuery,
};

// ============================================================================
// Helpers
// ============================================================================

/// Absolute path to `tests/fixtures/search/`.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate parent")
        .parent()
        .expect("workspace root")
        .join("tests/fixtures/search")
}

/// Return all six Wave 1 fixture files with their language tags.
fn all_fixtures() -> Vec<(&'static str, Language)> {
    vec![
        ("user_service.ts", Language::TypeScript),
        ("auth_handler.rs", Language::Rust),
        ("config.json", Language::Json),
        ("deploy.yaml", Language::Yaml),
        ("README.md", Language::Markdown),
        ("utils.py", Language::Python),
    ]
}

/// Build an index from a subset of fixture files into `dir`.
///
/// Each file is registered under its real on-disk path so that `FileTable`
/// lookups work correctly in assertions.
fn build_fixture_index(dir: &Path, files: &[(&str, Language)]) -> Box<dyn SearchIndex> {
    let mut builder = LexicalLayerBuilder::new(dir.to_path_buf(), fixtures_dir());
    for (name, lang) in files {
        let path = fixtures_dir().join(name);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"));
        builder
            .add_file(&path, &content, *lang)
            .unwrap_or_else(|e| panic!("add_file {name} failed: {e}"));
    }
    Box::new(builder).build().expect("build failed")
}

/// Build an index from all six fixtures into `dir`.
fn build_all(dir: &Path) -> Box<dyn SearchIndex> {
    build_fixture_index(dir, &all_fixtures())
}

/// Search `index` for `query` text and return `(file_name, score)` pairs.
///
/// `file_name` is the last path component of the stored path, or an empty
/// string if the path has no file-name component.
fn search_names(index: &dyn SearchIndex, query: &str) -> Vec<(String, f32)> {
    let q = SearchQuery::text(query);
    let results = index.search(&q).expect("search failed");
    results
        .iter()
        .map(|(fid, score)| {
            let name = index
                .file_table()
                .lookup(*fid)
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, *score)
        })
        .collect()
}

// ============================================================================
// 1. Search user_service.ts by class name
// ============================================================================

#[test]
fn search_user_service_by_class_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_fixture_index(
        dir.path(),
        &[("user_service.ts", Language::TypeScript)],
    );

    let results = search_names(index.as_ref(), "UserService");
    assert!(!results.is_empty(), "UserService should be found");
    assert_eq!(results[0].0, "user_service.ts");
    assert!(results[0].1 > 0.0, "score must be positive");
}

// ============================================================================
// 2. Search auth_handler.rs by struct name
// ============================================================================

#[test]
fn search_auth_handler_by_struct_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index =
        build_fixture_index(dir.path(), &[("auth_handler.rs", Language::Rust)]);

    let results = search_names(index.as_ref(), "AuthHandler");
    assert!(!results.is_empty(), "AuthHandler should be found");
    assert!(
        results.iter().any(|(name, _)| name == "auth_handler.rs"),
        "auth_handler.rs must appear in results"
    );
}

// ============================================================================
// 3. Search config.json by key name
// ============================================================================

#[test]
fn search_json_config_by_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_fixture_index(dir.path(), &[("config.json", Language::Json)]);

    let results = search_names(index.as_ref(), "database_url");
    assert!(!results.is_empty(), "database_url should be found in config.json");
    assert!(
        results.iter().any(|(name, _)| name == "config.json"),
        "config.json must appear in results"
    );
}

// ============================================================================
// 4. Search deploy.yaml by key
// ============================================================================

#[test]
fn search_yaml_by_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index =
        build_fixture_index(dir.path(), &[("deploy.yaml", Language::Yaml)]);

    let results = search_names(index.as_ref(), "replicas");
    assert!(!results.is_empty(), "replicas should be found in deploy.yaml");
    assert!(
        results.iter().any(|(name, _)| name == "deploy.yaml"),
        "deploy.yaml must appear in results"
    );
}

// ============================================================================
// 5. Search README.md by heading text
// ============================================================================

#[test]
fn search_markdown_by_heading() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index =
        build_fixture_index(dir.path(), &[("README.md", Language::Markdown)]);

    // "Authentication" appears in the README heading "MyApp Authentication Service"
    // and in API section text.
    let results = search_names(index.as_ref(), "Authentication");
    assert!(
        !results.is_empty(),
        "Authentication should be found in README.md"
    );
    assert!(
        results.iter().any(|(name, _)| name == "README.md"),
        "README.md must appear in results"
    );
}

// ============================================================================
// 6. Search utils.py by class name
// ============================================================================

#[test]
fn search_python_by_class() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_fixture_index(dir.path(), &[("utils.py", Language::Python)]);

    let results = search_names(index.as_ref(), "TokenGenerator");
    assert!(!results.is_empty(), "TokenGenerator should be found in utils.py");
    assert!(
        results.iter().any(|(name, _)| name == "utils.py"),
        "utils.py must appear in results"
    );
}

// ============================================================================
// 7. Multi-file ranking: TypeDefinition boost places user_service.ts first
// ============================================================================

#[test]
fn multi_file_ranking_prefers_type_definitions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    let results = search_names(index.as_ref(), "UserService");
    assert!(
        !results.is_empty(),
        "UserService should match at least one file"
    );
    assert_eq!(
        results[0].0, "user_service.ts",
        "user_service.ts must rank #1 because it contains the TypeDefinition for UserService \
         (boost=5.0 vs FunctionBody fallback=1.0); got {:?}",
        results
    );
}

// ============================================================================
// 8. Multi-file search returns matches from multiple files
// ============================================================================

#[test]
fn multi_file_search_returns_all_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    // "auth" appears in user_service.ts (authenticate method), auth_handler.rs
    // (AuthHandler struct), and config.json (auth section key).
    let results = search_names(index.as_ref(), "auth");
    assert!(
        results.len() >= 2,
        "auth should match at least 2 files, got {:?}",
        results
    );
}

// ============================================================================
// 9. Empty query returns empty results
// ============================================================================

#[test]
fn empty_query_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    let results = index.search(&SearchQuery::new()).expect("search");
    assert!(results.is_empty(), "no-text query must return empty results");
}

// ============================================================================
// 10. Whitespace-only query returns empty results
// ============================================================================

#[test]
fn whitespace_only_query_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    let results = index.search(&SearchQuery::text("   ")).expect("search");
    assert!(
        results.is_empty(),
        "whitespace-only query must return empty results"
    );
}

// ============================================================================
// 11. Single-character query returns empty (can't form a bigram)
// ============================================================================

#[test]
fn single_char_query_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    let results = index.search(&SearchQuery::text("a")).expect("search");
    assert!(
        results.is_empty(),
        "single-char query produces no bigrams so must return empty, got {} results",
        results.len()
    );
}

// ============================================================================
// 12. limit works correctly
// ============================================================================

#[test]
fn limit_works_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    // "fn" appears in multiple fixture files.
    let results = index
        .search(&SearchQuery::text("fn").with_limit(2))
        .expect("search");
    assert!(
        results.len() <= 2,
        "limit=2 must return at most 2 results, got {}",
        results.len()
    );
}

// ============================================================================
// 13. offset works correctly (skips leading results)
// ============================================================================

#[test]
fn offset_works_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    let all = index
        .search(&SearchQuery::text("fn"))
        .expect("search no offset");
    let with_offset = index
        .search(&SearchQuery::text("fn").with_offset(1))
        .expect("search with offset");

    if all.len() > 1 {
        assert_eq!(
            with_offset.len(),
            all.len() - 1,
            "offset=1 must skip exactly 1 result"
        );
        assert_eq!(
            with_offset[0].0, all[1].0,
            "first result after offset must equal second result of unoffset query"
        );
    }
    // If all returns 0 or 1 results the offset case is trivially correct.
}

// ============================================================================
// 14. offset past end returns empty
// ============================================================================

#[test]
fn offset_past_end_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    let results = index
        .search(&SearchQuery::text("fn").with_offset(10_000))
        .expect("search");
    assert!(
        results.is_empty(),
        "offset past end must return empty, got {} results",
        results.len()
    );
}

// ============================================================================
// 15. Scores are strictly non-increasing (descending order)
// ============================================================================

#[test]
fn scores_are_descending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    let results = index.search(&SearchQuery::text("fn")).expect("search");
    for window in results.windows(2) {
        let (_, a) = window[0];
        let (_, b) = window[1];
        assert!(
            a >= b,
            "results must be sorted descending by score: {a} >= {b}"
        );
    }
}

// ============================================================================
// 16. Index persists the three expected files to disk
// ============================================================================

#[test]
fn index_persists_to_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ = build_fixture_index(
        dir.path(),
        &[("user_service.ts", Language::TypeScript)],
    );

    for name in &["lexical.skidx", "lexical.skpost", "metadata.json"] {
        assert!(
            dir.path().join(name).exists(),
            "{name} must exist after build"
        );
    }
}

// ============================================================================
// 17. Index can be reopened and produces the same results
// ============================================================================

#[test]
fn index_can_be_reopened() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ = build_all(dir.path());

    let layer = LexicalSearchLayer::open(dir.path()).expect("open after build");
    let results = layer.search(&SearchQuery::text("UserService")).expect("search");

    assert!(
        !results.is_empty(),
        "reopened index should return results for UserService"
    );
}

// ============================================================================
// 18. stats().file_count matches the number of files added
// ============================================================================

#[test]
fn stats_match_file_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let files = all_fixtures();
    let n = files.len() as u64;
    let index = build_fixture_index(dir.path(), &files);

    assert_eq!(
        index.stats().file_count,
        n,
        "stats.file_count must equal the number of added files"
    );
}

// ============================================================================
// 19. stats().total_ngrams is non-zero after indexing real files
// ============================================================================

#[test]
fn stats_match_ngram_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let index = build_all(dir.path());

    assert!(
        index.stats().total_ngrams > 0,
        "total_ngrams must be > 0 after indexing real files"
    );
}

// ============================================================================
// 20. Large file (>5MB) is skipped — file_count stays 0
// ============================================================================

#[test]
fn large_file_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 6MB of content across many lines so avg_line_len doesn't trigger minified guard.
    let large: String = "a ".repeat(3_000_000); // ~6MB, avg_line_len=2
    let path = PathBuf::from("huge.rs");

    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    builder
        .add_file(&path, &large, Language::Rust)
        .expect("add_file should not error even for large files");
    let index = Box::new(builder).build().expect("build");

    assert_eq!(
        index.stats().file_count,
        0,
        "large file (>5MB) must be skipped, file_count should be 0"
    );
}

// ============================================================================
// 21. Minified file (single long line) is skipped — file_count stays 0
// ============================================================================

#[test]
fn minified_file_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 1000-char single line → avg_line_len=1000 > 500 → skipped.
    let minified: String = "x".repeat(1_000);
    let path = PathBuf::from("bundle.min.js");

    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    builder
        .add_file(&path, &minified, Language::JavaScript)
        .expect("add_file should not error");
    let index = Box::new(builder).build().expect("build");

    assert_eq!(
        index.stats().file_count,
        0,
        "minified file must be skipped, file_count should be 0"
    );
}

// ============================================================================
// 22. Corrupted index detected on open
// ============================================================================

#[test]
fn corrupted_index_detected() {
    let dir = tempfile::tempdir().expect("tempdir");

    // First build a valid index so metadata.json exists.
    let _ = build_fixture_index(
        dir.path(),
        &[("user_service.ts", Language::TypeScript)],
    );

    // Overwrite the binary index with garbage.
    std::fs::write(dir.path().join("lexical.skidx"), b"not a valid skim index")
        .expect("write garbage");

    let result = LexicalSearchLayer::open(dir.path());
    assert!(
        result.is_err(),
        "opening a corrupted index must return an error, not succeed"
    );
}

// ============================================================================
// 23. Version mismatch detected on open
// ============================================================================

#[test]
fn version_mismatch_detected() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Build a valid index first.
    let _ = build_fixture_index(
        dir.path(),
        &[("auth_handler.rs", Language::Rust)],
    );

    // Read the current binary, patch the version field (bytes 4..8) to 0xFF,
    // and write it back.  The format reader must reject this.
    let idx_path = dir.path().join("lexical.skidx");
    let mut bytes = std::fs::read(&idx_path).expect("read skidx");

    // INDEX_HEADER layout: magic[0..4], version[4..8], ...
    // Patch to a version that cannot be valid (0xFF_FF_FF_FF).
    if bytes.len() >= 8 {
        bytes[4] = 0xFF;
        bytes[5] = 0xFF;
        bytes[6] = 0xFF;
        bytes[7] = 0xFF;
    }
    std::fs::write(&idx_path, &bytes).expect("write patched skidx");

    let result = LexicalSearchLayer::open(dir.path());
    assert!(
        result.is_err(),
        "opening an index with wrong version must return an error"
    );
}

// ============================================================================
// 24. Empty index is valid: no files, empty search results
// ============================================================================

#[test]
fn empty_index_is_valid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    let index = Box::new(builder).build().expect("build empty index");

    assert_eq!(index.stats().file_count, 0);

    let results = index
        .search(&SearchQuery::text("anything"))
        .expect("search on empty index");
    assert!(
        results.is_empty(),
        "empty index must return empty results"
    );
}

// ============================================================================
// 25. Unicode content indexes and is searchable
// ============================================================================

#[test]
fn unicode_content_indexes_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Markdown heading with CJK characters.
    let content = "# 認証サービス\n\nThis service handles 認証 (authentication).\n";
    let path = PathBuf::from("unicode.md");

    let mut builder =
        LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    builder
        .add_file(&path, content, Language::Markdown)
        .expect("add_file");
    let index = Box::new(builder).build().expect("build");

    // At least indexed the file.
    assert_eq!(index.stats().file_count, 1, "unicode file should be indexed");

    // Searching for a substring that forms valid bigrams should not panic.
    let result = index.search(&SearchQuery::text("認証"));
    assert!(result.is_ok(), "unicode query must not error: {:?}", result);
}

// ============================================================================
// 26. Duplicate files are idempotent (same file added twice → 1 FileTable entry)
// ============================================================================

#[test]
fn duplicate_files_are_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = fixtures_dir().join("user_service.ts");
    let content = std::fs::read_to_string(&path).expect("read fixture");

    let mut builder =
        LexicalLayerBuilder::new(dir.path().to_path_buf(), fixtures_dir());
    builder
        .add_file(&path, &content, Language::TypeScript)
        .expect("first add_file");
    builder
        .add_file(&path, &content, Language::TypeScript)
        .expect("second add_file");
    let index = Box::new(builder).build().expect("build");

    assert_eq!(
        index.file_table().len(),
        1,
        "adding the same file twice must result in exactly 1 FileTable entry, \
         got {}",
        index.file_table().len()
    );
}

// ============================================================================
// 27. Field boost ordering: TypeDefinition file ranks above FunctionBody-only file
// ============================================================================

#[test]
fn field_boost_ordering() {
    let dir = tempfile::tempdir().expect("tempdir");

    // user_service.ts defines "UserService" as a class (TypeDefinition, boost=5.0).
    // auth_handler.rs does NOT define UserService but may have it in fallback body.
    let index = build_fixture_index(
        dir.path(),
        &[
            ("user_service.ts", Language::TypeScript),
            ("auth_handler.rs", Language::Rust),
        ],
    );

    let results = search_names(index.as_ref(), "UserService");
    if results.len() >= 2 {
        assert_eq!(
            results[0].0, "user_service.ts",
            "TypeDefinition context must rank above FunctionBody-only context; \
             got results: {:?}",
            results
        );
    } else {
        // Only one file matched — that's still correct (the file with the TypeDefinition).
        assert!(
            !results.is_empty(),
            "at least one result expected for UserService"
        );
        assert_eq!(results[0].0, "user_service.ts");
    }
}
