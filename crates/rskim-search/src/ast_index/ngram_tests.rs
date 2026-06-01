//! Tests for AST n-gram newtypes and vocabulary/weight helpers.
//!
//! Test cycles:
//!   T1  AstBigram encode/decode roundtrip
//!   T2  AstTrigram encode/decode roundtrip
//!   T3  key() matches encoding formula
//!   T4  from_raw() roundtrip
//!   T5  Display formatting
//!   T6  vocab_lookup
//!   T7  vocab_resolve
//!   T8  vocab_len
//!   T9  ast_bigram_idf returns weight > DEFAULT for known Rust bigram
//!   T10 ast_bigram_idf fallback for unknown bigram
//!   T11 ast_bigram_idf fallback for non-tree-sitter languages
//!   T12 ast_trigram_idf parallel tests
//!   T13 Encoding consistency with weight table entries
//!   T14 DEFAULT_AST_WEIGHT constant value

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
    let original = AstBigram::encode(42, 99);
    let reconstructed = AstBigram::from_raw(original.key());
    assert_eq!(reconstructed, original);
}

#[test]
fn bigram_from_raw_zero() {
    assert_eq!(AstBigram::from_raw(0), AstBigram::encode(0, 0));
}

#[test]
fn trigram_from_raw_roundtrip() {
    let original = AstTrigram::encode(10, 20, 30);
    let reconstructed = AstTrigram::from_raw(original.key());
    assert_eq!(reconstructed, original);
}

#[test]
fn trigram_from_raw_zero() {
    assert_eq!(AstTrigram::from_raw(0), AstTrigram::encode(0, 0, 0));
}

// ── T5: Display formatting ────────────────────────────────────────────────────

#[test]
fn bigram_display_known_ids() {
    // "identifier" and "function_item" are known Rust vocab entries.
    let id_identifier = vocab_lookup("identifier").expect("identifier must be in vocab");
    let id_fn_item = vocab_lookup("function_item").expect("function_item must be in vocab");

    let bg = AstBigram::encode(id_identifier, id_fn_item);
    let s = bg.to_string();
    assert!(
        s.contains("identifier"),
        "Display must contain 'identifier', got: {s}"
    );
    assert!(
        s.contains("function_item"),
        "Display must contain 'function_item', got: {s}"
    );
    assert!(
        s.contains(" > "),
        "Display must contain ' > ' separator, got: {s}"
    );
}

#[test]
fn bigram_display_unknown_ids_use_fallback() {
    // u16::MAX is far beyond vocabulary length — both IDs are out-of-bounds.
    let bg = AstBigram::encode(u16::MAX, u16::MAX);
    let s = bg.to_string();
    // Both sides should use the "?{id}" fallback.
    assert!(
        s.contains(&format!("?{}", u16::MAX)),
        "out-of-bounds ID must display as '?65535', got: {s}"
    );
}

#[test]
fn bigram_display_sentinel_id_zero() {
    // ID 0 maps to "" (sentinel). Display should render it as "<unknown>".
    let bg = AstBigram::encode(0, 0);
    let s = bg.to_string();
    assert!(
        s.contains("<unknown>"),
        "sentinel ID 0 must display as '<unknown>', got: {s}"
    );
}

#[test]
fn trigram_display_known_ids() {
    let id_identifier = vocab_lookup("identifier").expect("identifier in vocab");
    let id_fn_item = vocab_lookup("function_item").expect("function_item in vocab");
    let id_source = vocab_lookup("source_file").expect("source_file in vocab");

    let tg = AstTrigram::encode(id_source, id_fn_item, id_identifier);
    let s = tg.to_string();
    assert!(s.contains("source_file"), "got: {s}");
    assert!(s.contains("function_item"), "got: {s}");
    assert!(s.contains("identifier"), "got: {s}");
    // Two " > " separators expected
    let sep_count = s.matches(" > ").count();
    assert_eq!(
        sep_count, 2,
        "trigram Display must have 2 ' > ' separators, got: {s}"
    );
}

#[test]
fn trigram_display_unknown_ids_use_fallback() {
    let tg = AstTrigram::encode(u16::MAX, u16::MAX, u16::MAX);
    let s = tg.to_string();
    assert!(
        s.contains(&format!("?{}", u16::MAX)),
        "out-of-bounds ID must use '?65535' fallback, got: {s}"
    );
}

// ── T6: vocab_lookup ──────────────────────────────────────────────────────────

#[test]
fn vocab_lookup_identifier_found() {
    assert!(
        vocab_lookup("identifier").is_some(),
        "'identifier' must be in vocabulary"
    );
}

#[test]
fn vocab_lookup_function_item_found() {
    assert!(
        vocab_lookup("function_item").is_some(),
        "'function_item' must be in vocabulary"
    );
}

#[test]
fn vocab_lookup_nonexistent_returns_none() {
    assert_eq!(
        vocab_lookup("NONEXISTENT_KIND_XYZ"),
        None,
        "nonsense kind must not be in vocabulary"
    );
}

#[test]
fn vocab_lookup_sentinel_empty_string() {
    // ID 0 is the sentinel "" entry.
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
    assert_eq!(
        vocab_resolve(id),
        Some("identifier"),
        "vocab_resolve(vocab_lookup('identifier')) must equal 'identifier'"
    );
}

#[test]
fn vocab_resolve_out_of_bounds_returns_none() {
    assert_eq!(
        vocab_resolve(u16::MAX),
        None,
        "ID u16::MAX must be out of bounds and return None"
    );
}

#[test]
fn vocab_resolve_and_lookup_are_inverses() {
    // Spot-check several known kinds.
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
                "vocab_resolve(vocab_lookup({kind:?})) must equal {kind:?}"
            );
        }
    }
}

// ── T8: vocab_len ─────────────────────────────────────────────────────────────

#[test]
fn vocab_len_is_nonzero() {
    assert!(vocab_len() > 0, "vocabulary must not be empty");
}

#[test]
fn vocab_len_fits_in_u16() {
    assert!(
        vocab_len() < u16::MAX as usize,
        "vocabulary length {} must fit in u16 (< {})",
        vocab_len(),
        u16::MAX
    );
}

// ── T9: ast_bigram_idf returns weight > DEFAULT for known Rust bigram ─────────

#[test]
fn bigram_idf_known_rust_entry_above_default() {
    // First entry in RUST_AST_BIGRAM_WEIGHTS:
    //   (0x009A0125, 11.251047) → "abstract_type"(154) -> "bounded_type"(293)
    let parent = vocab_lookup("abstract_type").expect("abstract_type in vocab");
    let child = vocab_lookup("bounded_type").expect("bounded_type in vocab");
    let bg = AstBigram::encode(parent, child);
    let w = ast_bigram_idf(Language::Rust, bg);
    assert!(
        w > DEFAULT_AST_WEIGHT,
        "known Rust bigram must have weight > DEFAULT_AST_WEIGHT ({DEFAULT_AST_WEIGHT}), got {w}"
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
fn bigram_idf_json_returns_default() {
    let bg = AstBigram::encode(1, 2);
    assert_eq!(
        ast_bigram_idf(Language::Json, bg),
        DEFAULT_AST_WEIGHT,
        "JSON has no AST weight table — must return DEFAULT_AST_WEIGHT"
    );
}

#[test]
fn bigram_idf_yaml_returns_default() {
    let bg = AstBigram::encode(1, 2);
    assert_eq!(
        ast_bigram_idf(Language::Yaml, bg),
        DEFAULT_AST_WEIGHT,
        "YAML has no AST weight table — must return DEFAULT_AST_WEIGHT"
    );
}

#[test]
fn bigram_idf_toml_returns_default() {
    let bg = AstBigram::encode(1, 2);
    assert_eq!(
        ast_bigram_idf(Language::Toml, bg),
        DEFAULT_AST_WEIGHT,
        "TOML has no AST weight table — must return DEFAULT_AST_WEIGHT"
    );
}

// ── T12: ast_trigram_idf parallel tests ──────────────────────────────────────

#[test]
fn trigram_idf_known_rust_entry_above_default() {
    // First entry in RUST_AST_TRIGRAM_WEIGHTS:
    //   (0x0000009A01250035, 11.251047) → "abstract_type"(154) -> "bounded_type"(293) -> "+"(53)
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
fn trigram_idf_json_returns_default() {
    let tg = AstTrigram::encode(1, 2, 3);
    assert_eq!(ast_trigram_idf(Language::Json, tg), DEFAULT_AST_WEIGHT);
}

#[test]
fn trigram_idf_yaml_returns_default() {
    let tg = AstTrigram::encode(1, 2, 3);
    assert_eq!(ast_trigram_idf(Language::Yaml, tg), DEFAULT_AST_WEIGHT);
}

#[test]
fn trigram_idf_toml_returns_default() {
    let tg = AstTrigram::encode(1, 2, 3);
    assert_eq!(ast_trigram_idf(Language::Toml, tg), DEFAULT_AST_WEIGHT);
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

// ── T14: DEFAULT_AST_WEIGHT constant value ────────────────────────────────────

#[test]
fn default_ast_weight_is_one() {
    assert_eq!(DEFAULT_AST_WEIGHT, 1.0_f32);
}
