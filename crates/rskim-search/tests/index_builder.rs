//! Integration tests for `lexical::builder::LexicalLayerBuilder`.
//!
//! Tests exercise the builder at its public API boundary: `add_file`, `build`,
//! and the resulting `SearchIndex` implementation. No internal state is probed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use rskim_core::Language;
use rskim_search::lexical::{builder::LexicalLayerBuilder, query::LexicalSearchLayer};
use rskim_search::{LayerBuilder, SearchIndex};

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

/// Absolute path to a fixture file.
fn fixture_abs(name: &str) -> PathBuf {
    workspace_root().join("tests/fixtures/search").join(name)
}

/// Relative path to a fixture file (relative to workspace root).
/// This is what `register_within` expects — no absolute paths.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from("tests/fixtures/search").join(name)
}

/// Read a fixture file. Panics if the file does not exist.
fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_abs(name))
        .unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"))
}

// ============================================================================
// 1. Empty builder → valid empty index
// ============================================================================

#[test]
fn empty_builder_produces_valid_empty_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());
    let layer = Box::new(builder).build().expect("build");

    let stats = layer.stats();
    assert_eq!(stats.file_count, 0, "empty index should have 0 files");
    assert_eq!(stats.total_ngrams, 0, "empty index should have 0 ngrams");
}

// ============================================================================
// 2. Single TypeScript file
// ============================================================================

#[test]
fn single_typescript_file_indexes_correctly() {
    let content = read_fixture("user_service.ts");
    let path = fixture("user_service.ts");

    let dir = tempfile::tempdir().expect("tempdir");
    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());

    builder
        .add_file(&path, &content, Language::TypeScript)
        .expect("add_file");
    let layer = Box::new(builder).build().expect("build");

    let stats = layer.stats();
    assert_eq!(stats.file_count, 1, "should have 1 file");
    assert!(stats.total_ngrams > 0, "should have indexed some ngrams");

    // FileTable should contain the registered path.
    let ft = layer.file_table();
    assert_eq!(ft.len(), 1, "file table should have 1 entry");
}

// ============================================================================
// 3. Single Rust file
// ============================================================================

#[test]
fn single_rust_file_indexes_correctly() {
    let content = read_fixture("auth_handler.rs");
    let path = fixture("auth_handler.rs");

    let dir = tempfile::tempdir().expect("tempdir");
    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());

    builder
        .add_file(&path, &content, Language::Rust)
        .expect("add_file");
    let layer = Box::new(builder).build().expect("build");

    assert_eq!(layer.stats().file_count, 1);
    assert!(layer.stats().total_ngrams > 0);
}

// ============================================================================
// 4. Single JSON file (serde path)
// ============================================================================

#[test]
fn single_json_file_indexes_via_serde_path() {
    let content = read_fixture("config.json");
    let path = fixture("config.json");

    let dir = tempfile::tempdir().expect("tempdir");
    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());

    builder
        .add_file(&path, &content, Language::Json)
        .expect("add_file");
    let layer = Box::new(builder).build().expect("build");

    assert_eq!(layer.stats().file_count, 1);
}

// ============================================================================
// 5. Multiple files across languages
// ============================================================================

#[test]
fn multiple_files_across_languages() {
    let fixtures: &[(&str, Language)] = &[
        ("user_service.ts", Language::TypeScript),
        ("auth_handler.rs", Language::Rust),
        ("config.json", Language::Json),
        ("deploy.yaml", Language::Yaml),
        ("utils.py", Language::Python),
    ];

    let dir = tempfile::tempdir().expect("tempdir");
    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());

    for (name, lang) in fixtures {
        let path = fixture(name);
        let content = read_fixture(name);
        builder.add_file(&path, &content, *lang).expect("add_file");
    }

    let layer = Box::new(builder).build().expect("build");
    let stats = layer.stats();

    assert_eq!(stats.file_count, 5, "should have 5 files");
    assert!(stats.total_ngrams > 0, "should have some ngrams");
}

// ============================================================================
// 6. Minified file is skipped
// ============================================================================

#[test]
fn minified_file_is_skipped() {
    // avg_line_length = 1000 / 1 = 1000 > 500 → should be skipped.
    let minified: String = "x".repeat(1000);
    let path = PathBuf::from("minified.js");

    let dir = tempfile::tempdir().expect("tempdir");
    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());

    builder
        .add_file(&path, &minified, Language::JavaScript)
        .expect("add_file should not error");
    let layer = Box::new(builder).build().expect("build");

    // Skipped file should not appear in the file table.
    assert_eq!(
        layer.stats().file_count,
        0,
        "minified file should be skipped"
    );
}

// ============================================================================
// 7. Large file (>5MB) is skipped
// ============================================================================

#[test]
fn large_file_is_skipped() {
    // 6MB of content.
    let large: String = "a ".repeat(3_000_000); // ~6MB
    let path = PathBuf::from("huge.rs");

    let dir = tempfile::tempdir().expect("tempdir");
    let mut builder = LexicalLayerBuilder::new(dir.path().to_path_buf(), workspace_root());

    builder
        .add_file(&path, &large, Language::Rust)
        .expect("add_file should not error");
    let layer = Box::new(builder).build().expect("build");

    assert_eq!(
        layer.stats().file_count,
        0,
        "large file (>5MB) should be skipped"
    );
}

// ============================================================================
// 8. Index files exist on disk after build
// ============================================================================

#[test]
fn index_files_exist_on_disk_after_build() {
    let content = read_fixture("user_service.ts");
    let path = fixture("user_service.ts");

    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().to_path_buf();

    let mut builder = LexicalLayerBuilder::new(index_dir.clone(), PathBuf::from("/repo"));
    builder
        .add_file(&path, &content, Language::TypeScript)
        .expect("add_file");
    Box::new(builder).build().expect("build");

    assert!(
        index_dir.join("lexical.skidx").exists(),
        "lexical.skidx should exist"
    );
    assert!(
        index_dir.join("lexical.skpost").exists(),
        "lexical.skpost should exist"
    );
    assert!(
        index_dir.join("metadata.json").exists(),
        "metadata.json should exist"
    );
}

// ============================================================================
// 9. Built layer can be re-opened
// ============================================================================

#[test]
fn built_layer_can_be_reopened() {
    let content = read_fixture("auth_handler.rs");
    let path = fixture("auth_handler.rs");

    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().to_path_buf();

    let mut builder = LexicalLayerBuilder::new(index_dir.clone(), workspace_root());
    builder
        .add_file(&path, &content, Language::Rust)
        .expect("add_file");
    Box::new(builder).build().expect("build");

    // Re-open independently.
    let reopened = LexicalSearchLayer::open(&index_dir).expect("reopen");
    assert_eq!(
        reopened.stats().file_count,
        1,
        "reopened index should have 1 file"
    );
}

// ============================================================================
// 10. Metadata roundtrip
// ============================================================================

#[test]
fn metadata_roundtrip() {
    let content = read_fixture("user_service.ts");
    let path = fixture("user_service.ts");
    let repo_root = workspace_root();

    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().to_path_buf();

    let mut builder = LexicalLayerBuilder::new(index_dir.clone(), repo_root.clone());
    builder
        .add_file(&path, &content, Language::TypeScript)
        .expect("add_file");
    Box::new(builder).build().expect("build");

    // Read metadata.json directly and verify fields.
    let meta_str = std::fs::read_to_string(index_dir.join("metadata.json")).expect("read metadata");
    let meta: serde_json::Value = serde_json::from_str(&meta_str).expect("parse metadata");

    // repo_root is stored in the JSON.
    let stored_root = meta["repo_root"].as_str().expect("repo_root is string");
    assert_eq!(
        Path::new(stored_root),
        repo_root,
        "repo_root should roundtrip"
    );

    // file_mtimes should be non-empty.
    let mtimes = meta["file_mtimes"]
        .as_array()
        .expect("file_mtimes is array");
    assert!(
        !mtimes.is_empty(),
        "file_mtimes should be non-empty after indexing one file"
    );
}

// ============================================================================
// 11. stored_file_mtimes() returns expected (path, mtime) pairs after reopen
// ============================================================================

#[test]
fn stored_file_mtimes_after_reopen() {
    let content = read_fixture("user_service.ts");
    let rel_path = fixture("user_service.ts");
    let repo_root = workspace_root();

    let dir = tempfile::tempdir().expect("tempdir");
    let index_dir = dir.path().to_path_buf();

    let mut builder = LexicalLayerBuilder::new(index_dir.clone(), repo_root.clone());
    builder
        .add_file(&rel_path, &content, Language::TypeScript)
        .expect("add_file");
    let built = Box::new(builder).build().expect("build");

    // Capture the mtime that was stored during build (may be 0 if the path
    // cannot be stat'd from the test runner's CWD).
    let built_mtimes = built.stored_file_mtimes().to_vec();

    // Reopen the index and inspect stored_file_mtimes.
    use rskim_search::SearchIndex;
    let layer = LexicalSearchLayer::open(&index_dir).expect("reopen");
    let mtimes = layer.stored_file_mtimes();

    assert_eq!(
        mtimes.len(),
        1,
        "stored_file_mtimes must contain exactly one entry after indexing one file, got {}",
        mtimes.len()
    );

    let (stored_path, stored_mtime) = &mtimes[0];
    assert_eq!(
        stored_path, &rel_path,
        "stored path must match the registered relative path"
    );

    // Verify the mtime roundtrips correctly through build → persist → reopen.
    assert_eq!(
        *stored_mtime, built_mtimes[0].1,
        "mtime must roundtrip: built={}, reopened={}",
        built_mtimes[0].1, stored_mtime,
    );

    // The mtime must be a valid u64 (0 is allowed when stat fails for a relative path).
    let _ = *stored_mtime; // u64 is always a valid unix timestamp or 0
}
