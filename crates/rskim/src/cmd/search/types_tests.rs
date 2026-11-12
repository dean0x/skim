//! Unit tests for `Page` (AD-404-1/2/3).
//!
//! These tests exercise `Page` directly — not transitively through
//! higher-level query plumbing.  Each test maps to a named behaviour from the
//! doc comment on the type so the coverage story is easy to audit.

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
