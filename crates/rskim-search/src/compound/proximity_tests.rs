//! Tests for `compound::proximity` (AC7).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

// ============================================================================
// AC7 — Monotonicity
// ============================================================================

#[test]
fn test_deep_shared_prefix_scores_higher_than_mid() {
    // src/a/b/x.rs and src/a/b/y.rs share 3 path segments (src, a, b).
    let deep = dir_proximity_score("src/a/b/x.rs", "src/a/b/y.rs");
    // src/a/x.rs and src/a/q/y.rs share 2 segments (src, a).
    let mid = dir_proximity_score("src/a/x.rs", "src/a/q/y.rs");
    assert!(
        deep > mid,
        "deeper common prefix must score strictly higher: deep={deep}, mid={mid} (AC7)"
    );
}

#[test]
fn test_mid_shared_prefix_scores_higher_than_root_only() {
    // src/a/x.rs and src/a/q/y.rs share 2 segments.
    let mid = dir_proximity_score("src/a/x.rs", "src/a/q/y.rs");
    // src/x.rs and docs/y.md share 0 directory segments (different top-level dirs).
    let root_only = dir_proximity_score("src/x.rs", "docs/y.md");
    assert!(
        mid > root_only,
        "shared-prefix=2 must score higher than shared-prefix=0: mid={mid}, root_only={root_only}"
    );
}

#[test]
fn test_monotone_chain() {
    let deep = dir_proximity_score("src/a/b/x.rs", "src/a/b/y.rs");
    let mid = dir_proximity_score("src/a/x.rs", "src/a/q/y.rs");
    let root_only = dir_proximity_score("src/x.rs", "docs/y.md");
    assert!(
        deep > mid && mid > root_only,
        "monotone chain must hold: deep={deep} > mid={mid} > root_only={root_only} (AC7)"
    );
}

// ============================================================================
// AC7 — Degenerate inputs: no panic, always finite
// ============================================================================

#[test]
fn test_identical_paths() {
    let s = dir_proximity_score("src/auth.rs", "src/auth.rs");
    assert!(s.is_finite(), "identical paths must return finite score");
    // All components shared; score = n/(1+n) < 1.0.
    assert!(s > 0.0, "identical paths score must be > 0");
    assert!(s < 1.0, "identical paths score must be < 1.0");
}

#[test]
fn test_empty_path_a() {
    let s = dir_proximity_score("", "src/auth.rs");
    assert!(
        s.is_finite() && s >= 0.0,
        "empty path_a must return finite >= 0"
    );
    assert_eq!(s, 0.0, "no shared components → score 0");
}

#[test]
fn test_empty_path_b() {
    let s = dir_proximity_score("src/auth.rs", "");
    assert!(
        s.is_finite() && s >= 0.0,
        "empty path_b must return finite >= 0"
    );
}

#[test]
fn test_both_empty() {
    let s = dir_proximity_score("", "");
    assert_eq!(s, 0.0, "both empty must return 0.0");
}

#[test]
fn test_very_deep_paths_no_overflow() {
    // AC7: path-depth arithmetic must not overflow for unrealistically deep paths.
    let a: String = (0..300)
        .map(|i| format!("seg{i}"))
        .collect::<Vec<_>>()
        .join("/")
        + "/x.rs";
    let b: String = (0..300)
        .map(|i| format!("seg{i}"))
        .collect::<Vec<_>>()
        .join("/")
        + "/y.rs";
    let s = dir_proximity_score(&a, &b);
    assert!(
        s.is_finite(),
        "very deep paths must return finite score (AC7 overflow guard)"
    );
    assert!(
        s > 0.0 && s <= 1.0,
        "score in (0,1] for deeply-shared paths"
    );
}

#[test]
fn test_single_component_paths_no_shared() {
    let s = dir_proximity_score("foo.rs", "bar.rs");
    assert_eq!(
        s, 0.0,
        "different single-component paths share nothing → 0.0"
    );
}

#[test]
fn test_single_component_paths_identical() {
    let s = dir_proximity_score("foo.rs", "foo.rs");
    assert!(s.is_finite() && s > 0.0);
}

// ============================================================================
// Score range and ordering sanity
// ============================================================================

#[test]
fn test_score_in_zero_to_one_range() {
    let cases = [
        ("src/a/b/x.rs", "src/a/b/y.rs"),
        ("src/a/x.rs", "docs/y.md"),
        ("a.rs", "b.rs"),
        ("", ""),
    ];
    for (a, b) in cases {
        let s = dir_proximity_score(a, b);
        assert!(
            (0.0..=1.0).contains(&s),
            "score must be in [0,1]: {s} for ({a:?}, {b:?})"
        );
    }
}

#[test]
fn test_symmetry() {
    // score(a, b) must equal score(b, a).
    let a = "src/cmd/search/query.rs";
    let b = "src/cmd/temporal.rs";
    assert_eq!(
        dir_proximity_score(a, b),
        dir_proximity_score(b, a),
        "proximity score must be symmetric"
    );
}
