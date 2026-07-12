//! Lossless JSON minifier — whitespace stripping with byte-exact fidelity (#427 Pass 2).
//!
//! # Design (replacing the lossy structural compressor from Phase 2 / D5)
//!
//! The old engine replaced values with type-placeholder strings (`"<string>"`,
//! `"<number>"`, `"<bool>"`). This violated ADR-007 (lossless-only egress): it
//! changed the semantics of the block, breaking tool-result cache keys and LLM
//! context integrity. The new engine is a LOSSLESS whitespace stripper.
//!
//! ## Single-pass raw-token scanner
//!
//! The scanner tracks `in_string` + `escape_next` state as it walks the input
//! byte-by-byte:
//! - **Outside strings**: whitespace bytes (space, tab, CR, LF) are dropped;
//!   all other bytes are copied verbatim to the output.
//! - **Inside strings**: ALL bytes (including whitespace and special characters)
//!   are copied verbatim. Escapes are tracked to correctly identify string
//!   boundaries, but are NEVER rewritten.
//!
//! **Number tokens are never rewritten**: `1e3` stays `1e3`, `1.0` stays `1.0`,
//! `100.00` stays `100.00`. This preserves raw byte equality across the gate.
//!
//! ## Lossless guarantee
//!
//! For any valid JSON input, `compress_json` + the runtime gate (Step 5 / lib.rs)
//! together guarantee that the block forwarded to the LLM is byte-identical to
//! the original unless it is value-equivalent (proven by `value_equivalent_raw`).
//! The minifier itself is lossless by construction; the gate catches scanner bugs.
//!
//! ## Passthrough cases
//!
//! - Invalid JSON (pre-validation fails): `Passthrough` — fail open.
//! - Already-minimal input (output == input bytes): `Passthrough` — no savings.
//! - Depth bound exceeded (`MAX_JSON_DEPTH = 500`): `Passthrough`.
//! - Key count bound exceeded (`MAX_JSON_KEYS = 10_000` total): `Passthrough`.
//! - Duplicate key in any object: `Passthrough`.
//!
//! Duplicate-key and no-op passthrough cases are both recorded as `FailedOpen`
//! by the route loop (engine returned `Passthrough`), not `LossyRejected`
//! (per the #305 coordination note in `OutcomeReason::LossyRejected`).
//!
//! ## Per-object key-seen tracking
//!
//! A `HashSet<Vec<u8>>` of raw key bytes is maintained per object frame in the
//! stack. On the first duplicate key in any object, `Passthrough` is returned
//! immediately. This prevents the output from silently misrepresenting inputs
//! where JSON consumers disagree on which value wins for duplicate keys.
//!
//! ## Bounds (ADR-003 / PF-005)
//!
//! - `MAX_JSON_DEPTH = 500`: matches the old lossy engine; prevents stack-frame
//!   over-allocation on adversarial inputs.
//! - `MAX_JSON_KEYS = 10_000`: total keys across all objects; bounds the combined
//!   size of all key-seen `HashSet`s alive simultaneously.
//!
//! ## One output allocation
//!
//! `Vec<u8>::with_capacity(text.len())` — one allocation, pre-sized at input
//! length. The output can only be shorter than the input (whitespace stripped).
//! Converting to `String` at the end reuses the allocation.

use std::collections::HashSet;

/// Maximum JSON nesting depth before passthrough.
///
/// 500 levels is well above any real Anthropic/OpenAI payload (typically 4–6
/// levels deep). This prevents over-allocation in the frame stack on adversarial
/// inputs. Inherited from the prior engine; kept unchanged for stability.
const MAX_JSON_DEPTH: usize = 500;

/// Maximum number of object keys (cumulative across all objects) before passthrough.
///
/// 10,000 keys bounds worst-case allocation in the per-object key-seen `HashSet`s.
/// Real tool-result schemas have at most ~200 keys; 10,000 is a safe upper bound
/// for all legitimate LLM content blocks.
const MAX_JSON_KEYS: usize = 10_000;

/// Result of a JSON compression attempt.
#[derive(Debug, Clone)]
pub(crate) enum CompressResult {
    /// Minification succeeded: output is shorter than input.
    ///
    /// The content is value-equivalent to the input (lossless whitespace strip).
    /// Number tokens, string contents, key order, and structural bytes are all
    /// preserved byte-exactly. Only insignificant whitespace was removed.
    Compressed {
        /// The minified JSON string.
        content: String,
    },
    /// Minification was skipped; caller should forward original bytes unchanged.
    ///
    /// Causes: invalid JSON, already-minimal input (no whitespace to strip),
    /// depth bound exceeded, key bound exceeded, or duplicate key detected.
    Passthrough,
}

/// Parsing state within an object literal.
///
/// Variants are named for the token that WAS just consumed, not the next expected
/// token — e.g., `Key` means a key was just consumed and `:` comes next.
#[allow(clippy::enum_variant_names)] // `Want*` prefix is intentional state-machine convention
#[derive(Clone, Copy, PartialEq, Eq)]
enum ObjState {
    /// Just after `{` or after `,`: the next token is a key string.
    WantKey,
    /// After a key string: the next token is `:`.
    WantColon,
    /// After `:`: the next token is a value.
    WantValue,
    /// After a complete value: the next token is `,` or `}`.
    WantCommaOrEnd,
}

/// Context frame for the per-object/array nesting stack.
enum Frame {
    /// Object frame: tracks seen keys and the current parsing state.
    Object {
        /// Raw byte representation of each key seen so far in this object.
        /// Used for O(1) duplicate detection.
        seen_keys: HashSet<Vec<u8>>,
        /// Current state within this object.
        state: ObjState,
    },
    /// Array frame: no key tracking needed.
    Array,
}

/// Minify a JSON content block by stripping insignificant whitespace.
///
/// # Invariants
///
/// - **Lossless**: string contents, number tokens, key order, and all structural
///   bytes are preserved exactly. Only whitespace outside string literals is removed.
/// - **Byte-exact numbers**: `1e3` stays `1e3`, `1.0` stays `1.0` — never rewritten.
/// - **Fail open**: any error (invalid JSON, bound exceeded, duplicate key) returns
///   `Passthrough`. The caller must forward the original bytes unchanged.
///
/// # Arguments
///
/// - `text`: raw text payload of the block (UTF-8).
///
/// # Returns
///
/// `CompressResult::Compressed` when the minified output is strictly shorter
/// than the input. `CompressResult::Passthrough` in all other cases.
pub(crate) fn compress_json(text: &str) -> CompressResult {
    // Pre-validate: serde_json handles all JSON edge cases (invalid escapes,
    // invalid UTF-8, unmatched brackets, trailing garbage). If validation fails,
    // return Passthrough — never silently corrupt or produce invalid output.
    if serde_json::from_str::<Box<serde_json::value::RawValue>>(text).is_err() {
        return CompressResult::Passthrough;
    }

    // Single-pass minifier.
    // One output allocation, pre-sized at input length (output can only shrink).
    let mut output: Vec<u8> = Vec::with_capacity(text.len());

    // String-boundary tracking.
    let mut in_string = false;
    let mut escape_next = false;

    // Nesting stack and depth counter.
    let mut stack: Vec<Frame> = Vec::new();
    let mut depth: usize = 0;

    // Key accumulator: collects raw bytes of the current key string.
    // Reused across keys (capacity retained).
    let mut key_buf: Vec<u8> = Vec::new();
    let mut reading_key = false;

    // Cumulative key count across all objects (bounds key-set allocation).
    let mut total_keys: usize = 0;

    for &byte in text.as_bytes() {
        // ── Escape sequence inside a string ──────────────────────────────────
        if escape_next {
            escape_next = false;
            // Copy the escaped character verbatim — never rewrite escapes.
            output.push(byte);
            if reading_key {
                key_buf.push(byte);
            }
            continue;
        }

        // ── Start of escape sequence ──────────────────────────────────────────
        if in_string && byte == b'\\' {
            escape_next = true;
            output.push(b'\\');
            if reading_key {
                key_buf.push(b'\\');
            }
            continue;
        }

        // ── String boundary ───────────────────────────────────────────────────
        if byte == b'"' {
            if in_string {
                // End of string literal.
                in_string = false;
                output.push(b'"');

                if reading_key {
                    // Key string complete: insert raw bytes into seen_keys.
                    reading_key = false;
                    let key = std::mem::take(&mut key_buf);
                    if let Some(Frame::Object { seen_keys, state }) = stack.last_mut() {
                        if !seen_keys.insert(key) {
                            // Duplicate key — fail open immediately.
                            return CompressResult::Passthrough;
                        }
                        total_keys += 1;
                        if total_keys > MAX_JSON_KEYS {
                            return CompressResult::Passthrough;
                        }
                        *state = ObjState::WantColon;
                    }
                } else {
                    // Value string complete: advance object state.
                    if let Some(Frame::Object { state, .. }) = stack.last_mut()
                        && *state == ObjState::WantValue
                    {
                        *state = ObjState::WantCommaOrEnd;
                    }
                }
            } else {
                // Start of string literal.
                in_string = true;
                output.push(b'"');

                // Detect key position: object frame in WantKey state.
                if let Some(Frame::Object { state, .. }) = stack.last()
                    && *state == ObjState::WantKey
                {
                    reading_key = true;
                    key_buf.clear();
                }
            }
            continue;
        }

        // ── Inside string: copy bytes verbatim ───────────────────────────────
        if in_string {
            output.push(byte);
            if reading_key {
                key_buf.push(byte);
            }
            continue;
        }

        // ── Outside string: structural / scalar bytes ─────────────────────────
        match byte {
            // Insignificant whitespace: the core operation — drop these bytes.
            b' ' | b'\t' | b'\n' | b'\r' => {}

            b'{' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return CompressResult::Passthrough;
                }
                stack.push(Frame::Object {
                    seen_keys: HashSet::new(),
                    state: ObjState::WantKey,
                });
                output.push(b'{');
            }

            b'[' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return CompressResult::Passthrough;
                }
                stack.push(Frame::Array);
                output.push(b'[');
            }

            b'}' => {
                let _ = stack.pop();
                depth = depth.saturating_sub(1);
                // Closing `}` ends a nested container that was itself a value
                // for the parent object; advance the parent's state.
                if let Some(Frame::Object { state, .. }) = stack.last_mut()
                    && *state == ObjState::WantValue
                {
                    *state = ObjState::WantCommaOrEnd;
                }
                output.push(b'}');
            }

            b']' => {
                let _ = stack.pop();
                depth = depth.saturating_sub(1);
                // Same as `}`: closing `]` ends an array value for the parent object.
                if let Some(Frame::Object { state, .. }) = stack.last_mut()
                    && *state == ObjState::WantValue
                {
                    *state = ObjState::WantCommaOrEnd;
                }
                output.push(b']');
            }

            b':' => {
                if let Some(Frame::Object { state, .. }) = stack.last_mut()
                    && *state == ObjState::WantColon
                {
                    *state = ObjState::WantValue;
                }
                output.push(b':');
            }

            b',' => {
                if let Some(Frame::Object { state, .. }) = stack.last_mut() {
                    // A comma ends the current value (scalar or container).
                    // Both ExpectCommaOrEnd (normal path) and ExpectValue (scalar
                    // ended without an explicit container close) → ExpectKey.
                    if matches!(*state, ObjState::WantCommaOrEnd | ObjState::WantValue) {
                        *state = ObjState::WantKey;
                    }
                }
                // Arrays: no state change needed.
                output.push(b',');
            }

            _ => {
                // Scalar characters: digits, letters (for null/true/false/keywords),
                // '-', '.', '+', 'e', 'E'. Copy verbatim — NEVER rewrite number tokens.
                // State transition for scalars happens via `,`, `}`, or `]` above.
                output.push(byte);
            }
        }
    }

    // No-op guard: if the output is byte-identical to the input, there was no
    // whitespace to strip. Return Passthrough rather than zero-savings Compressed
    // (the byte gate would reject it anyway, but this is cleaner).
    if output.as_slice() == text.as_bytes() {
        return CompressResult::Passthrough;
    }

    // Convert output bytes to String.
    //
    // SAFETY: the pre-validation pass confirmed the input is valid UTF-8.
    // The scanner only copies bytes from the input (never synthesizes new bytes).
    // Subsetting a valid UTF-8 byte sequence (by removing ASCII whitespace, which
    // is always a complete 1-byte UTF-8 sequence) yields valid UTF-8.
    match String::from_utf8(output) {
        Ok(content) => CompressResult::Compressed { content },
        Err(_) => CompressResult::Passthrough, // should never happen
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // =========================================================================
    // AC5 — Valid JSON output guarantee (structural tests carried over from
    // old engine; adapted for lossless semantics)
    // =========================================================================

    #[test]
    fn output_is_valid_json_for_object_input() {
        let json = r#"{"name": "Alice", "age": 30, "active": true, "score": 9.5, "meta": null}"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { ref content } => {
                let reparsed: Result<serde_json::Value, _> = serde_json::from_str(content);
                assert!(
                    reparsed.is_ok(),
                    "AC5: lossless output must be valid JSON; got: {content}"
                );
            }
            CompressResult::Passthrough => {
                panic!("Expected Compressed for valid pretty-printed JSON object")
            }
        }
    }

    #[test]
    fn output_is_valid_json_for_array_of_objects() {
        let json = r#"[{"a": 1, "b": "hello"}, {"a": 2, "b": "world"}]"#;
        let result = compress_json(json);
        match result {
            CompressResult::Compressed { ref content } => {
                let reparsed: Result<serde_json::Value, _> = serde_json::from_str(content);
                assert!(
                    reparsed.is_ok(),
                    "AC5: lossless output must be valid JSON; got: {content}"
                );
            }
            CompressResult::Passthrough => {
                panic!("Expected Compressed for valid JSON array of objects")
            }
        }
    }

    // =========================================================================
    // Lossless — keys preserved, structure preserved, values unchanged
    // =========================================================================

    #[test]
    fn top_level_object_keys_preserved() {
        let json = r#"{"key1": "value1", "key2": 42}"#;
        match compress_json(json) {
            CompressResult::Compressed { content } => {
                let reparsed: serde_json::Value =
                    serde_json::from_str(&content).expect("valid JSON");
                let obj = reparsed.as_object().expect("object");
                assert!(obj.contains_key("key1"), "key1 must be preserved");
                assert!(obj.contains_key("key2"), "key2 must be preserved");
                // Values must be unchanged.
                assert_eq!(obj["key1"], serde_json::Value::String("value1".into()));
                assert_eq!(obj["key2"], serde_json::Value::Number(42.into()));
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    #[test]
    fn top_level_array_all_elements_preserved() {
        // Lossless: all array elements must be present (no first-element-only truncation).
        let json = r#"[{"a": 1}, {"a": 2}, {"a": 3}]"#;
        match compress_json(json) {
            CompressResult::Compressed { content } => {
                let reparsed: serde_json::Value =
                    serde_json::from_str(&content).expect("valid JSON");
                let arr = reparsed.as_array().expect("array");
                // All 3 elements preserved (unlike the lossy engine which kept only first).
                assert_eq!(
                    arr.len(),
                    3,
                    "all array elements must be preserved (lossless)"
                );
            }
            CompressResult::Passthrough => panic!("Expected Compressed for array input"),
        }
    }

    // =========================================================================
    // Exact minified bytes test
    // =========================================================================

    /// Verify the exact minified output for a known pretty-printed input.
    ///
    /// This is the primary discriminating test for correctness: the output must
    /// equal the input with all whitespace stripped and nothing else changed.
    #[test]
    fn exact_minified_bytes() {
        let pretty = "{\n  \"name\": \"Alice\",\n  \"age\": 30\n}";
        match compress_json(pretty) {
            CompressResult::Compressed { content } => {
                assert_eq!(
                    content, r#"{"name":"Alice","age":30}"#,
                    "minified output must strip ALL whitespace and nothing else"
                );
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    // =========================================================================
    // Number token byte-exactness (the primary lossless invariant)
    // =========================================================================

    /// Number tokens must survive byte-exact: 1e10, 1.0, 100.00 must not be rewritten.
    ///
    /// Discriminating: the old lossy engine ran through serde_json::Value which
    /// normalizes numbers (1e10 → 10000000000.0, etc.). This test fails on that engine.
    #[test]
    fn number_tokens_byte_exact() {
        let json = r#"{"a": 1e10, "b": 1.0, "c": 100.00, "d": -0.5e-3}"#;
        match compress_json(json) {
            CompressResult::Compressed { content } => {
                assert!(
                    content.contains("1e10"),
                    "1e10 must survive byte-exact, got: {content}"
                );
                assert!(
                    content.contains("1.0"),
                    "1.0 must survive byte-exact, got: {content}"
                );
                assert!(
                    content.contains("100.00"),
                    "100.00 must survive byte-exact, got: {content}"
                );
                assert!(
                    content.contains("-0.5e-3"),
                    "-0.5e-3 must survive byte-exact, got: {content}"
                );
                // Also confirm the output is valid JSON.
                serde_json::from_str::<serde_json::Value>(&content)
                    .expect("output must be valid JSON");
            }
            CompressResult::Passthrough => panic!("Expected Compressed for number-token input"),
        }
    }

    // =========================================================================
    // Key order preserved
    // =========================================================================

    /// Key order in output must match key order in input (lossless, no BTreeMap sort).
    ///
    /// Discriminating: the old canonical engine used BTreeMap which sorts keys.
    /// This engine must preserve insertion order.
    #[test]
    fn key_order_preserved() {
        let json = r#"{"z": 1, "a": 2, "m": 3}"#;
        match compress_json(json) {
            CompressResult::Compressed { content } => {
                // Find positions of keys in the output.
                let pos_z = content.find("\"z\"").expect("z key must exist");
                let pos_a = content.find("\"a\"").expect("a key must exist");
                let pos_m = content.find("\"m\"").expect("m key must exist");
                assert!(
                    pos_z < pos_a && pos_a < pos_m,
                    "key order z/a/m must be preserved: got {content}"
                );
            }
            CompressResult::Passthrough => panic!("Expected Compressed"),
        }
    }

    // =========================================================================
    // Duplicate key → Passthrough
    // =========================================================================

    /// Duplicate key in input → Passthrough (both directions).
    ///
    /// Discriminating: if dup-key detection is removed, the duplicate is silently
    /// dropped (serde_json last-wins) and Compressed is returned instead.
    #[test]
    fn duplicate_key_returns_passthrough() {
        let json_dup = r#"{"a": 1, "b": 2, "a": 3}"#;
        assert!(
            matches!(compress_json(json_dup), CompressResult::Passthrough),
            "duplicate top-level key must return Passthrough"
        );
    }

    /// Nested same-name keys in DIFFERENT objects must NOT be treated as duplicates.
    ///
    /// Discriminating: if the key-seen sets are shared across object levels
    /// (instead of per-object), this would incorrectly return Passthrough.
    #[test]
    fn nested_same_name_keys_different_objects_not_dup() {
        // "a" appears in the outer object AND in the inner object.
        // These are distinct objects — not duplicates.
        let json = r#"{"a": 1, "inner": {"a": 2}}"#;
        match compress_json(json) {
            CompressResult::Compressed { content } => {
                // Both "a" values must be present.
                let v: serde_json::Value = serde_json::from_str(&content).expect("valid JSON");
                assert_eq!(v["a"], 1, "outer 'a' must be 1");
                assert_eq!(v["inner"]["a"], 2, "inner 'a' must be 2");
            }
            CompressResult::Passthrough => {
                panic!("nested same-name keys in different objects must NOT be Passthrough")
            }
        }
    }

    // =========================================================================
    // No-op passthrough (already minimal)
    // =========================================================================

    /// Already-minimal JSON (no whitespace) → Passthrough.
    ///
    /// Discriminating: if the no-op guard is removed, Compressed is returned
    /// with zero savings (output == input), wasting a byte-gate comparison.
    #[test]
    fn already_minimal_returns_passthrough() {
        let minimal = r#"{"key":"value","n":42}"#;
        assert!(
            matches!(compress_json(minimal), CompressResult::Passthrough),
            "already-minimal JSON must return Passthrough (no whitespace to strip)"
        );
    }

    // =========================================================================
    // Escape preservation
    // =========================================================================

    /// Escape sequences inside strings must survive verbatim.
    ///
    /// This tests that `\"`, `\\`, `\n`, `\t`, `A` are preserved as-is,
    /// not decoded or re-encoded.
    #[test]
    fn escape_sequences_preserved_verbatim() {
        // The raw string has an escaped quote \"A\" inside the value.
        let json = r#"{"k": "\"A\"\t\nA"}"#;
        match compress_json(json) {
            CompressResult::Compressed { content } => {
                // The escape sequences must appear verbatim in the output.
                assert!(
                    content.contains(r#"\"A\""#),
                    "escaped quotes must be preserved verbatim: {content}"
                );
                assert!(
                    content.contains(r"\t"),
                    r"\t escape must be preserved: {content}"
                );
                assert!(
                    content.contains(r"A"),
                    r"A must not be decoded to 'A': {content}"
                );
            }
            CompressResult::Passthrough => {
                // Input has whitespace between key and value → must compress.
                panic!("Expected Compressed for input with whitespace")
            }
        }
    }

    // =========================================================================
    // AC5 negative — parse failure → passthrough
    // =========================================================================

    #[test]
    fn malformed_json_returns_passthrough() {
        assert!(
            matches!(compress_json("{invalid json"), CompressResult::Passthrough),
            "malformed JSON must return Passthrough"
        );
    }

    #[test]
    fn truncated_json_returns_passthrough() {
        assert!(
            matches!(
                compress_json(r#"{"key": "val"#),
                CompressResult::Passthrough
            ),
            "truncated JSON must return Passthrough"
        );
    }

    // =========================================================================
    // Depth bound exceeded → passthrough
    // =========================================================================

    #[test]
    fn depth_exceeded_returns_passthrough() {
        let mut json = String::new();
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            json.push_str(r#"{"a": "#);
        }
        json.push('1');
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            json.push('}');
        }
        assert!(
            matches!(compress_json(&json), CompressResult::Passthrough),
            "depth-exceeded JSON must return Passthrough"
        );
    }

    // =========================================================================
    // Determinism
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
                ) => assert_eq!(c1, c2, "output must be deterministic"),
                (CompressResult::Passthrough, CompressResult::Passthrough) => {}
                _ => panic!("result variant changed across runs"),
            }
        }
    }

    // =========================================================================
    // Proptest — round-trip value equivalence + number token exactness
    // =========================================================================

    use proptest::prelude::*;

    proptest! {
        /// For any pretty-printed JSON array with at least one element, the
        /// minified output is ALWAYS `Compressed` and value-equivalent.
        ///
        /// ## Why arrays and not objects
        ///
        /// Arrays eliminate the duplicate-key concern: every element is kept
        /// regardless of value, so `serde_json::to_string_pretty` on an array
        /// with >= 1 element ALWAYS produces removable whitespace (`[\n  …\n]`).
        /// This makes the Passthrough arm unreachable and therefore a hard
        /// failure — not a vacuous pass.
        ///
        /// ## Discriminating
        ///
        /// Deleting the scanner or making it lossy causes either:
        /// (a) the variant to become Passthrough (arm below panics), or
        /// (b) the value comparison to fail.
        #[test]
        fn prop_minify_round_trip_value_equivalent(
            // Arbitrary scalar values wrapped in an array — guarantees composite
            // with >= 1 element so pretty-printing always produces whitespace.
            values in proptest::collection::vec(
                prop_oneof![
                    proptest::string::string_regex("[a-zA-Z0-9 ]{1,20}").unwrap()
                        .prop_map(serde_json::Value::String),
                    (0i64..100_000i64).prop_map(|n| serde_json::Value::Number(n.into())),
                    any::<bool>().prop_map(serde_json::Value::Bool),
                ],
                1..=6usize
            )
        ) {
            // Wrap in an array: always a composite with >= 1 element.
            // `serde_json::to_string_pretty` on such an array always emits
            // `[\n  <element>,\n  …\n]` — whitespace is structurally guaranteed.
            let value = serde_json::Value::Array(values);
            let pretty = serde_json::to_string_pretty(&value).expect("must serialize");

            let result = compress_json(&pretty);

            match result {
                CompressResult::Compressed { ref content } => {
                    // Value-equivalent via serde_json::Value round-trip.
                    let orig_val: serde_json::Value =
                        serde_json::from_str(&pretty).expect("original must parse");
                    let min_val: serde_json::Value =
                        serde_json::from_str(content).expect("minified must parse as JSON");
                    prop_assert_eq!(
                        &orig_val, &min_val,
                        "minified output must be value-equivalent to original"
                    );
                    // Output is strictly shorter (whitespace was stripped).
                    prop_assert!(
                        content.len() < pretty.len(),
                        "Compressed output must be shorter than pretty input"
                    );
                }
                CompressResult::Passthrough => {
                    // Unreachable: the input is always a non-empty array rendered
                    // with `to_string_pretty`, which always contains whitespace.
                    // A Passthrough here means the engine is broken.
                    prop_assert!(
                        false,
                        "compress_json returned Passthrough for a pretty-printed array \
                         with >= 1 element; expected Compressed. Input was: {pretty}"
                    );
                }
            }
        }
    }
}
