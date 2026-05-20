//! Coordinate descent parameter tuning for BM25F.
//!
//! Sweeps k1, per-field boosts, and per-field b values to find the
//! configuration that maximises train-split MRR.
//!
//! # Algorithm
//!
//! 1. Start from `BM25FConfig::default()`
//! 2. Sweep k1 candidates; keep best
//! 3. For each field 0–7: sweep boost candidates; keep best per field
//! 4. For top-2 highest-boost fields: sweep b candidates; keep best
//! 5. Repeat steps 2–4 until MRR improvement < `CONVERGENCE_THRESHOLD` or
//!    `MAX_PASSES` passes reached
//!
//! The evaluator is injected as a closure so tests can use mock data without
//! network access.

use rskim_search::BM25FConfig;

use crate::types::{ConvergenceStep, TuningResult};

/// Maximum coordinate descent passes before stopping regardless of improvement.
const MAX_PASSES: usize = 3;

/// Stop tuning when MRR improvement per full pass is below this threshold.
const CONVERGENCE_THRESHOLD: f64 = 0.001;

/// Candidate k1 values.
const K1_CANDIDATES: &[f32] = &[0.5, 0.8, 1.0, 1.2, 1.5, 2.0];

/// Candidate field boost values.
const BOOST_CANDIDATES: &[f32] = &[0.0, 0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 7.0, 10.0];

/// Candidate b values.
const B_CANDIDATES: &[f32] = &[0.0, 0.25, 0.5, 0.75, 1.0];

/// Run coordinate descent tuning.
///
/// # Arguments
/// * `initial_config` — starting point (defaults to `BM25FConfig::default()` if None)
/// * `evaluate` — closure that accepts a `BM25FConfig` and returns train-split MRR
///
/// # Returns
///
/// `TuningResult` with the best config found, convergence trace, and pass count.
pub fn coordinate_descent<F>(initial_config: Option<BM25FConfig>, mut evaluate: F) -> TuningResult
where
    F: FnMut(BM25FConfig) -> f64,
{
    let mut current = initial_config.unwrap_or_default();
    let mut current_mrr = evaluate(current);
    let mut history: Vec<ConvergenceStep> = Vec::new();
    let mut passes_needed = 0;

    for pass in 1..=MAX_PASSES {
        let pass_start_mrr = current_mrr;

        // -- Sweep k1 --
        for &k1_candidate in K1_CANDIDATES {
            let candidate = BM25FConfig {
                k1: k1_candidate,
                ..current
            };
            if candidate.validate().is_err() {
                continue;
            }
            let candidate_mrr = evaluate(candidate);
            if candidate_mrr > current_mrr {
                history.push(ConvergenceStep {
                    pass,
                    parameter: "k1".to_string(),
                    from_value: current.k1 as f64,
                    to_value: k1_candidate as f64,
                    mrr_improvement: candidate_mrr - current_mrr,
                });
                current = candidate;
                current_mrr = candidate_mrr;
            }
        }

        // -- Sweep per-field boosts --
        for field_idx in 0..8 {
            for &boost_candidate in BOOST_CANDIDATES {
                let mut new_boosts = current.field_boosts;
                new_boosts[field_idx] = boost_candidate;
                let candidate = BM25FConfig {
                    k1: current.k1,
                    field_boosts: new_boosts,
                    field_b: current.field_b,
                };
                if candidate.validate().is_err() {
                    continue;
                }
                let candidate_mrr = evaluate(candidate);
                if candidate_mrr > current_mrr {
                    history.push(ConvergenceStep {
                        pass,
                        parameter: format!("boost[{field_idx}]"),
                        from_value: current.field_boosts[field_idx] as f64,
                        to_value: boost_candidate as f64,
                        mrr_improvement: candidate_mrr - current_mrr,
                    });
                    current = candidate;
                    current_mrr = candidate_mrr;
                }
            }
        }

        // -- Sweep b for top-2 highest-boost fields --
        let top2_fields = top_two_boost_fields(&current.field_boosts);
        for field_idx in top2_fields {
            for &b_candidate in B_CANDIDATES {
                let mut new_b = current.field_b;
                new_b[field_idx] = b_candidate;
                let candidate = BM25FConfig {
                    k1: current.k1,
                    field_boosts: current.field_boosts,
                    field_b: new_b,
                };
                if candidate.validate().is_err() {
                    continue;
                }
                let candidate_mrr = evaluate(candidate);
                if candidate_mrr > current_mrr {
                    history.push(ConvergenceStep {
                        pass,
                        parameter: format!("b[{field_idx}]"),
                        from_value: current.field_b[field_idx] as f64,
                        to_value: b_candidate as f64,
                        mrr_improvement: candidate_mrr - current_mrr,
                    });
                    current = candidate;
                    current_mrr = candidate_mrr;
                }
            }
        }

        passes_needed = pass;

        let improvement = current_mrr - pass_start_mrr;
        if improvement < CONVERGENCE_THRESHOLD {
            break;
        }
    }

    TuningResult {
        best_k1: current.k1,
        best_field_boosts: current.field_boosts.to_vec(),
        best_field_b: current.field_b.to_vec(),
        best_train_mrr: current_mrr,
        convergence_history: history,
        passes_needed,
    }
}

/// Reconstruct a `BM25FConfig` from a `TuningResult`.
///
/// # Errors
///
/// Returns an error if the reconstructed config fails validation (shouldn't
/// happen with well-formed tuning results).
pub fn result_to_config(result: &TuningResult) -> anyhow::Result<BM25FConfig> {
    use anyhow::Context;

    let mut boosts = [0.0f32; 8];
    let mut b = [0.0f32; 8];
    for (i, &v) in result.best_field_boosts.iter().take(8).enumerate() {
        boosts[i] = v;
    }
    for (i, &v) in result.best_field_b.iter().take(8).enumerate() {
        b[i] = v;
    }

    let cfg = BM25FConfig {
        k1: result.best_k1,
        field_boosts: boosts,
        field_b: b,
    };
    cfg.validate()
        .context("tuning result produced invalid config")?;
    Ok(cfg)
}

/// Return the indices of the two fields with the highest boosts.
fn top_two_boost_fields(boosts: &[f32; 8]) -> [usize; 2] {
    let mut indexed: [(f32, usize); 8] = std::array::from_fn(|i| (boosts[i], i));
    // Sort descending by boost value
    indexed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    [indexed[0].1, indexed[1].1]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Mock evaluator: returns 1.0 only when k1 == 1.5 and boost[0] >= 4.0,
    /// otherwise returns 0.5. Used to verify convergence finds the right params.
    fn mock_evaluator(cfg: BM25FConfig) -> f64 {
        let mut score = 0.5f64;
        if (cfg.k1 - 1.5).abs() < 0.01 {
            score += 0.3;
        }
        if cfg.field_boosts[0] >= 4.0 {
            score += 0.2;
        }
        score
    }

    #[test]
    fn converges_to_better_k1() {
        let result = coordinate_descent(None, mock_evaluator);
        // Should find k1=1.5 improves MRR
        assert!(
            (result.best_k1 - 1.5).abs() < 0.01,
            "should converge to k1=1.5, got {}",
            result.best_k1
        );
    }

    #[test]
    fn converges_within_max_passes() {
        let result = coordinate_descent(None, mock_evaluator);
        assert!(
            result.passes_needed <= MAX_PASSES,
            "should converge within {} passes, took {}",
            MAX_PASSES,
            result.passes_needed
        );
    }

    #[test]
    fn convergence_history_is_non_empty_when_improvement_found() {
        let result = coordinate_descent(None, mock_evaluator);
        assert!(
            !result.convergence_history.is_empty(),
            "should have convergence steps when mock evaluator rewards improvements"
        );
    }

    #[test]
    fn result_mrr_is_at_least_initial() {
        let initial = BM25FConfig::default();
        let initial_mrr = mock_evaluator(initial);
        let result = coordinate_descent(None, mock_evaluator);
        assert!(
            result.best_train_mrr >= initial_mrr - f64::EPSILON,
            "tuned MRR should be >= initial MRR"
        );
    }

    #[test]
    fn converges_immediately_when_already_optimal() {
        // Evaluator returns constant: nothing improves, convergence in 1 pass
        let result = coordinate_descent(None, |_cfg| 0.7);
        // No improvement found → history empty (no steps improved MRR)
        assert_eq!(
            result.passes_needed, 1,
            "should stop after 1 pass when nothing improves"
        );
        assert!(
            result.convergence_history.is_empty(),
            "no improvements → empty history"
        );
    }

    #[test]
    fn result_to_config_roundtrip() {
        let result = coordinate_descent(None, mock_evaluator);
        let cfg = result_to_config(&result).unwrap();
        cfg.validate().unwrap();
        assert!((cfg.k1 - result.best_k1).abs() < f32::EPSILON);
    }

    #[test]
    fn top_two_boost_fields_returns_highest_indices() {
        let boosts: [f32; 8] = [1.0, 5.0, 2.0, 8.0, 0.5, 3.0, 4.0, 1.0];
        let top2 = top_two_boost_fields(&boosts);
        // Field 3 has boost 8.0, field 1 has boost 5.0
        let mut sorted = top2;
        sorted.sort();
        assert!(sorted.contains(&3), "should include field 3 (boost=8.0)");
        assert!(sorted.contains(&1), "should include field 1 (boost=5.0)");
    }

    #[test]
    fn custom_initial_config_is_used() {
        // Start from a config where k1 is already optimal (1.5)
        // → no k1 improvement step should be needed
        let initial = crate::configs::tuned_8field(1.5, [1.0; 8], [0.75; 8]).unwrap();
        let result = coordinate_descent(Some(initial), mock_evaluator);
        // k1 should stay at 1.5 since it's already optimal
        assert!(
            (result.best_k1 - 1.5).abs() < 0.01,
            "k1 should remain at 1.5, got {}",
            result.best_k1
        );
    }
}
