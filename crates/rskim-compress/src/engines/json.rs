//! JSON structural compressor — new valid-JSON engine (#304 Phase 2 / D5).
//!
//! # Why a new engine, not rskim-core's transform_json
//!
//! `rskim_core::transform_json` (at `rskim-core/src/transform/json.rs`) emits
//! non-valid JSON: it produces unquoted keys and non-standard syntax for
//! human-readable display. This violates AC5 which requires that the engine
//! output must pass `serde_json::from_str`. Therefore this module implements
//! a new, purpose-built compressor that always emits valid JSON (D5).
//!
//! Additionally, `transform_json` is `pub(crate)` in rskim-core and cannot
//! be called from rskim-compress.
//!
//! # D5 — Finalized rendering strategy
//!
//! Scalars → short type-placeholder strings:
//! - String → `"<string>"`
//! - Number → `"<number>"`
//! - Boolean → `"<bool>"`
//! - Null → `null` (no change; already maximally compact)
//!
//! Arrays:
//! - Array of objects → first element as a structural exemplar (same strategy
//!   as `transform_json`'s array rule). Array of scalars → first scalar replaced.
//!   Empty array → `[]` (unchanged).
//!
//! Objects → every value recursively replaced by its placeholder.
//!
//! # Bounds (ADR-003 / PF-005)
//!
//! ## MAX_JSON_DEPTH = 500
//!
//! Basis: JSON bodies that reach 500 levels of nesting are adversarial or
//! pathological. Real Anthropic/OpenAI tool-result payloads nest 4–6 levels
//! deep. rskim-llm uses MAX_DEPTH=64 at parse time (preventing stack overflow
//! at parse); our structural compressor uses a higher bound because we operate
//! on validated, already-parsed content — but we still need a recursion cap to
//! prevent stack overflow on adversarial `serde_json::Value` trees that somehow
//! survived the rskim-llm depth check (e.g., embedded JSON strings re-parsed
//! here). 500 provides a generous margin (well below default stack depth of
//! ~8 MB / ~64 byte frame ≈ 100K frames) while stopping any reasonable pathology.
//!
//! ## MAX_JSON_KEYS = 10_000
//!
//! Basis: A JSON object with more than 10,000 keys is a data-dumping artifact,
//! not a meaningful LLM content block. 10,000 is generous enough to handle any
//! real tool-result schema (largest observed in practice: ~200 keys in a deeply
//! nested tool schema) while bounding worst-case allocation in the structural
//! compressor. If a block exceeds this bound, we forward byte-identical (AC5
//! negative: bound-exceeded → passthrough).

use serde_json::Value;

/// Maximum JSON nesting depth before passthrough.
///
/// Basis: 500 levels is well above any real Anthropic/OpenAI payload (typically
/// 4–6 levels). This prevents stack overflow in the recursive structural
/// compressor while allowing all legitimate content. See module doc for full
/// rationale (ADR-003 / PF-005).
const MAX_JSON_DEPTH: usize = 500;

/// Maximum number of keys across all objects before passthrough.
///
/// Basis: 10,000 keys bounds worst-case allocation in the structural walk.
/// Real tool-result schemas have at most ~200 keys; 10,000 is a safe upper
/// bound for all legitimate LLM content blocks. See module doc for full
/// rationale (ADR-003 / PF-005).
const MAX_JSON_KEYS: usize = 10_000;

/// Result of a JSON compression attempt.
#[derive(Debug, Clone)]
pub(crate) enum CompressResult {
    /// Compression produced valid JSON output.
    Compressed {
        /// The compressed JSON string. Always passes `serde_json::from_str`.
        content: String,
    },
    /// Compression was skipped; caller should forward original bytes.
    ///
    /// Causes: parse failure, depth bound exceeded, key bound exceeded, or
    /// the compressed output would not be shorter (gate applied by caller).
    Passthrough,
}

/// Compress a JSON content block into a valid-JSON structural summary.
///
/// # AC5 invariant
///
/// The output MUST pass `serde_json::from_str`. This invariant is enforced by
/// construction (we serialize from a `serde_json::Value` tree) and by the
/// assertion in tests.
///
/// # Arguments
///
/// - `text`: the raw text payload of the block (must be valid JSON to compress).
///
/// # Returns
///
/// `CompressResult::Compressed` on success; `CompressResult::Passthrough` on
/// parse failure, bound exceeded, or any error.
pub(crate) fn compress_json(text: &str) -> CompressResult {
    // Parse the input. Failure → passthrough (AC5 negative).
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return CompressResult::Passthrough,
    };

    // Run the structural compressor with fresh counters.
    let mut key_count = 0usize;
    let compressed = match compress_value(&value, 0, &mut key_count) {
        Some(v) => v,
        None => return CompressResult::Passthrough,
    };

    // Serialize back to a compact JSON string.
    // serde_json::to_string always produces valid JSON for a serde_json::Value.
    match serde_json::to_string(&compressed) {
        Ok(s) => CompressResult::Compressed { content: s },
        Err(_) => CompressResult::Passthrough,
    }
}

/// Recursively compress a `serde_json::Value` into a structural summary.
///
/// Returns `None` if a bound is exceeded (depth or key count), signalling
/// that the caller should return `CompressResult::Passthrough`.
fn compress_value(value: &Value, depth: usize, key_count: &mut usize) -> Option<Value> {
    // Depth bound (ADR-003 / PF-005 / MAX_JSON_DEPTH).
    if depth >= MAX_JSON_DEPTH {
        return None;
    }

    match value {
        // Scalars: replace with type-placeholder strings (D5).
        Value::String(_) => Some(Value::String("<string>".into())),
        Value::Number(_) => Some(Value::String("<number>".into())),
        Value::Bool(_) => Some(Value::String("<bool>".into())),
        // Null is maximally compact already.
        Value::Null => Some(Value::Null),

        // Objects: replace every value recursively.
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                *key_count += 1;
                // Key bound (ADR-003 / PF-005 / MAX_JSON_KEYS).
                if *key_count > MAX_JSON_KEYS {
                    return None;
                }
                let compressed_v = compress_value(v, depth + 1, key_count)?;
                out.insert(k.clone(), compressed_v);
            }
            Some(Value::Object(out))
        }

        // Arrays: first element as structural exemplar (D5 / transform_json array rule).
        Value::Array(arr) => {
            if arr.is_empty() {
                Some(Value::Array(vec![]))
            } else {
                // Compress only the first element as the structural exemplar.
                let first = compress_value(&arr[0], depth + 1, key_count)?;
                Some(Value::Array(vec![first]))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::Value;

    // =========================================================================
    // AC5 — Valid JSON output guarantee
    // =========================================================================

    #[test]
    fn output_is_valid_json_for_object_input() {
        let json = r#"{"name": "Alice", "age": 30, "active": true, "score": 9.5, "meta": null}"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                let reparsed: Result<Value, _> = serde_json::from_str(&content);
                assert!(
                    reparsed.is_ok(),
                    "AC5: output must be valid JSON; got: {content}"
                );
            }
            CompressResult::Passthrough => {
                panic!("Expected Compressed for valid JSON object");
            }
        }
    }

    #[test]
    fn output_is_valid_json_for_array_of_objects() {
        let json = r#"[{"a": 1, "b": "hello"}, {"a": 2, "b": "world"}]"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                let reparsed: Result<Value, _> = serde_json::from_str(&content);
                assert!(
                    reparsed.is_ok(),
                    "AC5: output must be valid JSON; got: {content}"
                );
            }
            CompressResult::Passthrough => {
                panic!("Expected Compressed for valid JSON array of objects");
            }
        }
    }

    // =========================================================================
    // AC5 — Top-level type and keys preserved
    // =========================================================================

    #[test]
    fn top_level_object_type_preserved() {
        let json = r#"{"key1": "value1", "key2": 42}"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                let reparsed: Value = serde_json::from_str(&content).expect("valid JSON");
                assert!(reparsed.is_object(), "Top-level type must be object");
                // Top-level keys must be present.
                let obj = reparsed.as_object().expect("object");
                assert!(obj.contains_key("key1"), "key1 must be present");
                assert!(obj.contains_key("key2"), "key2 must be present");
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    #[test]
    fn top_level_array_type_preserved() {
        let json = r#"[{"x": 1}, {"x": 2}, {"x": 3}]"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                let reparsed: Value = serde_json::from_str(&content).expect("valid JSON");
                assert!(reparsed.is_array(), "Top-level type must be array");
            }
            CompressResult::Passthrough => panic!("Expected Compressed for array input"),
        }
    }

    // =========================================================================
    // D5 — Scalar placeholder substitution
    // =========================================================================

    #[test]
    fn strings_replaced_with_placeholder() {
        let json = r#"{"name": "Alice"}"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                assert!(
                    content.contains("<string>"),
                    "String values must be replaced with <string> placeholder; got: {content}"
                );
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    #[test]
    fn numbers_replaced_with_placeholder() {
        let json = r#"{"count": 42, "ratio": 0.75}"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                assert!(
                    content.contains("<number>"),
                    "Number values must be replaced with <number> placeholder; got: {content}"
                );
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    #[test]
    fn booleans_replaced_with_placeholder() {
        let json = r#"{"active": true, "deleted": false}"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                assert!(
                    content.contains("<bool>"),
                    "Boolean values must be replaced with <bool> placeholder; got: {content}"
                );
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    #[test]
    fn null_preserved_as_null() {
        let json = r#"{"value": null}"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                let reparsed: Value = serde_json::from_str(&content).expect("valid JSON");
                let obj = reparsed.as_object().expect("object");
                assert!(
                    obj["value"].is_null(),
                    "null must be preserved as null; got: {content}"
                );
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    // =========================================================================
    // D5 — Array-of-objects first-element exemplar
    // =========================================================================

    #[test]
    fn array_of_objects_produces_single_exemplar() {
        let json = r#"[{"a": 1, "b": "hello"}, {"a": 2, "b": "world"}, {"a": 3, "b": "!"}]"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                let reparsed: Value = serde_json::from_str(&content).expect("valid JSON");
                let arr = reparsed.as_array().expect("array");
                // Array collapsed to first element as exemplar.
                assert_eq!(
                    arr.len(),
                    1,
                    "Array-of-objects must be collapsed to first exemplar"
                );
                // The exemplar must be an object with the same keys.
                let first = &arr[0];
                assert!(first.is_object(), "Exemplar must be an object");
                let obj = first.as_object().expect("object");
                assert!(obj.contains_key("a"), "Exemplar must have key 'a'");
                assert!(obj.contains_key("b"), "Exemplar must have key 'b'");
            }
            CompressResult::Passthrough => panic!("Expected Compressed for array of objects"),
        }
    }

    #[test]
    fn empty_array_preserved() {
        let json = r#"[]"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { content } => {
                let reparsed: Value = serde_json::from_str(&content).expect("valid JSON");
                assert!(
                    reparsed.as_array().is_some_and(Vec::is_empty),
                    "Empty array must be preserved"
                );
            }
            CompressResult::Passthrough => {
                // Also acceptable (empty JSON has nothing to compress).
            }
        }
    }

    // =========================================================================
    // AC5 negative — parse failure → passthrough
    // =========================================================================

    #[test]
    fn malformed_json_returns_passthrough() {
        // AC5 negative: parse failure → byte-identical passthrough.
        let malformed = "{invalid json";
        let result = compress_json(malformed);
        assert!(
            matches!(result, CompressResult::Passthrough),
            "Malformed JSON must return Passthrough"
        );
    }

    #[test]
    fn truncated_json_returns_passthrough() {
        let truncated = r#"{"key": "val"#;
        let result = compress_json(truncated);
        assert!(
            matches!(result, CompressResult::Passthrough),
            "Truncated JSON must return Passthrough"
        );
    }

    // =========================================================================
    // AC5 negative — depth bound exceeded → passthrough
    // =========================================================================

    #[test]
    fn depth_exceeded_returns_passthrough() {
        // Build a JSON object nested deeper than MAX_JSON_DEPTH.
        let mut json = String::new();
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            json.push_str(r#"{"a":"#);
        }
        json.push('1');
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            json.push('}');
        }

        let result = compress_json(&json);
        assert!(
            matches!(result, CompressResult::Passthrough),
            "Depth-exceeded JSON must return Passthrough"
        );
    }

    // =========================================================================
    // Determinism — same input → same output
    // =========================================================================

    #[test]
    fn deterministic_output_100_repeats() {
        let json = r#"{"name": "test", "values": [1, 2, 3], "nested": {"x": true}}"#;
        let first = compress_json(json);
        for _ in 1..100 {
            let result = compress_json(json);
            match (&first, &result) {
                (
                    CompressResult::Compressed { content: c1 },
                    CompressResult::Compressed { content: c2 },
                ) => {
                    assert_eq!(c1, c2, "Output must be deterministic");
                }
                (CompressResult::Passthrough, CompressResult::Passthrough) => {}
                _ => panic!("Result variant changed across runs"),
            }
        }
    }
}
