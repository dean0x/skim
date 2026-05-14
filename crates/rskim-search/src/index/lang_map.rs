//! Stable mapping between [`rskim_core::Language`] variants and their 1-byte
//! on-disk IDs.
//!
//! IDs are assigned in alphabetical order of the enum variant names and are
//! part of the stable on-disk format.  Adding a new language variant without
//! a format version bump is acceptable because [`lang_from_id`] returns `None`
//! for unknown IDs (graceful degradation).
//!
//! **Do not reorder or renumber existing entries** — that is a breaking change.

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "lang_map_tests.rs"]
mod tests;

/// Map a [`rskim_core::Language`] variant to a stable 1-byte ID.
#[must_use]
pub(crate) fn lang_to_id(lang: rskim_core::Language) -> u8 {
    match lang {
        rskim_core::Language::C => 0,
        rskim_core::Language::Cpp => 1,
        rskim_core::Language::CSharp => 2,
        rskim_core::Language::Go => 3,
        rskim_core::Language::Java => 4,
        rskim_core::Language::JavaScript => 5,
        rskim_core::Language::Json => 6,
        rskim_core::Language::Kotlin => 7,
        rskim_core::Language::Markdown => 8,
        rskim_core::Language::Python => 9,
        rskim_core::Language::Ruby => 10,
        rskim_core::Language::Rust => 11,
        rskim_core::Language::Sql => 12,
        rskim_core::Language::Swift => 13,
        rskim_core::Language::Toml => 14,
        rskim_core::Language::TypeScript => 15,
        rskim_core::Language::Yaml => 16,
    }
}

/// Recover a [`rskim_core::Language`] from its 1-byte index ID.
///
/// Returns `None` for IDs that don't correspond to any known language,
/// allowing the reader to degrade gracefully when opening indices written
/// by a newer version that supports additional languages.
#[must_use]
#[allow(dead_code)]
pub(crate) fn lang_from_id(id: u8) -> Option<rskim_core::Language> {
    match id {
        0 => Some(rskim_core::Language::C),
        1 => Some(rskim_core::Language::Cpp),
        2 => Some(rskim_core::Language::CSharp),
        3 => Some(rskim_core::Language::Go),
        4 => Some(rskim_core::Language::Java),
        5 => Some(rskim_core::Language::JavaScript),
        6 => Some(rskim_core::Language::Json),
        7 => Some(rskim_core::Language::Kotlin),
        8 => Some(rskim_core::Language::Markdown),
        9 => Some(rskim_core::Language::Python),
        10 => Some(rskim_core::Language::Ruby),
        11 => Some(rskim_core::Language::Rust),
        12 => Some(rskim_core::Language::Sql),
        13 => Some(rskim_core::Language::Swift),
        14 => Some(rskim_core::Language::Toml),
        15 => Some(rskim_core::Language::TypeScript),
        16 => Some(rskim_core::Language::Yaml),
        _ => None,
    }
}
