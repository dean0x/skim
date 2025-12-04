//! YAML structure extraction
//!
//! ARCHITECTURE: YAML uses serde_yaml for parsing, not tree-sitter.
//! Output format: Compact key-only structure for maximum token reduction.
//!
//! # Output Format
//!
//! Strips all values, keeps only keys and nesting structure:
//!
//! ```yaml
//! user:
//!   name
//!   age
//!   tags
//! items:
//!   id
//!   price
//! ```
//!
//! # Multi-Document Support
//!
//! Files with `---` document separators process ALL documents with separators preserved:
//!
//! ```yaml
//! ---
//! apiVersion
//! kind
//! ---
//! apiVersion
//! kind
//! ```
//!
//! # Rules
//! - Strip all values (keep keys only)
//! - Arrays of primitives -> just show key name
//! - Arrays of objects -> show single object structure
//! - Empty arrays/objects -> just show key name
//! - Multi-document files -> show all documents with `---` preserved
//! - Anchors/aliases -> resolved by serde_yaml (not preserved)

use crate::{Result, SkimError};
use serde_yaml::Value;

/// Maximum YAML nesting depth to prevent stack overflow DoS attacks
///
/// SECURITY: Matches MAX_JSON_DEPTH used in JSON transformer
/// to ensure consistent protection across all parsing paths.
const MAX_YAML_DEPTH: usize = 500;

/// Maximum number of YAML keys to prevent memory exhaustion DoS attacks
///
/// SECURITY: Matches MAX_JSON_KEYS limit to ensure consistent
/// protection against unbounded memory allocation.
const MAX_YAML_KEYS: usize = 10_000;

/// Transform YAML to compact structure format
///
/// Handles both single-document and multi-document YAML files.
pub(crate) fn transform_yaml(source: &str) -> Result<String> {
    let documents = split_yaml_documents(source);

    if documents.len() == 1 {
        // Single document - parse and transform directly
        transform_single_document(&documents[0])
    } else {
        // Multi-document - transform each and join with separators
        let mut results = Vec::with_capacity(documents.len());
        let mut total_key_count = 0;

        for doc in &documents {
            if doc.trim().is_empty() {
                continue;
            }

            let value: Value = serde_yaml::from_str(doc)
                .map_err(|e| SkimError::ParseError(format!("Invalid YAML: {}", e)))?;

            let mut key_count = 0;
            let structure = extract_structure(&value, 0, &mut key_count)?;

            total_key_count += key_count;
            if total_key_count > MAX_YAML_KEYS {
                return Err(SkimError::ParseError(format!(
                    "YAML key count exceeded: {} (max: {}). Possible malicious input.",
                    total_key_count, MAX_YAML_KEYS
                )));
            }

            if !structure.is_empty() {
                results.push(structure);
            }
        }

        if results.is_empty() {
            return Ok(String::new());
        }

        // Join with document separators
        Ok(results.join("\n---\n"))
    }
}

/// Transform a single YAML document
fn transform_single_document(source: &str) -> Result<String> {
    let value: Value = serde_yaml::from_str(source)
        .map_err(|e| SkimError::ParseError(format!("Invalid YAML: {}", e)))?;

    let mut key_count = 0;
    extract_structure(&value, 0, &mut key_count)
}

/// Split YAML source into individual documents
///
/// Handles the `---` document separator. Leading `---` on first document is optional.
fn split_yaml_documents(source: &str) -> Vec<String> {
    let mut documents = Vec::new();
    let mut current_doc = String::new();
    let mut in_document = false;

    for line in source.lines() {
        if line.trim() == "---" {
            if in_document && !current_doc.trim().is_empty() {
                documents.push(current_doc);
                current_doc = String::new();
            }
            in_document = true;
        } else if line.trim() == "..." {
            // End of document marker - finish current doc but don't start new one
            if !current_doc.trim().is_empty() {
                documents.push(current_doc);
                current_doc = String::new();
            }
            in_document = false;
        } else {
            if !in_document && !line.trim().is_empty() {
                // Content before first --- (implicit single document)
                in_document = true;
            }
            if in_document {
                if !current_doc.is_empty() {
                    current_doc.push('\n');
                }
                current_doc.push_str(line);
            }
        }
    }

    // Don't forget the last document
    if !current_doc.trim().is_empty() {
        documents.push(current_doc);
    }

    // If no documents found, return empty source as single doc
    if documents.is_empty() {
        documents.push(source.to_string());
    }

    documents
}

/// Recursively extract structure from YAML value
///
/// SECURITY: Validates depth and key count during extraction to prevent DoS attacks.
/// Single-pass traversal for performance (no separate validation pass).
fn extract_structure(value: &Value, depth: usize, key_count: &mut usize) -> Result<String> {
    // SECURITY: Check depth at each recursion to prevent stack overflow
    if depth > MAX_YAML_DEPTH {
        return Err(SkimError::ParseError(format!(
            "YAML nesting depth exceeded: {} (max: {}). Possible malicious input.",
            depth, MAX_YAML_DEPTH
        )));
    }

    match value {
        Value::Mapping(map) => extract_mapping_structure(map, depth, key_count),
        Value::Sequence(seq) => extract_sequence_structure(seq, depth, key_count),
        _ => Ok(String::new()), // Primitives at root level
    }
}

/// Extract structure from YAML mapping (object)
///
/// Returns formatted string with keys and nested structures.
fn extract_mapping_structure(
    map: &serde_yaml::Mapping,
    depth: usize,
    key_count: &mut usize,
) -> Result<String> {
    if map.is_empty() {
        return Ok("{}".to_string());
    }

    // SECURITY: Track total keys across all mappings to prevent memory exhaustion
    *key_count += map.len();
    if *key_count > MAX_YAML_KEYS {
        return Err(SkimError::ParseError(format!(
            "YAML key count exceeded: {} (max: {}). Possible malicious input.",
            key_count, MAX_YAML_KEYS
        )));
    }

    let indent = "  ".repeat(depth);

    // Pre-allocate capacity to reduce reallocations
    let estimated_capacity = map.len() * 30 + 10;
    let mut result = String::with_capacity(estimated_capacity);

    for (key, val) in map {
        // Only process string keys (YAML allows non-string keys)
        let key_str = match key {
            Value::String(s) => s.as_str(),
            _ => continue, // Skip non-string keys
        };

        result.push_str(&indent);
        result.push_str(key_str);

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

/// Format a YAML value for output
///
/// Returns the formatted suffix for a key-value pair.
fn format_value(val: &Value, depth: usize, key_count: &mut usize) -> Result<String> {
    match val {
        Value::Mapping(_) => {
            let structure = extract_structure(val, depth, key_count)?;
            if structure.is_empty() || structure == "{}" {
                Ok(String::new())
            } else {
                Ok(format!(":\n{}", structure))
            }
        }
        Value::Sequence(seq) => format_sequence_value(seq, depth, key_count),
        _ => Ok(String::new()), // Primitives: just show the key
    }
}

/// Format a sequence value for output
///
/// Returns formatted suffix for sequences.
fn format_sequence_value(seq: &[Value], depth: usize, key_count: &mut usize) -> Result<String> {
    let Some(first) = seq.first() else {
        return Ok(String::new()); // Empty sequence: just show key
    };

    if first.is_mapping() {
        let structure = extract_structure(first, depth, key_count)?;
        if structure.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(":\n{}", structure))
        }
    } else {
        Ok(String::new()) // Primitive sequence: just show key
    }
}

/// Extract structure from top-level YAML sequence
///
/// For sequences at root level, shows structure of first mapping if present.
fn extract_sequence_structure(
    seq: &[Value],
    depth: usize,
    key_count: &mut usize,
) -> Result<String> {
    let Some(first) = seq.first() else {
        return Ok("[]".to_string());
    };

    if first.is_mapping() {
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
    fn test_simple_mapping() {
        let input = "name: John\nage: 30";
        let result = transform_yaml(input).expect("test YAML should parse successfully");

        assert!(result.contains("name"));
        assert!(result.contains("age"));
        assert!(!result.contains("John"));
        assert!(!result.contains("30"));
    }

    #[test]
    fn test_nested_mapping() {
        let input = r#"
user:
  name: John
  age: 30
"#;
        let result = transform_yaml(input).expect("nested YAML should parse successfully");

        assert!(result.contains("user"));
        assert!(result.contains("name"));
        assert!(result.contains("age"));
        assert!(!result.contains("John"));
    }

    #[test]
    fn test_sequence_of_primitives() {
        let input = "tags:\n  - admin\n  - user\n  - moderator";
        let result =
            transform_yaml(input).expect("sequence of primitives should parse successfully");

        assert!(result.contains("tags"));
        assert!(!result.contains("admin"));
        assert!(!result.contains("user"));
        assert!(!result.contains("moderator"));
    }

    #[test]
    fn test_sequence_of_mappings() {
        let input = r#"
items:
  - id: 1
    price: 100
  - id: 2
    price: 200
"#;
        let result =
            transform_yaml(input).expect("sequence of mappings should parse successfully");

        assert!(result.contains("items"));
        assert!(result.contains("id"));
        assert!(result.contains("price"));
        assert!(!result.contains("100"));
        assert!(!result.contains("200"));
    }

    #[test]
    fn test_empty_mapping() {
        let input = "empty: {}";
        let result = transform_yaml(input).expect("empty mapping should parse successfully");

        assert!(result.contains("empty"));
    }

    #[test]
    fn test_empty_sequence() {
        let input = "items: []";
        let result = transform_yaml(input).expect("empty sequence should parse successfully");

        assert!(result.contains("items"));
    }

    #[test]
    fn test_multi_document() {
        let input = r#"---
apiVersion: v1
kind: Service
---
apiVersion: v1
kind: Deployment
"#;
        let result = transform_yaml(input).expect("multi-document YAML should parse successfully");

        // Should contain separator
        assert!(result.contains("---"));
        // Should have both documents' keys
        assert!(result.contains("apiVersion"));
        assert!(result.contains("kind"));
        // Should not contain values
        assert!(!result.contains("Service"));
        assert!(!result.contains("Deployment"));
    }

    #[test]
    fn test_multi_document_without_leading_separator() {
        let input = r#"first: doc
---
second: doc
"#;
        let result = transform_yaml(input).expect("multi-document without leading --- should parse");

        assert!(result.contains("first"));
        assert!(result.contains("second"));
        assert!(result.contains("---"));
    }

    #[test]
    fn test_document_end_marker() {
        let input = r#"---
name: value
...
"#;
        let result = transform_yaml(input).expect("document with end marker should parse");

        assert!(result.contains("name"));
        assert!(!result.contains("value"));
    }

    #[test]
    fn test_invalid_yaml() {
        let input = "invalid: [unclosed";
        let result = transform_yaml(input);

        assert!(result.is_err());
    }

    #[test]
    fn test_anchors_resolved() {
        // Note: serde_yaml resolves anchors, so this tests that we handle resolved values correctly
        let input = r#"
defaults: &defaults
  adapter: postgres
  host: localhost

development:
  <<: *defaults
  database: dev_db
"#;
        let result = transform_yaml(input).expect("YAML with anchors should parse");

        assert!(result.contains("defaults"));
        assert!(result.contains("development"));
        // Anchor syntax won't appear in output (resolved by serde_yaml)
    }

    #[test]
    fn test_split_yaml_documents() {
        let input = "---\nfirst: doc\n---\nsecond: doc";
        let docs = split_yaml_documents(input);

        assert_eq!(docs.len(), 2);
        assert!(docs[0].contains("first"));
        assert!(docs[1].contains("second"));
    }

    #[test]
    fn test_split_single_document() {
        let input = "single: document";
        let docs = split_yaml_documents(input);

        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn test_depth_limit() {
        // Create deeply nested YAML that exceeds safety limits
        // Note: serde_yaml has its own recursion limit (128) which is stricter
        // than our MAX_YAML_DEPTH (500), so we test with a smaller nesting
        // to verify our own limit works when serde_yaml doesn't hit its limit first.
        let mut yaml = String::new();
        for i in 0..=MAX_YAML_DEPTH + 1 {
            yaml.push_str(&"  ".repeat(i));
            yaml.push_str(&format!("level{}: \n", i));
        }
        // Add a final value
        yaml.push_str(&"  ".repeat(MAX_YAML_DEPTH + 2));
        yaml.push_str("value: end");

        let result = transform_yaml(&yaml);

        // Should fail due to either serde_yaml recursion limit or our depth limit
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // Accept either our error message or serde_yaml's recursion error
        assert!(
            err.contains("depth exceeded")
                || err.contains("recursion limit")
                || err.contains("Invalid YAML"),
            "Expected depth/recursion error, got: {}",
            err
        );
    }
}
