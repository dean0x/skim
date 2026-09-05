//! Unit tests for `Page` (AD-404-1/2/3) and `TemporalSort` name methods.
//!
//! These tests exercise `Page` and `TemporalSort` directly — not transitively
//! through higher-level query or AST plumbing.  Each test maps to a named
//! behaviour from the doc comment on the type so the coverage story is easy to
//! audit.

#![allow(clippy::unwrap_used)]

use super::{Page, TemporalSort};

// ============================================================================
// Page::new — None-to-0 defaulting (AD-404-1)
// ============================================================================

/// `None` offset is normalised to 0 — the default when the CLI flag is absent.
#[test]
fn new_none_offset_defaults_to_zero() {
    let p = Page::new(10, None);
    assert_eq!(p.offset(), 0, "None offset must default to 0");
    assert_eq!(p.limit(), 10);
}

/// `Some(k)` offset is preserved exactly.
#[test]
fn new_some_offset_preserved() {
    let p = Page::new(20, Some(7));
    assert_eq!(p.offset(), 7);
    assert_eq!(p.limit(), 20);
}

/// `Page::first(n)` is equivalent to `Page::new(n, None)`.
#[test]
fn first_is_zero_offset() {
    let a = Page::first(5);
    let b = Page::new(5, None);
    assert_eq!(a, b);
}

// ============================================================================
// Page::depth — saturating_add (AD-404-2)
// ============================================================================

/// Normal case: `depth() == limit + offset` when no overflow occurs.
#[test]
fn depth_normal() {
    let p = Page::new(10, Some(3));
    assert_eq!(p.depth(), 13);
}

/// When offset is 0, `depth() == limit` — zero-regression property.
#[test]
fn depth_zero_offset_equals_limit() {
    let p = Page::new(25, None);
    assert_eq!(p.depth(), 25, "depth must equal limit when offset is 0");
}

/// Saturating-add: `limit = usize::MAX, offset = 1` must not overflow.
#[test]
fn depth_saturates_at_usize_max() {
    let p = Page::new(usize::MAX, Some(1));
    assert_eq!(
        p.depth(),
        usize::MAX,
        "depth() must saturate rather than overflow"
    );
}

/// Saturating-add: both operands near max still saturate safely.
#[test]
fn depth_both_large_saturates() {
    let p = Page::new(usize::MAX / 2 + 1, Some(usize::MAX / 2 + 1));
    assert_eq!(p.depth(), usize::MAX);
}

// ============================================================================
// Page::apply — skip-then-take (AD-404-3)
// ============================================================================

/// Zero offset: apply only truncates, no drain.
#[test]
fn apply_zero_offset_truncates_only() {
    let mut rows = vec![0u32, 1, 2, 3, 4];
    Page::new(3, None).apply(&mut rows);
    assert_eq!(rows, [0, 1, 2]);
}

/// Normal skip-then-take: offset=2, limit=2 on a 5-element vec.
#[test]
fn apply_skip_then_take() {
    let mut rows = vec![10u32, 20, 30, 40, 50];
    Page::new(2, Some(2)).apply(&mut rows);
    assert_eq!(rows, [30, 40]);
}

/// When offset == len, the drain saturates and result is empty.
#[test]
fn apply_offset_equals_len_returns_empty() {
    let mut rows = vec![1u32, 2, 3];
    Page::new(5, Some(3)).apply(&mut rows);
    assert!(rows.is_empty(), "all rows skipped → empty result");
}

/// When offset > len, the drain saturates at len — no panic, empty result.
#[test]
fn apply_offset_exceeds_len_saturates() {
    let mut rows = vec![1u32, 2];
    Page::new(10, Some(100)).apply(&mut rows);
    assert!(
        rows.is_empty(),
        "offset beyond vec length must not panic or skip partial"
    );
}

/// Remaining elements after skip fit within limit — no truncation needed.
#[test]
fn apply_limit_larger_than_remaining_after_skip() {
    let mut rows = vec![0u32, 1, 2, 3, 4];
    // skip 3, limit 10 — only 2 remain (< limit), so all are kept
    Page::new(10, Some(3)).apply(&mut rows);
    assert_eq!(rows, [3, 4]);
}

/// Limit 0: apply always produces an empty result regardless of offset.
#[test]
fn apply_zero_limit_always_empty() {
    let mut rows = vec![1u32, 2, 3];
    Page::new(0, None).apply(&mut rows);
    assert!(rows.is_empty());

    let mut rows2 = vec![1u32, 2, 3];
    Page::new(0, Some(1)).apply(&mut rows2);
    assert!(rows2.is_empty());
}

/// apply on an already-empty vec is a no-op (no panic).
#[test]
fn apply_empty_vec_is_noop() {
    let mut rows: Vec<u32> = vec![];
    Page::new(5, Some(3)).apply(&mut rows);
    assert!(rows.is_empty());
}

// ============================================================================
// TemporalSort::json_name / flag_name — RD-5, AC-4, AC-7
// ============================================================================

/// `json_name()` must return the bare form (no `--` prefix) for every variant.
///
/// The bare form is required by `DegradedJson.requested` (AC-4 / AC-7 / RD-5).
/// A one-token regression at either call site in mod.rs (using `flag_name()`
/// instead of `json_name()`) would emit `"--hot"` instead of `"hot"` and the
/// integration tests that do NOT assert `requested` would not catch it.
#[test]
fn temporal_sort_json_name_is_bare_no_dashes() {
    assert_eq!(TemporalSort::Hot.json_name(), "hot");
    assert_eq!(TemporalSort::Cold.json_name(), "cold");
    assert_eq!(TemporalSort::Risky.json_name(), "risky");
}

/// `flag_name()` must return the `--`-prefixed form for every variant.
///
/// Human-readable notices and stderr messages use the dashed form.  A
/// regression here would emit `"hot"` in the message text instead of `"--hot"`.
#[test]
fn temporal_sort_flag_name_has_double_dash_prefix() {
    assert_eq!(TemporalSort::Hot.flag_name(), "--hot");
    assert_eq!(TemporalSort::Cold.flag_name(), "--cold");
    assert_eq!(TemporalSort::Risky.flag_name(), "--risky");
}

/// `json_name()` and `flag_name()` must never return the same string for any
/// variant — the two forms are deliberately distinct.
#[test]
fn temporal_sort_json_name_differs_from_flag_name() {
    for sort in [TemporalSort::Hot, TemporalSort::Cold, TemporalSort::Risky] {
        assert_ne!(
            sort.json_name(),
            sort.flag_name(),
            "json_name and flag_name must be distinct for {sort:?}"
        );
    }
}
