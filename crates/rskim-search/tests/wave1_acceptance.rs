//! Wave 1 acceptance tests — end-to-end verification of the lexical search layer.
//!
//! These tests build real indexes from fixture files and verify search results
//! match expectations. They exercise the complete pipeline: file walking →
//! field classification → n-gram extraction → index build → query → ranking.
//!
//! All assertions go through the public `SearchIndex` / `SearchLayer` traits.
//! No internal module state is probed.
//!
//! # Naming convention
//!
//! Wave-1 and later tests omit the `test_` prefix — `#[test]` already marks
//! test functions, making the prefix redundant (idiomatic Rust style). Existing
//! wave-0 tests retain the `test_` prefix to avoid a noisy rename-only commit.
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

/// Workspace root directory (absolute).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate parent")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// Absolute path to `tests/fixtures/search/` (for file reads).
fn fixtures_dir_abs() -> PathBuf {
    workspace_root().join("tests/fixtures/search")
}

/// Relative path to a fixture file (for register_within).
fn fixture_rel(name: &str) -> PathBuf {
    PathBuf::from("tests/fixtures/search").join(name)
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
/// Each file is registered under its relative path so that `register_within`
/// validates containment correctly.
fn build_fixture_index(dir: &Path, files: &[(&str, Language)]) -> Box<dyn SearchIndex> {
    let mut builder = LexicalLayerBuilder::new(dir.to_path_buf(), workspace_root());
    for (name, lang) in files {
        let abs_path = fixtures_dir_abs().join(name);
        let rel_path = fixture_rel(name);
        let content = std::fs::read_to_string(&abs_path)
            .unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"));
        builder
            .add_file(&rel_path, &content, *lang)
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
    let index = build_fixture_index(dir.path(), &[("user_service.ts", Language::TypeScript)]);

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
    let index = build_fixture_index(dir.path(), &[("auth_handler.rs", Language::Rust)]);

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
    assert!(
        !results.is_empty(),
        "database_url should be found in config.json"
    );
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
    let index = build_fixture_index(dir.path(), &[("deploy.yaml", Language::Yaml)]);

    let results = search_names(index.as_ref(), "replicas");
    assert!(
        !results.is_empty(),
        "replicas should be found in deploy.yaml"
    );
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
    let index = build_fixture_index(dir.path(), &[("README.md", Language::Markdown)]);

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
    assert!(
        !results.is_empty(),
        "TokenGenerator should be found in utils.py"
    );
    assert!(
        results.iter().any(|(name, _)| name == "utils.py"),
        "utils.py must appear in results"
    );
}

// ============================================================================
// 7. Multi-file search returns matches from multiple files
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
// 9. Whitespace-only query returns empty results
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
// 11. offset works correctly (skips leading results)
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
// 13. Index persists the three expected files to disk
// ============================================================================

#[test]
fn index_persists_to_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _ = build_fixture_index(dir.path(), &[("user_service.ts", Language::TypeScript)]);

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
    let results = layer
        .search(&SearchQuery::text("UserService"))
        .expect("search");

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
// 20. Corrupted index detected on open
// ============================================================================

#[test]
fn corrupted_index_detected() {
    let dir = tempfile::tempdir().expect("tempdir");

    // First build a valid index so metadata.json exists.
    let _ = build_fixture_index(dir.path(), &[("user_service.ts", Language::TypeScript)]);

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
// 21. Version mismatch detected on open
// ============================================================================

#[test]
fn version_mismatch_detected() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Build a valid index first.
    let _ = build_fixture_index(dir.path(), &[("auth_handler.rs", Language::Rust)]);

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
// 22. Empty index is valid: no files, empty search results
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
    assert!(results.is_empty(), "empty index must return empty results");
}

// ============================================================================
// 23. Unicode content indexes and is searchable
// ============================================================================

#[test]
fn unicode_content_indexes_correctly() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Markdown heading with CJK characters.
    let content = "# 認証サービス\n\nThis service handles 認証 (authentication).\n";
    let path = PathBuf::from("unicode.md");

    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), PathBuf::from("/repo"));
    builder
        .add_file(&path, content, Language::Markdown)
        .expect("add_file");
    let index = Box::new(builder).build().expect("build");

    // At least indexed the file.
    assert_eq!(
        index.stats().file_count,
        1,
        "unicode file should be indexed"
    );

    // Searching for a substring that forms valid bigrams should not panic.
    let result = index.search(&SearchQuery::text("認証"));
    assert!(result.is_ok(), "unicode query must not error: {:?}", result);
}

// ============================================================================
// 24. Duplicate files are idempotent (same file added twice → 1 FileTable entry)
// ============================================================================

#[test]
fn duplicate_files_are_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let abs_path = fixtures_dir_abs().join("user_service.ts");
    let rel_path = fixture_rel("user_service.ts");
    let content = std::fs::read_to_string(&abs_path).expect("read fixture");

    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());
    builder
        .add_file(&rel_path, &content, Language::TypeScript)
        .expect("first add_file");
    builder
        .add_file(&rel_path, &content, Language::TypeScript)
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
// 25. Field boost ordering: TypeDefinition file ranks above FunctionBody-only file
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
