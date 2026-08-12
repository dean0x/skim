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
///
/// # Relationship to `canonical_bytes_of_node`
///
/// `raw_nodes_equal(a, b)` iff `canonical_bytes_of_node(a) == canonical_bytes_of_node(b)`.
/// This is the property exploited by [`tools_arrays_set_equal`] to replace the O(n²)
/// greedy scan with an O(n log n) sort-and-compare.
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
            //
            // This linear scan per key is O(n²) in the number of object keys.
            // The bound is safe in practice: `parse_raw_node` only builds a
            // `RawNode::Object` for inputs that passed the `MAX_CANONICAL_DEPTH`
            // gate (64 levels), and real tool-schema objects are shallow (typically
            // 4–10 top-level keys). A HashMap rewrite would add allocation and
            // complexity for no measurable gain on this off-hot-path check.
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

/// Serialize a `RawNode` tree to its canonical compact bytes.
///
/// # Canonical form
///
/// - `Null` → `b"null"`
/// - `Bool` → `b"true"` / `b"false"`
/// - `Number(raw)` → verbatim raw token bytes (preserves `1e3`, `1.0`, etc.)
/// - `JsonString(s)` → the decoded string re-serialized as compact JSON
///   (normalises `"A"` → `"A"`, matching the decoded-value comparison in
///   [`raw_nodes_equal`])
/// - `Array(elems)` → compact JSON array, elements in source order
/// - `Object(pairs)` → compact JSON object with keys in BTreeMap-sorted order
///   (guaranteed by [`parse_raw_node`], which builds objects via `BTreeMap`)
///
/// # Semantic equivalence with `raw_nodes_equal`
///
/// `raw_nodes_equal(a, b)` iff `canonical_bytes_of_node(a) == canonical_bytes_of_node(b)`.
///
/// Proof sketch:
/// - **Object**: `raw_nodes_equal` is key-order-insensitive; `canonical_bytes_of_node`
///   iterates in BTreeMap order (both `a` and `b` were parsed via BTreeMap, so same
///   sorted key order). Equal objects → same key-value pairs → same sorted-key bytes.
/// - **Number**: both compare raw token bytes directly.
/// - **String**: both compare decoded values; re-serialization produces identical
///   compact bytes for the same decoded string.
/// - **Array/null/bool**: direct structural equality → identical serialization.
///
/// # Depth bound (AC17)
///
/// The `RawNode` tree depth is bounded to [`MAX_CANONICAL_DEPTH`] by [`parse_raw_node`].
/// `canonical_bytes_of_node` recurses only into children of already-parsed nodes,
/// so its recursion depth is also ≤ `MAX_CANONICAL_DEPTH`.
fn canonical_bytes_of_node(node: &RawNode) -> Vec<u8> {
    match node {
        RawNode::Null => b"null".to_vec(),
        RawNode::Bool(true) => b"true".to_vec(),
        RawNode::Bool(false) => b"false".to_vec(),
        RawNode::Number(raw) => raw.as_bytes().to_vec(),
        RawNode::JsonString(s) => {
            // Re-serialise the decoded string to canonical compact JSON.
            // `serde_json::to_vec` for a `&str` is infallible: Rust strings
            // are always valid UTF-8 and always representable as JSON strings.
            serde_json::to_vec(s.as_str()).unwrap_or_else(|_| {
                // Unreachable: String is always a valid JSON string value.
                // Emit a quoted copy as a last-resort defensive fallback so that
                // the serialization is at worst pessimistic (two inputs that
                // differ only in this unreachable case will produce differing bytes
                // and be treated as non-equal — the conservative outcome).
                let mut out = Vec::with_capacity(s.len() + 2);
                out.push(b'"');
                out.extend_from_slice(s.as_bytes());
                out.push(b'"');
                out
            })
        }
        RawNode::Array(elems) => {
            let mut out = vec![b'['];
            for (i, elem) in elems.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(&canonical_bytes_of_node(elem));
            }
            out.push(b']');
            out
        }
        RawNode::Object(pairs) => {
            // Pairs are already in BTreeMap-sorted order (guaranteed at parse time
            // by `parse_raw_node`, which builds objects via a `BTreeMap` that
            // iterates in lexicographic key order).
            let mut out = vec![b'{'];
            for (i, (key, val)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                // Serialise the key as a JSON string.  Infallible for &str.
                let key_bytes = serde_json::to_vec(key.as_str()).unwrap_or_else(|_| {
                    // Unreachable: String key is always a valid JSON string.
                    let mut b = Vec::with_capacity(key.len() + 2);
                    b.push(b'"');
                    b.extend_from_slice(key.as_bytes());
                    b.push(b'"');
                    b
                });
                out.extend_from_slice(&key_bytes);
                out.push(b':');
                out.extend_from_slice(&canonical_bytes_of_node(val));
            }
            out.push(b'}');
            out
        }
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

/// Check **multiset (bag) equality** between two tools arrays.
///
/// This is the **set-equality gate** used exclusively by `#306` (`CacheAlignStage`)
/// to verify that a deterministic element-reorder of the tools array dropped, duplicated,
/// and mutated nothing. It is the complement of the order-sensitive [`tools_arrays_equal`],
/// which remains the gate for every other transform (preserving strict byte round-trip
/// globally and narrowing the set-equality relaxation to this one deliberate reorder).
///
/// # AD-CA-7 — ADR-007 re-argued for element reorder
///
/// A deliberate tools-array element reorder cannot satisfy byte-round-trip (the elements
/// appear in a different order). ADR-007's losslessness gate for this specific transform
/// is therefore **set-equality + total-order determinism** (approved by the user's
/// 2026-07-17 sign-off). This function proves the reorder lost, duplicated, and mutated
/// nothing, while permitting a deliberate order change; [`tools_arrays_equal`] stays
/// order-sensitive so every other transform keeps strict round-trip.
///
/// # Algorithm
///
/// Parse both `raw_a` and `raw_b` as JSON arrays. Require equal length (a length mismatch
/// immediately returns `false`). Convert each element to a canonical byte string via
/// [`canonical_bytes_of_node`], sort both vectors, and compare them pairwise. Two arrays
/// are set-equal iff their sorted canonical-byte vectors are identical.
///
/// Complexity: O(n log n) sort + O(n · |elem|) comparison, where `n` is the element count
/// and `|elem|` is the average canonical byte length per element.
///
/// # Number comparison
///
/// Uses **raw source-token bytes** (the AC11 / Decision 1 invariant). `1e3` and `1000`
/// are treated as distinct tokens even though they are mathematically equal. This is
/// the correct invariant for cache-key faithfulness.
///
/// # Returns
///
/// - `true` — both arrays parse successfully, have equal length, and their sorted
///   canonical-byte vectors are identical
/// - `false` — either array fails to parse, arrays differ in length, or any element's
///   canonical bytes differ after sorting
///
/// # Examples
///
/// ```rust
/// use rskim_contract::canonical::tools_arrays_set_equal;
///
/// // Permutation → equal under set equality
/// let a = r#"[{"name":"a"},{"name":"b"}]"#;
/// let b = r#"[{"name":"b"},{"name":"a"}]"#;
/// assert!(tools_arrays_set_equal(a, b));
///
/// // Same order → also equal
/// assert!(tools_arrays_set_equal(a, a));
///
/// // Drop one element → false
/// let dropped = r#"[{"name":"a"}]"#;
/// assert!(!tools_arrays_set_equal(a, dropped));
///
/// // Duplicate one element → false (b has two "a", a has one "a" + one "b")
/// let dup = r#"[{"name":"a"},{"name":"a"}]"#;
/// assert!(!tools_arrays_set_equal(a, dup));
///
/// // Mutated element → false
/// let mutated = r#"[{"name":"a"},{"name":"CHANGED"}]"#;
/// assert!(!tools_arrays_set_equal(a, mutated));
/// ```
pub fn tools_arrays_set_equal(raw_a: &str, raw_b: &str) -> bool {
    // Parse both arrays using the raw-node path so numbers are compared as
    // token bytes (AC11 invariant). Fail-open (false) on any parse error.
    let (a_node, b_node) = match (parse_raw_node(raw_a, 0), parse_raw_node(raw_b, 0)) {
        (Some(a), Some(b)) => (a, b),
        _ => return false,
    };

    // Both must be arrays.
    let (a_elems, b_elems) = match (a_node, b_node) {
        (RawNode::Array(a), RawNode::Array(b)) => (a, b),
        _ => return false,
    };

    // Require equal length — a drop or duplication changes the count.
    if a_elems.len() != b_elems.len() {
        return false;
    }

    // Fast path: empty arrays are trivially equal.
    if a_elems.is_empty() {
        return true;
    }

    // O(n log n) multiset equality via canonical-bytes sort-and-compare.
    //
    // `canonical_bytes_of_node` produces a deterministic compact JSON byte string
    // for each element, with the property:
    //
    //   raw_nodes_equal(a, b)  iff  canonical_bytes_of_node(a) == canonical_bytes_of_node(b)
    //
    // Sorting both sides' byte-string vectors and comparing pairwise is therefore
    // equivalent to multiset equality:
    //  - Any element drop or duplication changes the sorted-byte multiset → mismatch.
    //  - Any content mutation changes at least one element's canonical bytes → mismatch.
    //  - A pure permutation changes neither the multiset nor any element's bytes → equal.
    //
    // Complexity: O(n log n) sort + O(n · |elem|) comparison, versus the former
    // O(n²) greedy scan with O(k²) raw_nodes_equal calls per element pair (where k
    // is the number of object keys per element). At 64 tools the old code made ~4096
    // deep tree comparisons; the new code sorts 64 compact byte strings.
    //
    // Semantics unchanged: the AC11 raw-token number invariant is preserved because
    // `canonical_bytes_of_node` copies number tokens verbatim (see its implementation).
    let mut a_keys: Vec<Vec<u8>> = a_elems.iter().map(canonical_bytes_of_node).collect();
    let mut b_keys: Vec<Vec<u8>> = b_elems.iter().map(canonical_bytes_of_node).collect();

    a_keys.sort_unstable();
    b_keys.sort_unstable();

    a_keys == b_keys
}

// ============================================================================
// value_equivalent_raw — O(n) value-equivalence comparator (#427 Pass 2)
// ============================================================================

/// Maximum recursion depth for [`value_equivalent_raw`].
///
/// Set to 500 to match `MAX_JSON_DEPTH` in `crates/rskim-compress/src/engines/json.rs`.
///
/// **Rationale for 500 (not 64):**
/// This comparator verifies the lossless gate on arbitrary JSON content blocks —
/// not just shallow tool-schema objects. The json.rs minifier already accepts inputs
/// up to depth 500; using a tighter bound here would cause spurious `None` returns
/// for valid minified content near depth 64. Setting both bounds to 500 ensures the
/// gate never rejects valid output that the minifier accepted.
///
/// **Effective limit in practice:**
/// `serde_json` enforces its own internal recursion limit of approximately 128
/// levels during JSON parsing. For any input nested deeper than ~128 levels,
/// `serde_json::from_str` returns an error, causing `value_equiv_inner` to return
/// `None` (parse failure) before the `VALUE_EQUIV_MAX_DEPTH` guard fires. In
/// practice this means inputs nested between ~128 and 500 levels deep yield `None`
/// (passthrough, fail-safe), not the `Some(false)` that a correct depth check would
/// produce. This is acceptable: the gate's contract is to pass `Some(true)` for
/// value-equivalent inputs and to pass through (`None`) when uncertain.
///
/// `MAX_CANONICAL_DEPTH` (64) is intentionally different: it is for tools-array
/// waiver verification, which operates on shallow (4–10 level) tool schemas.
const VALUE_EQUIV_MAX_DEPTH: usize = 500;

/// Parse all key-value pairs from a raw JSON object string, preserving duplicates.
///
/// Returns `None` if:
/// - `raw` is not a valid JSON object.
/// - Any key appears more than once (duplicate key detected — not provable).
///
/// Returns `Some(pairs)` where `pairs` contains `(key, raw_value)` for each
/// entry in source order. The `Box<RawValue>` values are owned (bytes copied).
///
/// **O(n) duplicate detection:** a `HashSet<String>` is maintained during the
/// streaming walk — insertion is O(1) amortised, so total complexity is O(n).
fn parse_object_entries(raw: &str) -> Option<Vec<(String, Box<serde_json::value::RawValue>)>> {
    use serde::de::{MapAccess, Visitor};
    use std::fmt;

    struct PairsVisitor;

    impl<'de> Visitor<'de> for PairsVisitor {
        type Value = Vec<(String, Box<serde_json::value::RawValue>)>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a JSON object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut pairs: Vec<(String, Box<serde_json::value::RawValue>)> = Vec::new();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

            while let Some(k) = map.next_key::<String>()? {
                let v: Box<serde_json::value::RawValue> = map.next_value()?;
                if !seen.insert(k.clone()) {
                    // Duplicate key: the object is not provably representable.
                    return Err(serde::de::Error::custom("duplicate key"));
                }
                pairs.push((k, v));
            }
            Ok(pairs)
        }
    }

    // serde_json's streaming Deserializer yields ALL key-value pairs in source
    // order, including duplicates. PairsVisitor returns an Err on the first
    // duplicate, which `deserialize_map` propagates → `from_str` returns `Err`.
    struct Pairs(Vec<(String, Box<serde_json::value::RawValue>)>);

    impl<'de> serde::Deserialize<'de> for Pairs {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_map(PairsVisitor).map(Pairs)
        }
    }

    serde_json::from_str::<Pairs>(raw).ok().map(|p| p.0)
}

/// JSON value-equivalence comparator over two raw `&str` inputs.
///
/// # Semantics
///
/// Returns `Some(true)` if the two JSON values are proven value-equivalent,
/// `Some(false)` if they are proven different, and `None` if value-equivalence
/// cannot be determined (see cases below).
///
/// ## Comparison rules
///
/// | JSON type | Comparison rule |
/// |-----------|-----------------|
/// | null | Always equal |
/// | bool | Token byte equality (`true`/`true` or `false`/`false`) |
/// | number | **Raw byte equality** — `1e3` vs `1000` → `Some(false)` |
/// | string | Decoded value equality — `"A"` vs `"A"` → `Some(true)` |
/// | object | Key-order-insensitive, hashed per-object `HashMap<&str, &str>` |
/// | array | Order-sensitive, element-by-element |
///
/// Number tokens are compared as **raw byte strings** (not as parsed f64 values),
/// matching the invariant-6 rule from [`canonical_equal_raw`].
///
/// ## Tri-state `None` cases
///
/// `None` is returned (not provable) when:
/// - Either input fails to parse as valid JSON.
/// - Either object input contains duplicate keys.
/// - Recursion depth exceeds [`VALUE_EQUIV_MAX_DEPTH`].
/// - The `budget` counter is exhausted (reaches zero) during traversal.
///
/// ## Work budget
///
/// `budget` is a caller-owned saturating counter. Each value node visited
/// (each recursive call) decrements `budget` by 1. When `budget` reaches 0,
/// `None` is returned immediately. Callers can share a budget across multiple
/// calls to bound total work per request.
///
/// ## Performance
///
/// Object key lookup uses a `HashMap<&str, &str>` built from the first object's
/// entries — O(1) amortised per lookup, O(n) total per object. This is O(n)
/// overall, unlike `canonical_equal_raw`'s `raw_nodes_equal` which uses an
/// O(n²) linear scan per object (intentionally — `canonical_equal_raw` is for
/// shallow tool schemas; this function handles arbitrary-depth content blocks).
///
/// # Examples
///
/// ```rust
/// use rskim_contract::canonical::value_equivalent_raw;
///
/// let mut budget = 1000;
///
/// // Raw number byte inequality: 1e3 ≠ 1000.
/// assert_eq!(value_equivalent_raw("1e3", "1000", &mut budget), Some(false));
///
/// // String decoded equality: "A" == "A".
/// assert_eq!(value_equivalent_raw(r#""A""#, r#""A""#, &mut budget), Some(true));
///
/// // Key-order-insensitive objects.
/// assert_eq!(
///     value_equivalent_raw(r#"{"a":1,"b":2}"#, r#"{"b":2,"a":1}"#, &mut budget),
///     Some(true)
/// );
///
/// // Duplicate key → None.
/// assert_eq!(
///     value_equivalent_raw(r#"{"a":1,"a":2}"#, r#"{"a":1}"#, &mut budget),
///     None
/// );
/// ```
pub fn value_equivalent_raw(raw_a: &str, raw_b: &str, budget: &mut usize) -> Option<bool> {
    value_equiv_inner(raw_a, raw_b, 0, budget)
}

/// Internal recursive implementation of [`value_equivalent_raw`].
fn value_equiv_inner(raw_a: &str, raw_b: &str, depth: usize, budget: &mut usize) -> Option<bool> {
    if depth > VALUE_EQUIV_MAX_DEPTH {
        return None;
    }
    if *budget == 0 {
        return None;
    }
    *budget = budget.saturating_sub(1);

    // Parse both tokens via RawValue to obtain trimmed token bytes without
    // allocating a full serde_json::Value tree.
    let a_raw: Box<serde_json::value::RawValue> = serde_json::from_str(raw_a).ok()?;
    let b_raw: Box<serde_json::value::RawValue> = serde_json::from_str(raw_b).ok()?;

    let a_tok = a_raw.get().trim();
    let b_tok = b_raw.get().trim();

    let a_first = *a_tok.as_bytes().first()?;
    let b_first = *b_tok.as_bytes().first()?;

    // Classify both values by their first byte. In valid JSON:
    //   'n' → null,  't'/'f' → bool,  '"' → string,  '[' → array,  '{' → object
    //   '-' or digit → number
    let a_is_num = a_first == b'-' || a_first.is_ascii_digit();
    let b_is_num = b_first == b'-' || b_first.is_ascii_digit();

    match (a_first, b_first) {
        // null == null
        (b'n', b'n') => Some(true),

        // bool: same first byte means same value in valid JSON (only "true"/"false")
        (b't', b't') | (b'f', b'f') => Some(true),
        // bool type mismatch: one true, one false
        (b't', b'f') | (b'f', b't') => Some(false),

        // strings: compare DECODED values (e.g. "A" == "A")
        (b'"', b'"') => {
            let a_s: String = serde_json::from_str(a_tok).ok()?;
            let b_s: String = serde_json::from_str(b_tok).ok()?;
            Some(a_s == b_s)
        }

        // numbers: compare RAW BYTE strings — 1e3 ≠ 1000 (AC11 invariant)
        _ if a_is_num && b_is_num => Some(a_tok == b_tok),

        // arrays: element-by-element, order-sensitive
        (b'[', b'[') => {
            let a_elems: Vec<Box<serde_json::value::RawValue>> =
                serde_json::from_str(a_tok).ok()?;
            let b_elems: Vec<Box<serde_json::value::RawValue>> =
                serde_json::from_str(b_tok).ok()?;
            if a_elems.len() != b_elems.len() {
                return Some(false);
            }
            for (ae, be) in a_elems.iter().zip(b_elems.iter()) {
                match value_equiv_inner(ae.get(), be.get(), depth + 1, budget) {
                    None => return None,
                    Some(false) => return Some(false),
                    Some(true) => {}
                }
            }
            Some(true)
        }

        // objects: key-order-insensitive, O(n) via HashMap
        (b'{', b'{') => {
            // parse_object_entries returns None on dup keys or parse failure.
            let a_pairs = parse_object_entries(a_tok)?;
            let b_pairs = parse_object_entries(b_tok)?;

            if a_pairs.len() != b_pairs.len() {
                return Some(false);
            }

            // Build a HashMap from a's entries: key str → raw value str.
            // Borrows from a_pairs which is live for this scope.
            let a_map: std::collections::HashMap<&str, &str> =
                a_pairs.iter().map(|(k, v)| (k.as_str(), v.get())).collect();

            // For each entry in b, look up in a and recurse.
            for (bk, bv) in &b_pairs {
                match a_map.get(bk.as_str()) {
                    None => return Some(false), // key in b not in a
                    Some(&av_str) => match value_equiv_inner(av_str, bv.get(), depth + 1, budget) {
                        None => return None,
                        Some(false) => return Some(false),
                        Some(true) => {}
                    },
                }
            }
            Some(true)
        }

        // Any other first-byte combination is a type mismatch (e.g. number vs string).
        _ => Some(false),
    }
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
    // tools_arrays_set_equal tests (AD-CA-7 multiset gate, AC29)
    // ========================================================================

    /// Permutation → true (the primary use case: element reorder is still set-equal).
    ///
    /// Discriminating: if this were [`tools_arrays_equal`] (order-sensitive), it would
    /// return `false` for a permuted array, proving the two functions serve different roles.
    #[test]
    fn tools_arrays_set_equal_permutation_is_true() {
        let a = r#"[{"name":"search","description":"Search files"},{"name":"compile","description":"Compile"}]"#;
        let b = r#"[{"name":"compile","description":"Compile"},{"name":"search","description":"Search files"}]"#;
        assert!(
            tools_arrays_set_equal(a, b),
            "permuted arrays must be set-equal"
        );
        // Confirm the order-sensitive check disagrees (non-tautology proof).
        assert!(
            !tools_arrays_equal(a, b),
            "permuted arrays must NOT be order-equal (proves tools_arrays_equal is distinct)"
        );
    }

    /// Same order → also set-equal (symmetric, reflexive).
    #[test]
    fn tools_arrays_set_equal_same_order_is_true() {
        let a = r#"[{"name":"a"},{"name":"b"},{"name":"c"}]"#;
        assert!(
            tools_arrays_set_equal(a, a),
            "identical arrays must be set-equal"
        );
    }

    /// Drop one element → false (length mismatch catches it).
    ///
    /// Discriminating: the length check at the top of `tools_arrays_set_equal` returns
    /// `false` immediately on a length mismatch. If that check were removed, the
    /// subsequent sorted-vector comparison (`a_keys == b_keys`) would still return
    /// `false` for different-length inputs (Rust's `Vec` equality is length-sensitive),
    /// but the early-exit optimisation would be absent.
    #[test]
    fn tools_arrays_set_equal_drop_is_false() {
        let a = r#"[{"name":"a"},{"name":"b"}]"#;
        let dropped = r#"[{"name":"a"}]"#;
        assert!(
            !tools_arrays_set_equal(a, dropped),
            "dropped element must fail set-equality"
        );
        assert!(
            !tools_arrays_set_equal(dropped, a),
            "set-equality must be symmetric: drop fails in both directions"
        );
    }

    /// Duplicate one element → false.
    ///
    /// `b` has two copies of `{"name":"a"}` and zero copies of `{"name":"b"}`.
    /// After sorting, `a`'s canonical keys are `[b"a", b"b"]` and `b`'s are
    /// `[b"a", b"a"]`; the pairwise comparison fails at the second element.
    ///
    /// Discriminating: the sort-and-compare correctly distinguishes a duplicated
    /// element from the original because the sorted multisets differ.
    #[test]
    fn tools_arrays_set_equal_duplicate_is_false() {
        let a = r#"[{"name":"a"},{"name":"b"}]"#;
        let dup = r#"[{"name":"a"},{"name":"a"}]"#; // "b" replaced with second "a"
        assert!(
            !tools_arrays_set_equal(a, dup),
            "array with duplicated element must fail set-equality"
        );
    }

    /// Single-element mutation → false.
    ///
    /// One element in `b` has its description changed. The raw-token comparison
    /// (`raw_nodes_equal`) catches the content mutation.
    ///
    /// Discriminating: if raw-token comparison were replaced with structural
    /// type-only comparison (ignoring string values), this would return true,
    /// letting a mutated description through the gate.
    #[test]
    fn tools_arrays_set_equal_mutation_is_false() {
        let original = r#"[{"name":"search","description":"Search files"},{"name":"compile","description":"Compile"}]"#;
        let mutated = r#"[{"name":"compile","description":"Compile"},{"name":"search","description":"MUTATED"}]"#;
        assert!(
            !tools_arrays_set_equal(original, mutated),
            "mutated element must fail set-equality"
        );
    }

    /// Empty arrays → set-equal (vacuous).
    #[test]
    fn tools_arrays_set_equal_empty_is_true() {
        assert!(
            tools_arrays_set_equal("[]", "[]"),
            "empty arrays are set-equal"
        );
    }

    /// Non-array inputs → false.
    #[test]
    fn tools_arrays_set_equal_non_array_is_false() {
        assert!(
            !tools_arrays_set_equal(r#"{"name":"a"}"#, r#"[{"name":"a"}]"#),
            "object is not set-equal to array"
        );
    }

    /// Raw-number token faithfulness in set-equality (AC11 invariant preserved).
    ///
    /// `1e3` and `1000` are different raw tokens; set-equality must treat them as
    /// distinct elements. Without raw-token comparison, they would normalise to f64
    /// and the mutation would be invisible.
    #[test]
    fn tools_arrays_set_equal_raw_number_mutation_is_false() {
        let original = r#"[{"name":"t","parameters":{"default":1e3}},{"name":"u"}]"#;
        let mutated = r#"[{"name":"u"},{"name":"t","parameters":{"default":1000}}]"#;
        assert!(
            !tools_arrays_set_equal(original, mutated),
            "1e3 vs 1000 raw-token mutation must fail set-equality (AC11)"
        );
    }

    /// Genuine duplicates in both arrays are handled correctly (count-preserving).
    ///
    /// If both `a` and `b` contain two identical elements, set-equality must pass —
    /// the multiset {x, x} equals the multiset {x, x}.
    #[test]
    fn tools_arrays_set_equal_genuine_duplicates_both_sides() {
        let a = r#"[{"name":"x"},{"name":"x"}]"#;
        let b = r#"[{"name":"x"},{"name":"x"}]"#;
        assert!(
            tools_arrays_set_equal(a, b),
            "identical arrays with genuine duplicates must be set-equal"
        );
    }

    // ========================================================================
    // proptest cross-check: new O(n log n) implementation vs O(n²) reference
    //
    // The new sort-and-compare implementation must agree with the original
    // greedy multiset matcher on every input reachable by the strategy below.
    // ========================================================================

    /// O(n²) reference implementation preserved for proptest cross-checking.
    ///
    /// This is the original greedy multiset matcher. It is intentionally kept
    /// here (rather than deleted) so the proptest can verify the new O(n log n)
    /// `tools_arrays_set_equal` produces identical results on all generated inputs.
    fn tools_arrays_set_equal_reference(raw_a: &str, raw_b: &str) -> bool {
        let (a_node, b_node) = match (parse_raw_node(raw_a, 0), parse_raw_node(raw_b, 0)) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };
        let (a_elems, b_elems) = match (a_node, b_node) {
            (RawNode::Array(a), RawNode::Array(b)) => (a, b),
            _ => return false,
        };
        if a_elems.len() != b_elems.len() {
            return false;
        }
        let mut used = vec![false; b_elems.len()];
        for a_elem in &a_elems {
            let matched = b_elems
                .iter()
                .enumerate()
                .find(|(j, b_elem)| !used[*j] && raw_nodes_equal(a_elem, b_elem));
            match matched {
                Some((j, _)) => used[j] = true,
                None => return false,
            }
        }
        true
    }

    use proptest::prelude::*;

    /// Strategy: generate a compact JSON representation of one tool element.
    ///
    /// Elements are simple objects to keep proptest shrinking fast while still
    /// covering the key shape space: name, optional description, optional integer,
    /// optional raw-number token (for AC11 coverage), optional nested object.
    ///
    /// Integer values are drawn from a fixed small set (not filtered arbitrary
    /// i32) to avoid prop_filter local-reject exhaustion at high rejection rates.
    fn arb_tool_element() -> impl Strategy<Value = String> {
        // Fixed-shape tools so keys are stable; proptest varies only values.
        prop_oneof![
            // Simple name-only tool
            prop_oneof![Just("alpha"), Just("beta"), Just("gamma"), Just("delta"),]
                .prop_map(|name| format!(r#"{{"name":"{name}"}}"#)),
            // Tool with name + description
            (
                prop_oneof![Just("search"), Just("read"), Just("write"), Just("exec")],
                prop_oneof![Just("do it"), Just("do something else"), Just("run")],
            )
                .prop_map(|(name, desc)| format!(r#"{{"name":"{name}","description":"{desc}"}}"#)),
            // Tool with name + integer parameter.
            // Use a small fixed set instead of filtered i32::ANY: a filter
            // that passes only 2M out of 4B values causes >65k local rejects
            // and proptest aborts the run.
            (
                prop_oneof![Just("compute"), Just("fetch"), Just("store")],
                prop_oneof![
                    Just(0i32),
                    Just(1),
                    Just(-1),
                    Just(42),
                    Just(-100),
                    Just(1000),
                ],
            )
                .prop_map(|(name, n)| {
                    format!(r#"{{"name":"{name}","parameters":{{"limit":{n}}}}}"#)
                }),
            // Tool with raw-number token (AC11 coverage: 1e3 vs 1000 are distinct)
            prop_oneof![Just("raw_num_1e3"), Just("raw_num_42")]
                .prop_map(|name| format!(r#"{{"name":"{name}","default":1e3}}"#)),
        ]
    }

    /// Strategy: generate a tools JSON array with 0–5 elements.
    fn arb_tools_array() -> impl Strategy<Value = String> {
        prop::collection::vec(arb_tool_element(), 0..=5)
            .prop_map(|elems| format!("[{}]", elems.join(",")))
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 300,
            max_shrink_iters: 500,
            ..ProptestConfig::default()
        })]

        /// Cross-check: new O(n log n) implementation must agree with the O(n²)
        /// reference greedy matcher on all generated (a, b) array pairs.
        ///
        /// This is the primary regression guard for the algorithmic refactor.
        /// Any input on which the two implementations disagree is a bug.
        #[test]
        fn prop_tools_arrays_set_equal_agrees_with_reference(
            a in arb_tools_array(),
            b in arb_tools_array(),
        ) {
            let new_result = tools_arrays_set_equal(&a, &b);
            let ref_result = tools_arrays_set_equal_reference(&a, &b);
            prop_assert_eq!(
                new_result, ref_result,
                "new O(n log n) impl disagrees with O(n²) reference for a={:?} b={:?}",
                a,
                b
            );
        }

        /// Cross-check: permuted array is always set-equal to original (both impls agree).
        #[test]
        fn prop_permuted_array_is_set_equal(
            elems in prop::collection::vec(arb_tool_element(), 0..=4),
            // Use a simple rotation as the permutation
            rotation in 0usize..=4,
        ) {
            if elems.is_empty() {
                return Ok(());
            }
            let rotation = rotation % elems.len();
            let a_str = format!("[{}]", elems.join(","));
            let mut rotated = elems.clone();
            rotated.rotate_right(rotation);
            let b_str = format!("[{}]", rotated.join(","));

            let new_result = tools_arrays_set_equal(&a_str, &b_str);
            let ref_result = tools_arrays_set_equal_reference(&a_str, &b_str);

            // Both must agree
            prop_assert_eq!(
                new_result, ref_result,
                "impls disagree on permuted array: a={:?} b={:?}",
                a_str,
                b_str
            );
            // And both must say true (permutation is always set-equal)
            prop_assert!(
                new_result,
                "permuted array must be set-equal: a={:?} b={:?}",
                a_str,
                b_str
            );
        }
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
    // in `parse_raw_node` uses BTreeMap (deterministic key order), so
    // `raw_nodes_equal` iterates in sorted-key order. However, `raw_nodes_equal`
    // compares objects order-insensitively via `.find()` — so the boolean result
    // is deterministic regardless of which collection backs the node.
    // These tests make that property observable with objects whose keys arrive in
    // different source orders.
    // ========================================================================

    /// AC9 map-iteration fixture: two objects with the same keys in different
    /// source orders must compare equal via `canonical_equal_raw`.
    ///
    /// This is the canonical AC9 map-iteration test: the raw JSON strings
    /// differ in key order, yet `raw_nodes_equal` uses order-insensitive
    /// comparison (`.find()` per key), so the result must be `true`.
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
    /// This exercises `parse_raw_node`'s object arm on a realistic tool schema
    /// and confirms the order-insensitive `.find()` loop in `raw_nodes_equal`
    /// produces the same verdict regardless of source key order.
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

    // ========================================================================
    // value_equivalent_raw tests (#427 Pass 2)
    // ========================================================================

    /// Correctness table: number raw byte inequality — 1e3 vs 1000.
    ///
    /// This is the primary discriminating test for the raw-number-byte-equality rule.
    /// Deleting the raw-byte path and using f64 comparison instead would change this
    /// to Some(true), breaking the lossless gate.
    #[test]
    fn value_equiv_raw_number_1e3_vs_1000_is_false() {
        let mut budget = 100;
        assert_eq!(
            value_equivalent_raw("1e3", "1000", &mut budget),
            Some(false),
            "1e3 and 1000 are different raw byte tokens — must NOT be equal"
        );
    }

    /// String decoded equality: "A" == "A" (decoded form, not raw bytes).
    #[test]
    fn value_equiv_raw_string_equality() {
        let mut budget = 100;
        assert_eq!(
            value_equivalent_raw(r#""A""#, r#""A""#, &mut budget),
            Some(true),
            "identical strings must be equal"
        );
        assert_eq!(
            value_equivalent_raw(r#""hello""#, r#""world""#, &mut budget),
            Some(false),
            "different strings must not be equal"
        );
    }

    /// Key-order-permuted objects → Some(true).
    ///
    /// Objects are compared key-order-insensitively. Discriminating: if iteration
    /// order matters, this test would fail for reversed-key objects.
    #[test]
    fn value_equiv_raw_object_key_order_insensitive() {
        let mut budget = 200;
        let a = r#"{"z":1,"a":2,"m":3}"#;
        let b = r#"{"m":3,"z":1,"a":2}"#;
        assert_eq!(
            value_equivalent_raw(a, b, &mut budget),
            Some(true),
            "key-order-permuted objects must be equal"
        );
    }

    /// Duplicate key in input → None (not provable).
    ///
    /// Discriminating: if dup-key detection is removed, this returns Some(true)
    /// or Some(false) instead of None.
    #[test]
    fn value_equiv_raw_dup_key_returns_none() {
        let mut budget = 200;
        // Duplicate key in a.
        assert_eq!(
            value_equivalent_raw(r#"{"a":1,"a":2}"#, r#"{"a":1}"#, &mut budget),
            None,
            "duplicate key in a must return None"
        );
        // Duplicate key in b.
        assert_eq!(
            value_equivalent_raw(r#"{"a":1}"#, r#"{"a":1,"a":2}"#, &mut budget),
            None,
            "duplicate key in b must return None"
        );
    }

    /// Over-depth input → None.
    ///
    /// Discriminating: if the depth bound is removed, this returns Some(true)
    /// instead of None (both sides are deeply nested identical values).
    #[test]
    fn value_equiv_raw_over_depth_returns_none() {
        // Build a JSON array nested deeper than VALUE_EQUIV_MAX_DEPTH.
        let mut json = String::new();
        for _ in 0..(VALUE_EQUIV_MAX_DEPTH + 5) {
            json.push('[');
        }
        json.push('1');
        for _ in 0..(VALUE_EQUIV_MAX_DEPTH + 5) {
            json.push(']');
        }
        let mut budget = 10_000;
        // Should return None because recursion exceeds VALUE_EQUIV_MAX_DEPTH.
        assert_eq!(
            value_equivalent_raw(&json, &json, &mut budget),
            None,
            "over-depth input must return None"
        );
    }

    /// Budget exhaustion → None.
    ///
    /// Discriminating: if the budget check is removed, this returns Some(true)
    /// for equal inputs instead of None when budget is 0.
    #[test]
    fn value_equiv_raw_budget_exhaustion_returns_none() {
        let mut budget = 0; // already exhausted
        assert_eq!(
            value_equivalent_raw(r#"{"a":1}"#, r#"{"a":1}"#, &mut budget),
            None,
            "budget=0 must return None immediately"
        );

        // Budget of 1 can compare a single scalar but not an object.
        let mut budget1 = 1;
        assert_eq!(
            value_equivalent_raw("42", "42", &mut budget1),
            Some(true),
            "budget=1 is enough for a single scalar"
        );
        assert_eq!(
            budget1, 0,
            "budget must be fully consumed after one scalar call"
        );
    }

    /// 10k-key linear-work test: asserts the budget consumed scales ~linearly
    /// (no quadratic blowup from O(n²) linear scan).
    ///
    /// The test builds objects with 1_000 and 5_000 keys and compares budget
    /// consumed. A quadratic implementation would consume ~25× more budget for
    /// 5k vs 1k; a linear implementation consumes ~5×. We assert ratio < 10.
    ///
    /// Discriminating: replacing the HashMap with a linear `.find()` per key
    /// (O(n²)) would still complete (HashMap saves time, not nodes visited),
    /// but this test documents the O(n) guarantee at the function level.
    /// The real O(n) proof is structural: we build the HashMap in O(n) and
    /// look up each of b's n keys in O(1) → O(n) total.
    #[test]
    fn value_equiv_raw_10k_keys_linear_budget() {
        fn make_flat_obj(n: usize) -> String {
            let mut s = String::from("{");
            for i in 0..n {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!(r#""k{i}":{i}"#));
            }
            s.push('}');
            s
        }

        let obj_1k = make_flat_obj(1_000);
        let obj_5k = make_flat_obj(5_000);

        // Budget for 1k keys: start high, measure consumption.
        let start_1k: usize = 100_000;
        let mut b1 = start_1k;
        let result_1k = value_equivalent_raw(&obj_1k, &obj_1k, &mut b1);
        assert_eq!(
            result_1k,
            Some(true),
            "1k-key identical objects must be equal"
        );
        let consumed_1k = start_1k - b1;

        // Budget for 5k keys.
        let start_5k: usize = 1_000_000;
        let mut b5 = start_5k;
        let result_5k = value_equivalent_raw(&obj_5k, &obj_5k, &mut b5);
        assert_eq!(
            result_5k,
            Some(true),
            "5k-key identical objects must be equal"
        );
        let consumed_5k = start_5k - b5;

        // Linear growth: ratio should be ~5 (5k/1k). Allow 10× for generous slack.
        // Quadratic would be ~25.
        let ratio = consumed_5k / consumed_1k.max(1);
        assert!(
            ratio < 10,
            "budget consumption must scale linearly: 5k/1k ratio = {ratio} (≥10 implies quadratic)"
        );
    }

    /// Correctness: nulls, booleans, arrays, nested objects.
    #[test]
    fn value_equiv_raw_correctness_various_types() {
        let mut b = 10_000usize;

        // null
        assert_eq!(value_equivalent_raw("null", "null", &mut b), Some(true));
        assert_eq!(value_equivalent_raw("null", "false", &mut b), Some(false));

        // bool
        assert_eq!(value_equivalent_raw("true", "true", &mut b), Some(true));
        assert_eq!(value_equivalent_raw("false", "false", &mut b), Some(true));
        assert_eq!(value_equivalent_raw("true", "false", &mut b), Some(false));

        // number raw-byte exact
        assert_eq!(value_equivalent_raw("1.0", "1.0", &mut b), Some(true));
        assert_eq!(value_equivalent_raw("1.0", "1", &mut b), Some(false));
        assert_eq!(value_equivalent_raw("100.00", "100.00", &mut b), Some(true));
        assert_eq!(value_equivalent_raw("100.00", "100", &mut b), Some(false));

        // arrays order-sensitive
        assert_eq!(
            value_equivalent_raw("[1,2,3]", "[1,2,3]", &mut b),
            Some(true)
        );
        assert_eq!(
            value_equivalent_raw("[1,2,3]", "[3,2,1]", &mut b),
            Some(false)
        );
        assert_eq!(
            value_equivalent_raw("[1,2]", "[1,2,3]", &mut b),
            Some(false)
        );

        // type mismatch
        assert_eq!(value_equivalent_raw("1", r#""1""#, &mut b), Some(false));
        assert_eq!(value_equivalent_raw("[]", "{}", &mut b), Some(false));
    }

    /// Parse failure → None.
    #[test]
    fn value_equiv_raw_invalid_json_returns_none() {
        let mut b = 100usize;
        assert_eq!(value_equivalent_raw("{invalid}", "{}", &mut b), None);
        assert_eq!(value_equivalent_raw("{}", "{invalid}", &mut b), None);
    }
}
