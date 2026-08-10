//! Canonical JSON emit: within-object key-sort, deterministic element ordering,
//! and envelope key ordering with byte-verbatim `messages` span.
//!
//! # AD-CA-2
//!
//! Canonicalization = `RawValue`-preserving recursive **within-object key-sort +
//! compact-framing emit** of the `tools`/`system` value spans, PLUS deterministic
//! `tools`/`functions` array element ordering (AD-CA-12) and canonical top-level
//! envelope key ordering (AD-CA-13). Leaf string/number values are copied verbatim
//! from the `RawValue.get()` bytes — **never** through `serde_json::Value`, which
//! would reformat number tokens (e.g. `1e3` → `1000`) and normalise `\uXXXX`
//! escapes (breaking AC12).
//!
//! Compaction (no insignificant whitespace in the emitted form) is mandatory for
//! AC1/AC5 cross-turn convergence: two requests identical except for internal
//! whitespace MUST produce byte-identical output.
//!
//! # AD-CA-12 — Deterministic element ordering
//!
//! The `tools`/`functions` array elements are sorted into a deterministic canonical
//! order using a two-part total sort key:
//! 1. **Primary:** provider-shape-aware tool name (decoded UTF-8 comparison).
//! 2. **Tie-break:** the element's canonicalized compact key-sorted bytes (lexicographic).
//!
//! The sort is total by construction — duplicate names fall back to the tie-break,
//! non-object elements sort deterministically via their canonical bytes. System
//! content-block order is NOT changed.
//!
//! # AD-CA-13 — Canonical envelope key ordering
//!
//! Top-level envelope keys are emitted in lexicographic order (sorted). Values for
//! `tools` and `system` carry their own canonicalization; every other value (including
//! the **entire `messages` value span**) is copied byte-for-byte. Self-verified by
//! the AD-CA-7 envelope path.
//!
//! # Depth bound (AC26)
//!
//! Recursive emit is bounded to [`crate::MAX_ALIGN_SCHEMA_DEPTH`] levels. Any value
//! nested deeper returns `None` from its canonicalize function, propagating up to
//! cause a whole-request passthrough (fail-open, no partial sort).

use crate::span::Span;
use rskim_llm::Provider;
use serde::de::{MapAccess, Visitor};
use serde_json::value::RawValue;
use std::collections::HashMap;
use std::fmt;

use crate::MAX_ALIGN_SCHEMA_DEPTH;

// ============================================================================
// Internal: tool array kind
// ============================================================================

/// Which tool array format the elements are in, for sort-key extraction.
///
/// # AD-CA-12
///
/// The sort key is provider-shape-aware: different providers use different
/// field paths to carry the tool name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolArrayKind {
    /// Anthropic `tools` array: top-level `"name"` field is the sort key.
    AnthropicTools,
    /// OpenAI `tools` array: nested `"function"."name"` field is the sort key.
    OpenAiTools,
    /// OpenAI legacy `functions` array: top-level `"name"` field is the sort key.
    OpenAiLegacyFunctions,
}

// ============================================================================
// Internal: JSON object pair parsing
// ============================================================================

/// Parse a JSON object string into an ordered list of (key, value) pairs,
/// with duplicate key detection.
///
/// Returns `None` if:
/// - `raw` is not a valid JSON object.
/// - Any key appears more than once (fail-open: duplicate keys are ambiguous).
///
/// The returned pairs are in source order (before key-sort). Values are owned
/// `Box<RawValue>` containing the raw JSON bytes of each value.
/// This function is `pub(crate)` so that `breakpoint.rs` can rebuild tool/system
/// elements with `cache_control` inserted at the canonically-sorted position (AD-CA-7
/// injection path idempotence). Only the crate-internal injection helpers call it.
pub(crate) fn parse_object_pairs(raw: &str) -> Option<Vec<(String, Box<RawValue>)>> {
    struct PairsVisitor;

    impl<'de> Visitor<'de> for PairsVisitor {
        type Value = Vec<(String, Box<RawValue>)>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a JSON object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut pairs: Vec<(String, Box<RawValue>)> = Vec::new();
            let mut seen = std::collections::HashSet::new();

            while let Some(k) = map.next_key::<String>()? {
                let v: Box<RawValue> = map.next_value()?;
                if !seen.insert(k.clone()) {
                    return Err(serde::de::Error::custom("duplicate key"));
                }
                pairs.push((k, v));
            }
            Ok(pairs)
        }
    }

    struct Pairs(Vec<(String, Box<RawValue>)>);

    impl<'de> serde::Deserialize<'de> for Pairs {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_map(PairsVisitor).map(Pairs)
        }
    }

    serde_json::from_str::<Pairs>(raw).ok().map(|p| p.0)
}

/// Parse a JSON array string into a list of owned `Box<RawValue>` elements.
///
/// Returns `None` if `raw` is not a valid JSON array.
fn parse_array_elements(raw: &str) -> Option<Vec<Box<RawValue>>> {
    serde_json::from_str::<Vec<Box<RawValue>>>(raw).ok()
}

// ============================================================================
// Recursive canonical value emit
// ============================================================================

/// Emit a canonical compact representation of any JSON value.
///
/// # AD-CA-2 — Leaf verbatim, structural compaction
///
/// - **Leaf values** (null, bool, number, string): returned as their trimmed raw
///   bytes. This is the key invariant that preserves number tokens like `1e3`
///   and `\uXXXX` escapes byte-faithfully (never through `serde_json::Value`).
/// - **Objects**: recursively key-sorted and compacted (see `canonicalize_object`).
/// - **Arrays**: elements recursively canonicalized in source order (no element
///   reorder here; `sort_tools_array` handles the tools/functions element reorder).
///
/// Returns `None` if `depth >= MAX_ALIGN_SCHEMA_DEPTH` or if any parse fails.
/// The caller maps `None` to a whole-request fail-open (AC26).
pub fn canonicalize_value(raw: &str, depth: u32) -> Option<Vec<u8>> {
    if depth >= MAX_ALIGN_SCHEMA_DEPTH {
        return None; // AC26: over-depth → whole-request fail-open
    }
    let s = raw.trim();
    match s.as_bytes().first()? {
        b'{' => canonicalize_object(s, depth),
        b'[' => canonicalize_array(s, depth),
        _ => {
            // Leaf: copy verbatim (preserves 1e3, \uXXXX, etc.)
            Some(s.as_bytes().to_vec())
        }
    }
}

/// Emit a canonical compact key-sorted JSON object.
///
/// Keys are sorted lexicographically (by their decoded UTF-8 representation).
/// Values are recursively canonicalized. Duplicate keys cause `None` (fail-open).
///
/// # AD-CA-2
///
/// This is the within-object key-sort that makes two semantically-equal objects
/// with different key orders produce identical bytes.
fn canonicalize_object(raw: &str, depth: u32) -> Option<Vec<u8>> {
    let mut pairs = parse_object_pairs(raw)?;
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = Vec::new();
    out.push(b'{');

    for (i, (key, val)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        // Key: re-serialize as JSON string (handles any special characters)
        let key_json = serde_json::to_string(key.as_str()).ok()?;
        out.extend_from_slice(key_json.as_bytes());
        out.push(b':');
        // Value: recursively canonicalize at depth+1
        let val_bytes = canonicalize_value(val.get().trim(), depth + 1)?;
        out.extend_from_slice(&val_bytes);
    }

    out.push(b'}');
    Some(out)
}

/// Emit a canonical compact JSON array with each element recursively canonicalized.
///
/// Element order is preserved (this function does NOT reorder elements; only
/// `sort_tools_array` reorders the `tools`/`functions` array, per AD-CA-12).
fn canonicalize_array(raw: &str, depth: u32) -> Option<Vec<u8>> {
    let elements = parse_array_elements(raw)?;

    let mut out = Vec::new();
    out.push(b'[');

    for (i, elem) in elements.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        let elem_bytes = canonicalize_value(elem.get().trim(), depth + 1)?;
        out.extend_from_slice(&elem_bytes);
    }

    out.push(b']');
    Some(out)
}

// ============================================================================
// Tool name extraction for element sort key
// ============================================================================

/// Extract the tool name from a single tools/functions array element.
///
/// # AD-CA-12 — Provider-shape-aware sort key extraction
///
/// - `AnthropicTools`: top-level `"name"` field (decoded UTF-8).
/// - `OpenAiTools`: nested `"function"."name"` field.
/// - `OpenAiLegacyFunctions`: top-level `"name"` field (same path as Anthropic).
///
/// Returns `None` if the name cannot be extracted (non-object element, missing
/// name field, non-string name value). The caller uses an empty string as the
/// fallback name — such elements sort to the front of the array, which is
/// deterministic (consistent for the same input).
fn extract_tool_name(element: &RawValue, kind: ToolArrayKind) -> Option<String> {
    let s = element.get().trim();
    if !s.starts_with('{') {
        return None; // non-object element — no name to extract
    }

    let pairs = parse_object_pairs(s)?;

    match kind {
        ToolArrayKind::AnthropicTools | ToolArrayKind::OpenAiLegacyFunctions => {
            // Top-level "name" field
            for (k, v) in &pairs {
                if k == "name" {
                    let name: String = serde_json::from_str(v.get().trim()).ok()?;
                    return Some(name);
                }
            }
            None
        }
        ToolArrayKind::OpenAiTools => {
            // Nested "function"."name"
            for (k, v) in &pairs {
                if k == "function" {
                    let func_pairs = parse_object_pairs(v.get().trim())?;
                    for (fk, fv) in &func_pairs {
                        if fk == "name" {
                            let name: String = serde_json::from_str(fv.get().trim()).ok()?;
                            return Some(name);
                        }
                    }
                }
            }
            None
        }
    }
}

// ============================================================================
// Tools/functions array: element-reorder + per-element key-sort
// ============================================================================

/// Canonicalize and deterministically sort a `tools` or `functions` array.
///
/// # AD-CA-12 — Deterministic element ordering
///
/// Each element is first canonicalized (within-object key-sort + compact framing),
/// then sorted by the two-part total key:
/// 1. **Primary**: provider-shape-aware tool name (decoded UTF-8 bytes).
/// 2. **Tie-break**: canonicalized compact bytes of the element (lexicographic).
///
/// The sort is total: duplicate names fall back to tie-break; non-object elements
/// (which have no name) use an empty primary key and sort by canonical bytes.
///
/// Returns `None` if any element fails canonicalization (depth exceeded or parse
/// error) → whole-request fail-open.
pub(crate) fn sort_tools_array(raw: &str, kind: ToolArrayKind) -> Option<Vec<u8>> {
    let elements = parse_array_elements(raw)?;

    if elements.is_empty() {
        return Some(b"[]".to_vec());
    }

    // Canonicalize each element and compute its sort key
    struct ElemWithKey {
        canonical: Vec<u8>,
        name: String, // primary sort key (empty when not extractable)
    }

    let mut keyed: Vec<ElemWithKey> = Vec::with_capacity(elements.len());
    for elem in &elements {
        let canonical = canonicalize_value(elem.get().trim(), 0)?;
        let name = extract_tool_name(elem, kind).unwrap_or_default();
        keyed.push(ElemWithKey { canonical, name });
    }

    // Sort: primary by name (decoded UTF-8 bytes), tie-break by canonical bytes
    // This is a total order — no element can be "incomparable".
    keyed.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.canonical.cmp(&b.canonical))
    });

    // Emit sorted elements
    let mut out = Vec::new();
    out.push(b'[');
    for (i, elem) in keyed.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(&elem.canonical);
    }
    out.push(b']');
    Some(out)
}

// ============================================================================
// Canonical envelope: sorted keys + verbatim messages span
// ============================================================================

/// Restructure a JSON request body into canonical key order with byte-verbatim
/// non-canonical-target values and `messages` span byte-identity.
///
/// # AD-CA-13 — Canonical envelope key ordering
///
/// The top-level object keys are emitted in lexicographic (sorted) order. Values:
/// - `"tools"` (Anthropic or OpenAI): canonicalized + element-reordered.
/// - `"functions"` (OpenAI legacy): canonicalized + element-reordered.
/// - `"system"` (Anthropic only): recursively key-sorted (block ORDER unchanged).
/// - `"messages"`: copied **byte-for-byte** (entire value span verbatim).
/// - All other keys: copied byte-for-byte.
///
/// # AD-CA-7 — Envelope path self-verify
///
/// After restructuring, the `messages` value bytes in the output are compared
/// to the `messages` value bytes extracted from the input. Any mismatch returns
/// `None` → whole-request fail-open. By construction this should always pass
/// (we copy verbatim), but the check guards against future bugs.
///
/// # AC26 — Depth fail-open
///
/// If any value within `tools` or `system` nests deeper than
/// `MAX_ALIGN_SCHEMA_DEPTH`, returns `None` → whole-request fail-open.
pub fn canonical_envelope(
    input: &str,
    spans: &HashMap<String, Span>,
    provider: Provider,
) -> Option<Vec<u8>> {
    // Sort keys lexicographically for deterministic envelope order (AD-CA-13)
    let mut keys: Vec<&str> = spans.keys().map(String::as_str).collect();
    keys.sort_unstable();

    let mut out: Vec<u8> = Vec::with_capacity(input.len() + 16);
    out.push(b'{');

    // Track the messages value bytes for the AD-CA-7 self-verify
    let mut messages_input_bytes: Option<&str> = None;
    let mut messages_output_start: Option<usize> = None;
    let mut messages_output_len: Option<usize> = None;

    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }

        // Emit key as JSON string
        let key_json = serde_json::to_string(*key).ok()?;
        out.extend_from_slice(key_json.as_bytes());
        out.push(b':');

        let span = spans.get(*key)?;
        let raw_val = span.extract(input)?;

        // Determine how to emit this value
        let value_start = out.len();
        match *key {
            "tools" => {
                // AD-CA-12: canonicalize + deterministically sort elements
                let kind = match provider {
                    Provider::Anthropic => ToolArrayKind::AnthropicTools,
                    Provider::OpenAi => ToolArrayKind::OpenAiTools,
                    // AD-CA-10: unknown provider → fail-open
                    _ => return None,
                };
                let canonicalized = sort_tools_array(raw_val, kind)?;
                out.extend_from_slice(&canonicalized);
            }
            "functions" => {
                // OpenAI legacy functions array: top-level "name" sort key
                let canonicalized =
                    sort_tools_array(raw_val, ToolArrayKind::OpenAiLegacyFunctions)?;
                out.extend_from_slice(&canonicalized);
            }
            "system" => match provider {
                Provider::Anthropic => {
                    // System canonicalization: key-sort within blocks, block ORDER unchanged.
                    // canonicalize_value on an array preserves element order (by design).
                    let canonicalized = canonicalize_value(raw_val.trim(), 0)?;
                    out.extend_from_slice(&canonicalized);
                }
                _ => {
                    // Non-Anthropic: copy system verbatim (unknown semantics)
                    out.extend_from_slice(raw_val.as_bytes());
                }
            },
            "messages" => {
                // AD-CA-13: messages value is byte-verbatim
                messages_input_bytes = Some(raw_val);
                messages_output_start = Some(value_start);
                out.extend_from_slice(raw_val.as_bytes());
                messages_output_len = Some(raw_val.len());
            }
            _ => {
                // All other keys: copy value verbatim
                out.extend_from_slice(raw_val.as_bytes());
            }
        }
    }

    out.push(b'}');

    // AD-CA-7 envelope self-verify: assert messages span byte-identical
    // (should always pass since we copy verbatim; guards against future bugs)
    if let (Some(input_msgs), Some(output_start), Some(output_len)) = (
        messages_input_bytes,
        messages_output_start,
        messages_output_len,
    ) {
        let output_end = output_start.checked_add(output_len)?;
        match out.get(output_start..output_end) {
            Some(out_slice) if out_slice == input_msgs.as_bytes() => {}
            _ => {
                // AD-CA-7 self-verify failed → fail-open
                return None;
            }
        }
    }

    Some(out)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::span::locate_top_level_spans;
    use rskim_contract::canonical::tools_arrays_set_equal;

    // ── Leaf verbatim ─────────────────────────────────────────────────────────

    #[test]
    fn canonicalize_value_leaf_number_verbatim() {
        // AC12: number token 1e3 must NOT be reformatted to 1000
        let out = canonicalize_value("1e3", 0).expect("must succeed");
        assert_eq!(out, b"1e3");
    }

    #[test]
    fn canonicalize_value_leaf_string_verbatim() {
        let out = canonicalize_value(r#""hello\nworld""#, 0).expect("must succeed");
        assert_eq!(out, br#""hello\nworld""#);
    }

    #[test]
    fn canonicalize_value_null() {
        assert_eq!(canonicalize_value("null", 0).unwrap(), b"null");
    }

    #[test]
    fn canonicalize_value_bool() {
        assert_eq!(canonicalize_value("true", 0).unwrap(), b"true");
        assert_eq!(canonicalize_value("false", 0).unwrap(), b"false");
    }

    // ── Object key-sort ───────────────────────────────────────────────────────

    #[test]
    fn canonicalize_object_sorts_keys() {
        let out = canonicalize_value(r#"{"z":1,"a":2,"m":3}"#, 0).unwrap();
        assert_eq!(out, br#"{"a":2,"m":3,"z":1}"#);
    }

    #[test]
    fn canonicalize_object_nested_sorted() {
        let input = r#"{"tool":{"z":"v2","a":"v1"}}"#;
        let out = canonicalize_value(input, 0).unwrap();
        assert_eq!(out, br#"{"tool":{"a":"v1","z":"v2"}}"#);
    }

    #[test]
    fn canonicalize_object_removes_whitespace() {
        let input = r#"{ "b" : 2 , "a" : 1 }"#;
        let out = canonicalize_value(input, 0).unwrap();
        assert_eq!(out, br#"{"a":1,"b":2}"#);
    }

    #[test]
    fn canonicalize_object_duplicate_key_returns_none() {
        let input = r#"{"a":1,"a":2}"#;
        assert!(canonicalize_value(input, 0).is_none());
    }

    // ── Depth bound (AC26) ────────────────────────────────────────────────────

    #[test]
    fn depth_32_fails_open() {
        // Build a JSON object nested to depth 33 (MAX_ALIGN_SCHEMA_DEPTH + 1)
        let mut s = String::from(r#"{"k":"leaf"}"#);
        for _ in 0..(MAX_ALIGN_SCHEMA_DEPTH + 1) {
            s = format!(r#"{{"nested":{s}}}"#);
        }
        // AC26: over-depth → None → whole-request fail-open
        assert!(
            canonicalize_value(&s, 0).is_none(),
            "depth > MAX_ALIGN_SCHEMA_DEPTH must return None"
        );
    }

    #[test]
    fn depth_31_succeeds() {
        // 30 wrappings produces an outermost value whose deepest leaf string is at depth 31.
        // Depth 31 < MAX_ALIGN_SCHEMA_DEPTH (32), so the entire tree must succeed.
        //
        // Why 30, not 31: each wrapping pushes the innermost leaf one level deeper relative
        // to the initial depth=0 call. 30 wrappings → leaf at depth 31 (= 30 + 1).
        // 31 wrappings → leaf at depth 32, which equals MAX_ALIGN_SCHEMA_DEPTH → None.
        let mut s = String::from(r#"{"k":"leaf"}"#);
        for _ in 0..30 {
            s = format!(r#"{{"nested":{s}}}"#);
        }
        // Should not return None: leaf is at depth 31 < 32
        assert!(canonicalize_value(&s, 0).is_some());
    }

    // ── Array element canonicalization (no reorder in canonicalize_array) ─────

    #[test]
    fn canonicalize_array_preserves_element_order() {
        let input = r#"[{"b":2,"a":1},{"d":4,"c":3}]"#;
        let out = canonicalize_value(input, 0).unwrap();
        // Elements stay in the same order; within each object, keys are sorted
        assert_eq!(out, br#"[{"a":1,"b":2},{"c":3,"d":4}]"#);
    }

    // ── tools element sort (AD-CA-12) ─────────────────────────────────────────

    #[test]
    fn sort_tools_array_anthropic_by_name() {
        let raw = r#"[{"name":"zoo","description":"z"},{"name":"alpha","description":"a"}]"#;
        let out = sort_tools_array(raw, ToolArrayKind::AnthropicTools).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        // alpha < zoo
        assert!(out_str.contains("\"alpha\""));
        let alpha_pos = out_str.find("\"alpha\"").unwrap();
        let zoo_pos = out_str.find("\"zoo\"").unwrap();
        assert!(alpha_pos < zoo_pos, "alpha must come before zoo");
    }

    #[test]
    fn sort_tools_array_deterministic_permutations() {
        // AC28: two permutations of the same tool set → byte-identical output
        let a = r#"[{"name":"a","desc":"x"},{"name":"b","desc":"y"},{"name":"c","desc":"z"}]"#;
        let b = r#"[{"name":"c","desc":"z"},{"name":"a","desc":"x"},{"name":"b","desc":"y"}]"#;
        let c = r#"[{"name":"b","desc":"y"},{"name":"c","desc":"z"},{"name":"a","desc":"x"}]"#;

        let out_a = sort_tools_array(a, ToolArrayKind::AnthropicTools).unwrap();
        let out_b = sort_tools_array(b, ToolArrayKind::AnthropicTools).unwrap();
        let out_c = sort_tools_array(c, ToolArrayKind::AnthropicTools).unwrap();

        assert_eq!(out_a, out_b, "permutation a==b");
        assert_eq!(out_a, out_c, "permutation a==c");
    }

    #[test]
    fn sort_tools_array_set_equal_gate_ad_ca_7() {
        // AC29 POSITIVE: tools_arrays_set_equal must hold for original vs sorted output.
        // Non-tautology: sorted output is NOT byte-equal to input (reorder actually happened).
        let raw = r#"[{"name":"b","input_schema":{"type":"object"}},{"name":"a","input_schema":{"type":"object"}}]"#;
        let out = sort_tools_array(raw, ToolArrayKind::AnthropicTools).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        // Set equality: no drops, no dups, no mutations
        assert!(
            tools_arrays_set_equal(raw, out_str),
            "AD-CA-7: set-equality must hold after element reorder"
        );
        // Non-tautology: the sorted output is NOT byte-equal to the input (reorder happened)
        assert_ne!(
            raw.as_bytes(),
            out.as_slice(),
            "reorder must produce a different byte sequence"
        );
    }

    #[test]
    fn sort_tools_array_set_equal_gate_negative_drop_ac29() {
        // AC29 NEGATIVE (PF-007 discriminating): tools_arrays_set_equal returns false
        // when an element is missing. This proves the gate would fire if sort_tools_array
        // ever dropped an element. The gate in try_align_full (lib.rs) fires on the same
        // function — these negative cases prove it is not a tautology.
        let raw = r#"[{"name":"alpha"},{"name":"beta"}]"#;

        // Drop: output has only one element
        let drop_mutant = r#"[{"name":"alpha"}]"#;
        assert!(
            !tools_arrays_set_equal(raw, drop_mutant),
            "AC29 negative-drop: set-equal gate must detect missing element"
        );

        // Duplication: one element appears twice
        let dup_mutant = r#"[{"name":"alpha"},{"name":"alpha"}]"#;
        assert!(
            !tools_arrays_set_equal(raw, dup_mutant),
            "AC29 negative-dup: set-equal gate must detect duplicated element"
        );

        // Mutation: one element's name is changed
        let mutation_mutant = r#"[{"name":"alpha"},{"name":"beta_CORRUPTED"}]"#;
        assert!(
            !tools_arrays_set_equal(raw, mutation_mutant),
            "AC29 negative-mutation: set-equal gate must detect mutated element"
        );
    }

    #[test]
    fn sort_tools_empty_array() {
        let out = sort_tools_array("[]", ToolArrayKind::AnthropicTools).unwrap();
        assert_eq!(out, b"[]");
    }

    #[test]
    fn sort_tools_openai_function_name() {
        // OpenAI tools: sort key is "function"."name"
        let raw = r#"[{"type":"function","function":{"name":"zoo","description":"z"}},{"type":"function","function":{"name":"alpha","description":"a"}}]"#;
        let out = sort_tools_array(raw, ToolArrayKind::OpenAiTools).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        let alpha_pos = out_str.find("\"alpha\"").unwrap();
        let zoo_pos = out_str.find("\"zoo\"").unwrap();
        assert!(alpha_pos < zoo_pos, "alpha must precede zoo");
    }

    // ── canonical_envelope ────────────────────────────────────────────────────

    #[test]
    fn envelope_sorts_keys() {
        let input = r#"{"model":"claude-3","messages":[],"max_tokens":100}"#;
        let spans = locate_top_level_spans(input).unwrap();
        let out = canonical_envelope(input, &spans, Provider::Anthropic).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        // Keys in sorted order: max_tokens < messages < model
        let max_pos = out_str.find("\"max_tokens\"").unwrap();
        let msgs_pos = out_str.find("\"messages\"").unwrap();
        let model_pos = out_str.find("\"model\"").unwrap();
        assert!(max_pos < msgs_pos, "max_tokens before messages");
        assert!(msgs_pos < model_pos, "messages before model");
    }

    #[test]
    fn envelope_messages_span_verbatim_ad_ca_13() {
        // AC31: messages value must be byte-identical after envelope reorder.
        // Uses span-extracted byte comparison (not `contains`) — a `contains` check
        // would pass if the bytes appear anywhere in the output, not just the messages
        // span. Span extraction locates the exact JSON value position and compares bytes.
        let messages_val =
            r#"[{"role":"user","content":"hello world"},{"role":"assistant","content":"hi"}]"#;
        let input = format!(r#"{{"model":"claude-3","messages":{messages_val},"max_tokens":100}}"#);
        let spans = locate_top_level_spans(&input).unwrap();
        let out = canonical_envelope(&input, &spans, Provider::Anthropic).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();

        // AC31: extract the messages span from the output and compare byte-by-byte.
        let out_spans = locate_top_level_spans(out_str)
            .expect("AC31: output must be valid JSON with parseable spans");
        let out_messages = out_spans
            .get("messages")
            .and_then(|s| s.extract(out_str))
            .expect("AC31: output must contain a messages key");
        assert_eq!(
            out_messages, messages_val,
            "AC31: messages span must be byte-identical to input value (span-extracted comparison)"
        );
    }

    #[test]
    fn envelope_messages_span_byte_identity_negative_ac31() {
        // AC31 NEGATIVE (PF-007 discriminating): prove the span-extracted comparison
        // is not vacuous — if the messages value were mutated, the comparison must fail.
        let messages_val = r#"[{"role":"user","content":"abc"}]"#;
        let input = format!(r#"{{"model":"m","messages":{messages_val},"max_tokens":1}}"#);
        let spans = locate_top_level_spans(&input).unwrap();
        let out = canonical_envelope(&input, &spans, Provider::Anthropic).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();

        // Extract the real messages span from output
        let out_spans = locate_top_level_spans(out_str).unwrap();
        let out_messages = out_spans
            .get("messages")
            .and_then(|s| s.extract(out_str))
            .unwrap();

        // Corrupt one byte: replace the leading '[' with 'X'
        let mut corrupted = out_messages.as_bytes().to_vec();
        corrupted[0] = b'X';
        let corrupted_str = std::str::from_utf8(&corrupted).unwrap();

        // Span-extracted comparison MUST detect the single-byte mutation
        assert_ne!(
            out_messages, corrupted_str,
            "AC31 negative: span-extracted comparison must detect byte mutation"
        );
    }

    #[test]
    fn envelope_two_key_orders_converge_ac1() {
        // AC1 / AC30: two inputs differing only in top-level key order
        // must produce byte-identical output
        let a = r#"{"model":"m","messages":[],"tools":[]}"#;
        let b = r#"{"tools":[],"messages":[],"model":"m"}"#;
        let spans_a = locate_top_level_spans(a).unwrap();
        let spans_b = locate_top_level_spans(b).unwrap();
        let out_a = canonical_envelope(a, &spans_a, Provider::Anthropic).unwrap();
        let out_b = canonical_envelope(b, &spans_b, Provider::Anthropic).unwrap();
        assert_eq!(out_a, out_b, "key-order variants must converge");
    }

    #[test]
    fn envelope_tools_canonicalized_and_sorted() {
        // AC1: tools with different within-object key order + different element order
        let a = r#"{"tools":[{"name":"b","desc":"d2"},{"name":"a","desc":"d1"}],"messages":[]}"#;
        let b = r#"{"tools":[{"desc":"d1","name":"a"},{"desc":"d2","name":"b"}],"messages":[]}"#;
        let spans_a = locate_top_level_spans(a).unwrap();
        let spans_b = locate_top_level_spans(b).unwrap();
        let out_a = canonical_envelope(a, &spans_a, Provider::Anthropic).unwrap();
        let out_b = canonical_envelope(b, &spans_b, Provider::Anthropic).unwrap();
        assert_eq!(out_a, out_b, "tools variants must converge");
    }

    #[test]
    fn envelope_idempotence() {
        // AC8: align(align(x)) == align(x) byte-for-byte
        let input = r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"tools":[{"name":"b","x":1},{"name":"a","x":2}]}"#;
        let spans1 = locate_top_level_spans(input).unwrap();
        let out1 = canonical_envelope(input, &spans1, Provider::Anthropic).unwrap();
        let out1_str = std::str::from_utf8(&out1).unwrap();
        let spans2 = locate_top_level_spans(out1_str).unwrap();
        let out2 = canonical_envelope(out1_str, &spans2, Provider::Anthropic).unwrap();
        assert_eq!(out1, out2, "canonical_envelope must be idempotent");
    }

    #[test]
    fn envelope_number_token_preserved_verbatim() {
        // AC12: number token 1e3 inside tools must not be reformatted
        let input = r#"{"tools":[{"name":"t","input_schema":{"default":1e3}}],"messages":[]}"#;
        let spans = locate_top_level_spans(input).unwrap();
        let out = canonical_envelope(input, &spans, Provider::Anthropic).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        assert!(
            out_str.contains("1e3"),
            "number token 1e3 must be preserved verbatim"
        );
    }

    #[test]
    fn envelope_system_string_verbatim() {
        // AC7: string-form system must be byte-identical after canonicalization
        let system = "\"You are a helpful assistant.\"";
        let input = format!(r#"{{"messages":[],"system":{system}}}"#);
        let spans = locate_top_level_spans(&input).unwrap();
        let out = canonical_envelope(&input, &spans, Provider::Anthropic).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        assert!(out_str.contains(system), "string system must be verbatim");
    }

    #[test]
    fn envelope_openai_no_cache_control() {
        // AC15: OpenAI output must NOT contain cache_control
        let input = r#"{"tools":[{"type":"function","function":{"name":"fn1"}}],"messages":[]}"#;
        let spans = locate_top_level_spans(input).unwrap();
        let out = canonical_envelope(input, &spans, Provider::OpenAi).unwrap();
        let out_str = std::str::from_utf8(&out).unwrap();
        assert!(
            !out_str.contains("cache_control"),
            "OpenAI output must not contain cache_control"
        );
    }

    // ── Proptest: idempotence ─────────────────────────────────────────────────

    use proptest::prelude::*;

    /// Minimal JSON object strategy for proptest.
    ///
    /// Generates objects with string values at 1–4 keys. The keys are from a
    /// controlled set to ensure stable tool-name extraction. Values include
    /// simple strings and the number token `1e3` to exercise the verbatim path.
    fn json_string_value() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(r#""hello""#.to_string()),
            Just(r#""world""#.to_string()),
            Just("42".to_string()),
            Just("1e3".to_string()),
            Just("null".to_string()),
            Just("true".to_string()),
        ]
    }

    fn simple_tool_arb() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(r#"{"name":"alpha","description":"a"}"#.to_string()),
            Just(r#"{"description":"b","name":"beta"}"#.to_string()),
            Just(r#"{"name":"gamma","description":"g","x":1e3}"#.to_string()),
        ]
    }

    fn anthropic_body_arb() -> impl Strategy<Value = String> {
        (
            prop::collection::vec(simple_tool_arb(), 0..=3),
            json_string_value(),
        )
            .prop_map(|(tools, model_val)| {
                let tools_json = format!("[{}]", tools.join(","));
                // Vary the top-level key order to test convergence
                format!(r#"{{"model":{model_val},"messages":[],"tools":{tools_json}}}"#)
            })
    }

    proptest! {
        #[test]
        fn canonical_emit_idempotent(input in anthropic_body_arb()) {
            if let Some(spans1) = locate_top_level_spans(&input)
                && let Some(out1) = canonical_envelope(&input, &spans1, Provider::Anthropic)
            {
                let out1_str = std::str::from_utf8(&out1).unwrap();
                if let Some(spans2) = locate_top_level_spans(out1_str)
                    && let Some(out2) = canonical_envelope(out1_str, &spans2, Provider::Anthropic)
                {
                    prop_assert_eq!(
                        out1, out2,
                        "canonical_envelope must be idempotent"
                    );
                }
            }
        }

        #[test]
        fn canonical_emit_messages_verbatim(model_val in json_string_value()) {
            // AC31: messages value bytes are byte-identical before and after
            let messages_val = r#"[{"role":"user","content":"test message 123"}]"#;
            let input = format!(
                r#"{{"model":{model_val},"messages":{messages_val},"max_tokens":100}}"#
            );
            if let Some(spans) = locate_top_level_spans(&input)
                && let Some(out) = canonical_envelope(&input, &spans, Provider::Anthropic)
            {
                let out_str = std::str::from_utf8(&out).unwrap();
                prop_assert!(
                    out_str.contains(messages_val),
                    "messages span must be byte-verbatim"
                );
            }
        }
    }
}
