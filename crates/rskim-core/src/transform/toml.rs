//! TOML structure extraction
//!
//! ARCHITECTURE: TOML uses the `toml` crate for parsing, not tree-sitter.
//! Output format: Compact key-only structure for maximum token reduction.
//!
//! # Output Format
//!
//! Strips all values, keeps only keys and nesting structure:
//!
//! ```toml
//! name
//! age
//! server:
//!   host
//!   port
//! database:
//!   url
//!   pool_size
//! ```
//!
//! # Rules
//! - Strip all values (keep keys only)
//! - Tables show nested key structure with indentation
//! - Arrays of tables show first element's structure
//! - Primitive arrays just show key name
//! - Empty tables/arrays just show key name
//! - Inline tables are expanded into nested key structure

use crate::{Result, SkimError};
use toml::Value;

/// Maximum TOML nesting depth to prevent stack overflow DoS attacks
///
/// SECURITY: Matches MAX_JSON_DEPTH / MAX_YAML_DEPTH used in other transformers
/// to ensure consistent protection across all parsing paths.
const MAX_TOML_DEPTH: usize = 500;

/// Maximum number of TOML keys to prevent memory exhaustion DoS attacks
///
/// SECURITY: Matches MAX_JSON_KEYS / MAX_YAML_KEYS limit to ensure consistent
/// protection against unbounded memory allocation.
const MAX_TOML_KEYS: usize = 10_000;

/// Transform TOML to compact structure format
pub(crate) fn transform_toml(source: &str) -> Result<String> {
    let value: Value = source
        .parse::<Value>()
        .map_err(|e| SkimError::ParseError(format!("Invalid TOML: {}", e)))?;

    let mut key_count = 0;
    extract_structure(&value, 0, &mut key_count)
}

/// Recursively extract structure from TOML value
///
/// SECURITY: Validates depth and key count during extraction to prevent DoS attacks.
/// Single-pass traversal for performance (no separate validation pass).
fn extract_structure(value: &Value, depth: usize, key_count: &mut usize) -> Result<String> {
    // SECURITY: Check depth at each recursion to prevent stack overflow
    if depth > MAX_TOML_DEPTH {
        return Err(SkimError::ParseError(format!(
            "TOML nesting depth exceeded: {} (max: {}). Possible malicious input.",
            depth, MAX_TOML_DEPTH
        )));
    }

    match value {
        Value::Table(table) => extract_table_structure(table, depth, key_count),
        Value::Array(arr) => extract_array_structure(arr, depth, key_count),
        _ => Ok(String::new()), // Primitives at root level
    }
}

/// Extract structure from TOML table
///
/// Returns formatted string with keys and nested structures.
fn extract_table_structure(
    table: &toml::map::Map<String, Value>,
    depth: usize,
    key_count: &mut usize,
) -> Result<String> {
    if table.is_empty() {
        return Ok("{}".to_string());
    }

    // SECURITY: Track total keys across all tables to prevent memory exhaustion
    *key_count += table.len();
    if *key_count > MAX_TOML_KEYS {
        return Err(SkimError::ParseError(format!(
            "TOML key count exceeded: {} (max: {}). Possible malicious input.",
            key_count, MAX_TOML_KEYS
        )));
    }

    let indent = "  ".repeat(depth);

    // Pre-allocate capacity to reduce reallocations
    let estimated_capacity = table.len() * 30 + 10;
    let mut result = String::with_capacity(estimated_capacity);

    for (key, val) in table {
        result.push_str(&indent);
        result.push_str(key);

        // Format value based on type
        let value_str = format_value(val, depth + 1, key_count)?;
        result.push_str(&value_str);
        result.push('\n');
    }

    // Remove trailing newline
    if result.ends_with('\n') {
        result.pop();
    }

    Ok(result)
}

/// Format a TOML value for output
///
/// Returns the formatted suffix for a key-value pair.
fn format_value(val: &Value, depth: usize, key_count: &mut usize) -> Result<String> {
    match val {
        Value::Table(_) => {
            let structure = extract_structure(val, depth, key_count)?;
            if structure.is_empty() || structure == "{}" {
                Ok(String::new())
            } else {
                Ok(format!(":\n{}", structure))
            }
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

    if first.is_table() {
        let structure = extract_structure(first, depth, key_count)?;
        if structure.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(":\n{}", structure))
        }
    } else {
        Ok(String::new()) // Primitive array: just show key
    }
}

/// Extract structure from top-level TOML array
///
/// For arrays at root level, shows structure of first table if present.
fn extract_array_structure(arr: &[Value], depth: usize, key_count: &mut usize) -> Result<String> {
    let Some(first) = arr.first() else {
        return Ok("[]".to_string());
    };

    if first.is_table() {
        extract_structure(first, depth, key_count)
    } else {
        Ok("[]".to_string())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // Allow expect in tests
mod tests {
    use super::*;

    #[test]
    fn test_simple_table() {
        let input = r#"
name = "my-project"
version = "1.0.0"
edition = "2021"
"#;
        let result = transform_toml(input).expect("test TOML should parse successfully");

        assert!(result.contains("name"));
        assert!(result.contains("version"));
        assert!(result.contains("edition"));
        assert!(!result.contains("my-project"));
        assert!(!result.contains("1.0.0"));
        assert!(!result.contains("2021"));
    }

    #[test]
    fn test_nested_table() {
        let input = r#"
[server]
host = "localhost"
port = 8080

[database]
url = "postgres://localhost/db"
pool_size = 10
"#;
        let result = transform_toml(input).expect("nested TOML should parse successfully");

        assert!(result.contains("server"));
        assert!(result.contains("host"));
        assert!(result.contains("port"));
        assert!(result.contains("database"));
        assert!(result.contains("url"));
        assert!(result.contains("pool_size"));
        assert!(!result.contains("localhost"));
        assert!(!result.contains("8080"));
    }

    #[test]
    fn test_array_of_tables() {
        let input = r#"
[[users]]
name = "Alice"
role = "admin"

[[users]]
name = "Bob"
role = "user"
"#;
        let result = transform_toml(input).expect("array of tables should parse successfully");

        assert!(result.contains("users"));
        assert!(result.contains("name"));
        assert!(result.contains("role"));
        assert!(!result.contains("Alice"));
        assert!(!result.contains("Bob"));
    }

    #[test]
    fn test_primitive_array() {
        let input = r#"
tags = ["rust", "parser", "cli"]
"#;
        let result = transform_toml(input).expect("primitive array should parse successfully");

        assert!(result.contains("tags"));
        assert!(!result.contains("rust"));
        assert!(!result.contains("parser"));
    }

    #[test]
    fn test_inline_table() {
        let input = r#"
point = { x = 1, y = 2 }
"#;
        let result = transform_toml(input).expect("inline table should parse successfully");

        assert!(result.contains("point"));
        assert!(result.contains("x"));
        assert!(result.contains("y"));
        assert!(!result.contains("1"));
        assert!(!result.contains("2"));
    }

    #[test]
    fn test_empty_table() {
        let input = r#"
[empty]
"#;
        let result = transform_toml(input).expect("empty table should parse successfully");

        assert!(result.contains("empty"));
    }

    #[test]
    fn test_invalid_toml() {
        let input = "[invalid";
        let result = transform_toml(input);

        assert!(result.is_err());
    }

    #[test]
    fn test_key_count_limit() {
        // SECURITY TEST: Ensure TOML with >10,000 keys is rejected
        let mut toml_str = String::new();
        for i in 0..10_001 {
            toml_str.push_str(&format!("key_{} = {}\n", i, i));
        }

        let result = transform_toml(&toml_str);

        assert!(result.is_err(), "Expected error for excessive keys");
        let err = result
            .expect_err("Expected error for key count limit")
            .to_string();
        assert!(
            err.contains("key count exceeded"),
            "Error message should mention key count limit, got: {}",
            err
        );
    }

    #[test]
    fn test_depth_limit() {
        // SECURITY TEST: Ensure deeply nested TOML is rejected (depth > 500)
        // Build nested inline tables: key = { key = { key = { ... } } }
        // Note: the toml crate may have its own recursion limit that fires
        // before our MAX_TOML_DEPTH of 500. Either error is acceptable.
        let mut toml_str = String::from("key = ");
        for _ in 0..550 {
            toml_str.push_str("{ nested = ");
        }
        toml_str.push_str("\"value\"");
        for _ in 0..550 {
            toml_str.push_str(" }");
        }

        let result = transform_toml(&toml_str);

        // Should reject with depth or parse error
        assert!(result.is_err(), "Expected error for deeply nested TOML");
        let err_msg = result
            .expect_err("Expected error for depth limit")
            .to_string();
        assert!(
            err_msg.contains("depth exceeded")
                || err_msg.contains("recursion limit")
                || err_msg.contains("Invalid TOML"),
            "Error message should mention depth/recursion limit or parse error, got: {}",
            err_msg
        );
    }
}
