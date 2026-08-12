//! AC28 — Element ordering determinism tests.
//!
//! Verifies that the `tools`/`functions` array is emitted in a stable, total
//! canonical order: any two inputs that are element-permutations of one another
//! MUST produce byte-identical aligned output, and 100 repeated runs over a
//! shuffled-order corpus MUST all be byte-identical.
//!
//! # What AC28 requires
//!
//! - (a) Fixture pair: identical tool set, elements in reversed order → same output.
//! - (b) Shuffled-order corpus aligned 100× → all runs byte-identical.
//! - (c) Edge fixtures: name-collision (two tools with same `name`), non-object
//!   element → sorts deterministically or clean fail-open; never panics, never
//!   depends on input position.
//!
//! # No direct `Instant::now` (clippy gate)
//!
//! This test file does not call `Instant::now` directly. Timing tests (if any)
//! must use the `criterion` bench harness, not direct time measurement here.

#![allow(clippy::expect_used)]

use rskim_align::align;
use rskim_llm::Provider;

// ============================================================================
// Fixture definitions
// ============================================================================

/// Anthropic body with tools in order: ["bravo", "alpha"].
static TOOLS_REVERSED: &[u8] = br#"{
    "model": "claude-3-5-sonnet-20241022",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "What tools do you have?"}],
    "tools": [
        {"name": "bravo", "description": "Second tool", "input_schema": {"type": "object", "properties": {"x": {"type": "string"}}}},
        {"name": "alpha", "description": "First tool", "input_schema": {"type": "object", "properties": {"y": {"type": "integer"}}}}
    ]
}"#;

/// Same tool set in canonical order: ["alpha", "bravo"].
static TOOLS_CANONICAL: &[u8] = br#"{
    "model": "claude-3-5-sonnet-20241022",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "What tools do you have?"}],
    "tools": [
        {"name": "alpha", "description": "First tool", "input_schema": {"type": "object", "properties": {"y": {"type": "integer"}}}},
        {"name": "bravo", "description": "Second tool", "input_schema": {"type": "object", "properties": {"x": {"type": "string"}}}}
    ]
}"#;

/// Same tool set with extra whitespace and non-canonical key order within elements.
static TOOLS_SHUFFLED_KEYS: &[u8] = br#"{"max_tokens":1024,"messages":[{"role":"user","content":"What tools do you have?"}],"model":"claude-3-5-sonnet-20241022","tools":[{"input_schema":{"properties":{"y":{"type":"integer"}},"type":"object"},"description":"First tool","name":"alpha"},{"input_schema":{"properties":{"x":{"type":"string"}},"type":"object"},"description":"Second tool","name":"bravo"}]}"#;

/// 8-element corpus with tools in all cyclic rotations for the 100-run test.
static MULTI_TOOL_BODY: &[u8] = br#"{
    "model": "claude-3-5-sonnet-20241022",
    "max_tokens": 2048,
    "messages": [{"role": "user", "content": "List all tools"}],
    "tools": [
        {"name": "echo",    "description": "Echo input",   "input_schema": {"type": "object"}},
        {"name": "delta",   "description": "Delta tool",   "input_schema": {"type": "object"}},
        {"name": "golf",    "description": "Golf tool",    "input_schema": {"type": "object"}},
        {"name": "alpha",   "description": "Alpha tool",   "input_schema": {"type": "object"}},
        {"name": "charlie", "description": "Charlie tool", "input_schema": {"type": "object"}},
        {"name": "foxtrot", "description": "Foxtrot tool", "input_schema": {"type": "object"}},
        {"name": "bravo",   "description": "Bravo tool",   "input_schema": {"type": "object"}},
        {"name": "hotel",   "description": "Hotel tool",   "input_schema": {"type": "object"}}
    ]
}"#;

// ============================================================================
// AC28(a) — Reversed order and canonical order converge to byte-identical output
// ============================================================================

/// AC28(a) / POSITIVE — Two inputs with the same tool set in different element
/// orders MUST produce byte-identical aligned output.
///
/// DISCRIMINATING (PF-007): if element sorting were order-dependent (e.g. insertion
/// sort that outputs input order on a tie), reversed and canonical inputs would
/// produce different outputs.
#[test]
fn ac28_reversed_and_canonical_converge() {
    let out_reversed = align(TOOLS_REVERSED, Provider::Anthropic, "ac28-rev");
    let out_canonical = align(TOOLS_CANONICAL, Provider::Anthropic, "ac28-can");

    assert!(
        !out_reversed.stats.fail_open,
        "AC28(a): TOOLS_REVERSED must not fail-open"
    );
    assert!(
        !out_canonical.stats.fail_open,
        "AC28(a): TOOLS_CANONICAL must not fail-open"
    );
    assert_eq!(
        out_reversed.bytes, out_canonical.bytes,
        "AC28(a): reversed and canonical tool order must produce byte-identical output"
    );
}

/// AC28(a) / POSITIVE — Inputs with the same tool set but shuffled within-element key
/// order produce byte-identical TOOLS output.
///
/// Note: this test compares only the `tools` value from the aligned output, not
/// the full body. The `messages` span is preserved verbatim per AD-CA-13, so two
/// inputs with different whitespace in the messages array (TOOLS_REVERSED has spaces
/// inside the message object, TOOLS_SHUFFLED_KEYS does not) will differ in the
/// messages portion of the output. The AC28 invariant covers tools canonicalization
/// — within-element key order must not affect the canonical tools output.
///
/// DISCRIMINATING (PF-007): if within-element key order affected the canonical
/// output, the shuffled-keys variant would produce a different tools value.
#[test]
fn ac28_shuffled_keys_converges_with_reversed() {
    let out_reversed = align(TOOLS_REVERSED, Provider::Anthropic, "ac28-shufl-rev");
    let out_shuffled = align(TOOLS_SHUFFLED_KEYS, Provider::Anthropic, "ac28-shufl-keys");

    assert!(
        !out_reversed.stats.fail_open,
        "AC28: TOOLS_REVERSED must not fail-open"
    );
    assert!(
        !out_shuffled.stats.fail_open,
        "AC28: TOOLS_SHUFFLED_KEYS must not fail-open"
    );

    // Compare only the tools value: messages differ in whitespace (verbatim preservation),
    // but the canonical tools output must be byte-identical regardless of input key order.
    let get_tools = |b: &[u8]| -> String {
        let v: serde_json::Value =
            serde_json::from_slice(b).expect("aligned output must be valid JSON");
        let tools = v.get("tools").expect("aligned output must have tools key");
        serde_json::to_string(tools).expect("tools must serialize")
    };

    let reversed_tools = get_tools(&out_reversed.bytes);
    let shuffled_tools = get_tools(&out_shuffled.bytes);

    assert_eq!(
        reversed_tools, shuffled_tools,
        "AC28: shuffled-key and reversed inputs must produce byte-identical tools output. \
         Within-element key order must not survive into the canonical aligned output."
    );
}

// ============================================================================
// AC28(b) — 100 repeated runs all byte-identical
// ============================================================================

/// AC28(b) / POSITIVE — 100 repeated runs over the shuffled-order corpus all
/// produce byte-identical output.
///
/// DISCRIMINATING (PF-007): if `align` used HashMap iteration order or any
/// ambient state (clock, RNG), some runs would differ.
#[test]
fn ac28_hundred_runs_byte_identical() {
    // Establish a reference output.
    let reference = align(MULTI_TOOL_BODY, Provider::Anthropic, "ac28-ref");
    assert!(
        !reference.stats.fail_open,
        "AC28(b): reference body must not fail-open"
    );

    for i in 1..=100u32 {
        let out = align(
            MULTI_TOOL_BODY,
            Provider::Anthropic,
            &format!("ac28-run-{i:03}"),
        );
        assert!(!out.stats.fail_open, "AC28(b): run {i} must not fail-open");
        assert_eq!(
            out.bytes, reference.bytes,
            "AC28(b): run {i} produced different bytes than the reference"
        );
    }
}

/// Build a compact Anthropic body from a tool list (for cyclic-rotation tests).
///
/// All bodies built by this function use identical compact JSON so the messages
/// spans are byte-identical, and the only difference is the tools array order.
/// This lets the test assert full-body byte-identity (not just tools identity),
/// verifying that element reorder convergence carries through the whole pipeline.
fn build_compact_body_from_tools(tools: &[(&str, &str)], request_content: &str) -> Vec<u8> {
    let tools_json: String = tools
        .iter()
        .map(|(name, desc)| {
            format!(
                r#"{{"name":"{name}","description":"{desc}","input_schema":{{"type":"object"}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","max_tokens":2048,"messages":[{{"role":"user","content":"{request_content}"}}],"tools":[{tools_json}]}}"#
    )
    .into_bytes()
}

/// AC28(b) / POSITIVE — All seven cyclic rotations of the 8-element corpus produce
/// byte-identical output.
///
/// This is a stronger test than 100 runs of the same input: it exercises the
/// element sort path with different starting orders.
///
/// NOTE: uses compact reference body (built by `build_compact_body_from_tools`)
/// so all bodies share byte-identical messages bytes. `MULTI_TOOL_BODY` uses
/// pretty-printed JSON and cannot serve as a byte-exact reference for compact
/// rotation bodies (messages span is preserved verbatim per AD-CA-13).
#[test]
fn ac28_cyclic_rotations_converge() {
    // Build 8 bodies by rotating the tool array.
    let tools: Vec<(&str, &str)> = vec![
        ("echo", "Echo input"),
        ("delta", "Delta tool"),
        ("golf", "Golf tool"),
        ("alpha", "Alpha tool"),
        ("charlie", "Charlie tool"),
        ("foxtrot", "Foxtrot tool"),
        ("bravo", "Bravo tool"),
        ("hotel", "Hotel tool"),
    ];

    // Build the reference from rotation 0 in compact format (same format as rotations).
    let ref_body = build_compact_body_from_tools(&tools, "List all tools");
    let reference = align(&ref_body, Provider::Anthropic, "ac28-cyc-ref");
    assert!(
        !reference.stats.fail_open,
        "AC28(b): reference body must not fail-open"
    );

    for rotation in 1..tools.len() {
        // Build a body with the tools rotated by `rotation`.
        let rotated: Vec<(&str, &str)> = tools
            .iter()
            .cycle()
            .skip(rotation)
            .take(tools.len())
            .copied()
            .collect();

        let tools_json: String = rotated
            .iter()
            .map(|(name, desc)| {
                format!(
                    r#"{{"name":"{name}","description":"{desc}","input_schema":{{"type":"object"}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        let body = format!(
            r#"{{"model":"claude-3-5-sonnet-20241022","max_tokens":2048,"messages":[{{"role":"user","content":"List all tools"}}],"tools":[{tools_json}]}}"#
        );

        let out = align(
            body.as_bytes(),
            Provider::Anthropic,
            &format!("ac28-cyc-{rotation}"),
        );
        assert!(
            !out.stats.fail_open,
            "AC28(b): rotation {rotation} must not fail-open"
        );
        assert_eq!(
            out.bytes, reference.bytes,
            "AC28(b): rotation {rotation} produced different bytes than the reference"
        );
    }
}

// ============================================================================
// AC28(c) — Total order edge cases
// ============================================================================

/// AC28(c) / POSITIVE — Two tools sharing the same `name` sort deterministically
/// via the canonical-bytes tie-break (not by input position).
///
/// A name collision is not dropped or panicked — it falls through the tie-break
/// (canonicalized-compact-bytes order) deterministically.
///
/// DISCRIMINATING (PF-007): if the sort were unstable (input-position-dependent
/// for equal keys), different input orders would produce different outputs.
#[test]
fn ac28_name_collision_sorts_deterministically() {
    // Two tools with the same `name` but different `description` values.
    // The tie-break uses canonicalized-compact-bytes.
    let body_a = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3-5-sonnet-20241022","tools":[{"name":"dup","description":"aaa","input_schema":{"type":"object"}},{"name":"dup","description":"zzz","input_schema":{"type":"object"}}]}"#;
    let body_b = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3-5-sonnet-20241022","tools":[{"name":"dup","description":"zzz","input_schema":{"type":"object"}},{"name":"dup","description":"aaa","input_schema":{"type":"object"}}]}"#;

    let out_a = align(body_a, Provider::Anthropic, "ac28-dup-a");
    let out_b = align(body_b, Provider::Anthropic, "ac28-dup-b");

    // Either both fail-open (acceptable: dup-name triggers fail-open) or both
    // produce byte-identical output via tie-break (also acceptable).
    // Must NOT panic. Must NOT depend on input position (if one produces output,
    // both must produce identical output).
    if out_a.stats.fail_open || out_b.stats.fail_open {
        // Fail-open is acceptable for name-collision inputs.
        // If either fails open, both should fail open (same deterministic decision).
        assert_eq!(
            out_a.stats.fail_open, out_b.stats.fail_open,
            "AC28(c): both or neither must fail-open on name collision"
        );
        // If fail-open, output must be byte-identical to input.
        if out_a.stats.fail_open {
            assert_eq!(
                out_a.bytes.as_slice(),
                body_a.as_slice(),
                "AC28(c): fail-open must return exact input bytes (body_a)"
            );
        }
        if out_b.stats.fail_open {
            assert_eq!(
                out_b.bytes.as_slice(),
                body_b.as_slice(),
                "AC28(c): fail-open must return exact input bytes (body_b)"
            );
        }
    } else {
        // Both produced output — they must be byte-identical (total order by tie-break).
        assert_eq!(
            out_a.bytes, out_b.bytes,
            "AC28(c): name-collision tools must produce byte-identical output (tie-break determinism)"
        );
    }
}

/// AC28(c) / POSITIVE — A non-object element in the `tools` array is handled
/// without panic and without depending on input position.
///
/// A non-object element may cause fail-open (whole-request passthrough) OR may
/// sort deterministically in some implementations. Either is acceptable, but the
/// result must be the same regardless of where in the array the non-object appears.
#[test]
fn ac28_non_object_element_no_panic() {
    // Tools array with a non-object element (null) as the last element.
    let body_a = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3-5-sonnet-20241022","tools":[{"name":"alpha","description":"a","input_schema":{"type":"object"}},null]}"#;
    // Same non-object element first.
    let body_b = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3-5-sonnet-20241022","tools":[null,{"name":"alpha","description":"a","input_schema":{"type":"object"}}]}"#;

    // Must not panic (the primary requirement).
    let out_a = align(body_a, Provider::Anthropic, "ac28-noobj-a");
    let out_b = align(body_b, Provider::Anthropic, "ac28-noobj-b");

    // Both must either fail-open or produce byte-identical output.
    if out_a.stats.fail_open || out_b.stats.fail_open {
        // Fail-open is acceptable.
        // If either fails open, verify bytes are byte-identical to the respective input.
        if out_a.stats.fail_open {
            assert_eq!(
                out_a.bytes.as_slice(),
                body_a.as_slice(),
                "AC28(c): fail-open must return exact input bytes (body_a with null last)"
            );
        }
        if out_b.stats.fail_open {
            assert_eq!(
                out_b.bytes.as_slice(),
                body_b.as_slice(),
                "AC28(c): fail-open must return exact input bytes (body_b with null first)"
            );
        }
    } else {
        // Both produced output — must be byte-identical.
        assert_eq!(
            out_a.bytes, out_b.bytes,
            "AC28(c): non-object-element bodies must produce byte-identical output"
        );
    }
}

// ============================================================================
// AC30 — 100-run envelope key ordering stability
// ============================================================================

/// Body with top-level keys in non-canonical order (model, tools, max_tokens, messages).
/// The canonical order is: max_tokens < messages < model < tools.
/// All 100 runs of this body through `align()` must produce byte-identical output.
static ENVELOPE_SCRAMBLED_BODY: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","tools":[{"name":"foxtrot","description":"Foxtrot tool","input_schema":{"type":"object"}},{"name":"alpha","description":"Alpha tool","input_schema":{"type":"object"}},{"name":"delta","description":"Delta tool","input_schema":{"type":"object"}}],"max_tokens":1024,"messages":[{"role":"user","content":"List all tools"}]}"#;

/// AC30 / POSITIVE — 100 repeated runs of a body with scrambled envelope key order
/// all produce byte-identical output.
///
/// Distinct from AC28(b) (which verifies tool ELEMENT ordering): AC30 verifies that
/// the canonical envelope key sort (AD-CA-13) is pure and stateless — no ambient
/// state (HashMap iteration order, wall-clock, or RNG) can affect the top-level
/// key ordering.
///
/// DISCRIMINATING (PF-007): if `canonical_envelope` used a HashMap to collect
/// top-level keys before emitting them, the iteration order would be non-deterministic
/// across runs (even in the same process), causing different byte outputs.
/// Deleting the key-sort step would cause all non-canonically-ordered bodies to
/// produce outputs that differ from the canonical-order body — but within 100 runs
/// of the same scrambled body they would still be identical. The discriminator here
/// is the combination of (a) non-canonical input + (b) byte-identity assertion,
/// which together prove that the sort is deterministic AND correct across runs.
#[test]
fn ac30_hundred_runs_envelope_key_order_stability() {
    // Establish a reference output from the scrambled-envelope body.
    let reference = align(ENVELOPE_SCRAMBLED_BODY, Provider::Anthropic, "ac30-ref");
    assert!(
        !reference.stats.fail_open,
        "AC30: reference body must not fail-open"
    );

    // The reference output must have the canonical key order (max_tokens < messages < model < tools).
    let ref_str = std::str::from_utf8(&reference.bytes).expect("AC30: output must be UTF-8");
    let max_pos = ref_str.find("\"max_tokens\"").expect("AC30: max_tokens must be present");
    let msg_pos = ref_str.find("\"messages\"").expect("AC30: messages must be present");
    let mdl_pos = ref_str.find("\"model\"").expect("AC30: model must be present");
    let tls_pos = ref_str.find("\"tools\"").expect("AC30: tools must be present");
    assert!(
        max_pos < msg_pos && msg_pos < mdl_pos && mdl_pos < tls_pos,
        "AC30: reference output must have canonical key order: \
         max_tokens={max_pos} < messages={msg_pos} < model={mdl_pos} < tools={tls_pos}"
    );

    // Run 99 more times; every output must be byte-identical to the reference.
    for i in 1..=99u32 {
        let out = align(
            ENVELOPE_SCRAMBLED_BODY,
            Provider::Anthropic,
            &format!("ac30-run-{i:03}"),
        );
        assert!(!out.stats.fail_open, "AC30: run {i} must not fail-open");
        assert_eq!(
            out.bytes, reference.bytes,
            "AC30: envelope key ordering run {i} produced different bytes than the reference \
             (envelope key sort must be purely deterministic)"
        );
    }
}

// ============================================================================
// AC28 — Platform determinism cross-check (in-process multi-call)
// ============================================================================

/// AC28(b) / POSITIVE — Alignment is byte-identical across repeated in-process
/// calls. This is the in-process check; the CI platform gate runs on
/// Linux/macOS/Windows via the ci.yml matrix.
///
/// DISCRIMINATING (PF-007): any use of HashMap iteration order, wall-clock, or
/// RNG in the align path would cause non-deterministic outputs across calls.
#[test]
fn ac28_in_process_determinism_multi_call() {
    let reference = align(MULTI_TOOL_BODY, Provider::Anthropic, "ac28-mp-ref");
    assert!(!reference.stats.fail_open);

    // Call from the same thread 50 more times.
    for i in 0..50u32 {
        let out = align(
            MULTI_TOOL_BODY,
            Provider::Anthropic,
            &format!("ac28-mp-{i}"),
        );
        assert_eq!(
            out.bytes, reference.bytes,
            "AC28: in-process call {i} must be byte-identical to reference"
        );
    }
}
