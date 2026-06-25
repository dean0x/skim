//! Tests for NgramIndexReader (reader.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
    // AC10 / PF-007: subsumed by lexical_index_size_ratio (below) which asserts
    // a grounded ceiling, not just > 0.  This narrower check is kept as a
    // precondition guard for the stats API itself (verifies stats() is connected
    // to the actual mmap sizes, not hardcoded zero).
    assert!(
        stats.index_size_bytes > 0,
        "index_size_bytes must be positive for a single-file index"
    );
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

/// AD-355-7 / PF-007: short-query fallback — a 2-byte query cannot produce trigrams,
/// but the reader must still return all indexed files as score-0 candidates so the
/// Part A verify step can apply a literal substring filter.
///
/// Discriminating observable (PF-007): the file containing the short term IS present
/// in the raw candidate set (FileId(0)), and a file that does NOT contain the term
/// is also returned so the verify step can distinguish them.  An empty set would hide
/// the bug and a vacuous `!is_empty()` would pass even without the fix.
#[test]
fn test_search_short_query_returns_all_file_candidates_ad355_7() {
    // File 0 contains "fn"; File 1 does not.
    let (_dir, layer) = build_index_with(&[
        (FileId(0), "fn main() {}", rskim_core::Language::Rust),
        (FileId(1), "def run(): pass", rskim_core::Language::Python),
    ]);
    let mut q = SearchQuery::new("fn"); // 2-byte query — no trigrams possible
    q.limit = Some(50);
    let results = layer.search(&q).unwrap();

    let file_ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // Both files must be present — the caller (verify layer) decides who survives.
    assert!(
        file_ids.contains(&0),
        "AD-355-7: FileId(0) containing 'fn' must appear as a candidate; got {file_ids:?}"
    );
    assert!(
        file_ids.contains(&1),
        "AD-355-7: FileId(1) must also appear so verify can filter; got {file_ids:?}"
    );

    // Score must be 0.0 — no BM25F scoring for short queries; ranking is deferred to verify.
    for r in &results {
        assert_eq!(
            r.score, 0.0,
            "AD-355-7: short-query candidates carry score 0.0, got {} for FileId({})",
            r.score, r.file_id.0
        );
    }
}

/// A 1-byte query also returns all candidates (same AD-355-7 path as 2-byte).
#[test]
fn test_search_single_byte_query_returns_all_file_candidates() {
    let (_dir, layer) = build_index_with(&[
        (FileId(0), "abc", rskim_core::Language::Rust),
        (FileId(1), "xyz", rskim_core::Language::Python),
    ]);
    let mut q = SearchQuery::new("a"); // 1-byte — no trigrams
    q.limit = Some(50);
    let results = layer.search(&q).unwrap();
    let file_ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();
    assert!(
        file_ids.contains(&0) && file_ids.contains(&1),
        "single-byte query must return all candidates; got {file_ids:?}"
    );
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
    // PF-007 (discriminating): assert rank and membership, not just !is_empty().
    // The single file indexed must be FileId(0) and must be results[0].
    assert_eq!(
        results[0].file_id.0, 0,
        "single-file index: the only file must rank first; got {:?}",
        results[0].file_id
    );
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
    // PF-007 (discriminating negative, plan §5): file 2 has "completely different
    // content here" — zero trigram overlap with "unique_token_alpha" — and must be
    // ABSENT from the raw candidate set.  Without this assertion the test passes
    // even if the reader erroneously returns unrelated files.
    assert!(
        !file_ids.contains(&2),
        "file 2 ('completely different content') shares no trigrams with \
        'unique_token_alpha' and must be absent from raw results; got {file_ids:?}"
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

    let bad_k1 = BM25FConfig {
        k1: -0.1,
        ..BM25FConfig::default()
    };
    assert!(bad_k1.validate().is_err(), "negative k1 must be rejected");

    let mut bad_boost = BM25FConfig::default();
    bad_boost.field_boosts[0] = -1.0; // array element mutation — struct-update syntax doesn't apply here
    assert!(
        bad_boost.validate().is_err(),
        "negative boost must be rejected"
    );

    let mut bad_b = BM25FConfig::default();
    bad_b.field_b[2] = 1.5; // array element mutation — struct-update syntax doesn't apply here
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
    let custom_config = BM25FConfig {
        k1: 2.0,
        ..BM25FConfig::default()
    }; // higher k1 → lower saturation → lower score
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

    let bad_config = BM25FConfig {
        k1: -1.0,
        ..BM25FConfig::default()
    }; // invalid: must be >= 0.0

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
// AC10: This test is a build/write throughput smoke test.
// It is SUPERSEDED for query-latency grounding by
// lexical_query_latency_representative_corpus (AD-LXLAT-1) below, which uses a
// diverse multi-statement corpus that exercises real posting-list scan cost.
// The 100ms bounds here are intentionally loose (best-case identical-file
// corpus) and must NOT be treated as the #174 50ms query-latency authority.
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
    // Note: this corpus has near-zero trigram diversity (1000 near-identical
    // files) → maximally sparse posting lists → best-case latency.  The 100ms
    // bound is a write+read smoke-test, NOT a grounded query-latency guard.
    // See lexical_query_latency_representative_corpus (AD-LXLAT-1) for the
    // grounded 50ms authority on a diverse corpus (AC10 / #174).
    assert!(
        read_elapsed.as_millis() < 100,
        "query 1000-file index took {}ms (limit: 100ms)",
        read_elapsed.as_millis()
    );
}

// -----------------------------------------------------------------------
// AC1: Grounded lexical index size-ratio guard (ADR-003 + PF-007)
// -----------------------------------------------------------------------

/// AD-LXSZ-1: The #174 30% (0.30x) figure has no measured basis and is
/// structurally impossible for an uncompressed per-occurrence inverted index
/// (posting entries are 9 bytes each; the builder indexes every byte-window
/// per file with no dedup; posting bytes scale at O(source) not a fraction
/// of it). Per ADR-003 that target is replaced by a measured ratio guard
/// modeled on ast_index_size_ratio (~1.23-1.3x measured, <2.2x guard,
/// ast_index/store/reader_tests.rs:574-666) and issue #273.
///
/// Measured lexical baseline (trigram, v4 delta+varint, 1000 diverse Rust
/// modules, 4 fns each ~480 bytes, multi-field classified path): 3.53x.
/// v3 uncompressed baseline was 9.04x; delta+varint compression (#358 Item 2)
/// reduced postings ~61%.
/// Guard ceiling: measured_baseline + 1.5x headroom = 5.0x (round number).
/// The test fails on a genuine bloat regression (discriminating per PF-007
/// -- a vacuous assert(>0) would pass even with 100x bloat).
///
/// # Production-representative indexing path
///
/// This test uses `add_file_classified` with a real `field_map` from
/// `classify_source` — the same path that production indexing (`index.rs`)
/// uses.  The previous version used `add_file` (empty field_map, all bytes
/// classified as `SearchField::Other`), which exercises only the single-field
/// code path and misses the field-boundary delta-reset that the v4 codec adds.
/// Building with a real multi-field field_map makes this guard representative
/// of the production on-disk size (ADR-003: grounded in the actual path).
#[test]
fn lexical_index_size_ratio() {
    use crate::classify_source;
    use crate::test_corpus::gen_representative_rust_module;

    let dir = tmp_dir();

    let n_files = 1000usize;
    let fns_per_file = 4usize;
    let sources: Vec<String> = (0..n_files)
        .map(|i| gen_representative_rust_module(i, fns_per_file))
        .collect();

    // Build the index from the representative diverse corpus using the
    // production-representative classified path (multi-field field_map).
    // This matches what `index.rs::run()` does: classify_source produces a
    // real (Range<usize>, SearchField) field_map, then add_file_classified
    // is called with it.  Using add_file (empty field_map) would exercise
    // only the single-field best-case path, making the size guard non-
    // representative of the on-disk ratio users actually observe.
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        for (i, src) in sources.iter().enumerate() {
            let lang = rskim_core::Language::Rust;
            let field_map = classify_source(src, lang).unwrap_or_default();
            builder
                .add_file_classified(FileId(i as u32), src, lang, &field_map)
                .unwrap();
        }
        builder.build().unwrap();
    }

    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let stats = reader.stats();

    // AC1 precondition guard: if no n-grams were indexed, the ratio is
    // meaningless (would be 0.0, vacuously passing any ceiling check).
    assert!(
        stats.total_ngrams > 0,
        "AD-LXSZ-1 precondition: corpus must produce at least one n-gram \
         (got 0 -- builder or corpus is broken)"
    );

    let total_source_bytes: u64 = sources.iter().map(|s| s.len() as u64).sum();
    let total_index_bytes = stats.index_size_bytes;

    let ratio = total_index_bytes as f64 / total_source_bytes as f64;

    let idx_bytes = std::fs::metadata(dir.path().join("index.skidx"))
        .unwrap()
        .len();
    let post_bytes = std::fs::metadata(dir.path().join("index.skpost"))
        .unwrap()
        .len();

    eprintln!(
        "AD-LXSZ-1 lexical size ratio: {ratio:.4} \
         (index={total_index_bytes} bytes, source={total_source_bytes} bytes, \
         skidx={idx_bytes} bytes, skpost={post_bytes} bytes, \
         n_files={n_files}, fns_per_file={fns_per_file})"
    );

    // Guard ceiling: measured trigram-v4 (delta+varint) baseline + 1.5x headroom.
    //
    // Rationale for the ceiling value:
    //   - Measured v4 trigram baseline on this corpus (1000 diverse Rust modules,
    //     4 fns/file, ~1055 KB source): 3.53x (skidx=58 KB, skpost=3.5 MB).
    //     Delta+varint encoding (#358 Item 2) reduced posting bytes ~61% vs
    //     v3 fixed-9-byte entries (which measured 9.04x on the same corpus).
    //   - Industry uncompressed code-search trigram indexes (Zoekt, Sourcegraph)
    //     run 3-5x source bytes; v4 delta+varint brings skim below that range.
    //     The #174 <30% (0.30x) target has no empirical origin and is
    //     structurally impossible (see AD-LXSZ-1 comment above). ADR-003 replaces it.
    //   - Ceiling = 3.53x measured + 1.5x headroom = 5.0x (round number).
    //     Headroom absorbs minor corpus variation and overhead growth without
    //     allowing a genuine O(files^2) bloat regression to pass.
    //   - A genuine posting-list explosion (e.g. dedup bug, accidental O(n^2)
    //     growth) would push the ratio 2-5x above 3.53x and still fires.
    //
    // ADR-003: regression guard must be empirically grounded, not the
    // baseless 0.30x inherited from the original ticket text.
    const LEXICAL_SIZE_RATIO_CEILING: f64 = 5.0;
    assert!(
        ratio < LEXICAL_SIZE_RATIO_CEILING,
        "AD-LXSZ-1: lexical index size ratio {ratio:.4} exceeds the \
         <{LEXICAL_SIZE_RATIO_CEILING}x bloat guard (v4 delta+varint baseline 3.53x). \
         If ratio exceeded: check for O(files^2) posting growth, \
         missing dedup, unbounded trigram emission, or codec regression. \
         index={total_index_bytes} bytes, source={total_source_bytes} bytes."
    );
}

// -----------------------------------------------------------------------
// AC2: Grounded query-latency guard on representative corpus (ADR-003 + PF-007)
// Release-mode only: debug builds run under sanitizers and without
// optimizations, making latency measurements meaningless.
// -----------------------------------------------------------------------

/// AD-LXLAT-1: The existing test_1000_file_benchmark uses 1000 identical ~60B
/// files (near-zero unique-trigram diversity) -> maximally sparse vocabulary ->
/// best-case latency (shortest posting lists). This test uses the diverse
/// gen_representative_rust_module corpus so query latency reflects real
/// posting-list scan cost on distinct n-grams across diverse source files.
///
/// Guards against performance regression on the #174 50ms target.
/// Release-mode gated (#[cfg(not(debug_assertions))]) matching the AST
/// test pattern.
///
/// PF-007 compliance: asserts the result-set is non-empty BEFORE asserting
/// latency, so the test fails both when the feature is deleted (empty results)
/// AND when performance regresses (latency > 50ms). A timer-only assertion
/// would be vacuous -- it passes even if the reader returns nothing.
#[test]
#[cfg(not(debug_assertions))]
fn lexical_query_latency_representative_corpus() {
    use crate::test_corpus::gen_representative_rust_module;
    use std::time::Instant;

    let dir = tmp_dir();

    let n_files = 1000usize;
    let fns_per_file = 4usize;
    let sources: Vec<String> = (0..n_files)
        .map(|i| gen_representative_rust_module(i, fns_per_file))
        .collect();

    // Build the index once.
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        for (i, src) in sources.iter().enumerate() {
            builder
                .add_file(FileId(i as u32), src, rskim_core::Language::Rust)
                .unwrap();
        }
        builder.build().unwrap();
    }

    let reader = NgramIndexReader::open(dir.path()).unwrap();

    // Warm-up query: amortize cold-start I/O and OS page-cache misses.
    // The timed query below measures only steady-state reader.search() cost.
    let mut warmup = SearchQuery::new("process");
    warmup.limit = Some(1000);
    let _ = reader.search(&warmup).unwrap();

    // Timed query on a term that matches all 1000 files ("wrapping_add" appears
    // in every generated function body, so it exercises the full posting-list
    // scan path -- NOT the short-circuit path for rare terms).
    // Set limit=1000 to request all files; the reader's default of 20 would
    // return only the top-20 candidates and make the >=100 coverage check fail.
    let mut query = SearchQuery::new("wrapping_add");
    query.limit = Some(1000);

    // Take 5 timed samples and use the minimum to reduce CI noise.  A single
    // sample on a shared-cargo-target CI machine is noise-prone under parallel
    // load (PF-010); the minimum is robust (a genuinely slow implementation
    // cannot hide behind one lucky fast sample, and one slow sample from OS
    // scheduling jitter cannot flake the gate).
    const TIMED_SAMPLES: usize = 5;
    let mut min_elapsed = std::time::Duration::from_secs(u64::MAX);
    let mut results = Vec::new();
    for _ in 0..TIMED_SAMPLES {
        let timed_start = Instant::now();
        results = reader.search(&query).unwrap();
        let elapsed = timed_start.elapsed();
        if elapsed < min_elapsed {
            min_elapsed = elapsed;
        }
    }

    // PF-007 (discriminating): assert non-empty result set BEFORE latency.
    // Without this, the test passes even if search() returns nothing (e.g. if
    // "wrapping_add" is not indexed), masking a correctness bug as a perf pass.
    assert!(
        !results.is_empty(),
        "AD-LXLAT-1: 'wrapping_add' must match in all 1000 files -- \
         got 0 results (corpus or indexing is broken). \
         This assertion must pass before the latency gate is checked."
    );

    // Verify the result set has real coverage (not just 1 of 1000 files).
    assert!(
        results.len() >= 100,
        "AD-LXLAT-1: expected >=100 results for 'wrapping_add' across 1000 files, \
         got {} (the limit may be capping the result; increase or remove limit if so)",
        results.len()
    );

    // Emit the measured latency so the AD-LXLAT-1 grounding number is
    // observable from test output and reproducible (mirrors the size test's
    // eprintln! discipline).
    eprintln!(
        "AD-LXLAT-1 lexical query latency (min of {TIMED_SAMPLES} samples): {}ms \
         (corpus={n_files} diverse files, results={})",
        min_elapsed.as_millis(),
        results.len()
    );

    // #174 latency budget: 50ms for a warm query on a ~1000-file corpus.
    assert!(
        min_elapsed.as_millis() < 50,
        "AD-LXLAT-1: query latency {}ms (min of {TIMED_SAMPLES} samples) exceeds the \
         #174 50ms budget on a representative 1000-file diverse corpus. \
         This test uses gen_representative_rust_module (diverse n-grams, \
         real posting-list scan) not the identical-file best-case. \
         Profile reader.rs scan_postings or posting_list_for_ngram if this fires.",
        min_elapsed.as_millis()
    );
}

// -----------------------------------------------------------------------
// AC6: Result-set / rank non-regression across the v3→v4 codec change
// -----------------------------------------------------------------------

/// AC6 baseline-equality: asserts that for a fixed query on a fixed corpus
/// the v4 delta+varint codec returns the same doc_ids and rank-1 result as
/// expected, and that a gibberish query's result count does not exceed a
/// documented baseline ceiling.
///
/// Plan section 7 (AC6) and Test Plan scenario "AC6 - no result-set
/// regression" call for a baseline-comparison test.  This test implements
/// that requirement with a fixed, deterministic corpus so the expected values
/// are stable across codec changes.
///
/// PF-007 compliance:
/// - Asserts the exact doc_id set (not just !is_empty()).
/// - Asserts rank-1 file_id explicitly.
/// - Asserts gibberish query result count <= documented baseline ceiling,
///   guarding against the #355 "gibberish matches ~100 files" regression.
///
/// Why this is discriminating: the AC4 codec round-trip tests (format_tests.rs)
/// prove encode_postings_varint . decode_postings_varint = identity in isolation,
/// but NOT that the production read path (mmap slice -> lookup_postings ->
/// decode_postings_varint -> BM25F scoring in reader.rs) returns the same
/// documents and order.  This test exercises the full end-to-end path.
#[test]
fn test_ac6_result_set_non_regression_v4_codec() {
    // Fixed corpus: 5 files with distinct, known trigram overlap with the query.
    //
    // File 0: contains "unique_term_alpha" many times (high density) → rank 1
    // File 1: contains "unique_term_alpha" once (low density) → rank 2
    // File 2: completely unrelated to "unique_term_alpha" → must be ABSENT
    // File 3: contains "unique_term_alpha" twice → rank between 1 and 2
    // File 4: unrelated content → must be ABSENT
    //
    // Expected: files 0, 1, 3 in results; files 2 and 4 absent; rank-1 = file 0.
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(
            FileId(0),
            "unique_term_alpha unique_term_alpha unique_term_alpha unique_term_alpha",
            rskim_core::Language::Rust,
        )
        .unwrap();
    builder
        .add_file(
            FileId(1),
            "unique_term_alpha is mentioned here once",
            rskim_core::Language::Rust,
        )
        .unwrap();
    builder
        .add_file(
            FileId(2),
            "completely different stuff with no overlap at all",
            rskim_core::Language::Rust,
        )
        .unwrap();
    builder
        .add_file(
            FileId(3),
            "unique_term_alpha is mentioned here twice unique_term_alpha",
            rskim_core::Language::Rust,
        )
        .unwrap();
    builder
        .add_file(
            FileId(4),
            "something entirely unrelated xyz abc def ghi jkl",
            rskim_core::Language::Rust,
        )
        .unwrap();
    builder.build().unwrap();

    let reader = NgramIndexReader::open(dir.path()).unwrap();

    // --- AC6 Part 1: fixed query, exact doc_id set + rank-1 ---

    let mut q = SearchQuery::new("unique_term_alpha");
    q.limit = Some(50);
    let results = reader.search(&q).unwrap();

    let file_ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // Files 0, 1, 3 all contain "unique_term_alpha" — must be present.
    assert!(
        file_ids.contains(&0),
        "AC6: FileId(0) must be in results; got {file_ids:?}"
    );
    assert!(
        file_ids.contains(&1),
        "AC6: FileId(1) must be in results; got {file_ids:?}"
    );
    assert!(
        file_ids.contains(&3),
        "AC6: FileId(3) must be in results; got {file_ids:?}"
    );

    // Files 2 and 4 share no trigrams with "unique_term_alpha" — must be absent.
    assert!(
        !file_ids.contains(&2),
        "AC6: FileId(2) (unrelated) must be absent from results; got {file_ids:?}"
    );
    assert!(
        !file_ids.contains(&4),
        "AC6: FileId(4) (unrelated) must be absent from results; got {file_ids:?}"
    );

    // Rank-1 must be FileId(0): highest density of the query term → highest BM25F.
    assert_eq!(
        results[0].file_id.0, 0,
        "AC6: rank-1 must be FileId(0) (highest density); got FileId({})",
        results[0].file_id.0
    );

    // --- AC6 Part 2: gibberish query must not produce a large result set ---
    //
    // The #355 "gibberish matches ~100 files" regression (#174) fired because
    // common bigrams overlapped almost all files.  With v4 trigram codec the
    // gibberish ceiling is much lower.  We use a 5-file corpus here; gibberish
    // must match at most 1 file (ADR-006 spirit: regression must not worsen).
    //
    // Baseline ceiling for this 5-file corpus: 1.  A corpus-size-relative
    // ceiling (20%) rounds to 1 for N=5, and is tight enough to catch a
    // genuine "gibberish matches everything" regression.
    let mut gq = SearchQuery::new("XYZZY_GIBBERISH_NONEXISTENT_42");
    gq.limit = Some(50);
    let gibberish_results = reader.search(&gq).unwrap();

    // AC6 baseline ceiling: gibberish must match at most 1 file out of 5.
    // If this fires, the lexical index is returning spurious matches for
    // terms that share no trigrams with any indexed content.
    const GIBBERISH_CEILING: usize = 1;
    assert!(
        gibberish_results.len() <= GIBBERISH_CEILING,
        "AC6: gibberish query matched {} files (ceiling: {GIBBERISH_CEILING}). \
         The v4 codec must not inflate the false-positive rate vs #355 baseline. \
         This guards against the #355 bigram-noise regression (#174). \
         Results: {:?}",
        gibberish_results.len(),
        gibberish_results.iter().map(|r| r.file_id.0).collect::<Vec<_>>()
    );
}
