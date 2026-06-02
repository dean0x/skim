//! T5: Display formatting tests for AstBigram and AstTrigram.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

// ── T5: Display formatting ────────────────────────────────────────────────────

#[test]
fn bigram_display_known_ids() {
    let id_ident = vocab_lookup("identifier").expect("identifier in vocab");
    let id_fn = vocab_lookup("function_item").expect("function_item in vocab");
    let s = AstBigram::encode(id_ident, id_fn).to_string();
    assert!(s.contains("identifier"), "got: {s}");
    assert!(s.contains("function_item"), "got: {s}");
    assert!(s.contains(" > "), "must contain ' > ' separator, got: {s}");
}

#[test]
fn bigram_display_unknown_ids_use_fallback() {
    let s = AstBigram::encode(u16::MAX, u16::MAX).to_string();
    assert!(
        s.contains("?65535"),
        "out-of-bounds ID must display as '?65535', got: {s}"
    );
}

#[test]
fn bigram_display_sentinel_id_zero() {
    let s = AstBigram::encode(0, 0).to_string();
    assert!(
        s.contains("<unknown>"),
        "sentinel ID 0 must display as '<unknown>', got: {s}"
    );
}

#[test]
fn trigram_display_known_ids() {
    let id_ident = vocab_lookup("identifier").expect("identifier in vocab");
    let id_fn = vocab_lookup("function_item").expect("function_item in vocab");
    let id_src = vocab_lookup("source_file").expect("source_file in vocab");
    let s = AstTrigram::encode(id_src, id_fn, id_ident).to_string();
    assert!(s.contains("source_file"), "got: {s}");
    assert!(s.contains("function_item"), "got: {s}");
    assert!(s.contains("identifier"), "got: {s}");
    assert_eq!(
        s.matches(" > ").count(),
        2,
        "trigram must have 2 ' > ' separators, got: {s}"
    );
}

#[test]
fn trigram_display_unknown_ids_use_fallback() {
    let s = AstTrigram::encode(u16::MAX, u16::MAX, u16::MAX).to_string();
    assert!(
        s.contains("?65535"),
        "out-of-bounds must use '?65535' fallback, got: {s}"
    );
}
