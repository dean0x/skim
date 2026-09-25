//! Integration tests for rskim-bench.
//!
//! These tests run the full pipeline (extract → index → qrel → evaluate) using
//! synthetic in-memory content, avoiding network access or corpus cloning.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rskim_bench::{
    configs,
    harness::{BenchConfig, aggregate_results, run_on_files},
    metrics::{mrr, precision_at_k, reciprocal_rank},
    report,
    split::{Split, assign_split},
    tuning::{coordinate_descent, result_to_config},
    types::{BenchResult, ConfigMetrics, IndexedFile, RepoBenchResult},
};
use rskim_core::Language;
use rskim_search::FileId;

// ============================================================================
// Helper: build a set of Rust source files large enough to generate qrels
// ============================================================================

fn synthetic_rust_files() -> (Vec<IndexedFile>, HashMap<FileId, String>) {
    let files = vec![
        (
            0u32,
            "src/core.rs",
            r#"
pub fn compute_value(x: i32) -> i32 { x * 2 }
pub fn process_item(s: &str) -> String { s.to_uppercase() }
pub fn handle_event(e: u32) -> bool { e > 0 }
pub struct DataModel { id: u32, name: String }
pub struct UserRecord { name: String, email: String }
pub enum LogLevel { Debug, Info, Warn, Error }
"#,
        ),
        (
            1u32,
            "src/utils.rs",
            r#"
pub fn validate_data(d: &str) -> bool { !d.is_empty() }
pub fn format_output(v: i32) -> String { format!("{}", v) }
pub fn load_resource(path: &str) -> Vec<u8> { vec![] }
pub struct ConfigEntry { key: String, value: String }
pub struct BufferPool { size: usize }
pub enum Direction { North, South, East, West }
"#,
        ),
        (
            2u32,
            "src/service.rs",
            r#"
pub fn save_state(key: &str, val: i32) {}
pub fn init_logger(level: u8) {}
pub fn start_server(port: u16) -> bool { true }
pub struct EventLoop { running: bool }
pub struct TaskQueue { items: Vec<String> }
pub fn cleanup_resources() {}
"#,
        ),
    ];

    let mut indexed = Vec::new();
    let mut contents = HashMap::new();

    for (id, path, content) in &files {
        let fid = FileId(*id);
        indexed.push(IndexedFile {
            file_id: fid,
            path: PathBuf::from(path),
            language: Language::Rust,
        });
        contents.insert(fid, content.to_string());
    }

    (indexed, contents)
}

// ============================================================================
// AC3: All 4 configs pass validate()
// ============================================================================

#[test]
fn all_named_configs_validate() {
    for (name, cfg) in configs::all_named() {
        cfg.validate()
            .unwrap_or_else(|e| panic!("Config '{}' failed validation: {}", name, e));
    }
}

#[test]
fn tuned_8field_valid_params_validate() {
    let cfg = configs::tuned_8field(1.5, [2.0; 8], [0.5; 8]).unwrap();
    cfg.validate().unwrap();
}

// ============================================================================
// AC5: Deterministic train/test split across 100 runs
// ============================================================================

#[test]
fn split_is_deterministic_across_100_runs() {
    let queries: Vec<String> = (0..50).map(|i| format!("symbol_name_{i}")).collect();
    let first_pass: Vec<Split> = queries.iter().map(|q| assign_split(q)).collect();

    for _ in 0..99 {
        let pass: Vec<Split> = queries.iter().map(|q| assign_split(q)).collect();
        assert_eq!(
            pass, first_pass,
            "split assignment must be deterministic across runs"
        );
    }
}

// ============================================================================
// AC6: Coordinate descent converges within 3 passes
// ============================================================================

#[test]
fn coordinate_descent_converges_within_3_passes() {
    // Evaluator rewards k1=1.5 and boost[0] >= 4.0
    let result = coordinate_descent(None, |cfg| {
        let mut score = 0.5f64;
        if (cfg.k1 - 1.5).abs() < 0.01 {
            score += 0.3;
        }
        if cfg.field_boosts[0] >= 4.0 {
            score += 0.2;
        }
        score
    });
    assert!(
        result.passes_needed <= 3,
        "should converge within 3 passes, took {}",
        result.passes_needed
    );
}

// ============================================================================
// AC21: Empty results → MRR=0 included in average
// ============================================================================

#[test]
fn empty_ranked_list_contributes_zero_mrr() {
    let rr = reciprocal_rank(&[], FileId(1));
    assert!((rr - 0.0).abs() < f64::EPSILON, "empty results → RR=0");

    // MRR over [1.0, 0.0, 0.5] includes the zero
    let rrs = [1.0, 0.0, 0.5];
    let expected = (1.0 + 0.0 + 0.5) / 3.0;
    assert!(
        (mrr(&rrs) - expected).abs() < 1e-9,
        "zero RR should be included in MRR average"
    );
}

// ============================================================================
// AC22: All configs produce same result count for same query
// ============================================================================

#[test]
fn all_configs_produce_results_for_same_query() {
    let (files, contents) = synthetic_rust_files();
    let dir = tempfile::tempdir().unwrap();

    let bench_configs: Vec<BenchConfig> = configs::all_named()
        .into_iter()
        .map(|(name, bm25f)| BenchConfig {
            name: name.to_string(),
            bm25f,
        })
        .collect();

    let result =
        run_on_files(&files, &contents, &bench_configs, dir.path(), "test://repo").unwrap();

    // All configs should have the same query count on each split
    let train_counts: Vec<usize> = result.train_metrics.iter().map(|m| m.query_count).collect();
    let test_counts: Vec<usize> = result.test_metrics.iter().map(|m| m.query_count).collect();

    if let Some(&first) = train_counts.first() {
        for &count in &train_counts {
            assert_eq!(
                count, first,
                "all configs should evaluate the same number of train queries"
            );
        }
    }
    if let Some(&first) = test_counts.first() {
        for &count in &test_counts {
            assert_eq!(
                count, first,
                "all configs should evaluate the same number of test queries"
            );
        }
    }
}

// ============================================================================
// AC24: FileId assignment is deterministic (files sorted by path)
// ============================================================================

#[test]
fn file_id_assignment_deterministic_when_sorted() {
    // Two different orderings of the same file set.
    let ordering_a = vec![
        PathBuf::from("src/c.rs"),
        PathBuf::from("src/a.rs"),
        PathBuf::from("src/b.rs"),
    ];
    let ordering_b = vec![
        PathBuf::from("src/b.rs"),
        PathBuf::from("src/c.rs"),
        PathBuf::from("src/a.rs"),
    ];

    // Mirror the production pattern from main.rs (AC24): sort by path, then
    // assign FileIds sequentially via enumerate.
    let assign_ids = |mut paths: Vec<PathBuf>| -> Vec<(PathBuf, FileId)> {
        paths.sort();
        paths
            .into_iter()
            .enumerate()
            .map(|(i, p)| (p, FileId(i as u32)))
            .collect()
    };

    let run_a = assign_ids(ordering_a);
    let run_b = assign_ids(ordering_b);

    // (1) IDs must be sequential starting from 0.
    for (expected_id, (_, fid)) in run_a.iter().enumerate() {
        assert_eq!(
            fid.0, expected_id as u32,
            "FileId at position {expected_id} should equal {expected_id}"
        );
    }

    // (2) Both orderings must produce the same (path, FileId) mapping.
    assert_eq!(
        run_a, run_b,
        "FileId assignment must be identical regardless of initial ordering"
    );
}

// ============================================================================
// AC7: Report includes per-repo AND aggregate metrics
// ============================================================================

#[test]
fn report_includes_per_repo_and_aggregate() {
    let metrics = vec![ConfigMetrics {
        config_name: "uniform".to_string(),
        mrr: 0.5,
        precision_at_5: 0.3,
        precision_at_10: 0.2,
        query_count: 10,
        found_at_rank_1: 5,
    }];

    let result = BenchResult {
        repos: vec![RepoBenchResult {
            repo_url: "https://github.com/test/repo".to_string(),
            train_metrics: metrics.clone(),
            test_metrics: metrics.clone(),
            qrel_count: 15,
        }],
        aggregate_train: metrics.clone(),
        aggregate_test: metrics,
    };

    let json = report::to_json(&result, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert!(val["repos"].is_array(), "should have repos array");
    assert!(
        val["aggregate_train"].is_array(),
        "should have aggregate_train"
    );
    assert!(
        val["aggregate_test"].is_array(),
        "should have aggregate_test"
    );
    assert!(
        !val["repos"].as_array().unwrap().is_empty(),
        "repos should not be empty"
    );

    let md = report::to_markdown(&result, None);
    assert!(
        md.contains("Aggregate Results"),
        "markdown should include aggregate section"
    );
    assert!(md.contains("repo"), "markdown should reference repo name");
}

// ============================================================================
// Full pipeline smoke test: index → qrel → eval
// ============================================================================

#[test]
fn full_pipeline_produces_non_zero_metrics() {
    let (files, contents) = synthetic_rust_files();
    let dir = tempfile::tempdir().unwrap();

    let bench_configs = vec![
        BenchConfig {
            name: "uniform".to_string(),
            bm25f: configs::uniform(),
        },
        BenchConfig {
            name: "default_8field".to_string(),
            bm25f: configs::default_8field(),
        },
    ];

    let result =
        run_on_files(&files, &contents, &bench_configs, dir.path(), "test://repo").unwrap();

    // At least one config must have processed queries (sanity check that the
    // pipeline ran at all).
    let any_nonzero_query_count = result
        .train_metrics
        .iter()
        .chain(result.test_metrics.iter())
        .any(|m| m.query_count > 0);
    assert!(
        any_nonzero_query_count,
        "at least one config should have query_count > 0 (pipeline must run queries)"
    );

    // At least one config must return a relevant result (guards against search
    // returning nothing relevant for any query).
    let any_nonzero_mrr = result
        .train_metrics
        .iter()
        .chain(result.test_metrics.iter())
        .any(|m| m.mrr > 0.0);
    assert!(
        any_nonzero_mrr,
        "at least one config should have MRR > 0.0 (search must find relevant results)"
    );
    assert!(
        result.qrel_count >= 10,
        "should generate at least 10 qrels, got {}",
        result.qrel_count
    );
}

// ============================================================================
// Aggregate macro-average smoke test
// ============================================================================

#[test]
fn aggregate_results_macro_average() {
    let (files, contents) = synthetic_rust_files();
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();

    let bench_configs = vec![BenchConfig {
        name: "uniform".to_string(),
        bm25f: configs::uniform(),
    }];

    let r1 = run_on_files(
        &files,
        &contents,
        &bench_configs,
        dir1.path(),
        "https://github.com/test/repo1",
    )
    .unwrap();

    let r2 = run_on_files(
        &files,
        &contents,
        &bench_configs,
        dir2.path(),
        "https://github.com/test/repo2",
    )
    .unwrap();

    let aggregated = aggregate_results(vec![r1, r2]).unwrap();
    assert_eq!(aggregated.repos.len(), 2);
    assert_eq!(aggregated.aggregate_train.len(), 1);
    assert_eq!(aggregated.aggregate_test.len(), 1);
}

// ============================================================================
// Tuning result can be converted to a valid BM25FConfig
// ============================================================================

#[test]
fn tuning_result_to_config_is_valid() {
    let result = coordinate_descent(None, |_cfg| 0.5);
    let cfg = result_to_config(&result).unwrap();
    cfg.validate().unwrap();
}

// ============================================================================
// Precision@K is consistent with reciprocal rank for rank-1 results
// ============================================================================

#[test]
fn precision_at_5_consistent_with_rr_at_rank_1() {
    let ranked = [FileId(1), FileId(2), FileId(3), FileId(4), FileId(5)];
    // Relevant at rank 1 → P@5 = 1/5, RR = 1.0
    let p5 = precision_at_k(&ranked, FileId(1), 5);
    let rr = reciprocal_rank(&ranked, FileId(1));

    assert!(
        (p5 - 0.2).abs() < f64::EPSILON,
        "P@5 with relevant at rank 1 = 0.2"
    );
    assert!(
        (rr - 1.0).abs() < f64::EPSILON,
        "RR with relevant at rank 1 = 1.0"
    );
}

// ============================================================================
// Item 11: extract_symbols dispatch integration test
// ============================================================================

#[test]
fn extract_symbols_dispatch_integration() {
    // Rust: should extract symbols
    let rust_symbols = rskim_bench::extract::extract_symbols(
        Path::new("test.rs"),
        "pub fn test_func(x: i32) -> i32 { x }",
        Language::Rust,
    );
    assert!(
        !rust_symbols.is_empty(),
        "Rust extraction should find symbols"
    );
    assert!(
        rust_symbols
            .iter()
            .any(|s| s.field == rskim_search::SearchField::FunctionSignature),
        "Rust extraction should find a FunctionSignature"
    );

    // Python: should extract symbols
    let py_symbols = rskim_bench::extract::extract_symbols(
        Path::new("test.py"),
        "def test_func(x: int) -> int:\n    return x",
        Language::Python,
    );
    assert!(
        !py_symbols.is_empty(),
        "Python extraction should find symbols"
    );

    // Go: should extract symbols
    let go_symbols = rskim_bench::extract::extract_symbols(
        Path::new("test.go"),
        "package main\n\nfunc TestFunc(x int) int { return x }",
        Language::Go,
    );
    assert!(!go_symbols.is_empty(), "Go extraction should find symbols");

    // Unsupported: should return empty
    let ts_symbols = rskim_bench::extract::extract_symbols(
        Path::new("test.ts"),
        "function test() {}",
        Language::TypeScript,
    );
    assert!(
        ts_symbols.is_empty(),
        "Unsupported language should return empty"
    );
}

// ============================================================================
// Item 12: run_on_files error-path test
// ============================================================================

#[test]
fn run_on_files_too_few_qrels_returns_error() {
    let file = IndexedFile {
        file_id: FileId(0),
        path: PathBuf::from("test.rs"),
        language: Language::Rust,
    };
    let mut contents = HashMap::new();
    contents.insert(FileId(0), "fn x() {}".to_string());

    let dir = tempfile::tempdir().unwrap();
    let configs = vec![BenchConfig {
        name: "test".to_string(),
        bm25f: configs::uniform(),
    }];

    let result = run_on_files(&[file], &contents, &configs, dir.path(), "test://repo");
    assert!(result.is_err(), "should error with too few qrels");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("qrel") || err_msg.contains("Qrel"),
        "error should mention qrels, got: {err_msg}"
    );
}

// ============================================================================
// AC4 (#355): Precision@1 regression guard via curated labeled fixture set
//
// PF-007: asserts a discriminating observable (P@1 == 1.0 for the definer),
// not just exit-0 or non-empty results.  This test would fail the moment the
// trigram IDF table regresses to near-uniform scoring — which was the root
// cause of #355.
//
// Corpus design:
//   - definer (file 0): defines `ZyntheticUniqueIdentifier` (the target symbol)
//   - noise_a (file 1): mentions it once in a comment (incidental overlap)
//   - noise_b (file 2): a large file of common Rust code — HashMap/fmt/struct
//     patterns — with ZERO trigram overlap with "ZyntheticUniqueIdentifier".
//
// After indexing and querying via reader.search():
//   - Part A (trigram exclusion): noise_b must be absent because it has no
//     trigrams in common with the query — NOT because the verify gate dropped it.
//     NOTE: reader.search() does NOT run the CLI verify gate.  The verify gate
//     lives in the CLI layer (rskim::cmd::search::execute_query).  noise_b's
//     absence here is due to zero trigram overlap, not verify filtering.
//     Tests for the verify gate are in query_tests.rs (AC1/AC2/AC3).
//   - Part B (IDF ranking): definer must be at results[0] because its BM25F
//     score is higher — it contains the symbol more times than noise_a.
//   - AC4 guard: P@1 == 1.0 for this labelled qrel.
//
// This test measures the UNVERIFIED candidate layer (reader.search() only).
// ac4_verified_path_p_at_1_guard (below) applies a content-presence filter
// that mirrors the CLI verify gate and guards the verified-path ranking contract
// (F12 / Finding 12 from cycle-3 review).
//
// The corpus is toy-sized (3 files) — it is NOT a surrogate for full qrel
// evaluation.  AC4 guards specifically against IDF-uniform regression where
// all trigrams score DEFAULT_WEIGHT=1.0, making BM25F rank by TF/length alone.
// A 3-file corpus is the minimal guard for the AC4 acceptance criterion.
// ============================================================================

#[test]
fn ac4_precision_at_1_regression_guard() {
    use rskim_bench::metrics::{precision_at_k, rank_of};
    use rskim_search::{
        FileId, LayerBuilder, NgramIndexBuilder, NgramIndexReader, SearchLayer, SearchQuery,
    };

    // --- corpus ---
    // File 0: the "definer" — the one correct answer for the query.
    //         Contains the full symbol name repeated enough times that BM25F TF
    //         term frequency is higher than in the noise files.
    let definer_content = r#"
/// ZyntheticUniqueIdentifier is the canonical implementation.
/// It is defined here and only here.
pub struct ZyntheticUniqueIdentifier {
    pub value: u64,
}

impl ZyntheticUniqueIdentifier {
    pub fn new(value: u64) -> Self {
        Self { value }
    }

    pub fn display(&self) -> String {
        format!("ZyntheticUniqueIdentifier({})", self.value)
    }
}
"#;

    // File 1: noise — contains the symbol name once in a comment, not defining it.
    //         Should appear in results (literal match), but rank BELOW the definer.
    let noise_a_content = r#"
// We re-export ZyntheticUniqueIdentifier from the definer module.
pub use definer::ZyntheticUniqueIdentifier;

pub fn helper_function() -> u64 {
    42
}
"#;

    // File 2: pure noise — common Rust code (HashMap/fmt/Display/struct patterns)
    //         with ZERO trigram overlap with "ZyntheticUniqueIdentifier".
    //         This file is absent from results because the trigram index has no
    //         matching trigrams, NOT because the verify gate dropped it.
    //         (reader.search() does not run the verify gate — see note in block comment above.)
    let noise_b_content = r#"
use std::collections::HashMap;
use std::fmt;

pub struct DataManager {
    items: HashMap<u64, String>,
}

impl DataManager {
    pub fn new() -> Self {
        Self { items: HashMap::new() }
    }

    pub fn insert(&mut self, key: u64, val: String) {
        self.items.insert(key, val);
    }

    pub fn display_all(&self) {
        for (k, v) in &self.items {
            println!("{}: {}", k, v);
        }
    }
}

impl fmt::Display for DataManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DataManager({} items)", self.items.len())
    }
}
"#;

    let dir = tempfile::tempdir().unwrap();
    let definer_id = FileId(0);
    let noise_a_id = FileId(1);
    let noise_b_id = FileId(2);

    // Index all three files.
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(definer_id, definer_content, rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(noise_a_id, noise_a_content, rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(noise_b_id, noise_b_content, rskim_core::Language::Rust)
        .unwrap();
    let _layer = builder.build().unwrap();

    let reader = NgramIndexReader::open(dir.path()).unwrap();

    let query_str = "ZyntheticUniqueIdentifier";
    let mut query = SearchQuery::new(query_str);
    query.limit = Some(20);

    let results = reader.search(&query).unwrap();
    let ranked: Vec<FileId> = results.iter().map(|r| r.file_id).collect();

    // PF-007: assert discriminating observables, not just exit-0.

    // AC4-a: noise_b must be ABSENT because it has zero trigram overlap with
    // "ZyntheticUniqueIdentifier" — the trigram index never emits it as a candidate.
    // This is NOT the verify gate: reader.search() does not call the verify layer;
    // the verify gate test lives in query_tests.rs (test_ac1_verify_gate_drops_trigram_overlap_non_literal).
    assert!(
        !ranked.contains(&noise_b_id),
        "AC4-a (PF-007): noise_b has zero trigram overlap with the query and must be absent \
        from reader.search() results; got: {ranked:?}"
    );

    // AC4-b: definer must appear in results at all.
    assert!(
        ranked.contains(&definer_id),
        "AC4-b (PF-007): definer (file 0) must appear in results; got: {ranked:?}"
    );

    // AC4-c: P@1 == 1.0 — definer ranks at position 1.
    //
    // This is the core regression guard for Part B IDF selectivity (#355).
    // If the trigram weight table degrades to uniform IDF (DEFAULT_WEIGHT=1.0
    // for every query trigram), BM25F scores become dominated by document length
    // and TF, which may push noise_a (shorter file) above the definer.
    let p_at_1 = precision_at_k(&ranked, definer_id, 1);
    assert!(
        (p_at_1 - 1.0).abs() < f64::EPSILON,
        "AC4-c (PF-007): P@1 must be 1.0 — definer must rank first; got rank={}, results={ranked:?}",
        rank_of(&ranked, definer_id)
    );
}

// ============================================================================
// AC4 (#355, F12): Verified-path P@1 guard
//
// The ac4_precision_at_1_regression_guard test above calls reader.search()
// directly, which is the UNVERIFIED candidate layer.  Per Finding 12, a guard
// that only measures the unverified layer cannot detect regressions in the
// verify-then-truncate CLI path.
//
// This companion test closes that gap: it applies a content-presence filter
// to the candidate results using `rskim_search::query_substring_present` —
// the SAME predicate used by the CLI verify gate (`extract_snippet_and_verify`
// in snippet.rs).  Both the CLI and this bench guard now call the identical
// function, eliminating the prior drift where an inline copy could diverge.
//
// The predicate lives in rskim_search::types so it is reachable from both the
// rskim CLI crate and rskim-bench without a private-symbol workaround.  See
// rskim-search/src/types.rs for its canonical definition.
//
// PF-007: if the verify gate is removed from the CLI layer (query.rs), the
// production P@1 would differ from what this test measures; the companion
// ac4_precision_at_1_regression_guard guards the unverified ranking layer, and
// this test guards the verified-path contract.
// ============================================================================

#[test]
fn ac4_verified_path_p_at_1_guard() {
    use rskim_bench::metrics::{precision_at_k, rank_of};
    use rskim_search::{
        FileId, LayerBuilder, NgramIndexBuilder, NgramIndexReader, SearchLayer, SearchQuery,
        query_substring_present,
    };
    use std::collections::HashMap;

    // Identical corpus to ac4_precision_at_1_regression_guard.
    let definer_content = r#"
/// ZyntheticUniqueIdentifier is the canonical implementation.
/// It is defined here and only here.
pub struct ZyntheticUniqueIdentifier {
    pub value: u64,
}

impl ZyntheticUniqueIdentifier {
    pub fn new(value: u64) -> Self {
        Self { value }
    }

    pub fn display(&self) -> String {
        format!("ZyntheticUniqueIdentifier({})", self.value)
    }
}
"#;
    let noise_a_content = r#"
// We re-export ZyntheticUniqueIdentifier from the definer module.
pub use definer::ZyntheticUniqueIdentifier;

pub fn helper_function() -> u64 {
    42
}
"#;
    let noise_b_content = r#"
use std::collections::HashMap;
use std::fmt;

pub struct DataManager {
    items: HashMap<u64, String>,
}

impl DataManager {
    pub fn new() -> Self {
        Self { items: HashMap::new() }
    }

    pub fn insert(&mut self, key: u64, val: String) {
        self.items.insert(key, val);
    }
}
"#;

    let dir = tempfile::tempdir().unwrap();
    let definer_id = FileId(0);
    let noise_a_id = FileId(1);
    let noise_b_id = FileId(2);

    let file_contents: HashMap<FileId, &str> = [
        (definer_id, definer_content),
        (noise_a_id, noise_a_content),
        (noise_b_id, noise_b_content),
    ]
    .into();

    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder
        .add_file(definer_id, definer_content, rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(noise_a_id, noise_a_content, rskim_core::Language::Rust)
        .unwrap();
    builder
        .add_file(noise_b_id, noise_b_content, rskim_core::Language::Rust)
        .unwrap();
    let _layer = builder.build().unwrap();

    let reader = NgramIndexReader::open(dir.path()).unwrap();

    let query_str = "ZyntheticUniqueIdentifier";
    let mut query = SearchQuery::new(query_str);
    query.limit = Some(20);

    let raw_results = reader.search(&query).unwrap();

    // Apply the verified-path filter: keep only candidates whose content passes
    // the AND-of-tokens substring check using the shared rskim_search predicate.
    // This is the SAME function used by the CLI verify gate, so bench metrics
    // measure the identical verified surface that users see (F1/Finding 1 fix).
    // File IDs are 0-based, matching the insertion order above.
    let verified_ranked: Vec<FileId> = raw_results
        .iter()
        .filter(|r| {
            file_contents
                .get(&r.file_id)
                .map(|content| query_substring_present(content, query_str))
                .unwrap_or(false)
        })
        .map(|r| r.file_id)
        .collect();

    // AC4-verified-a: definer must survive verification (contains the symbol).
    assert!(
        verified_ranked.contains(&definer_id),
        "AC4-verified (F12/PF-007): definer must survive the verify gate; got: {verified_ranked:?}"
    );

    // AC4-verified-b: noise_b must NOT survive (its content does not contain
    // the symbol — zero trigram overlap means it is absent from raw_results too,
    // but we assert here to guard both the unverified and verified paths).
    assert!(
        !verified_ranked.contains(&noise_b_id),
        "AC4-verified (F12/PF-007): noise_b must be absent after verification; \
        got: {verified_ranked:?}"
    );

    // AC4-verified-c: P@1 == 1.0 over the VERIFIED result set.
    //
    // This is the core regression guard for the verified CLI path.  If the
    // ranking or verify gate regresses (verify drops the definer, or ranking
    // pushes noise_a above the definer), P@1 drops below 1.0.
    let p_at_1 = precision_at_k(&verified_ranked, definer_id, 1);
    assert!(
        (p_at_1 - 1.0).abs() < f64::EPSILON,
        "AC4-verified-c (F12/PF-007): verified P@1 must be 1.0 — definer must rank first \
        in the verified result set; rank={}, verified_results={verified_ranked:?}",
        rank_of(&verified_ranked, definer_id)
    );
}

// ============================================================================
// Item 13: aggregate_results mismatch validation
// ============================================================================

#[test]
fn aggregate_results_rejects_mismatched_config_names() {
    let repo1 = RepoBenchResult {
        repo_url: "repo1".to_string(),
        train_metrics: vec![ConfigMetrics {
            config_name: "cfg_a".to_string(),
            mrr: 0.5,
            precision_at_5: 0.3,
            precision_at_10: 0.2,
            query_count: 10,
            found_at_rank_1: 5,
        }],
        test_metrics: vec![ConfigMetrics {
            config_name: "cfg_a".to_string(),
            mrr: 0.4,
            precision_at_5: 0.2,
            precision_at_10: 0.1,
            query_count: 5,
            found_at_rank_1: 2,
        }],
        qrel_count: 15,
    };
    let repo2 = RepoBenchResult {
        repo_url: "repo2".to_string(),
        train_metrics: vec![ConfigMetrics {
            config_name: "cfg_b".to_string(), // different!
            mrr: 0.6,
            precision_at_5: 0.4,
            precision_at_10: 0.3,
            query_count: 10,
            found_at_rank_1: 6,
        }],
        test_metrics: vec![ConfigMetrics {
            config_name: "cfg_b".to_string(),
            mrr: 0.5,
            precision_at_5: 0.3,
            precision_at_10: 0.2,
            query_count: 5,
            found_at_rank_1: 3,
        }],
        qrel_count: 15,
    };
    let result = aggregate_results(vec![repo1, repo2]);
    assert!(result.is_err(), "should reject mismatched config names");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("config name mismatch"),
        "error message should say 'config name mismatch'"
    );
}
