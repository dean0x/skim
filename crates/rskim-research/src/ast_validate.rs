//! Validation and reporting for AST weight tables.
//!
//! Computes IDF distribution statistics and surfacing the most discriminating
//! bigrams and trigrams per language. Output goes to stdout so the report can
//! be piped or redirected by callers.

use crate::ast_types::{AstBigramWeight, AstTrigramWeight, AstWeightTable};

/// Per-language IDF distribution statistics.
#[derive(Debug, Clone)]
pub struct IdfDistribution {
    pub language: String,
    pub count: usize,
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    pub median: f32,
    pub p90: f32,
    pub p99: f32,
}

/// Validation report for a single language.
#[derive(Debug, Clone)]
pub struct LanguageValidationReport {
    pub bigram_distribution: IdfDistribution,
    pub trigram_distribution: IdfDistribution,
    pub top_bigrams: Vec<AstBigramWeight>,
    pub top_trigrams: Vec<AstTrigramWeight>,
    pub error_node_rate: f32,
    pub vocabulary_size: usize,
}

/// Complete validation report for all languages in the weight table.
#[derive(Debug, Clone)]
pub struct AstValidationReport {
    pub per_language: Vec<LanguageValidationReport>,
    pub vocabulary_size: usize,
}

/// Run validation on an `AstWeightTable` and return the full report.
#[must_use]
pub fn run_ast_validation(table: &AstWeightTable) -> AstValidationReport {
    let vocabulary_size = table.vocabulary.len();
    let mut per_language: Vec<LanguageValidationReport> = Vec::new();

    let mut sorted_langs: Vec<&String> = table.bigram_weights.keys().collect();
    sorted_langs.sort();

    for lang in sorted_langs {
        let bigrams = &table.bigram_weights[lang];
        let trigrams = table
            .trigram_weights
            .get(lang)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        let bigram_dist = compute_distribution(lang, bigrams.iter().map(|w| w.idf));
        let trigram_dist = compute_distribution(lang, trigrams.iter().map(|w| w.idf));

        // Top-20 most discriminating bigrams (already sorted descending by IDF in the table).
        let top_bigrams: Vec<AstBigramWeight> = bigrams.iter().take(20).cloned().collect();
        let top_trigrams: Vec<AstTrigramWeight> = trigrams.iter().take(20).cloned().collect();

        // Error node rate from corpus stats.
        let error_node_rate = table
            .corpus_stats
            .language_stats
            .iter()
            .find(|s| &s.language == lang)
            .map(|s| {
                if s.total_node_count == 0 {
                    0.0
                } else {
                    s.error_node_count as f32 / s.total_node_count as f32
                }
            })
            .unwrap_or(0.0);

        per_language.push(LanguageValidationReport {
            bigram_distribution: bigram_dist,
            trigram_distribution: trigram_dist,
            top_bigrams,
            top_trigrams,
            error_node_rate,
            vocabulary_size,
        });
    }

    AstValidationReport {
        per_language,
        vocabulary_size,
    }
}

/// Compute IDF distribution statistics from an iterator of IDF values.
fn compute_distribution(language: &str, idfs: impl Iterator<Item = f32>) -> IdfDistribution {
    let mut values: Vec<f32> = idfs.filter(|v| v.is_finite()).collect();

    if values.is_empty() {
        return IdfDistribution {
            language: language.to_string(),
            count: 0,
            min: 0.0,
            max: 0.0,
            mean: 0.0,
            median: 0.0,
            p90: 0.0,
            p99: 0.0,
        };
    }

    // Sort ascending for percentile computation.
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let count = values.len();
    let min = values[0];
    let max = values[count - 1];
    let mean = values.iter().copied().sum::<f32>() / count as f32;
    let median = percentile(&values, 50.0);
    let p90 = percentile(&values, 90.0);
    let p99 = percentile(&values, 99.0);

    IdfDistribution {
        language: language.to_string(),
        count,
        min,
        max,
        mean,
        median,
        p90,
        p99,
    }
}

/// Compute the `pct`-th percentile from a sorted slice.
fn percentile(sorted: &[f32], pct: f32) -> f32 {
    debug_assert!(
        (0.0..=100.0).contains(&pct) && !pct.is_nan(),
        "pct must be in [0, 100] and not NaN, got {pct}"
    );
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((pct / 100.0) * (sorted.len() - 1) as f32).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Print the validation report to stdout.
pub fn print_ast_validation_report(report: &AstValidationReport) {
    println!("=== AST Weight Table Validation Report ===");
    println!("Vocabulary size: {} node kinds", report.vocabulary_size);
    println!();

    for lang_report in &report.per_language {
        let dist = &lang_report.bigram_distribution;
        println!("--- {} ---", dist.language);
        println!(
            "  Bigrams:  count={}, min={:.2}, max={:.2}, mean={:.2}, p50={:.2}, p90={:.2}, p99={:.2}",
            dist.count, dist.min, dist.max, dist.mean, dist.median, dist.p90, dist.p99
        );

        let tdist = &lang_report.trigram_distribution;
        println!(
            "  Trigrams: count={}, min={:.2}, max={:.2}, mean={:.2}, p50={:.2}, p90={:.2}, p99={:.2}",
            tdist.count, tdist.min, tdist.max, tdist.mean, tdist.median, tdist.p90, tdist.p99
        );

        println!(
            "  Error node rate: {:.2}%",
            lang_report.error_node_rate * 100.0
        );

        if !lang_report.top_bigrams.is_empty() {
            println!("  Top discriminating bigrams:");
            for (i, w) in lang_report.top_bigrams.iter().enumerate().take(5) {
                println!(
                    "    {}. {} -> {} (IDF={:.2})",
                    i + 1,
                    w.parent_kind,
                    w.child_kind,
                    w.idf
                );
            }
        }

        if !lang_report.top_trigrams.is_empty() {
            println!("  Top discriminating trigrams:");
            for (i, w) in lang_report.top_trigrams.iter().enumerate().take(5) {
                println!(
                    "    {}. {} -> {} -> {} (IDF={:.2})",
                    i + 1,
                    w.grandparent_kind,
                    w.parent_kind,
                    w.child_kind,
                    w.idf
                );
            }
        }

        println!();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::HashMap;

    use super::*;
    use crate::ast_types::{
        AstBigramWeight, AstCorpusStats, AstLanguageStats, AstTrigramWeight, AstWeightTable,
        encode_ast_bigram, encode_ast_trigram,
    };

    fn sample_table() -> AstWeightTable {
        let idf_values = [9.0f32, 7.0, 5.0, 3.0, 2.0, 1.5];
        let bigrams: Vec<AstBigramWeight> = idf_values
            .iter()
            .enumerate()
            .map(|(i, &idf)| AstBigramWeight {
                parent_kind: format!("parent_{i}"),
                child_kind: format!("child_{i}"),
                bigram: encode_ast_bigram(i as u16, (i + 1) as u16),
                idf,
            })
            .collect();

        let trigrams: Vec<AstTrigramWeight> = idf_values
            .iter()
            .enumerate()
            .map(|(i, &idf)| AstTrigramWeight {
                grandparent_kind: format!("gp_{i}"),
                parent_kind: format!("parent_{i}"),
                child_kind: format!("child_{i}"),
                trigram: encode_ast_trigram(i as u16, (i + 1) as u16, (i + 2) as u16),
                idf,
            })
            .collect();

        let mut bigram_weights = HashMap::new();
        bigram_weights.insert("Rust".to_string(), bigrams);
        let mut trigram_weights = HashMap::new();
        trigram_weights.insert("Rust".to_string(), trigrams);

        AstWeightTable {
            version: 1,
            generated_at: "unix:0".to_string(),
            vocabulary: (0..10).map(|i| format!("kind_{i}")).collect(),
            corpus_stats: AstCorpusStats {
                total_files: 100,
                deduplicated_files: 5,
                language_stats: vec![AstLanguageStats {
                    language: "Rust".to_string(),
                    file_count: 95,
                    unique_bigrams: 6,
                    unique_trigrams: 6,
                    error_node_count: 10,
                    total_node_count: 1000,
                }],
            },
            bigram_weights,
            trigram_weights,
        }
    }

    #[test]
    fn distribution_stats_correct() {
        // For [1, 2, 3, 4, 5]:
        //   p50 → idx = round(0.50 * 4) = 2 → value 3.0
        //   p90 → idx = round(0.90 * 4) = round(3.6) = 4 → value 5.0
        //   p99 → idx = round(0.99 * 4) = round(3.96) = 4 → value 5.0
        let values = vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
        let dist = compute_distribution("TestLang", values.into_iter());
        assert_eq!(dist.count, 5);
        assert!((dist.min - 1.0).abs() < 0.01);
        assert!((dist.max - 5.0).abs() < 0.01);
        assert!((dist.mean - 3.0).abs() < 0.01);
        assert!((dist.median - 3.0).abs() < 0.01);
        assert!(
            (dist.p90 - 5.0).abs() < 0.01,
            "p90 should be 5.0, got {}",
            dist.p90
        );
        assert!(
            (dist.p99 - 5.0).abs() < 0.01,
            "p99 should be 5.0, got {}",
            dist.p99
        );
    }

    #[test]
    fn distribution_single_value() {
        let dist = compute_distribution("L", [7.5f32].into_iter());
        assert_eq!(dist.count, 1);
        assert!((dist.min - 7.5).abs() < 0.01);
        assert!((dist.max - 7.5).abs() < 0.01);
        assert!((dist.mean - 7.5).abs() < 0.01);
    }

    #[test]
    fn distribution_empty() {
        let dist = compute_distribution("L", [].into_iter());
        assert_eq!(dist.count, 0);
        assert_eq!(dist.min, 0.0);
        assert_eq!(dist.max, 0.0);
    }

    #[test]
    fn validation_report_has_correct_language() {
        let table = sample_table();
        let report = run_ast_validation(&table);
        assert_eq!(report.per_language.len(), 1);
        assert_eq!(report.per_language[0].bigram_distribution.language, "Rust");
    }

    #[test]
    fn top_bigrams_capped_at_20() {
        let table = sample_table();
        let report = run_ast_validation(&table);
        // Table has 6 bigrams — all 6 should appear (< 20 cap).
        assert_eq!(report.per_language[0].top_bigrams.len(), 6);
    }

    #[test]
    fn error_node_rate_computed_correctly() {
        let table = sample_table();
        let report = run_ast_validation(&table);
        // 10 errors / 1000 total = 0.01
        let rate = report.per_language[0].error_node_rate;
        assert!(
            (rate - 0.01).abs() < 0.0001,
            "error rate should be 0.01, got {rate}"
        );
    }

    #[test]
    fn print_report_does_not_panic() {
        let table = sample_table();
        let report = run_ast_validation(&table);
        // Just verifying it runs without panicking.
        print_ast_validation_report(&report);
    }
}
