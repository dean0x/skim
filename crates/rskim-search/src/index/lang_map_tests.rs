//! Tests for the language ID mapping (lang_map.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn test_lang_mapping_roundtrip() {
    let langs = [
        rskim_core::Language::C,
        rskim_core::Language::Cpp,
        rskim_core::Language::CSharp,
        rskim_core::Language::Go,
        rskim_core::Language::Java,
        rskim_core::Language::JavaScript,
        rskim_core::Language::Json,
        rskim_core::Language::Kotlin,
        rskim_core::Language::Markdown,
        rskim_core::Language::Python,
        rskim_core::Language::Ruby,
        rskim_core::Language::Rust,
        rskim_core::Language::Sql,
        rskim_core::Language::Swift,
        rskim_core::Language::Toml,
        rskim_core::Language::TypeScript,
        rskim_core::Language::Yaml,
    ];
    for lang in langs {
        let id = lang_to_id(lang);
        let recovered = lang_from_id(id);
        assert_eq!(
            recovered,
            Some(lang),
            "lang roundtrip failed for {lang:?}: id={id}"
        );
    }
}

#[test]
fn test_lang_from_id_unknown() {
    assert_eq!(lang_from_id(200), None);
    assert_eq!(lang_from_id(17), None);
    assert_eq!(lang_from_id(255), None);
}

#[test]
fn test_lang_ids_unique() {
    let langs = [
        rskim_core::Language::C,
        rskim_core::Language::Cpp,
        rskim_core::Language::CSharp,
        rskim_core::Language::Go,
        rskim_core::Language::Java,
        rskim_core::Language::JavaScript,
        rskim_core::Language::Json,
        rskim_core::Language::Kotlin,
        rskim_core::Language::Markdown,
        rskim_core::Language::Python,
        rskim_core::Language::Ruby,
        rskim_core::Language::Rust,
        rskim_core::Language::Sql,
        rskim_core::Language::Swift,
        rskim_core::Language::Toml,
        rskim_core::Language::TypeScript,
        rskim_core::Language::Yaml,
    ];
    let mut ids: Vec<u8> = langs.iter().map(|&l| lang_to_id(l)).collect();
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "lang IDs are not unique");
}
