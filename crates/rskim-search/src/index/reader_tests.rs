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
// BM25F acceptance criteria
// -----------------------------------------------------------------------

/// AC1: File with `struct UserService` in a TypeDefinition context scores
/// higher for "UserService" than a file that only contains it as a string literal.
///
/// We simulate this by using add_file_classified() to classify the relevant
/// bytes appropriately for each file.
#[test]
fn test_ac1_type_definition_ranks_above_string_literal() {
    use crate::SearchField;
    use crate::index::NgramIndexBuilder;
    use std::ops::Range;

    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // File 0: "UserService" in TypeDefinition context
    let type_def_content = "struct UserService { id: u32 }";
    let td_len = type_def_content.len();
    let type_def_map: Vec<(Range<usize>, SearchField)> =
        vec![(0..td_len, SearchField::TypeDefinition)];
    builder
        .add_file_classified(
            FileId(0),
            type_def_content,
            rskim_core::Language::Rust,
            &type_def_map,
        )
        .unwrap();

    // File 1: "UserService" only in StringLiteral context
    let string_content = "let s = \"UserService description here\";";
    let sl_len = string_content.len();
    let string_map: Vec<(Range<usize>, SearchField)> =
        vec![(0..sl_len, SearchField::StringLiteral)];
    builder
        .add_file_classified(
            FileId(1),
            string_content,
            rskim_core::Language::Rust,
            &string_map,
        )
        .unwrap();

    let reader = builder.build().unwrap();

    let results = reader.search(&SearchQuery::new("UserService")).unwrap();
    assert!(results.len() >= 2, "should find both files");
    assert_eq!(
        results[0].file_id.0, 0,
        "TypeDefinition context should rank above StringLiteral: first result was file {}",
        results[0].file_id.0
    );
}

/// AC2: Field boosts are configurable — a query with reversed boosts reverses ranking.
#[test]
fn test_ac2_configurable_boosts_reverse_ranking() {
    use crate::SearchField;
    use crate::index::NgramIndexBuilder;
    use crate::lexical::BM25FConfig;

    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // File 0: term in TypeDefinition (discriminant 0, default boost 5.0)
    let type_content = "struct SearchTarget { }";
    let tl = type_content.len();
    builder
        .add_file_classified(
            FileId(0),
            type_content,
            rskim_core::Language::Rust,
            &[(0..tl, SearchField::TypeDefinition)],
        )
        .unwrap();

    // File 1: term in StringLiteral (discriminant 6, default boost 0.5)
    let str_content = "let s = \"SearchTarget value\";";
    let sl = str_content.len();
    builder
        .add_file_classified(
            FileId(1),
            str_content,
            rskim_core::Language::Rust,
            &[(0..sl, SearchField::StringLiteral)],
        )
        .unwrap();

    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();

    // Default boosts: TypeDefinition=5.0 > StringLiteral=0.5 → file 0 ranks first.
    let default_results = reader.search(&SearchQuery::new("SearchTarget")).unwrap();
    assert!(default_results.len() >= 2);
    assert_eq!(
        default_results[0].file_id.0, 0,
        "default boosts: TypeDefinition should rank first"
    );

    // Reversed boosts: StringLiteral=5.0 > TypeDefinition=0.5 → file 1 ranks first.
    let mut reversed_config = BM25FConfig::default();
    reversed_config.field_boosts[0] = 0.5; // TypeDefinition
    reversed_config.field_boosts[6] = 5.0; // StringLiteral
    let mut query = SearchQuery::new("SearchTarget");
    query.bm25f_config = Some(reversed_config);
    let reversed_results = reader.search(&query).unwrap();
    assert!(reversed_results.len() >= 2);
    assert_eq!(
        reversed_results[0].file_id.0, 1,
        "reversed boosts: StringLiteral should rank first"
    );
}

/// AC3: BM25F params are tunable — validation rejects invalid values.
#[test]
fn test_ac3_bm25f_validation_rejects_invalid() {
    use crate::lexical::BM25FConfig;

    let mut bad_k1 = BM25FConfig::default();
    bad_k1.k1 = -0.1;
    assert!(bad_k1.validate().is_err(), "negative k1 must be rejected");

    let mut bad_boost = BM25FConfig::default();
    bad_boost.field_boosts[0] = -1.0;
    assert!(
        bad_boost.validate().is_err(),
        "negative boost must be rejected"
    );

    let mut bad_b = BM25FConfig::default();
    bad_b.field_b[2] = 1.5;
    assert!(bad_b.validate().is_err(), "b > 1.0 must be rejected");

    // Valid config must pass.
    assert!(
        BM25FConfig::default().validate().is_ok(),
        "default config must be valid"
    );
}

/// AC4: Scoring is deterministic — 100 identical searches produce identical results.
#[test]
fn test_ac4_scoring_deterministic() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for i in 0..5u32 {
        let content = format!("fn function_{i}() {{ let query = {i}; }}");
        builder
            .add_file(FileId(i), &content, rskim_core::Language::Rust)
            .unwrap();
    }
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();

    let first = reader.search(&SearchQuery::new("function")).unwrap();
    for _ in 0..100 {
        let run = reader.search(&SearchQuery::new("function")).unwrap();
        assert_eq!(run.len(), first.len(), "result count must be deterministic");
        for (a, b) in run.iter().zip(first.iter()) {
            assert_eq!(
                a.file_id, b.file_id,
                "file_id ordering must be deterministic"
            );
            assert!(
                (a.score - b.score).abs() < 1e-10,
                "score must be deterministic: {} vs {}",
                a.score,
                b.score
            );
        }
    }
}

/// Verify that open_with_config() applies the provided BM25FConfig correctly,
/// producing different scores than the default config.
#[test]
fn test_open_with_config_stores_config() {
    use crate::lexical::BM25FConfig;

    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // Repeat "main" several times so tf_weighted is large enough that changing
    // k1 from 1.2 (default) to 2.0 produces a measurable score difference.
    // BM25F saturation formula: IDF * tf_weighted / (tf_weighted + k1)
    builder
        .add_file(
            FileId(0),
            "fn main() { let main_count = 1; main_count + main_count; }",
            rskim_core::Language::Rust,
        )
        .unwrap();
    builder.build().unwrap();

    // Open twice: once with the default config, once with k1 = 2.0.
    let default_reader = NgramIndexReader::open(dir.path()).unwrap();
    let mut custom_config = BM25FConfig::default();
    custom_config.k1 = 2.0; // higher k1 → lower saturation → lower score
    let custom_reader = NgramIndexReader::open_with_config(dir.path(), custom_config).unwrap();

    let default_results = default_reader.search(&SearchQuery::new("main")).unwrap();
    let custom_results = custom_reader.search(&SearchQuery::new("main")).unwrap();

    // Both configs must find the document.
    assert!(
        !default_results.is_empty(),
        "default config should find results"
    );
    assert!(
        !custom_results.is_empty(),
        "custom config should find results"
    );

    // A higher k1 value reduces score saturation, so scores must differ.
    let default_score = default_results[0].score;
    let custom_score = custom_results[0].score;
    assert!(
        (default_score - custom_score).abs() > 1e-6,
        "k1=1.2 score ({default_score}) and k1=2.0 score ({custom_score}) should differ"
    );
    // k1=1.2 saturates faster than k1=2.0, so the default score should be higher.
    assert!(
        default_score > custom_score,
        "default k1=1.2 should score higher than k1=2.0 (less saturation dampening)"
    );
}

/// Verify SearchResult.field is populated from dominant_field, not hardcoded Other.
#[test]
fn test_search_result_field_populated_from_dominant_field() {
    use crate::SearchField;
    use crate::index::NgramIndexBuilder;
    use std::ops::Range;

    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    let content = "struct MyStruct { x: u32 }";
    let len = content.len();
    // Classify all bytes as TypeDefinition.
    let field_map: Vec<(Range<usize>, SearchField)> = vec![(0..len, SearchField::TypeDefinition)];
    builder
        .add_file_classified(FileId(0), content, rskim_core::Language::Rust, &field_map)
        .unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();

    let results = reader.search(&SearchQuery::new("MyStruct")).unwrap();
    assert!(!results.is_empty(), "should find MyStruct");
    assert_eq!(
        results[0].field,
        SearchField::TypeDefinition,
        "field should reflect dominant classification, not hardcoded Other"
    );
}

// -----------------------------------------------------------------------
// file_filter edge cases
// -----------------------------------------------------------------------

/// An empty HashSet passed as `file_filter` must return no results, because no
/// document can satisfy an allowlist with zero members.  This exercises the
/// `Some(empty_set)` path in the first sub-pass filter and the defense-in-depth
/// filter, both of which must agree that every doc is excluded.
#[test]
fn test_file_filter_empty_set_returns_no_results() {
    let (_dir, layer) = build_index_with(&[
        (FileId(0), "fn main() {}", rskim_core::Language::Rust),
        (FileId(1), "def main(): pass", rskim_core::Language::Python),
    ]);

    let mut query = SearchQuery::new("main");
    query.file_filter = Some(std::collections::HashSet::new());

    let results = layer.search(&query).unwrap();
    assert!(
        results.is_empty(),
        "Some(empty_set) file_filter must return no results, got {} results",
        results.len()
    );
}

// -----------------------------------------------------------------------
// BM25FConfig validation at trust boundaries
// -----------------------------------------------------------------------

/// open_with_config() must reject an invalid BM25FConfig before opening the index.
#[test]
fn test_open_with_config_rejects_invalid_config() {
    use crate::lexical::BM25FConfig;

    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust)
        .unwrap();
    builder.build().unwrap();

    let mut bad_config = BM25FConfig::default();
    bad_config.k1 = -1.0; // invalid: must be >= 0.0

    let result = NgramIndexReader::open_with_config(dir.path(), bad_config);
    assert!(
        result.is_err(),
        "open_with_config should reject an invalid BM25FConfig"
    );
    if let Err(e) = result {
        let err = format!("{e}");
        assert!(err.contains("k1"), "error should mention k1, got: {err}");
    }
}

/// search() must reject a per-query BM25FConfig with invalid parameters.
#[test]
fn test_search_rejects_invalid_per_query_config() {
    use crate::lexical::BM25FConfig;

    let (_dir, layer) =
        build_index_with(&[(FileId(0), "fn main() {}", rskim_core::Language::Rust)]);

    let mut bad_config = BM25FConfig::default();
    bad_config.field_b[0] = 1.5; // invalid: must be in [0.0, 1.0]

    let mut query = SearchQuery::new("main");
    query.bm25f_config = Some(bad_config);

    let result = layer.search(&query);
    assert!(
        result.is_err(),
        "search() should reject an invalid per-query BM25FConfig"
    );
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
