//! Unit tests for the `Page` value type (AD-404-1/2/3).
//!
//! These tests exercise `Page` directly — not transitively through higher-level
//! query or AST plumbing.  Each test maps to a named behaviour from the doc
//! comment on `Page` so the coverage story is easy to audit.

#![allow(clippy::unwrap_used)]

use super::Page;

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
