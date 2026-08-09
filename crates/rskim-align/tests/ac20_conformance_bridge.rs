//! AC20 — Conformance-bridge integration test for `CacheAlignContract`.
//!
// Unwrap/expect are necessary in test helpers and non-tautology variant functions.
// The deny is correct for production code (Cargo.toml [lints.clippy]); allow here
// so integration test helpers can use them without per-function attributes.
#![allow(clippy::unwrap_used, clippy::expect_used)]
//!
//! # What this test does
//!
//! `CacheAlignContract` is a *growing* component: it injects `cache_control`
//! markers, so its output may be up to `2 × MARKER_BYTES` larger than its input.
//! The core harness runs `check_never_inflate` unconditionally, so the AC4
//! `"AC4-never-inflate"` invariant WILL fail for this contract.
//!
//! Following the `assert_turn_dropping_fails_append_only` pattern from
//! `rskim_contract::harness::self_test`, this test:
//!
//! 1. Calls `run_conformance_suite_with_extensions` with a custom extension registry.
//! 2. Verifies growth via `MetadataReorderWithMarkers::verify_marker_cap`.
//! 3. Checks only the non-growth invariants by name, never calls `report.all_passed()`.
//! 4. Provides non-tautology variants that fail the targeted invariant.
//!
//! # Extension registry
//!
//! Two extensions are registered:
//! - `"lossless-content"` — custom check using `tools_arrays_set_equal` for the
//!   `tools` array (permits element reorder, rejects drop/dup/mutation) plus strict
//!   per-block message-content comparison (order-sensitive, byte-sensitive).
//! - `"marker-immutability"` — built-in `marker_immutability_check`.
//!
//! # Non-tautology (AC20 / PF-007)
//!
//! Five broken variant structs each trigger exactly the targeted invariant:
//! - `OverInjector` — injects >2 markers → fails `verify_marker_cap`
//! - `TurnDropper` — drops the last message → fails `AC8-append-only`
//! - `DescriptionMutator` — alters a tool description → fails `ext:lossless-content`
//! - `ElementDropper` — removes a tool element → fails `ext:lossless-content` (direct test)
//! - `ClientMarkerDropper` — strips a client `cache_control` → fails `ext:marker-immutability`

use rskim_align::CacheAlignContract;
use rskim_contract::canonical::tools_arrays_set_equal;
use rskim_contract::contract::{Contract, Outcome};
use rskim_contract::extension::{ExtensionRegistry, marker_immutability_check};
use rskim_contract::harness::run_conformance_suite_with_extensions;
use rskim_contract::waiver::MetadataReorderWithMarkers;

// ============================================================================
// Custom lossless-content extension check (AC20)
// ============================================================================

/// Build the `"lossless-content"` extension check for `CacheAlignContract`.
///
/// # Semantics
///
/// This check passes iff:
/// - `output == input` (byte-identical), **or**
/// - The `tools`/`functions` array in the output is **set-equal** to the input
///   (`tools_arrays_set_equal`): element reorder is permitted, but any drop,
///   duplicate, or mutation is a failure.
/// - Message text content blocks are **byte-identical** (order-sensitive,
///   element-count-identical).
///
/// The standard `lossless_content_check` from `rskim-contract` only checks
/// message text blocks. This custom variant adds the `tools` set-equality gate
/// so the sanctioned element reorder passes while any lossy change fails.
pub fn cache_align_lossless_content_check() -> Box<rskim_contract::extension::InvariantCheck> {
    Box::new(|input: &[u8], output: &[u8]| check_cache_align_lossless(input, output))
}

fn check_cache_align_lossless(input: &[u8], output: &[u8]) -> bool {
    // Byte-identical: trivially passes.
    if input == output {
        return true;
    }
    // Parse both as JSON.
    let Ok(input_str) = std::str::from_utf8(input) else {
        return false;
    };
    let Ok(output_str) = std::str::from_utf8(output) else {
        return false;
    };
    let Ok(input_val) = serde_json::from_str::<serde_json::Value>(input_str) else {
        return false;
    };
    let Ok(output_val) = serde_json::from_str::<serde_json::Value>(output_str) else {
        return false;
    };

    // 1. Check tools/functions set-equality (element reorder permitted; drop/dup/mutation not).
    if !tools_set_equal_check(&input_val, &output_val) {
        return false;
    }

    // 2. Check message content text blocks (strict: byte-identical, order-sensitive).
    if !message_content_unchanged(&input_val, &output_val) {
        return false;
    }

    true
}

/// Serialize a `serde_json::Value` to a compact string for comparison.
fn value_to_compact(v: &serde_json::Value) -> Option<String> {
    serde_json::to_string(v).ok()
}

/// Strip `cache_control` from every element of a tools/functions array.
///
/// `CacheAlignContract` injects `{"type":"ephemeral"}` markers into the last tool
/// object. The `ext:lossless-content` check compares tool content (names, descriptions,
/// schemas) — NOT skim-added markers. Stripping `cache_control` from both input and
/// output before comparing lets the check detect actual lossy mutations while ignoring
/// the sanctioned marker injection.
///
/// Client-added `cache_control` fields (tested separately by `ext:marker-immutability`)
/// are also stripped here; their presence is verified by the immutability check, not by
/// the set-equality gate.
fn strip_cc_from_array(v: &serde_json::Value) -> serde_json::Value {
    let arr = match v.as_array() {
        Some(a) => a,
        None => return v.clone(),
    };
    let stripped: Vec<serde_json::Value> = arr
        .iter()
        .map(|elem| match elem.as_object() {
            Some(map) => {
                let filtered: serde_json::Map<String, serde_json::Value> = map
                    .iter()
                    .filter(|(k, _)| k.as_str() != "cache_control")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                serde_json::Value::Object(filtered)
            }
            None => elem.clone(),
        })
        .collect();
    serde_json::Value::Array(stripped)
}

/// Compare the `tools` (or `functions`) array between input and output using
/// `tools_arrays_set_equal` (multiset equality: elements may reorder, but no
/// drop/dup/mutation).
///
/// `cache_control` fields are stripped from both sides before comparison so that
/// skim's marker injection does not trigger a false failure (it is sanctioned behavior
/// verified separately by `ext:marker-immutability`).
fn tools_set_equal_check(input_val: &serde_json::Value, output_val: &serde_json::Value) -> bool {
    // Check `tools` key.
    let in_tools = input_val.get("tools");
    let out_tools = output_val.get("tools");

    if in_tools.is_some() || out_tools.is_some() {
        match (in_tools, out_tools) {
            (Some(a), Some(b)) => {
                let a_no_cc = strip_cc_from_array(a);
                let b_no_cc = strip_cc_from_array(b);
                let a_str = value_to_compact(&a_no_cc).unwrap_or_default();
                let b_str = value_to_compact(&b_no_cc).unwrap_or_default();
                if !tools_arrays_set_equal(&a_str, &b_str) {
                    return false;
                }
            }
            (None, None) => {} // both absent: vacuously ok
            _ => return false, // one present, one absent
        }
    }

    // Check `functions` key (OpenAI legacy).
    let in_fns = input_val.get("functions");
    let out_fns = output_val.get("functions");

    if in_fns.is_some() || out_fns.is_some() {
        match (in_fns, out_fns) {
            (Some(a), Some(b)) => {
                let a_no_cc = strip_cc_from_array(a);
                let b_no_cc = strip_cc_from_array(b);
                let a_str = value_to_compact(&a_no_cc).unwrap_or_default();
                let b_str = value_to_compact(&b_no_cc).unwrap_or_default();
                if !tools_arrays_set_equal(&a_str, &b_str) {
                    return false;
                }
            }
            (None, None) => {}
            _ => return false,
        }
    }

    true
}

/// Verify that message text content blocks are byte-identical between input and output.
fn message_content_unchanged(
    input_val: &serde_json::Value,
    output_val: &serde_json::Value,
) -> bool {
    let in_texts = extract_message_texts(input_val);
    let out_texts = extract_message_texts(output_val);
    if in_texts.len() != out_texts.len() {
        return false;
    }
    in_texts.iter().zip(out_texts.iter()).all(|(a, b)| a == b)
}

fn extract_message_texts(val: &serde_json::Value) -> Vec<String> {
    let mut texts = Vec::new();
    let Some(messages) = val.get("messages").and_then(|m| m.as_array()) else {
        return texts;
    };
    for msg in messages {
        match msg.get("content") {
            Some(serde_json::Value::String(s)) => texts.push(s.clone()),
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    if block
                        .get("type")
                        .and_then(|t| t.as_str())
                        .is_some_and(|t| t == "text")
                        && let Some(text) = block.get("text").and_then(|t| t.as_str())
                    {
                        texts.push(text.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    texts
}

// ============================================================================
// Extension registry factory
// ============================================================================

fn build_extension_registry() -> ExtensionRegistry {
    let mut registry = ExtensionRegistry::new();
    registry.register("lossless-content", cache_align_lossless_content_check());
    registry.register("marker-immutability", marker_immutability_check());
    registry
}

// ============================================================================
// AC20 — Main conformance bridge test
// ============================================================================

/// AC20 / POSITIVE — `CacheAlignContract` passes all non-growth invariants.
///
/// `verify_marker_cap` must pass (bounded growth).
/// `AC4-never-inflate` is expected to FAIL (growing component — sanctioned by the waiver).
/// All other named invariants must pass: `AC8-append-only`, `AC12-sacrosanct-redaction`,
/// `ext:lossless-content`, `ext:marker-immutability`.
///
/// DISCRIMINATING (PF-007): does NOT call `report.all_passed()`. Filters by
/// invariant name, mirroring `assert_turn_dropping_fails_append_only`.
#[test]
#[allow(clippy::unwrap_used, clippy::expect_used)]
fn ac20_cache_align_contract_passes_non_growth_invariants() {
    let contract = CacheAlignContract;
    let request_id = "ac20-main-001";

    // ── verify_marker_cap: growth is bounded ─────────────────────────────────
    let body = br#"{"messages":[{"role":"user","content":"Hello"}],"model":"claude-3-5-sonnet-20241022","tools":[{"name":"search","description":"Search the web","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}]}"#;
    let result = contract.apply_reorder(body, request_id);
    let output_bytes = result.expect("apply_reorder must succeed for valid Anthropic body");
    assert!(
        contract.verify_marker_cap(body.len(), output_bytes.len()),
        "AC20: verify_marker_cap must hold — output is within 2×MARKER_BYTES of input"
    );

    // ── run_conformance_suite_with_extensions ─────────────────────────────────
    let registry = build_extension_registry();
    let report = run_conformance_suite_with_extensions(&contract, request_id, Some(&registry));

    // AC4-never-inflate: EXPECTED to fail for a growing component.
    // AC20 instructs us NOT to call report.all_passed() here.
    let ac4_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC4-never-inflate")
        .collect();
    assert!(
        !ac4_results.is_empty(),
        "AC20: harness must run AC4-never-inflate check"
    );
    assert!(
        ac4_results.iter().any(|r| !r.passed),
        "AC20: AC4-never-inflate MUST fail for CacheAlignContract (expected-failed; growth waivered)"
    );

    // AC8-append-only: must PASS.
    let ac8_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC8-append-only")
        .collect();
    assert!(
        !ac8_results.is_empty(),
        "AC20: harness must run AC8-append-only check"
    );
    assert!(
        ac8_results.iter().all(|r| r.passed),
        "AC20: AC8-append-only must pass. Failures: {:#?}",
        ac8_results.iter().filter(|r| !r.passed).collect::<Vec<_>>()
    );

    // AC12 / sacrosanct-redaction: must PASS.
    let ac12_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant.starts_with("AC12"))
        .collect();
    assert!(
        !ac12_results.is_empty(),
        "AC20: harness must run AC12 sacrosanct-redaction checks"
    );
    assert!(
        ac12_results.iter().all(|r| r.passed),
        "AC20: all AC12 invariants must pass. Failures: {:#?}",
        ac12_results
            .iter()
            .filter(|r| !r.passed)
            .collect::<Vec<_>>()
    );

    // ext:lossless-content: must PASS (set-equality permits the element reorder).
    let lossless_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "ext:lossless-content")
        .collect();
    assert!(
        !lossless_results.is_empty(),
        "AC20: harness must run ext:lossless-content extension check"
    );
    assert!(
        lossless_results.iter().all(|r| r.passed),
        "AC20: ext:lossless-content must pass — element reorder is set-equal. \
         Failures: {:#?}",
        lossless_results
            .iter()
            .filter(|r| !r.passed)
            .collect::<Vec<_>>()
    );

    // ext:marker-immutability: must PASS.
    let immutability_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "ext:marker-immutability")
        .collect();
    assert!(
        !immutability_results.is_empty(),
        "AC20: harness must run ext:marker-immutability extension check"
    );
    assert!(
        immutability_results.iter().all(|r| r.passed),
        "AC20: ext:marker-immutability must pass. Failures: {:#?}",
        immutability_results
            .iter()
            .filter(|r| !r.passed)
            .collect::<Vec<_>>()
    );
}

// ============================================================================
// Non-tautology variant tests (AC20 / PF-007)
// ============================================================================

// ── Variant 1: OverInjector — fails verify_marker_cap ────────────────────────

/// A contract that inflates output far beyond the 2×MARKER_BYTES cap.
struct OverInjector;

impl Contract for OverInjector {
    fn component_name(&self) -> &'static str {
        "over-injector"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        if let Some(out) = self.apply_reorder(input, request_id) {
            Outcome::modified(out, input.len(), request_id, self.component_name())
        } else {
            Outcome::passthrough(input.to_vec(), request_id, self.component_name())
        }
    }
}

impl MetadataReorderWithMarkers for OverInjector {
    fn apply_reorder(&self, input: &[u8], _request_id: &str) -> Option<Vec<u8>> {
        // Append 500 bytes of dummy marker data (far exceeds 2 × 37 = 74 cap).
        let mut out = input.to_vec();
        out.extend_from_slice(&[b'x'; 500]);
        Some(out)
    }
}

/// AC20 / NON-TAUTOLOGY: `OverInjector` fails `verify_marker_cap`.
///
/// DISCRIMINATING (PF-007): if `verify_marker_cap` always returned `true`,
/// a contract that inflates output by 500 bytes would pass unchallenged.
#[test]
fn ac20_over_injector_fails_verify_marker_cap() {
    let contract = OverInjector;
    let input_len = 100usize;
    let output_len = input_len + 500; // far exceeds 2 × 37 = 74
    assert!(
        !contract.verify_marker_cap(input_len, output_len),
        "AC20: OverInjector must fail verify_marker_cap (exceeds 2×MARKER_BYTES)"
    );
}

// ── Variant 2: TurnDropper — fails AC8-append-only ───────────────────────────

/// A contract that drops the last message (turn-dropping).
struct TurnDropper;

impl Contract for TurnDropper {
    fn component_name(&self) -> &'static str {
        "turn-dropper"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        if let Some(out) = drop_last_message(input) {
            Outcome::modified(out, input.len(), request_id, self.component_name())
        } else {
            Outcome::passthrough(input.to_vec(), request_id, self.component_name())
        }
    }
}

impl MetadataReorderWithMarkers for TurnDropper {
    fn apply_reorder(&self, input: &[u8], _request_id: &str) -> Option<Vec<u8>> {
        drop_last_message(input)
    }
}

fn drop_last_message(input: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(input).ok()?;
    let mut val: serde_json::Value = serde_json::from_str(s).ok()?;
    let messages = val.get_mut("messages")?.as_array_mut()?;
    if messages.is_empty() {
        return None;
    }
    messages.pop();
    serde_json::to_vec(&val).ok()
}

/// AC20 / NON-TAUTOLOGY: `TurnDropper` fails `AC8-append-only`.
///
/// DISCRIMINATING (PF-007): if `check_append_only` were a no-op, this test
/// would silently pass a contract that destroys conversation history.
#[test]
fn ac20_turn_dropper_fails_append_only() {
    let contract = TurnDropper;
    let registry = build_extension_registry();
    let report =
        run_conformance_suite_with_extensions(&contract, "ac20-turn-drop", Some(&registry));

    let ac8_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC8-append-only")
        .collect();
    assert!(!ac8_results.is_empty(), "AC8-append-only check must run");
    assert!(
        ac8_results.iter().any(|r| !r.passed),
        "TurnDropper must fail AC8-append-only"
    );
    // AC20 isolation: AC4-never-inflate must NOT also fail (dropping bytes shrinks output).
    let ac4_all_pass = report
        .results
        .iter()
        .filter(|r| r.invariant == "AC4-never-inflate")
        .all(|r| r.passed);
    assert!(
        ac4_all_pass,
        "AC20: TurnDropper must NOT also fail AC4-never-inflate"
    );
}

// ── Variant 3: DescriptionMutator — fails ext:lossless-content ───────────────

/// A contract that mutates the first tool's `description` field.
struct DescriptionMutator;

impl Contract for DescriptionMutator {
    fn component_name(&self) -> &'static str {
        "description-mutator"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        if let Some(out) = mutate_first_tool_description(input) {
            Outcome::modified(out, input.len(), request_id, self.component_name())
        } else {
            Outcome::passthrough(input.to_vec(), request_id, self.component_name())
        }
    }
}

impl MetadataReorderWithMarkers for DescriptionMutator {
    fn apply_reorder(&self, input: &[u8], _request_id: &str) -> Option<Vec<u8>> {
        mutate_first_tool_description(input)
    }
}

fn mutate_first_tool_description(input: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(input).ok()?;
    let mut val: serde_json::Value = serde_json::from_str(s).ok()?;
    let tools = val.get_mut("tools")?.as_array_mut()?;
    let first = tools.first_mut()?;
    *first.get_mut("description")? = serde_json::Value::String("MUTATED".to_owned());
    serde_json::to_vec(&val).ok()
}

/// AC20 / NON-TAUTOLOGY: `DescriptionMutator` fails `ext:lossless-content` via harness.
///
/// DISCRIMINATING (PF-007): if `tools_arrays_set_equal` always returned `true`,
/// a contract that corrupts tool descriptions would pass unchallenged.
#[test]
fn ac20_description_mutator_fails_lossless_content_harness() {
    let contract = DescriptionMutator;
    let registry = build_extension_registry();
    let report = run_conformance_suite_with_extensions(&contract, "ac20-desc-mut", Some(&registry));

    let lossless_results: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.invariant == "ext:lossless-content")
        .collect();
    assert!(
        !lossless_results.is_empty(),
        "ext:lossless-content check must run"
    );
    // The corpus includes ANTHROPIC_TOOL_RESULT which has a tools array. DescriptionMutator
    // mutates the description of the first tool; tools_arrays_set_equal must return false.
    assert!(
        lossless_results.iter().any(|r| !r.passed),
        "DescriptionMutator must fail ext:lossless-content on corpus entries with tools"
    );
}

/// AC20 / NON-TAUTOLOGY: `DescriptionMutator` fails `ext:lossless-content` directly.
///
/// Direct check (no harness overhead) on a body with one tool.
#[test]
fn ac20_description_mutator_fails_lossless_content_direct() {
    let check = cache_align_lossless_content_check();
    let input = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3","tools":[{"name":"search","description":"Search the web","input_schema":{"type":"object"}}]}"#;
    let output = mutate_first_tool_description(input)
        .expect("description mutation must succeed on well-formed input");
    assert!(
        !check(input, &output),
        "AC20: ext:lossless-content check must fail when a tool description is mutated"
    );
}

// ── Variant 4: ElementDropper — fails ext:lossless-content (direct test) ─────

/// Drop the first element from the tools array (requires ≥2 tools).
fn drop_first_tool(input: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(input).ok()?;
    let mut val: serde_json::Value = serde_json::from_str(s).ok()?;
    let tools = val.get_mut("tools")?.as_array_mut()?;
    if tools.len() < 2 {
        return None; // need ≥2 tools to drop one and still have a tools array
    }
    tools.remove(0);
    serde_json::to_vec(&val).ok()
}

/// AC20 / NON-TAUTOLOGY: dropping a tool element fails `ext:lossless-content`.
///
/// This is a direct check (not harness-driven) because the standard corpus
/// does not include bodies with ≥2 tools. The direct test is equally discriminating.
///
/// DISCRIMINATING (PF-007): if `tools_arrays_set_equal` returned `true` for
/// arrays with different element counts, this would silently pass.
#[test]
fn ac20_element_dropper_fails_lossless_content_direct() {
    let check = cache_align_lossless_content_check();
    // Two tools: dropping one changes the array length → set-equality fails.
    let input = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3","tools":[{"name":"alpha","description":"aaa","input_schema":{"type":"object"}},{"name":"bravo","description":"bbb","input_schema":{"type":"object"}}]}"#;
    let output = drop_first_tool(input).expect("drop must succeed with ≥2 tools");
    assert!(
        !check(input, &output),
        "AC20: ext:lossless-content check must fail when a tool element is dropped"
    );
}

// ── Variant 5: ClientMarkerDropper — fails ext:marker-immutability ────────────

/// Strip all `cache_control` fields (simulates a marker-dropping contract).
fn strip_cache_control(input: &[u8]) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(input).ok()?;
    if !s.contains("\"cache_control\"") {
        return None; // no markers to drop → passthrough, test not applicable
    }
    let stripped = s
        .replace(",\"cache_control\":{\"type\":\"ephemeral\"}", "")
        .replace("\"cache_control\":{\"type\":\"ephemeral\"},", "")
        .replace("\"cache_control\":{\"type\":\"ephemeral\"}", "");
    Some(stripped.into_bytes())
}

/// AC20 / NON-TAUTOLOGY: `marker-immutability` check fails when a client marker is dropped.
///
/// DISCRIMINATING (PF-007): if the marker-immutability check always returned `true`,
/// a contract that strips client markers would pass unchallenged.
///
/// Direct test on a known marked input/output pair (no harness needed since the
/// harness corpus entries do not carry `cache_control` markers by default).
#[test]
fn ac20_client_marker_dropper_fails_marker_immutability_direct() {
    let check = marker_immutability_check();
    // Input carries a client marker.
    let input = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"hi"}],"tools":[{"cache_control":{"type":"ephemeral"},"description":"search","input_schema":{"type":"object"},"name":"search"}]}"#;
    // Strip the marker (simulating ClientMarkerDropper).
    let stripped = strip_cache_control(input).expect("strip must succeed on marked input");
    assert!(
        !check(input, &stripped),
        "AC20: marker-immutability check must fail when a client cache_control marker is dropped"
    );
}

/// AC20 — lossless-content check passes for a legitimate element reorder.
///
/// Non-tautology in the other direction: the set-equality gate is not always `false`.
/// Two tools in reversed order must PASS the set-equality check.
#[test]
fn ac20_element_reorder_passes_lossless_content_direct() {
    let check = cache_align_lossless_content_check();
    let input = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3","tools":[{"name":"alpha","description":"aaa","input_schema":{"type":"object"}},{"name":"bravo","description":"bbb","input_schema":{"type":"object"}}]}"#;
    // Reorder: bravo first, alpha second (same set, different order).
    let reordered = br#"{"messages":[{"role":"user","content":"hi"}],"model":"claude-3","tools":[{"name":"bravo","description":"bbb","input_schema":{"type":"object"}},{"name":"alpha","description":"aaa","input_schema":{"type":"object"}}]}"#;
    assert!(
        check(input, reordered),
        "AC20: ext:lossless-content must PASS for a legitimate element reorder"
    );
}
