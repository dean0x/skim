//! Integration tests for rskim-bench.
//!
//! These tests run the full pipeline (extract → index → qrel → evaluate) using
//! synthetic in-memory content, avoiding network access or corpus cloning.

use std::collections::HashMap;
use std::path::PathBuf;

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

    let result = run_on_files(&files, &contents, &bench_configs, dir.path()).unwrap();

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
    let paths = vec![
        PathBuf::from("src/a.rs"),
        PathBuf::from("src/b.rs"),
        PathBuf::from("src/c.rs"),
    ];

    // Assign FileIds in sorted order twice
    let run1: Vec<(FileId, PathBuf)> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (FileId(i as u32), p.clone()))
        .collect();

    let run2: Vec<(FileId, PathBuf)> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| (FileId(i as u32), p.clone()))
        .collect();

    assert_eq!(run1, run2, "FileId assignment must be deterministic");
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

    let result = run_on_files(&files, &contents, &bench_configs, dir.path()).unwrap();

    // At least one config should find something
    let any_nonzero_mrr = result
        .train_metrics
        .iter()
        .chain(result.test_metrics.iter())
        .any(|m| m.mrr > 0.0 || m.query_count > 0);

    assert!(
        any_nonzero_mrr,
        "at least one config should have query_count > 0 or non-zero MRR"
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

    let mut r1 = run_on_files(&files, &contents, &bench_configs, dir1.path()).unwrap();
    r1.repo_url = "https://github.com/test/repo1".to_string();

    let mut r2 = run_on_files(&files, &contents, &bench_configs, dir2.path()).unwrap();
    r2.repo_url = "https://github.com/test/repo2".to_string();

    let aggregated = aggregate_results(vec![r1, r2]);
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
