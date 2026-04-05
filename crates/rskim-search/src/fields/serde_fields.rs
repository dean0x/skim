//! Field classification for serde-based data formats (JSON, YAML, TOML).
//!
//! These formats don't use tree-sitter, so classification operates on
//! parsed serde structures rather than AST nodes. Returns `(byte_range, SearchField)`
//! pairs for each semantically meaningful region.
//!
//! # JSON strategy
//! Parses with `serde_json`, walks the Value tree tracking byte positions via
//! substring search. Top-level keys → TypeDefinition, nested keys → SymbolName.
//!
//! # YAML / TOML strategy
//! Line-by-line scanning (no additional parser deps). Pattern matching on
//! indentation and key syntax determines the field type.

use std::ops::Range;

use crate::SearchField;

// ============================================================================
// JSON classification
// ============================================================================

/// Classify regions in JSON content into `SearchField` spans.
///
/// On parse error returns an empty vec (graceful degradation for search indexing).
pub fn classify_json_fields(
    source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    let value: serde_json::Value = match serde_json::from_str(source) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };

    let mut results = Vec::new();
    match &value {
        serde_json::Value::Object(map) => {
            classify_json_object(source, map, /* depth */ 0, &mut results);
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let serde_json::Value::Object(map) = item {
                    classify_json_object(source, map, 0, &mut results);
                }
            }
        }
        _ => {}
    }

    Ok(results)
}

/// Walk a JSON object at a given nesting depth, appending classified spans.
fn classify_json_object(
    source: &str,
    map: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
    out: &mut Vec<(Range<usize>, SearchField)>,
) {
    for (key, value) in map {
        // Classify the key.
        let key_field = if depth == 0 {
            SearchField::TypeDefinition
        } else {
            SearchField::SymbolName
        };

        // Locate the key string in source (search for `"key"`).
        // This is a best-effort linear scan — adequate for search indexing.
        let quoted_key = format!("\"{}\"", key);
        if let Some(pos) = source.find(quoted_key.as_str()) {
            out.push((pos..pos + quoted_key.len(), key_field));
        }

        // Classify the value.
        match value {
            serde_json::Value::String(s) => {
                let quoted_val = format!("\"{}\"", s);
                if let Some(pos) = source.find(quoted_val.as_str()) {
                    out.push((pos..pos + quoted_val.len(), SearchField::StringLiteral));
                }
            }
            serde_json::Value::Object(nested) => {
                classify_json_object(source, nested, depth + 1, out);
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    if let serde_json::Value::Object(nested) = item {
                        classify_json_object(source, nested, depth + 1, out);
                    } else if let serde_json::Value::String(s) = item {
                        let quoted_val = format!("\"{}\"", s);
                        if let Some(pos) = source.find(quoted_val.as_str()) {
                            out.push((
                                pos..pos + quoted_val.len(),
                                SearchField::StringLiteral,
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ============================================================================
// YAML classification
// ============================================================================

/// Classify regions in YAML content into `SearchField` spans.
///
/// Uses line-by-line scanning to avoid pulling in `serde_yaml_ng` as a dep.
/// Returns `(byte_range, SearchField)` pairs for meaningful regions.
pub fn classify_yaml_fields(
    source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    let mut results = Vec::new();
    let mut byte_offset: usize = 0;

    for line in source.lines() {
        let line_len = line.len();

        // Skip blank lines.
        if line.trim().is_empty() {
            // +1 for the newline character.
            byte_offset += line_len + 1;
            continue;
        }

        // Comment lines.
        if line.trim_start().starts_with('#') {
            results.push((byte_offset..byte_offset + line_len, SearchField::Comment));
            byte_offset += line_len + 1;
            continue;
        }

        // Detect YAML keys: `^<indent><identifier>:` pattern.
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if let Some(colon_pos) = trimmed.find(':') {
            let key_part = &trimmed[..colon_pos];
            if is_yaml_key(key_part) {
                let key_field = if indent == 0 {
                    SearchField::TypeDefinition
                } else {
                    SearchField::SymbolName
                };
                // Span covers the key portion (indent-relative start to colon).
                let key_start = byte_offset + indent;
                let key_end = key_start + colon_pos;
                results.push((key_start..key_end, key_field));

                // Check for an inline string value after the colon.
                let after_colon = trimmed[colon_pos + 1..].trim_start();
                if !after_colon.is_empty()
                    && !after_colon.starts_with('{')
                    && !after_colon.starts_with('[')
                    && !after_colon.starts_with('#')
                {
                    // Strip optional quotes for string values.
                    let val = after_colon.trim_matches('"').trim_matches('\'');
                    if !val.is_empty() {
                        // Locate the value within this line.
                        if let Some(val_pos_in_line) = line.find(after_colon) {
                            let val_start = byte_offset + val_pos_in_line;
                            results.push((
                                val_start..val_start + after_colon.len(),
                                SearchField::StringLiteral,
                            ));
                        }
                    }
                }
            }
        }

        byte_offset += line_len + 1;
    }

    Ok(results)
}

/// Returns true if `s` looks like a YAML key (identifier or quoted string before `:`)
fn is_yaml_key(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Quoted keys are valid.
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        return true;
    }
    // Plain keys: alphanumeric + underscores + hyphens + dots (common in YAML).
    s.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
}

// ============================================================================
// TOML classification
// ============================================================================

/// Classify regions in TOML content into `SearchField` spans.
///
/// Uses line-by-line scanning. Pattern matching on `[section]` headers,
/// `key = value` assignments, and `#` comments.
pub fn classify_toml_fields(
    source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    let mut results = Vec::new();
    let mut byte_offset: usize = 0;

    for line in source.lines() {
        let line_len = line.len();
        let trimmed = line.trim();

        // Skip blank lines.
        if trimmed.is_empty() {
            byte_offset += line_len + 1;
            continue;
        }

        // Comment lines.
        if trimmed.starts_with('#') {
            results.push((byte_offset..byte_offset + line_len, SearchField::Comment));
            byte_offset += line_len + 1;
            continue;
        }

        // Section headers: `[table]` or `[[array_of_tables]]`.
        if trimmed.starts_with('[') {
            results.push((byte_offset..byte_offset + line_len, SearchField::TypeDefinition));
            byte_offset += line_len + 1;
            continue;
        }

        // Key-value pairs: `key = value`.
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim();
            if !key.is_empty() {
                // Key span: locate within the original line.
                let key_start_in_line = line
                    .find(key)
                    .unwrap_or(0);
                let key_end = key_start_in_line + key.len();
                results.push((
                    byte_offset + key_start_in_line..byte_offset + key_end,
                    SearchField::SymbolName,
                ));

                // Value span: classify string values.
                let value_part = trimmed[eq_pos + 1..].trim();
                if value_part.starts_with('"') || value_part.starts_with('\'') {
                    // Find value in original line.
                    if let Some(val_pos) = line.find(value_part) {
                        results.push((
                            byte_offset + val_pos..byte_offset + val_pos + value_part.len(),
                            SearchField::StringLiteral,
                        ));
                    }
                }
            }
        }

        byte_offset += line_len + 1;
    }

    Ok(results)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- JSON ----

    #[test]
    fn json_empty_object_is_empty() {
        let result = classify_json_fields("{}").expect("should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn json_malformed_returns_empty() {
        let result = classify_json_fields("{not valid json").expect("should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn json_top_level_key_is_type_definition() {
        let source = r#"{"name": "alice"}"#;
        let result = classify_json_fields(source).expect("should succeed");
        let has_type_def = result
            .iter()
            .any(|(_, f)| *f == SearchField::TypeDefinition);
        assert!(has_type_def, "top-level JSON key should be TypeDefinition");
    }

    #[test]
    fn json_string_value_is_string_literal() {
        let source = r#"{"name": "alice"}"#;
        let result = classify_json_fields(source).expect("should succeed");
        let has_str_lit = result
            .iter()
            .any(|(_, f)| *f == SearchField::StringLiteral);
        assert!(has_str_lit, "string value should be StringLiteral");
    }

    // ---- YAML ----

    #[test]
    fn yaml_empty_string_is_empty() {
        let result = classify_yaml_fields("").expect("should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn yaml_comment_line_is_comment() {
        let source = "# this is a comment\nname: alice\n";
        let result = classify_yaml_fields(source).expect("should succeed");
        let has_comment = result.iter().any(|(_, f)| *f == SearchField::Comment);
        assert!(has_comment, "comment line should be Comment");
    }

    #[test]
    fn yaml_top_level_key_is_type_definition() {
        let source = "name: alice\n";
        let result = classify_yaml_fields(source).expect("should succeed");
        let has_type_def = result
            .iter()
            .any(|(_, f)| *f == SearchField::TypeDefinition);
        assert!(has_type_def, "top-level YAML key should be TypeDefinition");
    }

    #[test]
    fn yaml_nested_key_is_symbol_name() {
        let source = "server:\n  host: localhost\n";
        let result = classify_yaml_fields(source).expect("should succeed");
        let has_symbol = result.iter().any(|(_, f)| *f == SearchField::SymbolName);
        assert!(has_symbol, "nested YAML key should be SymbolName");
    }

    // ---- TOML ----

    #[test]
    fn toml_empty_string_is_empty() {
        let result = classify_toml_fields("").expect("should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn toml_section_header_is_type_definition() {
        let source = "[package]\nname = \"skim\"\n";
        let result = classify_toml_fields(source).expect("should succeed");
        let has_type_def = result
            .iter()
            .any(|(_, f)| *f == SearchField::TypeDefinition);
        assert!(has_type_def, "[section] should be TypeDefinition");
    }

    #[test]
    fn toml_key_is_symbol_name() {
        let source = "name = \"skim\"\n";
        let result = classify_toml_fields(source).expect("should succeed");
        let has_symbol = result.iter().any(|(_, f)| *f == SearchField::SymbolName);
        assert!(has_symbol, "TOML key should be SymbolName");
    }

    #[test]
    fn toml_comment_is_comment() {
        let source = "# a comment\nname = \"x\"\n";
        let result = classify_toml_fields(source).expect("should succeed");
        let has_comment = result.iter().any(|(_, f)| *f == SearchField::Comment);
        assert!(has_comment, "# comment should be Comment");
    }
}
