//! Tests for AST n-gram newtypes and vocabulary/weight helpers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rskim_core::Language;

use super::*;
use crate::ast_weights::RUST_AST_BIGRAM_WEIGHTS;

// ── T1: AstBigram encode/decode roundtrip ────────────────────────────────────

#[test]
fn bigram_roundtrip_zero() {
    let bg = AstBigram::encode(0, 0);
    assert_eq!(bg.decode(), (0, 0));
}

#[test]
fn bigram_roundtrip_one() {
    let bg = AstBigram::encode(1, 1);
    assert_eq!(bg.decode(), (1, 1));
}

#[test]
fn bigram_roundtrip_max() {
    let bg = AstBigram::encode(u16::MAX, u16::MAX);
    assert_eq!(bg.decode(), (u16::MAX, u16::MAX));
}

#[test]
fn bigram_roundtrip_asymmetric() {
    let bg = AstBigram::encode(0, u16::MAX);
    assert_eq!(bg.decode(), (0, u16::MAX));

    let bg2 = AstBigram::encode(u16::MAX, 0);
    assert_eq!(bg2.decode(), (u16::MAX, 0));
}

#[test]
fn bigram_roundtrip_typical_ids() {
    // Typical vocab IDs in range [1, 1740]
    for parent in [1u16, 42, 154, 293, 500, 1000, 1740] {
        for child in [1u16, 10, 154, 293, 800, 1740] {
            let bg = AstBigram::encode(parent, child);
            assert_eq!(
                bg.decode(),
                (parent, child),
                "roundtrip failed for ({parent},{child})"
            );
        }
    }
}

// ── T2: AstTrigram encode/decode roundtrip ────────────────────────────────────

#[test]
fn trigram_roundtrip_zero() {
    let tg = AstTrigram::encode(0, 0, 0);
    assert_eq!(tg.decode(), (0, 0, 0));
}

#[test]
fn trigram_roundtrip_max() {
    let tg = AstTrigram::encode(u16::MAX, u16::MAX, u16::MAX);
    assert_eq!(tg.decode(), (u16::MAX, u16::MAX, u16::MAX));
}

#[test]
fn trigram_roundtrip_asymmetric() {
    let tg = AstTrigram::encode(0, 0, u16::MAX);
    assert_eq!(tg.decode(), (0, 0, u16::MAX));

    let tg2 = AstTrigram::encode(u16::MAX, 0, 0);
    assert_eq!(tg2.decode(), (u16::MAX, 0, 0));

    let tg3 = AstTrigram::encode(0, u16::MAX, 0);
    assert_eq!(tg3.decode(), (0, u16::MAX, 0));
}

#[test]
fn trigram_roundtrip_typical_ids() {
    for gp in [1u16, 42, 154] {
        for parent in [1u16, 293, 500] {
            for child in [53u16, 100, 800] {
                let tg = AstTrigram::encode(gp, parent, child);
                assert_eq!(
                    tg.decode(),
                    (gp, parent, child),
                    "roundtrip failed for ({gp},{parent},{child})"
                );
            }
        }
    }
}

// ── T3: key() matches encoding formula ───────────────────────────────────────

#[test]
fn bigram_key_matches_formula() {
    assert_eq!(
        AstBigram::encode(1, 2).key(),
        (1u32 << 16) | 2u32,
        "bigram key formula mismatch"
    );
}

#[test]
fn bigram_key_formula_boundary_values() {
    assert_eq!(AstBigram::encode(0, 0).key(), 0u32);
    assert_eq!(
        AstBigram::encode(u16::MAX, u16::MAX).key(),
        (u32::from(u16::MAX) << 16) | u32::from(u16::MAX)
    );
    assert_eq!(AstBigram::encode(0, u16::MAX).key(), u32::from(u16::MAX));
    assert_eq!(
        AstBigram::encode(u16::MAX, 0).key(),
        u32::from(u16::MAX) << 16
    );
}

#[test]
fn trigram_key_matches_formula() {
    assert_eq!(
        AstTrigram::encode(1, 2, 3).key(),
        (1u64 << 32) | (2u64 << 16) | 3u64,
        "trigram key formula mismatch"
    );
}

#[test]
fn trigram_key_formula_boundary_values() {
    assert_eq!(AstTrigram::encode(0, 0, 0).key(), 0u64);
    assert_eq!(
        AstTrigram::encode(u16::MAX, u16::MAX, u16::MAX).key(),
        (u64::from(u16::MAX) << 32) | (u64::from(u16::MAX) << 16) | u64::from(u16::MAX)
    );
}

// ── T4: from_raw() roundtrip ──────────────────────────────────────────────────

#[test]
fn bigram_from_raw_roundtrip() {
    assert_eq!(AstBigram::from_raw(0), AstBigram::encode(0, 0));
    let bg = AstBigram::encode(42, 99);
    assert_eq!(AstBigram::from_raw(bg.key()), bg);
}

#[test]
fn trigram_from_raw_roundtrip() {
    assert_eq!(AstTrigram::from_raw(0), AstTrigram::encode(0, 0, 0));
    let tg = AstTrigram::encode(10, 20, 30);
    assert_eq!(AstTrigram::from_raw(tg.key()), tg);
}

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

// ── T6: vocab_lookup ──────────────────────────────────────────────────────────

#[test]
fn vocab_lookup_known_kinds_found() {
    assert!(
        vocab_lookup("identifier").is_some(),
        "'identifier' must be in vocabulary"
    );
    assert!(
        vocab_lookup("function_item").is_some(),
        "'function_item' must be in vocabulary"
    );
}

#[test]
fn vocab_lookup_nonexistent_returns_none() {
    assert_eq!(vocab_lookup("NONEXISTENT_KIND_XYZ"), None);
}

#[test]
fn vocab_lookup_sentinel_empty_string() {
    assert_eq!(
        vocab_lookup(""),
        Some(0),
        "empty string must resolve to ID 0"
    );
}

// ── T7: vocab_resolve ─────────────────────────────────────────────────────────

#[test]
fn vocab_resolve_zero_is_sentinel() {
    assert_eq!(
        vocab_resolve(0),
        Some(""),
        "ID 0 must resolve to empty string sentinel"
    );
}

#[test]
fn vocab_resolve_roundtrip() {
    let id = vocab_lookup("identifier").expect("identifier in vocab");
    assert_eq!(vocab_resolve(id), Some("identifier"));
}

#[test]
fn vocab_resolve_out_of_bounds_returns_none() {
    assert_eq!(vocab_resolve(u16::MAX), None);
}

#[test]
fn vocab_resolve_and_lookup_are_inverses() {
    for kind in [
        "abstract_type",
        "bounded_type",
        "function_item",
        "source_file",
    ] {
        if let Some(id) = vocab_lookup(kind) {
            assert_eq!(
                vocab_resolve(id),
                Some(kind),
                "roundtrip failed for {kind:?}"
            );
        }
    }
}

// ── T8: vocab_len ─────────────────────────────────────────────────────────────

#[test]
fn vocab_len_nonzero_and_fits_in_u16() {
    let len = vocab_len();
    assert!(len > 0, "vocabulary must not be empty");
    assert!(
        len < u16::MAX as usize,
        "vocabulary length {len} must fit in u16"
    );
}

// ── T9: ast_bigram_idf returns weight > DEFAULT for known Rust bigram ─────────

#[test]
fn bigram_idf_known_rust_entry_above_default() {
    // First entry in RUST_AST_BIGRAM_WEIGHTS: "abstract_type" -> "bounded_type"
    let parent = vocab_lookup("abstract_type").expect("abstract_type in vocab");
    let child = vocab_lookup("bounded_type").expect("bounded_type in vocab");
    let bg = AstBigram::encode(parent, child);
    let w = ast_bigram_idf(Language::Rust, bg);
    assert!(
        w > DEFAULT_AST_WEIGHT,
        "known Rust bigram must have weight > DEFAULT_AST_WEIGHT ({DEFAULT_AST_WEIGHT}), got {w}"
    );
}

// ── T9b: ast_bigram_idf returns weight > DEFAULT for known TypeScript bigram ───

#[test]
fn bigram_idf_known_typescript_entry_above_default() {
    // First entry in TYPESCRIPT_AST_BIGRAM_WEIGHTS: "abstract_class_declaration" -> "abstract"
    let parent = vocab_lookup("abstract_class_declaration").expect("abstract_class_declaration in vocab");
    let child = vocab_lookup("abstract").expect("abstract in vocab");
    let bg = AstBigram::encode(parent, child);
    let w = ast_bigram_idf(Language::TypeScript, bg);
    assert!(
        w > DEFAULT_AST_WEIGHT,
        "known TypeScript bigram must have weight > DEFAULT_AST_WEIGHT ({DEFAULT_AST_WEIGHT}), got {w}"
    );
}

// ── T10: ast_bigram_idf fallback for unknown bigram ───────────────────────────

#[test]
fn bigram_idf_unknown_bigram_returns_default() {
    let bg = AstBigram::encode(u16::MAX, u16::MAX);
    assert_eq!(
        ast_bigram_idf(Language::Rust, bg),
        DEFAULT_AST_WEIGHT,
        "unknown bigram must return DEFAULT_AST_WEIGHT"
    );
}

// ── T11: ast_bigram_idf fallback for non-tree-sitter languages ────────────────

#[test]
fn bigram_idf_non_treesitter_languages_return_default() {
    let bg = AstBigram::encode(1, 2);
    for lang in [Language::Json, Language::Yaml, Language::Toml] {
        assert_eq!(
            ast_bigram_idf(lang, bg),
            DEFAULT_AST_WEIGHT,
            "{} has no AST weight table — must return DEFAULT_AST_WEIGHT",
            lang.name()
        );
    }
}

// ── T12: ast_trigram_idf parallel tests ──────────────────────────────────────

#[test]
fn trigram_idf_known_rust_entry_above_default() {
    // First entry in RUST_AST_TRIGRAM_WEIGHTS: "abstract_type" -> "bounded_type" -> "+"
    let gp = vocab_lookup("abstract_type").expect("abstract_type in vocab");
    let parent = vocab_lookup("bounded_type").expect("bounded_type in vocab");
    let child = vocab_lookup("+").expect("'+' in vocab");
    let tg = AstTrigram::encode(gp, parent, child);
    let w = ast_trigram_idf(Language::Rust, tg);
    assert!(
        w > DEFAULT_AST_WEIGHT,
        "known Rust trigram must have weight > DEFAULT_AST_WEIGHT ({DEFAULT_AST_WEIGHT}), got {w}"
    );
}

#[test]
fn trigram_idf_unknown_trigram_returns_default() {
    let tg = AstTrigram::encode(u16::MAX, u16::MAX, u16::MAX);
    assert_eq!(
        ast_trigram_idf(Language::Rust, tg),
        DEFAULT_AST_WEIGHT,
        "unknown trigram must return DEFAULT_AST_WEIGHT"
    );
}

#[test]
fn trigram_idf_non_treesitter_languages_return_default() {
    let tg = AstTrigram::encode(1, 2, 3);
    for lang in [Language::Json, Language::Yaml, Language::Toml] {
        assert_eq!(
            ast_trigram_idf(lang, tg),
            DEFAULT_AST_WEIGHT,
            "{} has no AST trigram table — must return DEFAULT_AST_WEIGHT",
            lang.name()
        );
    }
}

// ── T13: Encoding consistency with weight table entries ───────────────────────

#[test]
fn bigram_encoding_consistent_with_weight_table() {
    // Verify that our encode() produces the same u32 key as stored in the table.
    let (expected_key, _weight) = RUST_AST_BIGRAM_WEIGHTS[0];
    let parent_id = (expected_key >> 16) as u16;
    let child_id = (expected_key & 0xFFFF) as u16;
    assert_eq!(
        AstBigram::encode(parent_id, child_id).key(),
        expected_key,
        "encode() must produce the same key as stored in RUST_AST_BIGRAM_WEIGHTS[0]"
    );
}

#[test]
fn bigram_from_raw_consistent_with_encode() {
    // from_raw(encode(a,b).key()) must equal encode(a,b).
    let bg = AstBigram::encode(154, 293);
    assert_eq!(AstBigram::from_raw(bg.key()), bg);
}

// ── T14: Ordering semantics ───────────────────────────────────────────────────

/// Bigrams are parent-major: a higher parent always sorts after a lower parent,
/// regardless of the child component.
#[test]
fn bigram_ordering_is_parent_major() {
    let low_parent = AstBigram::encode(1, u16::MAX); // parent=1, child=MAX
    let high_parent = AstBigram::encode(2, 0); // parent=2, child=0
    assert!(
        high_parent > low_parent,
        "bigrams must sort parent-major: encode(2,0) > encode(1, MAX)"
    );
}

/// Trigrams are grandparent-major: a higher grandparent always sorts last,
/// then parent, then child.
#[test]
fn trigram_ordering_is_grandparent_major() {
    let low_gp = AstTrigram::encode(1, u16::MAX, u16::MAX); // gp=1, rest=MAX
    let high_gp = AstTrigram::encode(2, 0, 0); // gp=2, rest=0
    assert!(
        high_gp > low_gp,
        "trigrams must sort grandparent-major: encode(2,0,0) > encode(1,MAX,MAX)"
    );
}

/// Within the same parent, bigrams sort by child.
#[test]
fn bigram_ordering_child_tiebreak() {
    let small_child = AstBigram::encode(5, 1);
    let large_child = AstBigram::encode(5, 100);
    assert!(
        large_child > small_child,
        "bigrams with equal parent must sort by child"
    );
}

// ── T16: DEFAULT_AST_WEIGHT constant value ────────────────────────────────────

#[test]
fn default_ast_weight_is_one() {
    assert_eq!(DEFAULT_AST_WEIGHT, 1.0_f32);
}
