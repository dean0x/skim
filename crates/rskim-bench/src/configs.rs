//! Named BM25F configurations for benchmarking.
//!
//! All configurations are validated at construction time so callers receive a
//! guaranteed-valid `BM25FConfig` without needing to call `validate()`.

use rskim_search::{BM25FConfig, FIELD_COUNT};

// Field order (SearchField discriminants):
// TypeDefinition=0, FunctionSignature=1, SymbolName=2, ImportExport=3,
// FunctionBody=4, Comment=5, StringLiteral=6, Other=7

/// Uniform configuration: all fields equally weighted.
///
/// Baseline for comparison — no structural awareness.
/// Validated at build time — all values are compile-time constants.
#[must_use]
pub fn uniform() -> BM25FConfig {
    BM25FConfig {
        k1: 1.2,
        field_boosts: [1.0; FIELD_COUNT],
        field_b: [0.75; FIELD_COUNT],
    }
}

/// Sourcegraph-style configuration: structural fields boosted 3×.
///
/// TypeDefinition, FunctionSignature, SymbolName, ImportExport get boost=3.0;
/// implementation fields (FunctionBody, Comment, StringLiteral, Other) stay at 1.0.
/// Validated at build time — all values are compile-time constants.
#[must_use]
pub fn sourcegraph_style() -> BM25FConfig {
    BM25FConfig {
        k1: 1.2,
        // TypeDefinition=0, FunctionSignature=1, SymbolName=2, ImportExport=3
        // FunctionBody=4, Comment=5, StringLiteral=6, Other=7
        field_boosts: [3.0, 3.0, 3.0, 3.0, 1.0, 1.0, 1.0, 1.0],
        field_b: [0.75; FIELD_COUNT],
    }
}

/// Default 8-field configuration from BM25FConfig::default().
///
/// TypeDef=5.0, FnSig=4.0, Symbol=3.5, Import=3.0, FnBody=1.0, Comment=0.8,
/// StringLit=0.5, Other=1.0.
#[must_use]
pub fn default_8field() -> BM25FConfig {
    BM25FConfig::default()
}

/// Custom tuned configuration with explicit parameters.
///
/// # Errors
///
/// Returns an error string if the parameters violate BM25F invariants:
/// - `k1` must be finite and >= 0.0
/// - each `boosts[i]` must be finite and >= 0.0
/// - each `b[i]` must be finite and in [0.0, 1.0]
pub fn tuned_8field(
    k1: f32,
    boosts: [f32; FIELD_COUNT],
    b: [f32; FIELD_COUNT],
) -> Result<BM25FConfig, rskim_search::SearchError> {
    let cfg = BM25FConfig {
        k1,
        field_boosts: boosts,
        field_b: b,
    };
    cfg.validate()?;
    Ok(cfg)
}

/// All named configurations in benchmark order.
///
/// The returned Vec has names paired with configs for labeling results.
#[must_use]
pub fn all_named() -> Vec<(&'static str, BM25FConfig)> {
    vec![
        ("uniform", uniform()),
        ("sourcegraph_style", sourcegraph_style()),
        ("default_8field", default_8field()),
    ]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code — unwrap acceptable for test assertions
mod tests {
    use super::*;

    #[test]
    fn uniform_validates() {
        uniform().validate().unwrap();
    }

    #[test]
    fn uniform_all_boosts_equal() {
        let cfg = uniform();
        assert!(
            cfg.field_boosts
                .iter()
                .all(|&b| (b - 1.0).abs() < f32::EPSILON)
        );
        assert!(cfg.field_b.iter().all(|&b| (b - 0.75).abs() < f32::EPSILON));
    }

    #[test]
    fn sourcegraph_style_validates() {
        sourcegraph_style().validate().unwrap();
    }

    #[test]
    fn sourcegraph_style_structural_boosted() {
        let cfg = sourcegraph_style();
        // Structural fields (0-3) should be 3.0
        for i in 0..4 {
            assert!(
                (cfg.field_boosts[i] - 3.0).abs() < f32::EPSILON,
                "field_boosts[{i}] should be 3.0"
            );
        }
        // Implementation fields (4-7) should be 1.0
        for i in 4..8 {
            assert!(
                (cfg.field_boosts[i] - 1.0).abs() < f32::EPSILON,
                "field_boosts[{i}] should be 1.0"
            );
        }
    }

    #[test]
    fn default_8field_validates() {
        default_8field().validate().unwrap();
    }

    #[test]
    fn tuned_8field_valid_params() {
        let cfg = tuned_8field(1.5, [2.0; FIELD_COUNT], [0.5; FIELD_COUNT]).unwrap();
        assert!((cfg.k1 - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn tuned_8field_invalid_k1() {
        let result = tuned_8field(-1.0, [1.0; FIELD_COUNT], [0.75; FIELD_COUNT]);
        assert!(result.is_err(), "negative k1 should be rejected");
    }

    #[test]
    fn tuned_8field_invalid_b_out_of_range() {
        let mut b = [0.75; FIELD_COUNT];
        b[3] = 1.5; // out of [0, 1]
        let result = tuned_8field(1.2, [1.0; FIELD_COUNT], b);
        assert!(result.is_err(), "b > 1.0 should be rejected");
    }

    #[test]
    fn tuned_8field_invalid_negative_boost() {
        let mut boosts = [1.0; FIELD_COUNT];
        boosts[2] = -0.5;
        let result = tuned_8field(1.2, boosts, [0.75; FIELD_COUNT]);
        assert!(result.is_err(), "negative boost should be rejected");
    }

    #[test]
    fn all_named_returns_three_entries() {
        let configs = all_named();
        assert_eq!(configs.len(), 3);
    }

    #[test]
    fn all_named_all_validate() {
        for (name, cfg) in all_named() {
            cfg.validate()
                .unwrap_or_else(|e| panic!("{name} config failed validate(): {e}"));
        }
    }
}
