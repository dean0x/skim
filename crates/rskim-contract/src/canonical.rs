//! Tools-array canonical equality: recursive key-sort deep-equality.
//!
//! # Purpose
//!
//! Any waivered tools-array reordering must be **deep-equal** to the original
//! under the pinned canonicalization (invariant 6 / AC11). This module provides
//! the equality check used by the harness to verify waivered reorders.
//!
//! # Number comparison via RawValue (Decision 1)
//!
//! Numbers are compared as **raw source-token bytes** via `serde_json::value::RawValue`
//! (the `raw_value` feature, enabled workspace-wide). They are NEVER re-serialized
//! via JCS/RFC-8785, which would silently change values:
//! - `1e3` → `1000` (different bytes, different cache key)
//! - `1.0` → `1` (different bytes)
//! - Large precision floats may be altered
//!
//! The canonical equality in this module uses [`serde_json::value::RawValue`] to
//! preserve number token bytes exactly. Two number tokens are equal iff their raw
//! string representations are byte-identical (see [`raw_numbers_equal`]).
//!
//! The primary entry points [`tools_arrays_equal`] and [`canonical_equal_raw`] both
//! route number comparison through the raw-token path, not through `Value::Number`
//! (which normalises tokens after parse without `arbitrary_precision`).
//!
//! # Recursive key-sort deep-equality
//!
//! Two JSON values are canonically equal if:
//! - Both are null, bool, or string: standard equality
//! - Both are numbers: raw source-token bytes are equal (via [`raw_numbers_equal`])
//! - Both are objects: same keys (order-insensitive), each key's value is
//!   recursively canonically equal
//! - Both are arrays: same length, each element is recursively canonically equal
//!   (order-sensitive for arrays)
//!
//! Object key order is ignored because the provider may reorder top-level tool
//! schema fields without semantic change (e.g., `"description"` before or after
//! `"parameters"`). Array element order is preserved because tools in the array
//! order matters to the model.
//!
//! # Depth bound (AC17)
//!
//! Recursive equality checks are bounded to [`MAX_CANONICAL_DEPTH`] levels.
//! Exceeding the depth returns `false` (conservative: treat as not-equal →
//! harness would flag the reorder as unsafe).

use serde_json::Value;

// ============================================================================
// RawValue-based recursive comparison (the canonical path, AC11)
// ============================================================================

/// Node parsed from a raw JSON string, keeping numbers as their raw token bytes.
///
/// This is the internal representation used by [`tools_arrays_equal`] to ensure
/// number tokens are compared byte-for-byte rather than as parsed f64 values.
enum RawNode {
    Null,
    Bool(bool),
    Number(String), // raw token bytes, owned
    JsonString(String),
    Array(Vec<RawNode>),
    Object(Vec<(String, RawNode)>),
}

/// Parse a raw JSON string into a `RawNode`, preserving raw number tokens.
///
/// Returns `None` if the source cannot be parsed, falling back conservative.
fn parse_raw_node(raw_src: &str, depth: usize) -> Option<RawNode> {
    if depth > MAX_CANONICAL_DEPTH {
        return None;
    }
    // Parse via RawValue to get the canonical token.
    let raw: Box<serde_json::value::RawValue> = serde_json::from_str(raw_src).ok()?;
    let token = raw.get().trim();
    // Determine JSON type by first byte.
    let first = token.as_bytes().first()?;
    match first {
        b'n' => Some(RawNode::Null),
        b't' => Some(RawNode::Bool(true)),
        b'f' => Some(RawNode::Bool(false)),
        b'"' => {
            // Unescape by parsing as a serde_json string.
            let s: String = serde_json::from_str(token).ok()?;
            Some(RawNode::JsonString(s))
        }
        b'[' => {
            // Parse array elements preserving individual raw tokens.
            let elems: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(token).ok()?;
            let mut result = Vec::with_capacity(elems.len());
            for elem in elems {
                let node = parse_raw_node(elem.get(), depth + 1)?;
                result.push(node);
            }
            Some(RawNode::Array(result))
        }
        b'{' => {
            // Parse object entries preserving individual raw value tokens.
            // `BTreeMap` is used here (not HashMap) to make the determinism guarantee
            // structural rather than argued-in-a-comment (AC9). Although `raw_nodes_equal`
            // compares objects order-insensitively via `.find()` — so the boolean result
            // is deterministic regardless of iteration order — using BTreeMap makes it
            // impossible for a future maintainer to copy this pattern into a path where
            // iteration order DOES reach output bytes and silently break determinism.
            //
            // Duplicate keys: BTreeMap last-wins semantics match serde_json's own `Value`
            // parse behaviour. Duplicate keys in tool schemas are pathological and not
            // produced by real providers.
            let map: std::collections::BTreeMap<String, Box<serde_json::value::RawValue>> =
                serde_json::from_str(token).ok()?;
            let mut result = Vec::with_capacity(map.len());
            for (k, v) in map {
                let node = parse_raw_node(v.get(), depth + 1)?;
                result.push((k, node));
            }
            Some(RawNode::Object(result))
        }
        _ => {
            // A number: keep the raw token as an owned string.
            Some(RawNode::Number(token.to_owned()))
        }
    }
}

/// Compare two `RawNode` trees for canonical equality.
///
/// - Objects: order-insensitive key comparison
/// - Arrays: order-sensitive
/// - Numbers: raw byte equality (the AC11 invariant)
///
/// # Depth bound (AC17)
///
/// `raw_nodes_equal` is recursive but carries no explicit depth counter.
/// Termination is transitively bounded: `RawNode` trees are produced by
/// `parse_raw_node`, which checks `depth > MAX_CANONICAL_DEPTH` at each
/// recursion level and returns `None` for over-depth inputs. A tree deeper
/// than the bound therefore cannot be constructed — so `raw_nodes_equal`
/// can only receive trees whose depth is ≤ `MAX_CANONICAL_DEPTH`.
/// This satisfies AC17's "all recursion explicitly bounded" requirement,
/// though the bound is at the parse site rather than the comparison site.
fn raw_nodes_equal(a: &RawNode, b: &RawNode) -> bool {
    match (a, b) {
        (RawNode::Null, RawNode::Null) => true,
        (RawNode::Bool(x), RawNode::Bool(y)) => x == y,
        (RawNode::Number(x), RawNode::Number(y)) => x == y,
        (RawNode::JsonString(x), RawNode::JsonString(y)) => x == y,
        (RawNode::Array(xs), RawNode::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys.iter()).all(|(x, y)| raw_nodes_equal(x, y))
        }
        (RawNode::Object(xm), RawNode::Object(ym)) => {
            if xm.len() != ym.len() {
                return false;
            }
            // Order-insensitive: for each key in a, find it in b.
            xm.iter().all(|(k, xv)| {
                ym.iter()
                    .find(|(bk, _)| bk == k)
                    .map(|(_, yv)| raw_nodes_equal(xv, yv))
                    .unwrap_or(false)
            })
        }
        _ => false, // type mismatch
    }
}

/// Maximum recursion depth for canonical equality checks.
///
/// Aligned with `MAX_ANALYSIS_DEPTH` from [`crate::request`] (64 levels).
pub const MAX_CANONICAL_DEPTH: usize = 64;

/// Check canonical deep-equality between two JSON values.
///
/// # Footgun warning — wrong function for invariant-6 checks
///
/// This function compares numbers as **parsed f64 values** (not as raw token
/// bytes). Without `arbitrary_precision`, serde_json normalises `1e3 == 1000.0`,
/// so this function considers `1e3` and `1000` equal. That is intentionally
/// different from the invariant-6 requirement, where cache-key faithfulness
/// demands `1e3 ≠ 1000` (different source tokens, different cache key impact).
///
/// **For invariant-6 (tools-array reorder verification), use [`canonical_equal_raw`]
/// or [`tools_arrays_equal`] instead.** This function is for structural equality
/// tests that do NOT need raw-token number fidelity.
///
/// Returns `true` iff `a` and `b` are semantically equal under the pinned
/// canonicalization:
/// - Object keys are compared order-insensitively (recursive key-sort)
/// - Numbers are compared as parsed f64 values (f64-normalised, NOT raw tokens)
/// - Arrays are order-sensitive
/// - Exceeding `MAX_CANONICAL_DEPTH` returns `false`
///
/// # Examples
///
/// ```rust
/// use rskim_contract::canonical::canonical_equal;
/// use serde_json::json;
///
/// // Reordered object keys → equal
/// let a = json!({"b": 1, "a": 2});
/// let b = json!({"a": 2, "b": 1});
/// assert!(canonical_equal(&a, &b));
///
/// // Different values → not equal
/// let c = json!({"a": 1});
/// let d = json!({"a": 2});
/// assert!(!canonical_equal(&c, &d));
///
/// // Arrays are order-sensitive
/// let e = json!([1, 2]);
/// let f = json!([2, 1]);
/// assert!(!canonical_equal(&e, &f));
/// ```
pub fn canonical_equal(a: &Value, b: &Value) -> bool {
    canonical_equal_inner(a, b, 0)
}

fn canonical_equal_inner(a: &Value, b: &Value, depth: usize) -> bool {
    if depth > MAX_CANONICAL_DEPTH {
        // Fail conservative: treat as not-equal on depth overflow.
        return false;
    }

    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => {
            // Post-parse comparison: `arbitrary_precision` is NOT enabled, so
            // serde_json normalises numbers to f64 (e.g., `1e3` == `1000`).
            // For full invariant 6 (raw-token byte equality), use `canonical_equal_raw`.
            x == y
        }
        (Value::Array(xs), Value::Array(ys)) => {
            // Arrays are order-sensitive.
            if xs.len() != ys.len() {
                return false;
            }
            xs.iter()
                .zip(ys.iter())
                .all(|(x, y)| canonical_equal_inner(x, y, depth + 1))
        }
        (Value::Object(xm), Value::Object(ym)) => {
            // Objects are order-insensitive: compare by sorted keys.
            if xm.len() != ym.len() {
                return false;
            }
            // All keys in xm must exist in ym with canonically-equal values.
            xm.iter().all(|(k, xv)| match ym.get(k) {
                Some(yv) => canonical_equal_inner(xv, yv, depth + 1),
                None => false,
            })
        }
        // Type mismatch → not equal.
        _ => false,
    }
}

/// Check canonical equality using raw JSON source strings for numbers.
///
/// This is the full invariant 6 implementation: numbers are compared as the
/// raw bytes they appear as in the JSON source, NOT as parsed float values.
/// Use this for tools-array reorder verification where `1e3 ≠ 1000` (different
/// source tokens, different cache key impact).
///
/// `raw_a` and `raw_b` are the raw JSON strings (e.g., from `serde_json::RawValue`).
///
/// Returns `None` if either string fails to parse as a JSON value.
/// Returns `Some(true)` if the values are canonically equal with raw-token number comparison.
/// Returns `Some(false)` if they differ.
pub fn canonical_equal_raw(raw_a: &str, raw_b: &str) -> Option<bool> {
    // Use the RawNode path so numbers are compared as token bytes.
    let a = parse_raw_node(raw_a, 0)?;
    let b = parse_raw_node(raw_b, 0)?;
    Some(raw_nodes_equal(&a, &b))
}

/// Compare two number tokens as raw source byte strings.
///
/// Returns `true` iff the raw bytes of `a` and `b` are identical.
/// This is the definitive number comparison for invariant 6.
///
/// # Examples
///
/// ```rust
/// use rskim_contract::canonical::raw_numbers_equal;
///
/// // Same token → equal
/// assert!(raw_numbers_equal("1e3", "1e3"));
///
/// // Different tokens, same mathematical value → NOT equal under raw comparison.
/// // This is the key invariant: we do NOT normalize.
/// assert!(!raw_numbers_equal("1e3", "1000"));
/// assert!(!raw_numbers_equal("1.0", "1"));
/// ```
pub fn raw_numbers_equal(a: &str, b: &str) -> bool {
    a == b
}

/// Check that two tools arrays are canonically equal (waiver verification).
///
/// Used by the harness to verify that a `MetadataReorderWithMarkers` waiver
/// did not alter any tool description or schema content.
///
/// **Number comparison uses raw source-token bytes** (Decision 1 / AC11):
/// `1e3` and `1000` are treated as distinct tokens even though they are
/// mathematically equal. This is the correct invariant for cache-key faithfulness.
///
/// Both arguments must be valid JSON arrays; returns `false` if either fails
/// to parse or if the arrays are not canonically equal.
///
/// # Examples
///
/// ```rust
/// use rskim_contract::canonical::tools_arrays_equal;
///
/// let original = r#"[{"name":"search","description":"Search files"}]"#;
/// let reordered = r#"[{"description":"Search files","name":"search"}]"#;
/// // Key reorder within a tool object → equal
/// assert!(tools_arrays_equal(original, reordered));
///
/// // Modified description → not equal
/// let modified = r#"[{"name":"search","description":"MODIFIED"}]"#;
/// assert!(!tools_arrays_equal(original, modified));
/// ```
pub fn tools_arrays_equal(raw_original: &str, raw_reordered: &str) -> bool {
    // Use the raw-node path so numbers are compared as token bytes, not as
    // parsed f64 values (which would normalise 1e3 == 1000 without
    // arbitrary_precision). This is the canonical path for AC11.
    let (Some(a), Some(b)) = (
        parse_raw_node(raw_original, 0),
        parse_raw_node(raw_reordered, 0),
    ) else {
        return false;
    };
    // Both must be arrays.
    matches!((&a, &b), (RawNode::Array(_), RawNode::Array(_))) && raw_nodes_equal(&a, &b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ========================================================================
    // canonical_equal tests
    // ========================================================================

    #[test]
    fn equal_null_values() {
        assert!(canonical_equal(&Value::Null, &Value::Null));
    }

    #[test]
    fn equal_bool_values() {
        assert!(canonical_equal(&json!(true), &json!(true)));
        assert!(!canonical_equal(&json!(true), &json!(false)));
    }

    #[test]
    fn equal_string_values() {
        assert!(canonical_equal(&json!("hello"), &json!("hello")));
        assert!(!canonical_equal(&json!("hello"), &json!("world")));
    }

    #[test]
    fn equal_number_values() {
        assert!(canonical_equal(&json!(42), &json!(42)));
        assert!(!canonical_equal(&json!(42), &json!(43)));
    }

    #[test]
    fn object_key_order_insensitive() {
        let a = json!({"b": 1, "a": 2});
        let b = json!({"a": 2, "b": 1});
        assert!(canonical_equal(&a, &b));
    }

    #[test]
    fn object_different_values_not_equal() {
        let a = json!({"key": 1});
        let b = json!({"key": 2});
        assert!(!canonical_equal(&a, &b));
    }

    #[test]
    fn object_different_keys_not_equal() {
        let a = json!({"key_a": 1});
        let b = json!({"key_b": 1});
        assert!(!canonical_equal(&a, &b));
    }

    #[test]
    fn object_different_key_count_not_equal() {
        let a = json!({"a": 1, "b": 2});
        let b = json!({"a": 1});
        assert!(!canonical_equal(&a, &b));
    }

    #[test]
    fn array_order_sensitive() {
        let a = json!([1, 2, 3]);
        let b = json!([3, 2, 1]);
        assert!(!canonical_equal(&a, &b));
    }

    #[test]
    fn array_equal_same_order() {
        let a = json!([1, 2, 3]);
        let b = json!([1, 2, 3]);
        assert!(canonical_equal(&a, &b));
    }

    #[test]
    fn array_different_length_not_equal() {
        let a = json!([1, 2]);
        let b = json!([1, 2, 3]);
        assert!(!canonical_equal(&a, &b));
    }

    #[test]
    fn type_mismatch_not_equal() {
        assert!(!canonical_equal(&json!(1), &json!("1")));
        assert!(!canonical_equal(&json!(null), &json!(false)));
        assert!(!canonical_equal(&json!([]), &json!({})));
    }

    #[test]
    fn nested_objects_key_order_insensitive() {
        let a = json!({"outer": {"b": 2, "a": 1}});
        let b = json!({"outer": {"a": 1, "b": 2}});
        assert!(canonical_equal(&a, &b));
    }

    #[test]
    fn depth_exceeded_returns_false() {
        // Construct a deeply nested value.
        let mut v = json!(42);
        for _ in 0..MAX_CANONICAL_DEPTH + 5 {
            v = json!([v]);
        }
        // canonical_equal bails at depth limit → false (conservative).
        assert!(!canonical_equal(&v, &v));
    }

    // ========================================================================
    // raw_numbers_equal tests (AC11)
    // ========================================================================

    #[test]
    fn raw_numbers_same_token_equal() {
        assert!(raw_numbers_equal("1e3", "1e3"));
        assert!(raw_numbers_equal("42", "42"));
        assert!(raw_numbers_equal("1.5", "1.5"));
    }

    #[test]
    fn raw_numbers_different_tokens_not_equal_even_if_same_value() {
        // The key invariant 6 assertion: JCS would normalise these.
        assert!(!raw_numbers_equal("1e3", "1000"));
        assert!(!raw_numbers_equal("1.0", "1"));
        assert!(!raw_numbers_equal("1.500", "1.5")); // trailing zero differs
    }

    // ========================================================================
    // tools_arrays_equal tests (AC11)
    // ========================================================================

    #[test]
    fn tools_arrays_equal_key_reorder() {
        let original = r#"[{"name":"search","description":"Search files"}]"#;
        let reordered = r#"[{"description":"Search files","name":"search"}]"#;
        assert!(tools_arrays_equal(original, reordered));
    }

    #[test]
    fn tools_arrays_modified_description_not_equal() {
        let original = r#"[{"name":"search","description":"Search files"}]"#;
        let modified = r#"[{"name":"search","description":"Find files"}]"#;
        assert!(!tools_arrays_equal(original, modified));
    }

    #[test]
    fn tools_arrays_invalid_json_returns_false() {
        assert!(!tools_arrays_equal("{not json}", "[]"));
        assert!(!tools_arrays_equal("[]", "{not json}"));
    }

    #[test]
    fn tools_arrays_non_array_returns_false() {
        // Both must be arrays.
        assert!(!tools_arrays_equal(
            r#"{"name":"tool"}"#,
            r#"[{"name":"tool"}]"#
        ));
    }

    #[test]
    fn tools_arrays_equal_empty() {
        assert!(tools_arrays_equal("[]", "[]"));
    }

    #[test]
    fn tools_arrays_different_length_not_equal() {
        let a = r#"[{"name":"a"},{"name":"b"}]"#;
        let b = r#"[{"name":"a"}]"#;
        assert!(!tools_arrays_equal(a, b));
    }

    // ========================================================================
    // canonical_equal_raw tests
    // ========================================================================

    #[test]
    fn canonical_equal_raw_simple_match() {
        assert_eq!(canonical_equal_raw("42", "42"), Some(true));
        assert_eq!(canonical_equal_raw("\"hello\"", "\"hello\""), Some(true));
        assert_eq!(canonical_equal_raw("null", "null"), Some(true));
    }

    #[test]
    fn canonical_equal_raw_invalid_json_returns_none() {
        assert_eq!(canonical_equal_raw("{invalid}", "{}"), None);
    }

    #[test]
    fn canonical_equal_raw_object_key_order() {
        let a = r#"{"b":1,"a":2}"#;
        let b = r#"{"a":2,"b":1}"#;
        assert_eq!(canonical_equal_raw(a, b), Some(true));
    }

    // ========================================================================
    // AC11 critical gate: raw-token number comparison in canonical path
    // ========================================================================

    /// AC11 gate: `tools_arrays_equal` must treat `1e3` and `1000` as distinct
    /// tokens (different cache key bytes) even though they are mathematically equal.
    ///
    /// Without raw-token comparison, serde_json would normalise both to f64=1000.0
    /// and they would compare equal — silently breaking the cache-key invariant.
    #[test]
    fn tools_arrays_equal_raw_number_token_not_normalised() {
        // A tool with number literal `1e3` in a schema property.
        let original = r#"[{"name":"t","parameters":{"properties":{"n":{"default":1e3}}}}]"#;
        // Same value but different token representation — must be NOT equal.
        let normalised = r#"[{"name":"t","parameters":{"properties":{"n":{"default":1000}}}}]"#;
        assert!(
            !tools_arrays_equal(original, normalised),
            "1e3 and 1000 must be treated as distinct raw tokens (AC11)"
        );
    }

    /// AC11 gate: `canonical_equal_raw` must treat `1.0` and `1` as distinct tokens.
    #[test]
    fn canonical_equal_raw_number_token_1_0_vs_1_not_equal() {
        // 1.0 and 1 have different raw bytes → must NOT be equal.
        assert_eq!(
            canonical_equal_raw("1.0", "1"),
            Some(false),
            "1.0 and 1 must be distinct raw tokens (AC11)"
        );
    }

    /// AC11 gate: matching raw number tokens → equal.
    #[test]
    fn canonical_equal_raw_same_number_token_equal() {
        assert_eq!(canonical_equal_raw("1e3", "1e3"), Some(true));
        assert_eq!(canonical_equal_raw("1000", "1000"), Some(true));
    }

    // ========================================================================
    // AC9 map-iteration determinism: canonical_equal_raw / tools_arrays_equal
    // on objects with many keys in different source orders.
    //
    // AC9's plan mandates a fixture "exercising any internal map iteration"
    // that asserts map-backed output is deterministic. The RawNode::Object arm
    // in `parse_raw_node` uses HashMap, whose iteration order varies per run.
    // However, `raw_nodes_equal` is order-insensitive (it finds each key via
    // `.find()`), so the boolean RESULT is deterministic even if HashMap
    // iteration order is not. These tests make that property observable.
    //
    // Note: per-process HashMap seed is fixed at process start (Rust's
    // standard HashMap uses RandomState seeded once), so within a single
    // process run, iteration order is stable but NOT guaranteed to be the
    // same across processes. The equality RESULT must be stable across all
    // orderings because `raw_nodes_equal` uses order-insensitive comparison.
    // ========================================================================

    /// AC9 map-iteration fixture: two objects with the same keys in different
    /// source orders must compare equal via `canonical_equal_raw`.
    ///
    /// This is the canonical AC9 map-iteration test: the raw JSON strings
    /// differ in key order, so `parse_raw_node` will encounter the keys in
    /// different HashMap iteration sequences, yet the result must be `true`.
    #[test]
    fn canonical_equal_raw_many_keys_different_source_order() {
        // 10 key object in two different source orderings.
        let a = r#"{"k1":1,"k2":2,"k3":3,"k4":4,"k5":5,"k6":6,"k7":7,"k8":8,"k9":9,"k10":10}"#;
        let b = r#"{"k10":10,"k9":9,"k8":8,"k7":7,"k6":6,"k5":5,"k4":4,"k3":3,"k2":2,"k1":1}"#;
        assert_eq!(
            canonical_equal_raw(a, b),
            Some(true),
            "objects with same keys in different source order must be equal (AC9 map-iteration)"
        );
        // Same keys, same values — repeated call must produce the same result.
        assert_eq!(
            canonical_equal_raw(a, b),
            Some(true),
            "canonical_equal_raw must be deterministic across repeated calls (AC9)"
        );
    }

    /// AC9 map-iteration fixture for `tools_arrays_equal`: tool objects with
    /// many keys in different source orders must compare equal.
    ///
    /// This exercises `parse_raw_node`'s HashMap arm on a realistic tool schema
    /// and confirms the order-insensitive `.find()` loop produces the same verdict
    /// regardless of HashMap iteration sequence.
    #[test]
    fn tools_arrays_equal_many_keys_different_source_order() {
        let original = r#"[{"name":"t","description":"desc","parameters":{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"integer"},"c":{"type":"boolean"},"d":{"type":"number"},"e":{"type":"array","items":{"type":"string"}}}}}]"#;
        // Same content with shuffled key order inside the tool and parameters objects.
        let reordered = r#"[{"parameters":{"properties":{"e":{"items":{"type":"string"},"type":"array"},"d":{"type":"number"},"c":{"type":"boolean"},"b":{"type":"integer"},"a":{"type":"string"}},"type":"object"},"description":"desc","name":"t"}]"#;
        assert!(
            tools_arrays_equal(original, reordered),
            "tools_arrays_equal must be order-insensitive for object keys (AC9 map-iteration)"
        );
        // Determinism: same call produces the same result.
        assert!(
            tools_arrays_equal(original, reordered),
            "tools_arrays_equal must be deterministic across repeated calls (AC9)"
        );
    }

    /// AC9 map-iteration negative case: objects with same keys but different values
    /// must compare NOT equal, regardless of iteration order.
    #[test]
    fn canonical_equal_raw_same_keys_different_values_not_equal() {
        let a = r#"{"k1":1,"k2":2,"k3":3,"k4":4,"k5":99}"#;
        let b = r#"{"k5":100,"k4":4,"k3":3,"k2":2,"k1":1}"#; // k5 differs
        assert_eq!(
            canonical_equal_raw(a, b),
            Some(false),
            "objects with same keys but different values must not be equal (AC9 negative)"
        );
    }
}
