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

use anyhow::Context;
use rskim_search::{BM25FConfig, FIELD_COUNT};

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

/// Sweep a single BM25F parameter over its candidates, keeping the best.
///
/// Updates `current`, `current_mrr`, and `history` in-place when a candidate
/// improves over the current best.
///
/// # Arguments
/// * `current` — current best config (mutated in-place on improvement)
/// * `current_mrr` — MRR of current best (mutated in-place on improvement)
/// * `history` — convergence trace (extended on improvement)
/// * `pass` — current pass index for trace labelling
/// * `param_name` — display name for the parameter being swept
/// * `candidates` — candidate values to try
/// * `get_value` — extracts the current parameter value from a config
/// * `make_candidate` — produces a new config with the candidate value applied
/// * `evaluate` — closure that scores a config and returns MRR
#[allow(clippy::too_many_arguments)]
fn sweep_parameter<G>(
    current: &mut BM25FConfig,
    current_mrr: &mut f64,
    history: &mut Vec<ConvergenceStep>,
    pass: usize,
    param_name: &str,
    candidates: &[f32],
    get_value: impl Fn(&BM25FConfig) -> f32,
    make_candidate: impl Fn(&BM25FConfig, f32) -> BM25FConfig,
    evaluate: &mut G,
) where
    G: FnMut(BM25FConfig) -> f64,
{
    let from_value = get_value(current) as f64;
    for &val in candidates {
        let candidate = make_candidate(current, val);
        if candidate.validate().is_err() {
            continue;
        }
        let candidate_mrr = evaluate(candidate);
        if candidate_mrr > *current_mrr {
            history.push(ConvergenceStep {
                pass,
                parameter: param_name.to_string(),
                from_value,
                to_value: val as f64,
                mrr_improvement: candidate_mrr - *current_mrr,
            });
            *current = candidate;
            *current_mrr = candidate_mrr;
        }
    }
}

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
        sweep_parameter(
            &mut current,
            &mut current_mrr,
            &mut history,
            pass,
            "k1",
            K1_CANDIDATES,
            |c| c.k1,
            |c, v| BM25FConfig { k1: v, ..*c },
            &mut evaluate,
        );

        // -- Sweep per-field boosts --
        for field_idx in 0..FIELD_COUNT {
            sweep_parameter(
                &mut current,
                &mut current_mrr,
                &mut history,
                pass,
                &format!("boost[{field_idx}]"),
                BOOST_CANDIDATES,
                |c| c.field_boosts[field_idx],
                |c, v| {
                    let mut boosts = c.field_boosts;
                    boosts[field_idx] = v;
                    BM25FConfig {
                        field_boosts: boosts,
                        ..*c
                    }
                },
                &mut evaluate,
            );
        }

        // -- Sweep b for top-2 highest-boost fields --
        let top2_fields = top_two_boost_fields(&current.field_boosts);
        for field_idx in top2_fields {
            sweep_parameter(
                &mut current,
                &mut current_mrr,
                &mut history,
                pass,
                &format!("b[{field_idx}]"),
                B_CANDIDATES,
                |c| c.field_b[field_idx],
                |c, v| {
                    let mut b = c.field_b;
                    b[field_idx] = v;
                    BM25FConfig { field_b: b, ..*c }
                },
                &mut evaluate,
            );
        }

        passes_needed = pass;

        let improvement = current_mrr - pass_start_mrr;
        if improvement < CONVERGENCE_THRESHOLD {
            break;
        }
    }

    TuningResult {
        best_k1: current.k1,
        best_field_boosts: current.field_boosts,
        best_field_b: current.field_b,
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
    let cfg = BM25FConfig {
        k1: result.best_k1,
        field_boosts: result.best_field_boosts,
        field_b: result.best_field_b,
    };
    cfg.validate()
        .context("tuning result produced invalid config")?;
    Ok(cfg)
}

/// Return the indices of the two fields with the highest boosts.
fn top_two_boost_fields(boosts: &[f32; FIELD_COUNT]) -> [usize; 2] {
    let mut indexed: [(f32, usize); FIELD_COUNT] = std::array::from_fn(|i| (boosts[i], i));
    // Sort descending by boost value
    indexed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    [indexed[0].1, indexed[1].1]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code — unwrap acceptable for test assertions
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
        let boosts: [f32; FIELD_COUNT] = [1.0, 5.0, 2.0, 8.0, 0.5, 3.0, 4.0, 1.0];
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
        let initial =
            crate::configs::tuned_8field(1.5, [1.0; FIELD_COUNT], [0.75; FIELD_COUNT]).unwrap();
        let result = coordinate_descent(Some(initial), mock_evaluator);
        // k1 should stay at 1.5 since it's already optimal
        assert!(
            (result.best_k1 - 1.5).abs() < 0.01,
            "k1 should remain at 1.5, got {}",
            result.best_k1
        );
    }
}
