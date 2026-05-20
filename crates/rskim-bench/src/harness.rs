//! Benchmark harness — orchestrates clone → index → qrel → eval per repo.

use std::collections::{HashMap, HashSet};

use anyhow::Context;

use rskim_search::{
    BM25FConfig, FileId, LayerBuilder, NgramIndexBuilder, SearchLayer, SearchQuery,
};

use crate::metrics::{mrr, precision_at_k, rank_of, reciprocal_rank};
use crate::qrel::{QrelInput, generate_qrels, validate_qrel_coverage};
use crate::split::partition;
use crate::types::{BenchResult, ConfigMetrics, IndexedFile, Qrel, RepoBenchResult};

/// Configuration for a single benchmark run.
pub struct BenchConfig {
    pub name: String,
    pub bm25f: BM25FConfig,
}

/// Run the full benchmark on pre-loaded files for a single repo.
///
/// This is the low-level harness used by both the CLI and integration tests.
/// The caller is responsible for loading files and assigning `FileId`s.
///
/// # Arguments
/// * `files` — files with assigned IDs, sorted by path for determinism
/// * `configs` — named BM25F configurations to compare
/// * `index_dir` — writable directory for the index files
///
/// # Errors
///
/// Returns an error if indexing fails or no qrels can be generated.
pub fn run_on_files(
    files: &[IndexedFile],
    contents: &HashMap<FileId, String>,
    configs: &[BenchConfig],
    index_dir: &std::path::Path,
) -> anyhow::Result<RepoBenchResult> {
    // Build qrel input list from indexed files
    let qrel_inputs: Vec<QrelInput> = files
        .iter()
        .map(|f| {
            let content = contents.get(&f.file_id).cloned().unwrap_or_default();
            QrelInput {
                file_id: f.file_id,
                path: f.path.clone(),
                language: f.language,
                content,
            }
        })
        .collect();

    // Generate qrels
    let all_qrels = generate_qrels(&qrel_inputs).context("generating qrels")?;

    // Validate coverage
    let indexed_ids: HashSet<FileId> = files.iter().map(|f| f.file_id).collect();
    validate_qrel_coverage(&all_qrels, &indexed_ids).context("validating qrel coverage")?;

    // Split into train/test
    let (train_qrels, test_qrels) = partition_qrels(&all_qrels);

    // Build the base index (using the default config)
    let mut builder =
        NgramIndexBuilder::new(index_dir.to_path_buf()).context("creating index builder")?;

    for file in files {
        let content = contents
            .get(&file.file_id)
            .map(|s| s.as_str())
            .unwrap_or("");
        builder
            .add_file(file.file_id, content, file.language)
            .with_context(|| format!("indexing file {:?}", file.path))?;
    }

    let _base_layer = builder.build().context("building index")?;
    // Note: the layer is built above to flush index files to disk.
    // We then open per-config readers below.

    // Evaluate each config on train and test splits
    let mut train_metrics = Vec::new();
    let mut test_metrics = Vec::new();

    for config in configs {
        // Open index with this config's BM25F parameters
        let reader = rskim_search::NgramIndexReader::open_with_config(index_dir, config.bm25f)
            .with_context(|| format!("opening index with config '{}'", config.name))?;

        let train_m = evaluate_split(&reader, &train_qrels, &config.name)
            .with_context(|| format!("evaluating train split for config '{}'", config.name))?;
        let test_m = evaluate_split(&reader, &test_qrels, &config.name)
            .with_context(|| format!("evaluating test split for config '{}'", config.name))?;

        train_metrics.push(train_m);
        test_metrics.push(test_m);
    }

    Ok(RepoBenchResult {
        repo_url: String::new(), // filled in by caller
        train_metrics,
        test_metrics,
        qrel_count: all_qrels.len(),
    })
}

/// Partition qrels into train/test using deterministic split.
fn partition_qrels(qrels: &[Qrel]) -> (Vec<Qrel>, Vec<Qrel>) {
    partition(qrels, |q| q.query.as_str())
}

/// Evaluate a list of qrels against a search layer.
///
/// Returns `ConfigMetrics` with MRR, Precision@5, Precision@10.
pub fn evaluate_split(
    layer: &dyn SearchLayer,
    qrels: &[Qrel],
    config_name: &str,
) -> anyhow::Result<ConfigMetrics> {
    const TOP_K: usize = 20;

    if qrels.is_empty() {
        return Ok(ConfigMetrics {
            config_name: config_name.to_string(),
            mrr: 0.0,
            precision_at_5: 0.0,
            precision_at_10: 0.0,
            query_count: 0,
            found_at_rank_1: 0,
        });
    }

    let mut rrs: Vec<f64> = Vec::with_capacity(qrels.len());
    let mut p_at_5_sum = 0.0f64;
    let mut p_at_10_sum = 0.0f64;
    let mut found_at_rank_1 = 0usize;

    for qrel in qrels {
        let mut query = SearchQuery::new(&qrel.query);
        query.limit = Some(TOP_K);

        let results = layer.search(&query).unwrap_or_default();
        let ranked: Vec<FileId> = results.iter().map(|r| r.file_id).collect();

        rrs.push(reciprocal_rank(&ranked, qrel.relevant_file_id));
        p_at_5_sum += precision_at_k(&ranked, qrel.relevant_file_id, 5);
        p_at_10_sum += precision_at_k(&ranked, qrel.relevant_file_id, 10);
        if rank_of(&ranked, qrel.relevant_file_id) == 1 {
            found_at_rank_1 += 1;
        }
    }

    let n = qrels.len() as f64;
    let mrr_val = mrr(&rrs);
    let p_at_5 = p_at_5_sum / n;
    let p_at_10 = p_at_10_sum / n;

    Ok(ConfigMetrics {
        config_name: config_name.to_string(),
        mrr: mrr_val,
        precision_at_5: p_at_5,
        precision_at_10: p_at_10,
        query_count: qrels.len(),
        found_at_rank_1,
    })
}

/// Aggregate `RepoBenchResult` values into a single macro-average `BenchResult`.
pub fn aggregate_results(repos: Vec<RepoBenchResult>) -> BenchResult {
    if repos.is_empty() {
        return BenchResult {
            repos,
            aggregate_train: vec![],
            aggregate_test: vec![],
        };
    }

    // Collect config names from the first repo (all repos use same configs)
    let config_names: Vec<String> = repos[0]
        .train_metrics
        .iter()
        .map(|m| m.config_name.clone())
        .collect();

    let aggregate_train = macro_average(&repos, &config_names, |r| &r.train_metrics);
    let aggregate_test = macro_average(&repos, &config_names, |r| &r.test_metrics);

    BenchResult {
        repos,
        aggregate_train,
        aggregate_test,
    }
}

fn macro_average<F>(
    repos: &[RepoBenchResult],
    config_names: &[String],
    get_metrics: F,
) -> Vec<ConfigMetrics>
where
    F: Fn(&RepoBenchResult) -> &Vec<ConfigMetrics>,
{
    config_names
        .iter()
        .map(|name| {
            let matching: Vec<&ConfigMetrics> = repos
                .iter()
                .flat_map(|r| get_metrics(r).iter())
                .filter(|m| &m.config_name == name)
                .collect();

            if matching.is_empty() {
                return ConfigMetrics {
                    config_name: name.clone(),
                    mrr: 0.0,
                    precision_at_5: 0.0,
                    precision_at_10: 0.0,
                    query_count: 0,
                    found_at_rank_1: 0,
                };
            }

            let n = matching.len() as f64;
            ConfigMetrics {
                config_name: name.clone(),
                mrr: matching.iter().map(|m| m.mrr).sum::<f64>() / n,
                precision_at_5: matching.iter().map(|m| m.precision_at_5).sum::<f64>() / n,
                precision_at_10: matching.iter().map(|m| m.precision_at_10).sum::<f64>() / n,
                query_count: matching.iter().map(|m| m.query_count).sum(),
                found_at_rank_1: matching.iter().map(|m| m.found_at_rank_1).sum(),
            }
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::configs;

    fn make_rust_files_with_content() -> (Vec<IndexedFile>, HashMap<FileId, String>) {
        let content = r#"
pub fn compute_value(x: i32) -> i32 { x }
pub fn process_item(s: &str) -> String { s.to_string() }
pub fn handle_event(e: u32) {}
pub struct DataModel { id: u32 }
pub struct UserRecord { name: String }
pub struct ConfigEntry { key: String }
pub fn validate_data(d: &str) -> bool { true }
pub fn format_output(v: i32) -> String { format!("{}", v) }
pub fn load_resource(path: &str) -> Vec<u8> { vec![] }
pub fn save_state(key: &str, val: i32) {}
pub fn init_logger(level: u8) {}
pub enum LogLevel { Debug, Info, Warn, Error }
"#;
        let file_id = FileId(0);
        let indexed = IndexedFile {
            file_id,
            path: PathBuf::from("src/lib.rs"),
            language: rskim_core::Language::Rust,
        };
        let mut contents = HashMap::new();
        contents.insert(file_id, content.to_string());
        (vec![indexed], contents)
    }

    #[test]
    fn run_on_files_with_two_configs() {
        let (files, contents) = make_rust_files_with_content();
        let dir = tempfile::tempdir().expect("tempdir");

        let configs = vec![
            BenchConfig {
                name: "uniform".to_string(),
                bm25f: configs::uniform(),
            },
            BenchConfig {
                name: "default_8field".to_string(),
                bm25f: configs::default_8field(),
            },
        ];

        let mut result = run_on_files(&files, &contents, &configs, dir.path()).unwrap();
        result.repo_url = "test://repo".to_string();

        assert_eq!(
            result.train_metrics.len(),
            2,
            "should have metrics for 2 configs"
        );
        assert_eq!(
            result.test_metrics.len(),
            2,
            "should have metrics for 2 configs"
        );
        assert!(result.qrel_count >= 10, "should have at least 10 qrels");
    }

    #[test]
    fn empty_split_produces_zero_mrr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (files, contents) = make_rust_files_with_content();

        // Build index
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        for f in &files {
            let c = contents.get(&f.file_id).map(|s| s.as_str()).unwrap_or("");
            builder.add_file(f.file_id, c, f.language).unwrap();
        }
        let _layer = builder.build().unwrap();

        let reader = rskim_search::NgramIndexReader::open(dir.path()).unwrap();
        let metrics = evaluate_split(&reader, &[], "uniform").unwrap();
        assert!(
            (metrics.mrr - 0.0).abs() < f64::EPSILON,
            "empty split → MRR=0"
        );
        assert_eq!(metrics.query_count, 0);
    }

    #[test]
    fn aggregate_empty_repos_returns_empty_result() {
        let result = aggregate_results(vec![]);
        assert!(result.repos.is_empty());
        assert!(result.aggregate_train.is_empty());
        assert!(result.aggregate_test.is_empty());
    }

    #[test]
    fn aggregate_two_repos_averages_mrr() {
        let repo1 = RepoBenchResult {
            repo_url: "url1".to_string(),
            train_metrics: vec![ConfigMetrics {
                config_name: "cfg".to_string(),
                mrr: 0.8,
                precision_at_5: 0.5,
                precision_at_10: 0.3,
                query_count: 10,
                found_at_rank_1: 8,
            }],
            test_metrics: vec![ConfigMetrics {
                config_name: "cfg".to_string(),
                mrr: 0.6,
                precision_at_5: 0.4,
                precision_at_10: 0.2,
                query_count: 5,
                found_at_rank_1: 3,
            }],
            qrel_count: 15,
        };
        let repo2 = RepoBenchResult {
            repo_url: "url2".to_string(),
            train_metrics: vec![ConfigMetrics {
                config_name: "cfg".to_string(),
                mrr: 0.4,
                precision_at_5: 0.3,
                precision_at_10: 0.2,
                query_count: 10,
                found_at_rank_1: 4,
            }],
            test_metrics: vec![ConfigMetrics {
                config_name: "cfg".to_string(),
                mrr: 0.2,
                precision_at_5: 0.1,
                precision_at_10: 0.1,
                query_count: 5,
                found_at_rank_1: 1,
            }],
            qrel_count: 15,
        };
        let result = aggregate_results(vec![repo1, repo2]);
        let agg_train = &result.aggregate_train[0];
        // (0.8 + 0.4) / 2 = 0.6
        assert!(
            (agg_train.mrr - 0.6).abs() < 1e-9,
            "aggregate train MRR should be 0.6, got {}",
            agg_train.mrr
        );
    }
}
