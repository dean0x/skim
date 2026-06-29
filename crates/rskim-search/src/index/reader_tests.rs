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

/// #364 / Findings 1+4: a bit-flip in .skpost is detected at open() time via
/// the CRC32 checksum that now covers postings + entries + metadata.
///
/// Build a real index (ensures the on-disk files have a valid header+checksum),
/// flip one byte in index.skpost, then assert that NgramIndexReader::open
/// returns Err with "checksum mismatch" (Design Constraint: "fail loud").
///
/// PF-007 compliance: the assertion checks the DISCRIMINATING error substring
/// "checksum mismatch" so the test fails the moment the corruption-detection
/// path is removed or the error message changes semantically.
#[test]
fn test_open_corrupt_skpost_detected() {
    let dir = tmp_dir();
    // Build a minimal real index so the CRC is computed over actual data.
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        builder
            .add_file(
                FileId(0),
                "fn foo() { let x = 1; }",
                rskim_core::Language::Rust,
            )
            .unwrap();
        builder.build().unwrap();
    }

    // Flip the first byte of index.skpost to simulate a storage bit-flip.
    let post_path = dir.path().join("index.skpost");
    let mut data = std::fs::read(&post_path).unwrap();
    assert!(!data.is_empty(), "skpost must be non-empty for this test");
    data[0] ^= 0xFF;
    std::fs::write(&post_path, &data).unwrap();

    // open() must detect the corruption via CRC and fail loud.
    let result = NgramIndexReader::open(dir.path());
    assert!(
        result.is_err(),
        "corrupted .skpost must cause open() to fail"
    );
    // Use .err().unwrap() to extract the error without requiring T: Debug.
    let err_str = format!("{}", result.err().unwrap());
    assert!(
        err_str.contains("checksum mismatch"),
        "error must contain 'checksum mismatch' to confirm CRC detected the corruption: {err_str}"
    );
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

/// AD-355-7 / PF-007: short-query fallback MUST honour the lang filter.
///
/// A 2-byte query ("fn") cannot produce trigrams and takes the AD-355-7 fallback
/// path (emit all indexed files as score-0 candidates).  When `query.lang` is
/// set, the fallback must still apply the language filter — only files of the
/// requested language must appear.
///
/// **Discriminating** (PF-007): the corpus has two files — Rust and Python.
/// Both contain the short query "fn" somewhere in their content, so without
/// the lang filter both would survive verification.  The test asserts:
///
/// 1. The Rust file (FileId 0) IS in the candidate set.
/// 2. The Python file (FileId 1) is NOT in the candidate set.
///
/// If the lang filter is absent from the fallback path, the Python file
/// would appear — failing assertion (2) and proving the flag is silently
/// dropped on the short-query sub-path (PF-006 violation).
#[test]
fn test_short_query_fallback_honours_lang_filter_pf006() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // File 0 (Rust) — contains "fn" as a keyword.
    builder
        .add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust)
        .unwrap();
    // File 1 (Python) — also contains "fn" as part of "define", not a keyword,
    // but still a substring so verification would keep it if the lang filter fails.
    builder
        .add_file(
            FileId(1),
            "def fn_helper(): pass",
            rskim_core::Language::Python,
        )
        .unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();

    let mut query = SearchQuery::new("fn"); // 2 bytes → AD-355-7 fallback
    query.lang = Some(rskim_core::Language::Rust);
    query.limit = Some(50);

    let results = reader.search(&query).unwrap();
    let file_ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // (1) Rust file must be in the candidate set.
    assert!(
        file_ids.contains(&0),
        "AD-355-7 + lang filter: Rust FileId(0) must appear as a short-query candidate; \
        got {file_ids:?}"
    );

    // (2) Python file must NOT appear — the lang filter must be active on the
    //     fallback path.  If this assertion fails, the PF-006 violation is live:
    //     the --lang flag is silently ignored on the AD-355-7 sub-path.
    assert!(
        !file_ids.contains(&1),
        "AD-355-7 + lang filter (PF-006): Python FileId(1) must be excluded by the lang \
        filter even on the short-query fallback path; found in candidates — lang filter \
        is absent or broken on the fallback sub-path. Got {file_ids:?}"
    );
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
///
/// # AD-372-1 note
///
/// `bm25f_config` is a BM25F UNION-path parameter.  Single-token queries (≥3 bytes)
/// now route to `search_exact_intersection` (AD-372-1) which ranks by
/// occurrence-count / token-density (AD-372-6), NOT BM25F.  Configurable BM25F
/// field boosts therefore only apply to MULTI-WORD queries (the UNION path).
///
/// This test uses a two-word query "SearchTarget struct" to remain on the UNION
/// path where `bm25f_config` is effective.  A single-token query "SearchTarget"
/// would route to the intersection path and ignore `bm25f_config`.
#[test]
fn test_ac2_configurable_boosts_reverse_ranking() {
    use crate::SearchField;
    use crate::index::NgramIndexBuilder;
    use crate::lexical::BM25FConfig;

    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // File 0: both terms in TypeDefinition (discriminant 0, default boost 5.0).
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

    // File 1: both terms in StringLiteral (discriminant 6, default boost 0.5).
    let str_content = "let s = \"SearchTarget struct value\";";
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

    // Two-word query stays on the BM25F UNION path (AD-372-1: multi-word → UNION).
    // Default boosts: TypeDefinition=5.0 > StringLiteral=0.5 → file 0 ranks first.
    let default_results = reader
        .search(&SearchQuery::new("SearchTarget struct"))
        .unwrap();
    assert!(default_results.len() >= 2);
    assert_eq!(
        default_results[0].file_id.0, 0,
        "default boosts: TypeDefinition should rank first"
    );

    // Reversed boosts: StringLiteral=5.0 > TypeDefinition=0.5 → file 1 ranks first.
    let mut reversed_config = BM25FConfig::default();
    reversed_config.field_boosts[0] = 0.5; // TypeDefinition
    reversed_config.field_boosts[6] = 5.0; // StringLiteral
    let mut query = SearchQuery::new("SearchTarget struct");
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
///
/// # AD-372-1 note
///
/// `bm25f_config` (including k1) only affects the BM25F UNION path, which is
/// used for multi-word queries.  Single-token queries (≥3 bytes) route to
/// `search_exact_intersection` (AD-372-1) and rank by raw occurrence count
/// (AD-372-6) — k1 is irrelevant there.
///
/// This test uses a two-word query "main count" to remain on the UNION path.
#[test]
fn test_open_with_config_stores_config() {
    use crate::lexical::BM25FConfig;

    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // Repeat both terms many times so tf_weighted is large enough that changing
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

    // Two-word query → BM25F UNION path (AD-372-1: only single-token → intersection).
    let default_results = default_reader
        .search(&SearchQuery::new("main count"))
        .unwrap();
    let custom_results = custom_reader
        .search(&SearchQuery::new("main count"))
        .unwrap();

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
///
/// # AD-372-1 note
///
/// `bm25f_config` is validated on the BM25F UNION path only.  Single-token
/// queries (≥3 bytes) bypass BM25F entirely (AD-372-1: `search_exact_intersection`).
/// This test uses a two-word query "fn main" to stay on the UNION path where
/// per-query BM25FConfig validation fires.
#[test]
fn test_search_rejects_invalid_per_query_config() {
    use crate::lexical::BM25FConfig;

    let (_dir, layer) = build_index_with(&[(
        FileId(0),
        "fn main() { println!(\"hello\"); }",
        rskim_core::Language::Rust,
    )]);

    let mut bad_config = BM25FConfig::default();
    bad_config.field_b[0] = 1.5; // invalid: must be in [0.0, 1.0]

    // Two-word query → BM25F UNION path → bm25f_config validation fires.
    let mut query = SearchQuery::new("fn main");
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
/// modules, 4 fns each ~1055 bytes (~1.05 MB total source), multi-field
/// classified path): 3.53x.
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
fn test_lexical_index_size_ratio() {
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
    //     allowing a genuine bloat regression to pass.
    //   - True sensitivity threshold: ~1.42x bloat (5.0 / 3.53).  The FIRST
    //     regression that actually fires the assertion is ~1.42x above the
    //     measured baseline (e.g. ratio ~4.9x from a partial-compression
    //     regression would still PASS -- the gate is not a tight 2x guard).
    //     A genuine posting-list explosion (full revert to v3 fixed-9-byte
    //     encoding gives 9.04x >> 5.0x) definitively fires the gate (ADR-003).
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

    // Finding 6 / AC6 end-to-end: exercise the multi-field index through the
    // production read path (mmap slice → lookup_postings → decode_postings_varint
    // → BM25F scoring).  The v4 codec's field-boundary delta-reset
    // (encode_postings_varint / decode_postings_varint: `prev_position = 0`
    // on field_id change) is exercised here because the classified corpus has
    // postings in multiple fields within the same doc.  An encode/decode
    // asymmetry in the reset condition (e.g. dropping `|| field_id !=
    // prev_field_id`) would corrupt positions for multi-field docs yet keep the
    // size check above green — this query assertion catches that silent failure.
    //
    // "process" appears as a substring inside gen_representative_rust_module
    // function bodies ("process_{i}" call sites); the reader must decode
    // postings from the multi-field index and return >=1 matching file.
    let mut query_for_decode_path = SearchQuery::new("process");
    query_for_decode_path.limit = Some(100);
    let search_results = reader.search(&query_for_decode_path).unwrap();
    assert!(
        !search_results.is_empty(),
        "AD-LXSZ-1 / Finding 6: reader.search('process') on the multi-field classified \
         corpus must return >=1 result.  An empty result set indicates that the v4 \
         field-boundary decode path is broken (encode/decode asymmetry in \
         prev_position reset, or corpus was not indexed)."
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
///
/// # Production-representative indexing path (Finding 3 / Finding 9)
///
/// This test builds the corpus via `add_file_classified` with a real field_map
/// from `classify_source`, matching the sibling size test (`test_lexical_index_size_ratio`)
/// and the production indexing path (`index.rs::run()`).  Using `add_file`
/// (empty field_map, all bytes as `SearchField::Other`) was previously a
/// grounding inconsistency: the size test's own doc-comment explicitly rejects
/// that path as "non-representative" for a multi-field index.  For latency the
/// single-field path is neutral-to-conservative (one long posting run vs.
/// several shorter ones), so the 50ms budget is not loosened by the switch;
/// the fix aligns representativeness claims with the actual build path so both
/// guards are grounded on the same production-representative corpus (ADR-003).
#[test]
#[cfg(not(debug_assertions))]
fn test_lexical_query_latency_representative_corpus() {
    use crate::classify_source;
    use crate::test_corpus::gen_representative_rust_module;
    use std::time::Instant;

    let dir = tmp_dir();

    let n_files = 1000usize;
    let fns_per_file = 4usize;
    let sources: Vec<String> = (0..n_files)
        .map(|i| gen_representative_rust_module(i, fns_per_file))
        .collect();

    // Build the index once using the production-representative classified path.
    // This aligns with the sibling size test and with index.rs::run() (ADR-003).
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

    // AC15 extension (3-byte single-token worst-case): a common 3-byte token
    // such as "pub" that appears in ALL 1000 files produces the largest possible
    // smallest-posting-list for the exact-symbol path.  The AND-intersection
    // candidate set is the full corpus; this measures the worst-case latency for
    // `search_exact_intersection` on the representative corpus.
    //
    // Bound: must stay within the same 50ms budget as the multi-file "wrapping_add"
    // query above.  A 3-byte token has exactly ONE trigram ("pub") so the
    // intersection step is trivially a single posting-list scan — it should be
    // FASTER than the multi-trigram "wrapping_add" query.
    //
    // PF-007 (discriminating): if `search_exact_intersection` degrades on the
    // single-trigram case (e.g. an accidental O(n²) de-dup loop), this gate fires.
    let mut q3 = SearchQuery::new("pub");
    q3.limit = Some(1000);

    // Warm-up.
    let _ = reader.search(&q3).unwrap();

    let mut min3 = std::time::Duration::from_secs(u64::MAX);
    let mut results3 = Vec::new();
    for _ in 0..TIMED_SAMPLES {
        let t = std::time::Instant::now();
        results3 = reader.search(&q3).unwrap();
        let e = t.elapsed();
        if e < min3 {
            min3 = e;
        }
    }

    eprintln!(
        "AC15 3-byte worst-case latency (min of {TIMED_SAMPLES}): {}ms \
         (corpus={n_files} files, results={}, query='pub')",
        min3.as_millis(),
        results3.len()
    );

    // "pub" appears in every generated Rust module; results must be non-empty.
    assert!(
        !results3.is_empty(),
        "AC15 3-byte guard: 'pub' must match in the 1000-file representative corpus"
    );

    // Latency guard: single-trigram intersection must stay within 50ms.
    assert!(
        min3.as_millis() < 50,
        "AC15 3-byte worst-case: latency {}ms (min of {TIMED_SAMPLES}) exceeds the 50ms budget. \
         A single-trigram AND-intersection should be faster than a multi-trigram UNION scan. \
         Profile search_exact_intersection on the 1000-file corpus.",
        min3.as_millis()
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
        gibberish_results
            .iter()
            .map(|r| r.file_id.0)
            .collect::<Vec<_>>()
    );
}

// =============================================================================
// #372 — AND-intersection exact-symbol mode tests
// =============================================================================
//
// These tests are structured per the #372 Test Plan and Acceptance Criteria.
// Every test includes a discriminating negative assertion (PF-007): the test
// fails both when the feature is deleted AND when precision regresses.

/// Helper: build a real `NgramIndexReader` (not the boxed `SearchLayer` trait)
/// so we can call inherent methods like `intersect_posting_doc_ids`.
fn build_reader_with(
    files: &[(FileId, &str, rskim_core::Language)],
) -> (tempfile::TempDir, NgramIndexReader) {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for (id, content, lang) in files {
        builder.add_file(*id, content, *lang).unwrap();
    }
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    (dir, reader)
}

/// Helper: ground-truth substring scan over `files`.
/// Returns all FileIds whose content contains `token`.
fn ground_truth_file_ids(
    files: &[(FileId, &str, rskim_core::Language)],
    token: &str,
) -> std::collections::HashSet<u32> {
    files
        .iter()
        .filter(|(_, content, _)| content.contains(token))
        .map(|(id, _, _)| id.0)
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #1: Headline recall — large/sparse definer + incidental-overlap junk
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-1 / AC #1 / PF-007: single-token exact-symbol recall.
///
/// A large definer file (file 0) contains one occurrence of a long unique token.
/// 120 junk files each share ONE or TWO trigrams of the token but NOT the whole
/// token.  `search` with default limit must return EXACTLY the set of files that
/// actually contain the literal token — equal to the grep ground truth.
///
/// Discriminating negative (PF-007):
/// - Definer MUST be present → fails if intersection drops true matches.
/// - Junk files MUST be absent → fails if UNION+take was kept (no AND fix).
/// - If `search_exact_intersection` is deleted and the old UNION path is used,
///   junk files (which share a few trigrams) would survive `take(limit*5)`
///   and the `assert!(!file_ids.contains(junk_id))` check would fire.
#[test]
fn test_ac1_headline_recall_large_sparse_definer() {
    use crate::types::query_substring_present;

    let token = "decode_postings_varint";

    // File 0: long filler (to simulate a large file) plus ONE occurrence of the token.
    let long_filler: String = "some filler content xyz ".repeat(200);
    let definer_content = format!("{long_filler} {token} and more filler content abc");

    // Files 1..=120: each contains a short phrase that shares a few trigrams
    // of the token ("dec", "ode", "pos") but NOT the full literal token.
    let mut files: Vec<(FileId, String, rskim_core::Language)> =
        vec![(FileId(0), definer_content, rskim_core::Language::Rust)];
    let junk_phrases = ["dec_isions", "pos_ition", "node_ops", "cod_ex_ample"];
    for i in 1u32..=120 {
        let phrase = junk_phrases[(i as usize - 1) % junk_phrases.len()];
        files.push((
            FileId(i),
            format!("some code with {phrase} and other stuff x{i}"),
            rskim_core::Language::Rust,
        ));
    }
    // Files 121, 122: control files that contain the full literal token.
    files.push((
        FileId(121),
        format!("pub fn {token}(data: &[u8]) -> Vec<u8> {{ vec![] }}"),
        rskim_core::Language::Rust,
    ));
    files.push((
        FileId(122),
        format!("// This module uses {token} for encoding."),
        rskim_core::Language::Rust,
    ));

    let files_str: Vec<(FileId, &str, rskim_core::Language)> = files
        .iter()
        .map(|(id, s, l)| (*id, s.as_str(), *l))
        .collect();

    let (_dir, layer) = build_index_with(&files_str);

    // Ground truth: files that actually contain the literal token.
    let ground_truth = ground_truth_file_ids(&files_str, token);
    assert_eq!(
        ground_truth,
        std::collections::HashSet::from([0, 121, 122]),
        "setup: ground truth must be {{0, 121, 122}}"
    );

    // Test with default limit (None).
    let result_default = layer.search(&SearchQuery::new(token)).unwrap();
    let ids_default: std::collections::HashSet<u32> =
        result_default.iter().map(|r| r.file_id.0).collect();

    // Must match ground truth exactly.
    assert_eq!(
        ids_default, ground_truth,
        "AC#1: exact-symbol path must return exactly the ground-truth set at default limit; \
         got {ids_default:?}, want {ground_truth:?}"
    );

    // PF-007 discriminating negative: junk files must be absent.
    for i in 1u32..=120 {
        assert!(
            !ids_default.contains(&i),
            "AC#1: junk file FileId({i}) shares only 1-2 trigrams with the token and must be absent; \
             found it in results — UNION+take was not replaced with AND-intersection (AD-372-1)"
        );
    }

    // Test with explicit large limit (should produce the same set).
    let mut ql = SearchQuery::new(token);
    ql.limit = Some(500);
    let result_large = layer.search(&ql).unwrap();
    let ids_large: std::collections::HashSet<u32> =
        result_large.iter().map(|r| r.file_id.0).collect();
    assert_eq!(
        ids_large, ground_truth,
        "AC#2 (limit independence): result set at limit=500 must equal ground truth; \
         got {ids_large:?}"
    );

    // Verify each result actually contains the token (precision).
    for r in &result_default {
        let content = files[r.file_id.0 as usize].1.as_str();
        assert!(
            query_substring_present(content, token),
            "AC#3 precision: FileId({}) in results but content does not contain the token",
            r.file_id.0
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #3: Precision — gibberish query returns empty
// ─────────────────────────────────────────────────────────────────────────────

/// PF-007: a gibberish query that has trigrams but no file contains the full
/// literal must return an empty Vec.  Fails if the intersection is replaced
/// by UNION (some files would share individual trigrams with the gibberish).
#[test]
fn test_ac3_precision_gibberish_single_token_returns_empty() {
    let (_dir, layer) = build_index_with(&[
        (
            FileId(0),
            "fn foo() { let x = 1; }",
            rskim_core::Language::Rust,
        ),
        (
            FileId(1),
            "pub fn bar(a: u32) -> u32 { a + 1 }",
            rskim_core::Language::Rust,
        ),
    ]);

    let results = layer
        .search(&SearchQuery::new("zxqwvbnmkjhgfdsa12345"))
        .unwrap();
    assert!(
        results.is_empty(),
        "AC#3: gibberish query with unique trigrams must return empty intersection; got {} results",
        results.len()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #4: Intersection strictly narrows vs UNION (PF-007 both directions)
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-1 / AC #4 / PF-007: both directions tested.
///
/// Setup: N junk files each share exactly ONE trigram with the query;
/// M files contain the full literal token.
///
/// Asserts:
/// 1. Result count ≤ M (strictly less than N+M) — intersection narrows.
/// 2. All M literal-token files are present (recall).
/// 3. Junk files are absent — asserts via INDIVIDUAL checks (PF-007).
#[test]
fn test_ac4_intersection_strictly_narrows_vs_union() {
    // Token with 4 distinct trigrams: "foo_bar"
    let token = "foo_bar";
    // Build 10 junk files, each containing exactly one of the token's trigrams.
    // "foo", "oo_", "o_b", "_ba", "bar" — pick "foo" and "_ba" for the junk.
    let files: Vec<(FileId, &str, rskim_core::Language)> = vec![
        // Junk files: contain one trigram substring but NOT the full token.
        (FileId(0), "foo xyz only", rskim_core::Language::Rust), // contains "foo" but not "foo_bar"
        (FileId(1), "xyz_bar_only", rskim_core::Language::Rust), // contains "_bar" but not "foo_bar"
        (FileId(2), "some foo other", rskim_core::Language::Rust), // "foo"
        (FileId(3), "xbar no prefix", rskim_core::Language::Rust), // "bar"
        // Files that contain the full token:
        (FileId(4), "let x = foo_bar()", rskim_core::Language::Rust),
        (FileId(5), "pub fn foo_bar() {}", rskim_core::Language::Rust),
    ];

    let (_dir, layer) = build_index_with(&files);

    let ground_truth = ground_truth_file_ids(&files, token);
    assert_eq!(
        ground_truth,
        std::collections::HashSet::from([4, 5]),
        "setup: only files 4 and 5 contain the full token"
    );

    let results = layer.search(&SearchQuery::new(token)).unwrap();
    let ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // All literal-token files must be present (recall direction, PF-007).
    assert!(
        ids.contains(&4),
        "AC#4 recall: FileId(4) contains '{token}' and must be in results"
    );
    assert!(
        ids.contains(&5),
        "AC#4 recall: FileId(5) contains '{token}' and must be in results"
    );

    // Junk files must be absent (precision / narrowing direction, PF-007).
    for junk_id in [0, 1, 2, 3] {
        assert!(
            !ids.contains(&junk_id),
            "AC#4 narrowing: FileId({junk_id}) shares only one trigram and must be absent from \
             the AND-intersection; found it — the UNION path was not replaced (AD-372-1)"
        );
    }

    // Result count must be <= M (the literal-token file count), strictly less than N+M.
    assert!(
        ids.len() <= ground_truth.len(),
        "AC#4: result count {} must be <= ground-truth count {} (intersection narrows)",
        ids.len(),
        ground_truth.len()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #5: doc_id dedup in intersection
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-2 / AC #5: a token appearing many times in one file produces posting
/// lists with many same-doc_id entries.  `search` must return each file exactly
/// once (no duplicate SearchResult).
#[test]
fn test_ac5_doc_id_dedup_in_intersection() {
    // File 0: token appears 50 times in a single line.
    let token = "my_func_name";
    let file0: String = format!("{token} ").repeat(50);

    let (_dir, layer) = build_index_with(&[
        (FileId(0), &file0, rskim_core::Language::Rust),
        (
            FileId(1),
            &format!("single occurrence of {token} here"),
            rskim_core::Language::Rust,
        ),
    ]);

    let mut q = SearchQuery::new(token);
    q.limit = Some(100);
    let results = layer.search(&q).unwrap();

    let ids: Vec<u32> = results.iter().map(|r| r.file_id.0).collect();

    // No duplicate file_ids.
    let id_set: std::collections::HashSet<u32> = ids.iter().copied().collect();
    assert_eq!(
        ids.len(),
        id_set.len(),
        "AC#5: no duplicate FileIds in results; got duplicates: {ids:?}"
    );

    // Both files must appear exactly once.
    assert!(
        id_set.contains(&0),
        "AC#5: FileId(0) with 50 occurrences must appear once"
    );
    assert!(
        id_set.contains(&1),
        "AC#5: FileId(1) with 1 occurrence must appear once"
    );

    // PF-007 negative: neither file_id appears twice.
    assert_eq!(
        ids.iter().filter(|&&id| id == 0).count(),
        1,
        "AC#5: FileId(0) must appear exactly once"
    );
    assert_eq!(
        ids.iter().filter(|&&id| id == 1).count(),
        1,
        "AC#5: FileId(1) must appear exactly once"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #5 (corruption path): injected posting-decode failure on the exact-symbol
// path must propagate Err(SearchError::IndexCorrupted) via `?` in
// intersect_posting_doc_ids / search_exact_intersection / lookup_postings.
// ─────────────────────────────────────────────────────────────────────────────

/// AC#5 (second sentence, PF-007): a corrupt/truncated posting blob on the
/// exact-symbol path must cause `search()` (via `open()`) to return
/// `Err(SearchError::IndexCorrupted)`, NOT silently return empty results.
///
/// The implementation propagates corruption from `open()` (CRC32 check) through
/// the call chain: `open() → Err(IndexCorrupted)`.  On the search path the error
/// propagates via `?` through:
/// `lookup_postings → decode_postings_varint → intersect_posting_doc_ids →
/// search_exact_intersection → search()`.
///
/// Strategy:
/// 1. Build a valid index.
/// 2. Corrupt a posting payload byte in the .skpost file.
/// 3. Re-open → the CRC32 check in `open()` fires → `Err(IndexCorrupted)`.
///
/// PF-007 (discriminating): if CRC detection were removed from `open()`, corrupt
/// data would silently decode with garbage results; the `is_err()` assertion
/// would fail, surfacing the regression.  This test also pins the error message
/// so accidental removal of the corruption-detection path is caught.
#[test]
fn test_ac5_corruption_error_on_exact_symbol_path() {
    // Step 1: build a valid index with a single-token term that will use the
    // exact-symbol (AND-intersection) path when queried.
    let dir = tmp_dir();
    let token = "corruption_test_fn";
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        builder
            .add_file(
                FileId(0),
                &format!("pub fn {token}() {{ let x = 1; x }}"),
                rskim_core::Language::Rust,
            )
            .unwrap();
        builder.build().unwrap();
    }

    // Step 2: corrupt the posting blob in .skpost (flip a byte past the header).
    let post_path = dir.path().join("index.skpost");
    let mut data = std::fs::read(&post_path).unwrap();
    assert!(
        data.len() > 16,
        "test setup: .skpost must have at least 16 bytes; got {}",
        data.len()
    );
    let corrupt_idx = data.len() / 2;
    data[corrupt_idx] ^= 0xFF;
    std::fs::write(&post_path, &data).unwrap();

    // Step 3: re-open — CRC32 check in open() must detect the corruption and
    // return Err(SearchError::IndexCorrupted).
    let result = NgramIndexReader::open(dir.path());
    assert!(
        result.is_err(),
        "AC#5 (corruption path): a corrupt .skpost must cause open() to return Err; \
         got Ok — CRC detection is absent or disabled (search_exact_intersection \
         `?` propagation is load-bearing; removing CRC or swallowing errors breaks this)"
    );

    // PF-007 (discriminating): error message must contain a corruption indicator.
    // If the error type changes from IndexCorrupted to a generic I/O error, the
    // message check below catches the semantic change.
    let err_str = format!("{}", result.err().unwrap());
    assert!(
        err_str.contains("checksum mismatch")
            || err_str.contains("corrupt")
            || err_str.contains("CRC"),
        "AC#5: error must identify corruption (contain 'checksum mismatch', 'corrupt', \
         or 'CRC'); got: {err_str}"
    );

}

// ─────────────────────────────────────────────────────────────────────────────
// AC #7: Branch dispatch — 1-2 byte token routes to short fallback
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-1 / AD-372-4 / AC #7: a query shorter than 3 bytes must route to
/// `short_query_fallback`, NOT `search_exact_intersection` (which would receive
/// empty trigrams and return []).
///
/// Discriminating (PF-007): the test builds > CANDIDATE_POOL_FLOOR files so that
/// if the old `.take(limit)` pre-truncation were still in `short_query_fallback`,
/// files with file_id >= limit would be missing.  The `fn` query matches all
/// files, so the result count must equal the number of files containing `fn`.
#[test]
fn test_ac7_branch_dispatch_short_token_routes_to_fallback() {
    // Build 110 files: files 0..=109 each contain "fn".
    // file_id >= 100 would be silently dropped by the old take(limit) with limit=100.
    let files: Vec<(FileId, String, rskim_core::Language)> = (0u32..110)
        .map(|i| {
            (
                FileId(i),
                format!("pub fn process_{i}() {{ }}", i = i),
                rskim_core::Language::Rust,
            )
        })
        .collect();
    let files_str: Vec<(FileId, &str, rskim_core::Language)> = files
        .iter()
        .map(|(id, s, l)| (*id, s.as_str(), *l))
        .collect();

    let (_dir, layer) = build_index_with(&files_str);

    // "fn" is 2 bytes → routes to short_query_fallback (no trigrams possible).
    // short_query_fallback must return the FULL filtered set (AD-372-4).
    let q = SearchQuery::new("fn");
    // No limit: short_query_fallback returns all indexed files; verify is done by caller.
    let results = layer.search(&q).unwrap();
    let ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // All 110 files must appear in the candidate set (no internal take).
    for i in 0u32..110 {
        assert!(
            ids.contains(&i),
            "AC#7 / AD-372-4: FileId({i}) must appear in short_query_fallback result set; \
             missing — the old .take(limit) pre-truncation was not removed (AD-372-4)"
        );
    }

    // All results must have score 0.0 (short-query path, no BM25F).
    for r in &results {
        assert_eq!(
            r.score, 0.0,
            "AC#7: short-query candidates must carry score 0.0; got {} for FileId({})",
            r.score, r.file_id.0
        );
    }

    // PF-007 negative: verify that a 3-byte token does NOT go through the fallback.
    // Search for "fun" (3 bytes) — should return scored results (score > 0.0 if present).
    // This confirms the branch dispatch is based on trigram extraction, not string length.
    let results3 = layer.search(&SearchQuery::new("fun")).unwrap();
    // "fun" doesn't appear in the fixture, so results should be empty from the intersection.
    // The key property: results for "fun" do NOT have score 0.0 mixed in with 110 candidates.
    // This is satisfied if "fun" returns an empty set (OR a non-fallback scored set).
    // We check that the 3-byte query path was taken (no 110 score-0 candidates).
    assert!(
        results3.len() < 110 || results3.iter().any(|r| r.score > 0.0),
        "AC#7 dispatch: a 3-byte query must NOT route to the short_query_fallback (no 110 score-0 candidates)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #9: Multi-word UNION path preserved (branch boundary)
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-1 / AC #9: two-token queries must continue to use the BM25F UNION path.
/// A file containing both tokens (in different trigram neighborhoods) must be found.
/// A strict AND-intersection of ALL query trigrams would split across the whitespace
/// boundary and never find that file.
#[test]
fn test_ac9_multi_word_union_path_preserved() {
    // Token "alpha" appears in files 0 and 1.
    // Token "gamma" appears in files 0 and 2.
    // File 0 contains BOTH tokens — the two-token query must find it.
    let (_dir, layer) = build_index_with(&[
        (
            FileId(0),
            "the alpha component and the gamma factor",
            rskim_core::Language::Rust,
        ),
        (
            FileId(1),
            "only alpha here without the second token",
            rskim_core::Language::Rust,
        ),
        (
            FileId(2),
            "only gamma here without the first token",
            rskim_core::Language::Rust,
        ),
    ]);

    // Two-token query: is_single_token returns false → UNION path.
    let mut q = SearchQuery::new("alpha gamma");
    q.limit = Some(50);
    let results = layer.search(&q).unwrap();
    let ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // File 0 (both tokens present) MUST appear.
    assert!(
        ids.contains(&0),
        "AC#9: file containing both 'alpha' and 'gamma' must appear in UNION results"
    );

    // PF-007 negative: if the test deleted the UNION path and used only
    // AND-intersection of all 10 query trigrams, file 0 might be missed
    // because the query's bigrams span the whitespace boundary.
    // The `contains(&0)` assertion above catches this.

    // Confirm that scores are non-zero (BM25F scored, not score-0 fallback).
    assert!(
        results.iter().any(|r| r.score > 0.0),
        "AC#9: multi-word query must produce BM25F-scored results (score > 0.0), not score-0 fallback"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #8: v4 format compatibility — no rebuild required (non-regression)
// ─────────────────────────────────────────────────────────────────────────────

/// AC #8: a v4 index built by the current builder must be queryable with the
/// new single-token exact path WITHOUT `--rebuild`.  FORMAT_VERSION must be 4.
/// The existing `test_ac6_result_set_non_regression_v4_codec` test is the
/// companion guard; this test focuses on the #372 exact-symbol path.
#[test]
fn test_ac8_v4_format_compat_exact_symbol_no_rebuild() {
    use crate::index::format::FORMAT_VERSION;

    let token = "exact_symbol_token";
    let dir = tmp_dir();

    // Build a v4 index.
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        builder
            .add_file(
                FileId(0),
                &format!("pub fn {token}() -> u32 {{ 42 }}"),
                rskim_core::Language::Rust,
            )
            .unwrap();
        builder
            .add_file(
                FileId(1),
                "unrelated content xyz",
                rskim_core::Language::Rust,
            )
            .unwrap();
        builder.build().unwrap();
    }

    // Verify the on-disk format version is still v4 (unchanged by #372).
    let version = NgramIndexReader::lexical_index_version(dir.path()).unwrap();
    assert_eq!(
        version, FORMAT_VERSION,
        "AC#8: format version must be {} (v4 unchanged by #372); got {version}",
        FORMAT_VERSION
    );

    // Open and query WITHOUT rebuilding.
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    let results = reader.search(&SearchQuery::new(token)).unwrap();
    let ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // The exact-symbol path must find the file containing the token.
    assert!(
        ids.contains(&0),
        "AC#8: v4 index queried with exact-symbol path must find FileId(0); no rebuild needed"
    );

    // Unrelated file must be absent (precision).
    assert!(
        !ids.contains(&1),
        "AC#8: FileId(1) (unrelated) must be absent from exact-symbol results"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #11 (reader level): Offset semantics on exact path
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-3 / AC #11 reader-level: `search` with `offset=Some(1)` on a
/// deterministic 3-file ranked intersection must skip the top-ranked file and
/// start at rank 2.
///
/// Ranking key: occurrence-count / token-density (AD-372-6, length-norm-free).
/// File 0: token appears 5 times in a very short file → highest density → rank 1.
/// File 1: token appears 3 times in a medium file → rank 2.
/// File 2: token appears 1 time in a long file → rank 3.
#[test]
fn test_ac11_offset_semantics_exact_path_reader_level() {
    let token = "my_offset_token";
    let filler = "x ".repeat(50);

    // File 0: 5 occurrences, short file → highest density.
    let file0 = format!("{token} {token} {token} {token} {token}");
    // File 1: 3 occurrences, medium file.
    let file1 = format!("{filler} {token} {token} {token} more content");
    // File 2: 1 occurrence, very long file → lowest density.
    let filler_long = "y ".repeat(500);
    let file2 = format!("{filler_long} {token} end");

    let (_dir, layer) = build_index_with(&[
        (FileId(0), &file0, rskim_core::Language::Rust),
        (FileId(1), &file1, rskim_core::Language::Rust),
        (FileId(2), &file2, rskim_core::Language::Rust),
    ]);

    // First: get all results in rank order (no offset) to establish the ground order.
    let mut q_all = SearchQuery::new(token);
    q_all.limit = Some(10);
    let all_results = layer.search(&q_all).unwrap();

    let all_ids: Vec<u32> = all_results.iter().map(|r| r.file_id.0).collect();
    assert_eq!(
        all_ids.len(),
        3,
        "AC#11 setup: must find all 3 files; got {all_ids:?}"
    );

    // With offset=1: must skip the rank-1 file and start at rank 2.
    let mut q_off = SearchQuery::new(token);
    q_off.limit = Some(10);
    q_off.offset = Some(1);
    let offset_results = layer.search(&q_off).unwrap();

    let offset_ids: Vec<u32> = offset_results.iter().map(|r| r.file_id.0).collect();

    // The offset result must be exactly the all_ids slice from index 1 onward.
    assert_eq!(
        offset_ids,
        all_ids[1..].to_vec(),
        "AC#11: offset=1 must skip rank-1 file {}, got {offset_ids:?}; want {:?}",
        all_ids[0],
        &all_ids[1..]
    );

    // PF-007: the rank-1 file must NOT appear in the offset result.
    let rank1_id = all_ids[0];
    assert!(
        !offset_ids.contains(&rank1_id),
        "AC#11: rank-1 file FileId({rank1_id}) must be skipped with offset=1"
    );

    // PF-007: offset result must be non-empty (2 remaining files).
    assert_eq!(
        offset_ids.len(),
        2,
        "AC#11: offset=1 over 3 results must leave 2 results; got {}",
        offset_ids.len()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #16 / AD-372-6: Determinism of exact path result order
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-6 / AC #16: two consecutive `search()` calls for the same query
/// must return identical file_id ordering (sorted by occurrence-count /
/// token-density key, FileId tie-break for determinism).
#[test]
fn test_ac16_determinism_exact_path() {
    let token = "deterministic_token";
    // Build multiple files with varying occurrence counts.
    let (_dir, layer) = build_index_with(&[
        (
            FileId(0),
            &format!("{token} {token} {token} short"),
            rskim_core::Language::Rust,
        ),
        (
            FileId(1),
            &format!("{token} medium length content here and more"),
            rskim_core::Language::Rust,
        ),
        (
            FileId(2),
            &format!(
                "{token} very long content with lots of other words to dilute density yyy zzz aaa bbb ccc ddd eee fff ggg hhh"
            ),
            rskim_core::Language::Rust,
        ),
    ]);

    let mut q = SearchQuery::new(token);
    q.limit = Some(50);

    let r1 = layer.search(&q).unwrap();
    let r2 = layer.search(&q).unwrap();

    let ids1: Vec<u32> = r1.iter().map(|r| r.file_id.0).collect();
    let ids2: Vec<u32> = r2.iter().map(|r| r.file_id.0).collect();

    assert_eq!(
        ids1, ids2,
        "AC#16: consecutive identical queries must produce identical file_id ordering; \
         got {ids1:?} vs {ids2:?}"
    );

    // PF-007 negative: must have at least 2 results (otherwise no ordering to check).
    assert!(
        ids1.len() >= 2,
        "AC#16 setup: must return >= 2 results for a meaningful determinism check"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AC #6 (predicate): Punctuation-joined symbol (canonical code-search case)
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-5 / AC predicate: punctuation-joined tokens like "foo::bar" contain
/// no whitespace → `is_single_token` returns true → AND-intersection is used.
/// Files containing only "foo" or only "bar" must be absent; files with the
/// full "foo::bar" must be present.
#[test]
fn test_punctuation_joined_symbol_exact_intersection() {
    use crate::ngram::is_single_token;

    let token = "foo::bar";

    // Verify predicate first.
    assert!(
        is_single_token(token),
        "is_single_token('foo::bar') must be true (no whitespace)"
    );

    let (_dir, layer) = build_index_with(&[
        (
            FileId(0),
            "let x = foo::bar::new();",
            rskim_core::Language::Rust,
        ), // full token
        (
            FileId(1),
            "use foo::other_thing;",
            rskim_core::Language::Rust,
        ), // "foo" but not "foo::bar"
        (FileId(2), "fn bar() {}", rskim_core::Language::Rust), // "bar" but not "foo::bar"
        (
            FileId(3),
            "pub use foo::bar; // import",
            rskim_core::Language::Rust,
        ), // full token
    ]);

    let results = layer.search(&SearchQuery::new(token)).unwrap();
    let ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // Files 0 and 3 contain "foo::bar" → must be present.
    assert!(
        ids.contains(&0),
        "FileId(0) contains 'foo::bar' and must be in results"
    );
    assert!(
        ids.contains(&3),
        "FileId(3) contains 'foo::bar' and must be in results"
    );

    // Files 1 and 2 contain only partial matches → must be absent.
    assert!(
        !ids.contains(&1),
        "FileId(1) contains only 'foo' (not 'foo::bar') and must be absent from AND-intersection"
    );
    assert!(
        !ids.contains(&2),
        "FileId(2) contains only 'bar' (not 'foo::bar') and must be absent from AND-intersection"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// AD-372-6: Bench-surface ranking — large-file definer within TOP_K
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-6 / PF-007: the raw occurrence-count ranking key (length-norm-free, NOT BM25F)
/// must rank a large-file definer with 3 occurrences ABOVE small files with 1 occurrence.
///
/// This emulates the rskim-bench harness (`harness.rs:148-155`) which calls
/// `reader.search(limit=Some(TOP_K))` WITHOUT a verify step — rank order alone
/// determines which results are returned.
///
/// AD-372-6 ranking key = raw occurrence count (NOT BM25F, NOT occurrence/total_tokens).
/// - BM25F: divides by field_len → large files are penalized → large-file definer buried.
/// - occurrence/total_tokens: reintroduces length normalization (small files win on density).
/// - Raw occurrence count (AD-372-6): a file with 3 occurrences ranks above 1 occurrence,
///   regardless of file size.
///
/// The test MUST FAIL if the ranking reverts to BM25F or a density-divided key.
///
/// Discriminating (PF-007): we assert the large-file definer ranks #1 (3 occurrences)
/// over small junk files (1 occurrence each).
#[test]
fn test_ac_bench_surface_ranking_large_definer_within_top_k() {
    let token = "large_definer_fn";
    const TOP_K: usize = 5;

    // File 0 (LARGE definer): the unique token appears 3 times amid ~960 bytes of filler.
    // Under BM25F this file would rank low because field_len is large.
    // Under raw occurrence-count ranking (AD-372-6) it ranks #1 (3 > 1 for small files).
    let filler = "filler_word ".repeat(80); // ~960 bytes
    let large_definer = format!("{filler} {token} middle {token} end {token}");

    // Files 1..=3 (small dense): each contains the token once in a tiny snippet.
    let small1 = format!("fn {token}() {{ 42 }}");
    let small2 = format!("pub use crate::{token};");
    let small3 = format!("// {token} defined elsewhere");

    let (_dir, reader) = build_reader_with(&[
        (FileId(0), &large_definer, rskim_core::Language::Rust),
        (FileId(1), &small1, rskim_core::Language::Rust),
        (FileId(2), &small2, rskim_core::Language::Rust),
        (FileId(3), &small3, rskim_core::Language::Rust),
    ]);

    // Emulate bench harness: limit=Some(TOP_K), NO verify step.
    let mut q = SearchQuery::new(token);
    q.limit = Some(TOP_K);
    let results = reader.search(&q).unwrap();

    let ids: Vec<u32> = results.iter().map(|r| r.file_id.0).collect();

    // The large definer (3 occurrences) must rank #1.
    // AD-372-6: raw occurrence count → 3 > 1 → FileId(0) ranks above FileIds(1,2,3).
    // Under BM25F, FileId(0) would score lower than small files due to field_len division.
    assert!(
        ids.contains(&0),
        "AD-372-6: large-file definer (FileId 0, 3 occurrences) must appear in TOP_K={TOP_K} \
         results under the length-norm-free ranking key; got {ids:?}. \
         If this fails, the ranking key reverted to BM25F (divides by field_len) — AD-372-6 violated."
    );

    // PF-007 negative: if a BM25F key (divides by field_len) were used, FileId(0)
    // might be buried below the small files.  The assert above catches that.
    // Additionally verify rank-1 is FileId(0) (highest occurrence count / density).
    if !ids.is_empty() {
        assert_eq!(
            ids[0], 0,
            "AD-372-6: FileId(0) with 3 occurrences must rank #1 under occurrence-count/density key; \
             got rank-1 = FileId({}). BM25F would bury the large file — AD-372-6 prevents that.",
            ids[0]
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AD-372-4: short_query_fallback no longer pre-truncates (AC #14)
// ─────────────────────────────────────────────────────────────────────────────

/// AD-372-4 / AC #14: `short_query_fallback` returns the FULL filtered candidate set.
/// Build > CANDIDATE_POOL_FLOOR files; verify that files with file_id >=
/// CANDIDATE_POOL_FLOOR appear in the candidate set (no internal take).
///
/// PF-007: the test explicitly checks file IDs above the old internal cap.
/// If the old `.take(limit)` with `limit=query.limit.unwrap_or(20)` were re-added,
/// files with file_id >= 20 would disappear from the result, failing the assertion.
#[test]
fn test_ac14_short_query_fallback_returns_full_set() {
    // Build 120 files, each containing "fn" (a 2-byte token → short_query_fallback).
    let files: Vec<(FileId, String, rskim_core::Language)> = (0u32..120)
        .map(|i| {
            (
                FileId(i),
                format!("fn process_{i}() {{ }}", i = i),
                rskim_core::Language::Rust,
            )
        })
        .collect();
    let files_str: Vec<(FileId, &str, rskim_core::Language)> = files
        .iter()
        .map(|(id, s, l)| (*id, s.as_str(), *l))
        .collect();

    let (_dir, layer) = build_index_with(&files_str);

    // "fn" → 2 bytes → short_query_fallback → full set returned.
    // No limit set on the query: the fallback returns all 120 files.
    let results = layer.search(&SearchQuery::new("fn")).unwrap();
    let ids: std::collections::HashSet<u32> = results.iter().map(|r| r.file_id.0).collect();

    // All 120 files must appear (no pre-truncation).
    assert_eq!(
        ids.len(),
        120,
        "AC#14 / AD-372-4: short_query_fallback must return all 120 files; \
         got {} — the old .take(limit) pre-truncation was not removed",
        ids.len()
    );

    // Specifically check files with file_id >= 100 (above the old CANDIDATE_POOL_FLOOR).
    for i in 100u32..120 {
        assert!(
            ids.contains(&i),
            "AC#14: FileId({i}) (>= CANDIDATE_POOL_FLOOR=100) must appear in short_query_fallback; \
             missing — old .take(limit) truncation still active (AD-372-4)"
        );
    }

    // PF-007 negative: if ANY of the 120 files is missing, the assertion above fires.
    // If the test is vacuous (no files contain "fn"), the ids.len()==120 check fires.
}
