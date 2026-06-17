//! Tests for `compound::coupling` scaffold (AC8b).

use super::*;
use crate::types::FileId;

/// AC8(b): scaffold must compile, return 0.0 for any pair, and confirm
/// that the default weight is 0.0 (so it contributes nothing to fusion).
#[test]
fn test_scaffold_returns_zero() {
    assert_eq!(
        structural_coupling_score(FileId(0), FileId(1)),
        0.0,
        "scaffold must return 0.0 (AC8b: deferred, neutral score)"
    );
    assert_eq!(
        structural_coupling_score(FileId(42), FileId(99)),
        0.0,
        "scaffold must return 0.0 for any input"
    );
    assert_eq!(
        structural_coupling_score(FileId(0), FileId(0)),
        0.0,
        "scaffold must return 0.0 even for self-pair"
    );
}

/// Guard: confirm the default weight for structural_coupling is 0.0
/// so the scaffold phase never silently influences ranking.
#[test]
fn test_default_weight_is_zero() {
    use crate::compound::CompositeWeights6;
    let w = CompositeWeights6::default();
    assert_eq!(
        w.structural_coupling, 0.0,
        "structural_coupling default weight must be 0.0 (ADR-003 gated, AC8b)"
    );
}

/// Guard: no #NEW placeholder exists for the follow-up ticket.
/// The deferred ticket is #314 — referenced in the module doc comment.
/// This test verifies the module doc contains the real ticket number.
#[test]
fn test_no_new_placeholder_in_source() {
    // The deferred ticket number (#314) must be referenced and the source
    // must NOT contain the placeholder "#NEW".
    let source = include_str!("coupling.rs");
    assert!(
        !source.contains("#NEW"),
        "coupling.rs must not contain '#NEW' placeholder (AC8b requirement)"
    );
    assert!(
        source.contains("#314"),
        "coupling.rs must reference follow-up ticket #314 (AC8b requirement)"
    );
}
