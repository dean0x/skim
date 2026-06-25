//! Tests for NgramIndexBuilder (builder.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
fn test_add_file_non_sequential_id_returns_error() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // First file must be FileId(0), not FileId(5).
    let result = builder.add_file(crate::FileId(5), "content", rskim_core::Language::Rust);
    assert!(result.is_err(), "non-sequential FileId should be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("sequential"), "unexpected error: {err}");
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
    // All three files contain the trigrams for "main" (e.g. "mai" "ain")
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

// -----------------------------------------------------------------------
// compute_field_lengths — unit tests
// -----------------------------------------------------------------------

/// Empty field_map: all source bytes must be mapped to SearchField::Other.
#[test]
fn test_compute_field_lengths_empty_map_maps_to_other() {
    let lengths = compute_field_lengths(42, &[]);
    let other_idx = crate::SearchField::Other.discriminant() as usize;
    assert_eq!(
        lengths[other_idx], 42,
        "empty field_map should assign all bytes to Other"
    );
    // Every other field must be zero.
    for (i, &len) in lengths.iter().enumerate() {
        if i != other_idx {
            assert_eq!(len, 0, "field {i} should be zero when field_map is empty");
        }
    }
}

/// source_len = 0 with empty map: every field length must be zero.
#[test]
fn test_compute_field_lengths_zero_source_empty_map() {
    let lengths = compute_field_lengths(0, &[]);
    for (i, &len) in lengths.iter().enumerate() {
        assert_eq!(len, 0, "field {i} should be zero for zero-length source");
    }
}

/// Multiple non-overlapping ranges for the same field accumulate correctly.
#[test]
fn test_compute_field_lengths_multi_range_same_field() {
    // Two FunctionSignature ranges: 0..10 (10 bytes) and 20..35 (15 bytes) → 25 total.
    let field_map: &[(std::ops::Range<usize>, crate::SearchField)] = &[
        (0..10, crate::SearchField::FunctionSignature),
        (20..35, crate::SearchField::FunctionSignature),
    ];
    let lengths = compute_field_lengths(50, field_map);
    let sig_idx = crate::SearchField::FunctionSignature.discriminant() as usize;
    assert_eq!(
        lengths[sig_idx], 25,
        "two FunctionSignature ranges should sum to 25"
    );
}

/// Multiple ranges mapping to different fields each get their own total.
#[test]
fn test_compute_field_lengths_multi_range_different_fields() {
    let field_map: &[(std::ops::Range<usize>, crate::SearchField)] = &[
        (0..5, crate::SearchField::TypeDefinition),
        (5..15, crate::SearchField::FunctionBody),
        (15..20, crate::SearchField::Comment),
    ];
    let lengths = compute_field_lengths(20, field_map);
    assert_eq!(
        lengths[crate::SearchField::TypeDefinition.discriminant() as usize],
        5
    );
    assert_eq!(
        lengths[crate::SearchField::FunctionBody.discriminant() as usize],
        10
    );
    assert_eq!(
        lengths[crate::SearchField::Comment.discriminant() as usize],
        5
    );
    // Other fields must be zero.
    assert_eq!(
        lengths[crate::SearchField::Other.discriminant() as usize],
        0
    );
}

// -----------------------------------------------------------------------
// add_file_classified — partial / non-contiguous field map
// -----------------------------------------------------------------------

/// When the field_map leaves byte ranges uncovered, add_file_classified must
/// still succeed. compute_field_lengths only sums ranges explicitly in the map;
/// gap bytes are not added to Other in field_lengths (only the trigram scanning
/// loop assigns Other postings for positions outside any mapped range).
#[test]
fn test_add_file_classified_partial_field_map_succeeds() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // Source is 20 bytes. Map only the first 5 bytes to FunctionSignature;
    // bytes 5..20 are uncovered (gap).
    let source = "fn foo() { return; }"; // exactly 20 bytes
    assert_eq!(source.len(), 20, "test source must be exactly 20 bytes");

    let field_map: &[(std::ops::Range<usize>, crate::SearchField)] =
        &[(0..5, crate::SearchField::FunctionSignature)];

    builder
        .add_file_classified(
            crate::FileId(0),
            source,
            rskim_core::Language::Rust,
            field_map,
        )
        .expect("add_file_classified with partial field_map must succeed");

    // compute_field_lengths only sums ranges present in field_map. The 5-byte
    // FunctionSignature range is captured; the 15-byte gap is NOT added to Other
    // in field_lengths (gap bytes get Other postings in the trigram loop, not here).
    let meta = &builder.file_meta[0];
    let sig_idx = crate::SearchField::FunctionSignature.discriminant() as usize;
    let other_idx = crate::SearchField::Other.discriminant() as usize;
    assert_eq!(
        meta.field_lengths[sig_idx], 5,
        "FunctionSignature should cover the first 5 bytes"
    );
    assert_eq!(
        meta.field_lengths[other_idx], 0,
        "Other is 0 in field_lengths because compute_field_lengths only sums \
         ranges present in field_map — gap bytes don't appear here"
    );
    // doc_length still reflects the full source size.
    assert_eq!(
        meta.doc_length, 20,
        "doc_length must equal full source length"
    );
}

/// Non-contiguous field_map with a gap in the middle.  The metadata field
/// lengths should reflect only the mapped ranges; the build step should succeed.
#[test]
fn test_add_file_classified_non_contiguous_map_builds_successfully() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // Source: 30 bytes. Map bytes 0..10 and 20..30; bytes 10..20 are a gap.
    let source = "abcdefghijklmnopqrstuvwxyz1234"; // 30 bytes
    assert_eq!(source.len(), 30);

    let field_map: &[(std::ops::Range<usize>, crate::SearchField)] = &[
        (0..10, crate::SearchField::SymbolName),
        (20..30, crate::SearchField::Comment),
    ];

    builder
        .add_file_classified(
            crate::FileId(0),
            source,
            rskim_core::Language::Rust,
            field_map,
        )
        .expect("non-contiguous field_map must be accepted");

    let meta = &builder.file_meta[0];
    assert_eq!(
        meta.field_lengths[crate::SearchField::SymbolName.discriminant() as usize],
        10
    );
    assert_eq!(
        meta.field_lengths[crate::SearchField::Comment.discriminant() as usize],
        10
    );

    // Build must succeed and the layer must be searchable.
    let layer = builder
        .build()
        .expect("build with non-contiguous map must succeed");
    assert_eq!(layer.name(), "ngram-index");
}
