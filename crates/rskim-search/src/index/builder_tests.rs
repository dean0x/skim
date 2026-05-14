//! Tests for NgramIndexBuilder (builder.rs).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::SearchQuery;
use crate::index::format::lang_to_id;

fn tmp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// -----------------------------------------------------------------------
// Constructor
// -----------------------------------------------------------------------

#[test]
fn test_new_nonexistent_dir_returns_error() {
    let result = NgramIndexBuilder::new(PathBuf::from("/nonexistent/path/that/does/not/exist"));
    assert!(result.is_err());
    if let Err(e) = result {
        let err = format!("{e}");
        assert!(err.contains("does not exist"), "unexpected error: {err}");
    }
}

#[test]
fn test_new_existing_dir_succeeds() {
    let dir = tmp_dir();
    let builder = NgramIndexBuilder::new(dir.path().to_path_buf());
    assert!(builder.is_ok());
}

// -----------------------------------------------------------------------
// add_file
// -----------------------------------------------------------------------

#[test]
fn test_add_file_duplicate_id_returns_error() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(crate::FileId(0), "hello world", rskim_core::Language::Rust)
        .unwrap();
    let result = builder.add_file(
        crate::FileId(0),
        "different content",
        rskim_core::Language::Rust,
    );
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("duplicate FileId"), "unexpected error: {err}");
}

#[test]
fn test_add_file_empty_content_succeeds() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let result = builder.add_file(crate::FileId(0), "", rskim_core::Language::Rust);
    assert!(result.is_ok(), "empty content should succeed");
}

#[test]
fn test_add_file_single_byte_content_succeeds() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let result = builder.add_file(crate::FileId(0), "a", rskim_core::Language::Rust);
    assert!(result.is_ok(), "single byte content should succeed");
}

#[test]
fn test_add_file_increments_file_count() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(crate::FileId(0), "fn main() {}", rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(
            crate::FileId(1),
            "def hello(): pass",
            rskim_core::Language::Python,
        )
        .unwrap();
    assert_eq!(builder.file_count, 2);
}

// -----------------------------------------------------------------------
// build
// -----------------------------------------------------------------------

#[test]
fn test_build_empty_index_succeeds() {
    let dir = tmp_dir();
    let builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let layer = builder.build();
    assert!(
        layer.is_ok(),
        "empty build should succeed: {}",
        layer
            .err()
            .map_or_else(|| "no error".to_string(), |e| e.to_string())
    );
}

#[test]
fn test_build_creates_index_files() {
    let dir = tmp_dir();
    let builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder.build().unwrap();
    assert!(
        dir.path().join("index.skidx").exists(),
        ".skidx should exist"
    );
    assert!(
        dir.path().join("index.skpost").exists(),
        ".skpost should exist"
    );
}

#[test]
fn test_build_single_file_returns_functional_layer() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(crate::FileId(0), "fn main() {}", rskim_core::Language::Rust)
        .unwrap();
    let layer = builder.build().unwrap();
    assert_eq!(layer.name(), "ngram-index");
    let results = layer.search(&SearchQuery::new("main")).unwrap();
    assert!(!results.is_empty(), "search should return results");
}

#[test]
fn test_build_multiple_files() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(crate::FileId(0), "fn main() {}", rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(
            crate::FileId(1),
            "def main(): pass",
            rskim_core::Language::Python,
        )
        .unwrap();
    builder
        .add_file(
            crate::FileId(2),
            "function main() { return; }",
            rskim_core::Language::JavaScript,
        )
        .unwrap();
    let layer = builder.build().unwrap();
    let results = layer.search(&SearchQuery::new("main")).unwrap();
    // All three files contain "ma" "ai" "in" bigrams
    let file_ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();
    assert!(
        file_ids.len() >= 3,
        "should find all three files, got {:?}",
        file_ids
    );
}

#[test]
fn test_build_file_metadata_correctness() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(crate::FileId(0), "hello", rskim_core::Language::Rust)
        .unwrap();
    // file_meta[0] should record lang=Rust, doc_length=5
    let meta = &builder.file_meta[0];
    assert_eq!(
        meta.lang_id,
        lang_to_id(rskim_core::Language::Rust),
        "lang_id mismatch"
    );
    assert_eq!(meta.doc_length, 5, "doc_length mismatch");
}

#[test]
fn test_build_returns_layer_name() {
    let dir = tmp_dir();
    let builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let layer = builder.build().unwrap();
    assert_eq!(layer.name(), "ngram-index");
}

#[test]
fn test_empty_search_returns_empty() {
    let dir = tmp_dir();
    let builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let layer = builder.build().unwrap();
    let results = layer.search(&SearchQuery::new("anything")).unwrap();
    assert!(results.is_empty(), "empty index should return no results");
}
