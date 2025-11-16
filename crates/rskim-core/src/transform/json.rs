//! JSON structure extraction
//!
//! ARCHITECTURE: JSON uses serde_json for parsing, not tree-sitter.
//! Output format: Compact key-only structure for maximum token reduction.
//!
//! # Output Format
//!
//! Strips all values, keeps only keys and nesting structure:
//!
//! ```json
//! {
//!   user: {
//!     name,
//!     age,
//!     tags
//!   },
//!   items: {
//!     id,
//!     price
//!   }
//! }
//! ```
//!
//! # Rules
//! - Strip all quotes (keys and values)
//! - Strip all values (keep keys only)
//! - Arrays of primitives → just show key name
//! - Arrays of objects → show single object structure
//! - Empty arrays/objects → just show key name
//! - Nested arrays → just show key name
//! - Mixed types in arrays → just show key name

use crate::{Result, SkimError};
use serde_json::Value;

/// Maximum JSON nesting depth to prevent stack overflow DoS attacks
///
/// SECURITY: Matches MAX_AST_DEPTH used in other transformers (signatures.rs, types.rs)
/// to ensure consistent protection across all parsing paths.
/// Note: serde_json has a default recursion limit of 128, which is stricter.
const MAX_JSON_DEPTH: usize = 500;

/// Maximum number of JSON keys to prevent memory exhaustion DoS attacks
///
/// SECURITY: Matches MAX_SIGNATURES limit in signatures.rs to ensure consistent
/// protection against unbounded memory allocation.
const MAX_JSON_KEYS: usize = 10_000;

/// Transform JSON to compact structure format
pub(crate) fn transform_json(source: &str) -> Result<String> {
    // Parse JSON
    let value: Value = serde_json::from_str(source)
        .map_err(|e| SkimError::ParseError(format!("Invalid JSON: {}", e)))?;

    // Extract structure with integrated depth and key validation (single pass)
    let mut key_count = 0;
    let structure = extract_structure(&value, 0, &mut key_count)?;

    Ok(structure)
}

/// Recursively extract structure from JSON value
///
/// SECURITY: Validates depth and key count during extraction to prevent DoS attacks.
/// Single-pass traversal for performance (no separate validation pass).
fn extract_structure(value: &Value, depth: usize, key_count: &mut usize) -> Result<String> {
    // SECURITY: Check depth at each recursion to prevent stack overflow
    if depth > MAX_JSON_DEPTH {
        return Err(SkimError::ParseError(format!(
            "JSON nesting depth exceeded: {} (max: {}). Possible malicious input.",
            depth, MAX_JSON_DEPTH
        )));
    }

    match value {
        Value::Object(map) => extract_object_structure(map, depth, key_count),
        Value::Array(arr) => extract_array_structure(arr, depth, key_count),
        _ => Ok(String::new()), // Primitives at root level
    }
}

/// Extract structure from JSON object
///
/// Returns formatted string with keys and nested structures.
fn extract_object_structure(
    map: &serde_json::Map<String, Value>,
    depth: usize,
    key_count: &mut usize,
) -> Result<String> {
    if map.is_empty() {
        return Ok("{}".to_string());
    }

    // SECURITY: Track total keys across all objects to prevent memory exhaustion
    *key_count += map.len();
    if *key_count > MAX_JSON_KEYS {
        return Err(SkimError::ParseError(format!(
            "JSON key count exceeded: {} (max: {}). Possible malicious input.",
            key_count, MAX_JSON_KEYS
        )));
    }

    let indent = "  ".repeat(depth);
    let next_indent = "  ".repeat(depth + 1);

    // Pre-allocate capacity to reduce reallocations
    let estimated_capacity = map.len() * 30 + 10;
    let mut result = String::with_capacity(estimated_capacity);
    result.push_str("{\n");

    for (i, (key, val)) in map.iter().enumerate() {
        result.push_str(&next_indent);
        result.push_str(key);

        // Format value based on type
        let value_str = format_value(val, depth + 1, key_count)?;
        result.push_str(&value_str);

        // Add comma if not the last item
        if i < map.len() - 1 {
            result.push(',');
        }
        result.push('\n');
    }

    result.push_str(&indent);
    result.push('}');
    Ok(result)
}

/// Format a JSON value for output
///
/// Returns the formatted suffix for a key-value pair.
fn format_value(val: &Value, depth: usize, key_count: &mut usize) -> Result<String> {
    match val {
        Value::Object(_) => {
            let structure = extract_structure(val, depth, key_count)?;
            Ok(format!(": {}", structure))
        }
        Value::Array(arr) => format_array_value(arr, depth, key_count),
        _ => Ok(String::new()), // Primitives: just show the key
    }
}

/// Format an array value for output
///
/// Returns formatted suffix for arrays.
fn format_array_value(arr: &[Value], depth: usize, key_count: &mut usize) -> Result<String> {
    let Some(first) = arr.first() else {
        return Ok(String::new()); // Empty array: just show key
    };

    if first.is_object() {
        let structure = extract_structure(first, depth, key_count)?;
        Ok(format!(": {}", structure))
    } else {
        Ok(String::new()) // Primitive array: just show key
    }
}

/// Extract structure from top-level JSON array
///
/// For arrays at root level, shows structure of first object if present.
fn extract_array_structure(arr: &[Value], depth: usize, key_count: &mut usize) -> Result<String> {
    let Some(first) = arr.first() else {
        return Ok("[]".to_string());
    };

    if first.is_object() {
        extract_structure(first, depth, key_count)
    } else {
        Ok("[]".to_string())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // Allow expect in tests - it's acceptable for test code to panic on unexpected errors
mod tests {
    use super::*;

    #[test]
    fn test_simple_object() {
        let input = r#"{"name": "John", "age": 30}"#;
        let result = transform_json(input).expect("test JSON should parse successfully");

        assert!(result.contains("name"));
        assert!(result.contains("age"));
        assert!(!result.contains("John"));
        assert!(!result.contains("30"));
    }

    #[test]
    fn test_nested_object() {
        let input = r#"{
            "user": {
                "name": "John",
                "age": 30
            }
        }"#;
        let result = transform_json(input).expect("nested JSON should parse successfully");

        assert!(result.contains("user"));
        assert!(result.contains("name"));
        assert!(result.contains("age"));
        assert!(!result.contains("John"));
    }

    #[test]
    fn test_array_of_primitives() {
        let input = r#"{"tags": ["admin", "user", "moderator"]}"#;
        let result = transform_json(input).expect("array of primitives should parse successfully");

        assert!(result.contains("tags"));
        assert!(!result.contains("admin"));
        assert!(!result.contains("user"));
        assert!(!result.contains("moderator"));
    }

    #[test]
    fn test_array_of_objects() {
        let input = r#"{
            "items": [
                {"id": 1, "price": 100},
                {"id": 2, "price": 200}
            ]
        }"#;
        let result = transform_json(input).expect("array of objects should parse successfully");

        assert!(result.contains("items"));
        assert!(result.contains("id"));
        assert!(result.contains("price"));
        assert!(!result.contains("100"));
        assert!(!result.contains("200"));
    }

    #[test]
    fn test_empty_object() {
        let input = r#"{"empty": {}}"#;
        let result = transform_json(input).expect("empty object should parse successfully");

        assert!(result.contains("empty"));
    }

    #[test]
    fn test_empty_array() {
        let input = r#"{"items": []}"#;
        let result = transform_json(input).expect("empty array should parse successfully");

        assert!(result.contains("items"));
    }

    #[test]
    fn test_mixed_array() {
        let input = r#"{"mixed": [1, "string", {"id": 1}]}"#;
        let result = transform_json(input).expect("mixed array should parse successfully");

        assert!(result.contains("mixed"));
        // For mixed arrays, just show the key (no structure)
        assert!(!result.contains("id"));
    }

    #[test]
    fn test_invalid_json() {
        let input = r#"{"invalid": "#;
        let result = transform_json(input);

        assert!(result.is_err());
    }
}
