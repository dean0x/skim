//! Tests for NgramIndexReader (reader.rs).

#![allow(clippy::unwrap_used)]

use std::path::Path;

use super::*;
use crate::index::NgramIndexBuilder;
use crate::{FileId, LayerBuilder, SearchLayer, SearchQuery};

fn tmp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn build_index_with(
    files: &[(FileId, &str, rskim_core::Language)],
) -> (tempfile::TempDir, Box<dyn SearchLayer>) {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for (id, content, lang) in files {
        builder.add_file(*id, content, *lang).unwrap();
    }
    let layer = builder.build().unwrap();
    (dir, layer)
}

// -----------------------------------------------------------------------
// open errors
// -----------------------------------------------------------------------

#[test]
fn test_open_nonexistent_dir_fails() {
    let result = NgramIndexReader::open(Path::new("/nonexistent/path"));
    assert!(result.is_err());
}

#[test]
fn test_open_empty_dir_fails() {
    let dir = tmp_dir();
    let result = NgramIndexReader::open(dir.path());
    assert!(result.is_err());
}

#[test]
fn test_open_corrupt_index_fails() {
    let dir = tmp_dir();
    // Write garbage to .skidx
    std::fs::write(dir.path().join("index.skidx"), b"garbage data").unwrap();
    std::fs::write(dir.path().join("index.skpost"), b"").unwrap();
    let result = NgramIndexReader::open(dir.path());
    assert!(result.is_err());
    if let Err(e) = result {
        let err = format!("{e}");
        assert!(
            err.contains("bad magic") || err.contains("truncated") || err.contains("mismatch"),
            "unexpected error: {err}"
        );
    }
}

// -----------------------------------------------------------------------
// stats
// -----------------------------------------------------------------------

#[test]
fn test_stats_empty_index() {
    let dir = tmp_dir();
    let builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let stats = reader.stats();
    assert_eq!(stats.file_count, 0);
    assert_eq!(stats.total_ngrams, 0);
}

#[test]
fn test_stats_single_file() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust)
        .unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let stats = reader.stats();
    assert_eq!(stats.file_count, 1);
    assert!(stats.total_ngrams > 0, "should have n-grams");
    assert!(stats.index_size_bytes > 0);
}

// -----------------------------------------------------------------------
// search — basic
// -----------------------------------------------------------------------

#[test]
fn test_search_empty_query_returns_empty() {
    let (_dir, layer) =
        build_index_with(&[(FileId(0), "fn main() {}", rskim_core::Language::Rust)]);
    let results = layer.search(&SearchQuery::new("")).unwrap();
    assert!(results.is_empty(), "empty query should return no results");
}

#[test]
fn test_search_empty_index_returns_empty() {
    let (_dir, layer) = build_index_with(&[]);
    let results = layer.search(&SearchQuery::new("main")).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_single_file_roundtrip_finds_term() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(
            FileId(0),
            "fn main() { println!(\"hello\"); }",
            rskim_core::Language::Rust,
        )
        .unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let results = reader.search(&SearchQuery::new("main")).unwrap();
    assert!(!results.is_empty(), "should find 'main'");
    assert!(results[0].score > 0.0, "score should be positive");
}

#[test]
fn test_multi_file_search_returns_correct_file_ids() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(FileId(0), "unique_token_alpha", rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(
            FileId(1),
            "unique_token_alpha beta gamma",
            rskim_core::Language::Python,
        )
        .unwrap();
    builder
        .add_file(
            FileId(2),
            "completely different content here",
            rskim_core::Language::Go,
        )
        .unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let results = reader
        .search(&SearchQuery::new("unique_token_alpha"))
        .unwrap();
    let file_ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();
    assert!(
        file_ids.contains(&0) && file_ids.contains(&1),
        "should find files 0 and 1, got {:?}",
        file_ids
    );
}

// -----------------------------------------------------------------------
// search — language filter
// -----------------------------------------------------------------------

#[test]
fn test_lang_filter_restricts_results() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(FileId(1), "def main(): pass", rskim_core::Language::Python)
        .unwrap();
    builder
        .add_file(
            FileId(2),
            "function main() {}",
            rskim_core::Language::JavaScript,
        )
        .unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();

    let mut query = SearchQuery::new("main");
    query.lang = Some(rskim_core::Language::Rust);
    let results = reader.search(&query).unwrap();
    assert!(!results.is_empty(), "lang filter: should find Rust file");
    for r in &results {
        assert_eq!(
            r.file_id.0, 0,
            "lang filter: only FileId(0) should appear, got {:?}",
            r.file_id
        );
    }
}

// -----------------------------------------------------------------------
// search — BM25 ranking
// -----------------------------------------------------------------------

#[test]
fn test_bm25_short_dense_ranks_above_long_sparse() {
    // File 0: short and dense with the query term
    // File 1: long with sparse occurrences of the same term
    let short = "main main main";
    let long = format!(
        "main {} some other stuff that makes it very long indeed",
        "padding word ".repeat(50)
    );
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(FileId(0), short, rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(FileId(1), &long, rskim_core::Language::Rust)
        .unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let results = reader.search(&SearchQuery::new("main")).unwrap();
    assert!(results.len() >= 2, "expected at least 2 results");
    // File 0 should rank higher
    assert_eq!(
        results[0].file_id.0, 0,
        "short dense doc should rank first, got file_id={}",
        results[0].file_id.0
    );
}

// -----------------------------------------------------------------------
// search — offset / limit
// -----------------------------------------------------------------------

#[test]
fn test_limit_restricts_result_count() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for i in 0..10u32 {
        builder
            .add_file(FileId(i), "fn main() {}", rskim_core::Language::Rust)
            .unwrap();
    }
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let mut query = SearchQuery::new("main");
    query.limit = Some(3);
    let results = reader.search(&query).unwrap();
    assert!(results.len() <= 3, "limit should cap results");
}

// -----------------------------------------------------------------------
// search — offset pagination
// -----------------------------------------------------------------------

#[test]
fn test_offset_skips_top_results() {
    // Build an index with 10 files that all contain the query term.  Use
    // distinct per-file content to produce varied BM25 scores.
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for i in 0..10u32 {
        // Vary document length/frequency so scores differ.
        let content = format!("main {}", "padding ".repeat(i as usize));
        builder
            .add_file(FileId(i), &content, rskim_core::Language::Rust)
            .unwrap();
    }
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();

    // Fetch all results (no offset).
    let all_results = reader.search(&SearchQuery::new("main")).unwrap();
    assert!(
        all_results.len() >= 3,
        "need at least 3 results; got {}",
        all_results.len()
    );

    // Fetch with offset=2: the first result must equal the 3rd result from the
    // no-offset search.
    let mut query = SearchQuery::new("main");
    query.offset = Some(2);
    let offset_results = reader.search(&query).unwrap();
    assert!(
        !offset_results.is_empty(),
        "offset=2 should still return results"
    );
    assert_eq!(
        offset_results[0].file_id, all_results[2].file_id,
        "first result with offset=2 should match 3rd result of no-offset search"
    );
}

// -----------------------------------------------------------------------
// Persistence
// -----------------------------------------------------------------------

#[test]
fn test_build_drop_reopen_search_works() {
    let dir = tmp_dir();
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        builder
            .add_file(
                FileId(0),
                "persistence_test_term",
                rskim_core::Language::Rust,
            )
            .unwrap();
        builder.build().unwrap();
    }
    // Drop the original layer, reopen from disk.
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let results = reader
        .search(&SearchQuery::new("persistence_test_term"))
        .unwrap();
    assert!(!results.is_empty(), "index should survive reopen");
}

// -----------------------------------------------------------------------
// Corruption detection
// -----------------------------------------------------------------------

#[test]
fn test_corrupted_skidx_detected() {
    let dir = tmp_dir();
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        builder
            .add_file(FileId(0), "hello world", rskim_core::Language::Rust)
            .unwrap();
        builder.build().unwrap();
    }
    // Corrupt the middle of .skidx.
    let idx_path = dir.path().join("index.skidx");
    let mut bytes = std::fs::read(&idx_path).unwrap();
    if bytes.len() > 20 {
        bytes[20] ^= 0xFF;
    }
    std::fs::write(&idx_path, bytes).unwrap();
    let result = NgramIndexReader::open(dir.path());
    assert!(result.is_err(), "corrupted index should fail to open");
}

// -----------------------------------------------------------------------
// Duplicate FileId via builder
// -----------------------------------------------------------------------

#[test]
fn test_duplicate_file_id_rejected() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(FileId(0), "content one", rskim_core::Language::Rust)
        .unwrap();
    let result = builder.add_file(FileId(0), "content two", rskim_core::Language::Python);
    assert!(result.is_err(), "duplicate FileId should be rejected");
}

// -----------------------------------------------------------------------
// Large-index benchmark (release mode only)
// -----------------------------------------------------------------------

#[test]
#[cfg(not(debug_assertions))]
fn test_1000_file_benchmark() {
    use std::time::Instant;

    let dir = tmp_dir();
    let content_template = "fn function_name_here() { let x = 42; println!(\"{x}\"); }";

    let write_start = Instant::now();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for i in 0..1000u32 {
        let content = format!("{content_template} // file {i}");
        builder
            .add_file(FileId(i), &content, rskim_core::Language::Rust)
            .unwrap();
    }
    builder.build().unwrap();
    let write_elapsed = write_start.elapsed();
    assert!(
        write_elapsed.as_millis() < 100,
        "build 1000 files took {}ms (limit: 100ms)",
        write_elapsed.as_millis()
    );

    let read_start = Instant::now();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let results = reader
        .search(&SearchQuery::new("function_name_here"))
        .unwrap();
    let read_elapsed = read_start.elapsed();
    assert!(
        !results.is_empty(),
        "should find results in 1000-file index"
    );
    assert!(
        read_elapsed.as_millis() < 100,
        "query 1000-file index took {}ms (limit: 100ms)",
        read_elapsed.as_millis()
    );
}
