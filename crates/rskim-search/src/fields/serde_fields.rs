//! Field classification for serde-based data formats (JSON, YAML, TOML).
//!
//! These formats don't use tree-sitter, so classification operates on
//! parsed serde structures rather than AST nodes. Returns `(byte_range, SearchField)`
//! pairs for each semantically meaningful region.

use std::ops::Range;

use crate::SearchField;

/// Classify regions in JSON content into `SearchField` spans.
pub fn classify_json_fields(
    _source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    // Phase 1d: implement JSON field classification
    Ok(vec![])
}

/// Classify regions in YAML content into `SearchField` spans.
pub fn classify_yaml_fields(
    _source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    // Phase 1d: implement YAML field classification
    Ok(vec![])
}

/// Classify regions in TOML content into `SearchField` spans.
pub fn classify_toml_fields(
    _source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    // Phase 1d: implement TOML field classification
    Ok(vec![])
}
