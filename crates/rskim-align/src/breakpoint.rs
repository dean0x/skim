//! v1 breakpoint placement policy for `cache_control` marker injection.
//!
//! # v1 Policy
//!
//! Eligible injection positions (v1):
//! 1. The **last element** of the top-level `tools` (or `functions`) array,
//!    when it is a JSON object and does not already carry `cache_control`.
//! 2. The **last `"type":"text"` block** in a block-form `system` array,
//!    when that element does not already carry `cache_control`.
//!
//! Marker injection budget:
//! ```text
//! skim_count = min(eligible_positions, max(0, MAX_MARKERS − client_count), V1_SKIM_CAP)
//! ```
//! where `V1_SKIM_CAP = 2` is the v1 hard cap.
//!
//! # AD-CA-4 — Imported constants, never redefined
//!
//! `MARKER_BYTES` (= 37) and `MAX_MARKERS` (= 4) are **imported** from
//! `rskim_contract::waiver` and **never** redefined in this crate. The marker
//! string `,"cache_control":{"type":"ephemeral"}` (37 bytes) is verified against
//! `MARKER_BYTES` in the `marker_length_matches_constant` unit test.
//!
//! # AD-CA-6 — No import of volatile.rs
//!
//! This module does NOT import `crate::volatile`. Marker placement is determined
//! solely by request structure (tool cardinality, system block form, client marker
//! count). The volatile detector's output is used only for logging/stats.

// AD-CA-4: MARKER_BYTES and MAX_MARKERS IMPORTED from rskim_contract::waiver, NEVER redefined here.
use rskim_contract::canonical::tools_arrays_equal;
use rskim_contract::waiver::{MARKER_BYTES, MAX_MARKERS};
use serde_json::value::RawValue;
use std::collections::HashMap;

use crate::canonical_emit::parse_object_pairs;
use crate::span::Span;

// ============================================================================
// Constants
// ============================================================================

/// v1 hard cap on skim-injected markers per request.
///
/// Phase 1 injects at most 2 skim markers (last tool object + last system
/// block). History-zone breakpoints (v2) will raise this cap.
pub const V1_SKIM_CAP: usize = 2;

/// The marker bytes injected at each eligible position.
///
/// # AD-CA-4
///
/// The marker bytes MUST equal the rskim_contract::waiver compact form.
/// MARKER_BYTES (37) is imported from rskim_contract::waiver — this constant
/// is NOT `pub` to ensure callers use the imported MARKER_BYTES, not a local alias.
const MARKER: &[u8] = b",\"cache_control\":{\"type\":\"ephemeral\"}";

/// The marker member without its leading separator comma (36 bytes).
const CC_MEMBER: &[u8] = b"\"cache_control\":{\"type\":\"ephemeral\"}";

/// The `cache_control` key name, as decoded by `parse_object_pairs`.
const CC_KEY: &str = "cache_control";

/// The canonical compact value skim writes for `cache_control`.
const CC_VALUE: &str = "{\"type\":\"ephemeral\"}";

// ============================================================================
// BreakpointPlan
// ============================================================================

/// Computed injection plan for one request.
///
/// Produced by [`plan_injection`]. Describes how many markers to inject
/// and at which of the two v1 positions.
#[derive(Debug, Clone)]
pub struct BreakpointPlan {
    /// Whether to inject a marker into the last tool-definition object.
    pub inject_tools: bool,
    /// Whether to inject a marker into the last system text block.
    pub inject_system: bool,
    /// Total client-supplied `cache_control` markers found across all regions.
    pub client_count: usize,
    /// Number of skim markers that will be injected.
    pub skim_count: usize,
    /// Number of eligible injection positions (0–2 in v1).
    pub eligible_positions: usize,
}

// ============================================================================
// Public: count client markers
// ============================================================================

/// Count all client-supplied `cache_control` markers in the original input.
///
/// Scans the `tools`/`functions`, `system`, and `messages` spans for
/// `"cache_control"` JSON key occurrences. The count is used to determine
/// how many skim markers can be injected within the `MAX_MARKERS = 4` budget.
///
/// # Structural counting
///
/// The scan looks for the exact byte sequence `"cache_control":` within each
/// span. This reliably counts markers in well-formed JSON bodies.
///
/// # AC14 — Client sovereignty
///
/// This count is the sole input to the budget calculation. A client that
/// fills all 4 slots (`client_count >= 4`) receives 0 skim markers.
pub fn count_client_markers(input_str: &str, spans: &HashMap<String, Span>) -> usize {
    let mut count = 0;

    // Count in tools/functions span
    for key in ["tools", "functions"] {
        if let Some(span) = spans.get(key)
            && let Some(s) = span.extract(input_str)
        {
            count += count_cc_in_span(s);
        }
    }

    // Count in system span
    if let Some(span) = spans.get("system")
        && let Some(s) = span.extract(input_str)
    {
        count += count_cc_in_span(s);
    }

    // Count in messages span
    if let Some(span) = spans.get("messages")
        && let Some(s) = span.extract(input_str)
    {
        count += count_cc_in_span(s);
    }

    count
}

/// Count `"cache_control"` key occurrences in a JSON span string.
///
/// Counts occurrences of the byte sequence `"cache_control":` in `s`.
fn count_cc_in_span(s: &str) -> usize {
    let needle = b"\"cache_control\":";
    let haystack = s.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if haystack[i..i + needle.len()] == *needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

// ============================================================================
// Public: plan injection
// ============================================================================

/// Compute the marker injection plan.
///
/// Determines which eligible positions receive a skim marker, subject to
/// the budget and v1 cap.
///
/// # Arguments
///
/// - `canonical_tools` — canonical bytes of the `tools`/`functions` value,
///   or `None` if the request has no tools key.
/// - `canonical_system` — canonical bytes of the `system` value,
///   or `None` if the request has no system key.
/// - `client_count` — total client `cache_control` markers found by
///   [`count_client_markers`].
///
/// # AD-CA-4 — Budget formula
///
/// ```text
/// budget = max(0, MAX_MARKERS − client_count)   // imported MAX_MARKERS = 4
/// skim_count = min(eligible_positions, budget, V1_SKIM_CAP)   // V1_SKIM_CAP = 2
/// ```
///
/// Injection priority: tools first, then system.
pub fn plan_injection(
    canonical_tools: Option<&[u8]>,
    canonical_system: Option<&[u8]>,
    client_count: usize,
) -> BreakpointPlan {
    // Determine eligibility of each position
    let tools_eligible = canonical_tools.is_some_and(tools_position_eligible);
    let system_eligible = canonical_system.is_some_and(system_position_eligible);

    let eligible_positions = tools_eligible as usize + system_eligible as usize;

    // Budget: how many skim markers can we inject given the client's usage?
    let budget = MAX_MARKERS.saturating_sub(client_count);

    // v1 cap: inject at most V1_SKIM_CAP markers, and at most as many eligible positions as we have budget for
    let skim_count = eligible_positions.min(budget).min(V1_SKIM_CAP);

    // Decide which positions get markers (priority: tools first, then system).
    // When tools is eligible, system requires budget ≥ 2 (tools takes slot 1).
    // When tools is not eligible, system gets slot 1 if budget ≥ 1.
    let inject_tools = tools_eligible && skim_count >= 1;
    let inject_system = system_eligible && skim_count >= if tools_eligible { 2 } else { 1 };

    BreakpointPlan {
        inject_tools,
        inject_system,
        client_count,
        skim_count,
        eligible_positions,
    }
}

/// Check whether the last element of a canonical tools array is eligible for injection.
///
/// Delegates to [`last_injectable_object_index`], which parses the array rather than
/// scanning bytes — a byte scan cannot tell a structural `}`/`,` from one inside a
/// tool `description` string.
fn tools_position_eligible(canonical_tools: &[u8]) -> bool {
    last_injectable_object_index(canonical_tools).is_some()
}

/// Index of the last element of a canonical JSON array when that element is a
/// **non-empty JSON object without a top-level `cache_control` key**.
///
/// Returns `None` (position ineligible) when the value is not an array, the array is
/// empty, the last element is not an object, the last element is `{}`, or the last
/// element already carries a `cache_control` member.
///
/// # Why `{}` is ineligible
///
/// [`build_element_with_cc`] grows an element by exactly `MARKER_BYTES` (a 36-byte
/// member plus 1 separator comma). An empty object has no neighbouring member, so no
/// separator is needed and the growth would be `MARKER_BYTES - 1` — breaking the
/// exact-growth invariant that [`verify_injection`] enforces. Rejecting the position
/// here keeps the invariant total and leaves the rest of the request alignable.
///
/// # Why this parses instead of scanning
///
/// The previous byte scan walked backwards tracking `{}`/`[]` depth, which mis-locates
/// the last element whenever a string value contains a brace, bracket or comma
/// (e.g. `{"description":"emit }, then stop"}`).
fn last_injectable_object_index(canonical_array: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(canonical_array).ok()?;
    let elements: Vec<Box<RawValue>> = serde_json::from_str(s.trim()).ok()?;
    let last_idx = elements.len().checked_sub(1)?;
    let pairs = parse_object_pairs(elements[last_idx].get().trim())?;
    if pairs.is_empty() {
        return None;
    }
    if pairs.iter().any(|(k, _)| k == CC_KEY) {
        return None;
    }
    Some(last_idx)
}

/// Rebuild a canonical JSON array with `elements[target_idx]` replaced by `replacement`.
///
/// Every other element is copied verbatim from its trimmed source bytes, so the only
/// byte difference between input and output is the replaced element.
fn rebuild_array_with(
    elements: &[Box<RawValue>],
    target_idx: usize,
    replacement: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(replacement.len() + 2 + elements.len() * 8);
    out.push(b'[');
    for (i, elem) in elements.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        if i == target_idx {
            out.extend_from_slice(replacement);
        } else {
            out.extend_from_slice(elem.get().trim().as_bytes());
        }
    }
    out.push(b']');
    out
}

/// Check whether the system value contains an eligible last text block.
///
/// Eligible means:
/// - The system value is a JSON array (block form).
/// - At least one element has `"type":"text"` without `"cache_control"`.
fn system_position_eligible(canonical_system: &[u8]) -> bool {
    let s = match std::str::from_utf8(canonical_system) {
        Ok(s) => s.trim(),
        Err(_) => return false,
    };
    if !s.starts_with('[') {
        return false; // string-form system, not block-form
    }
    // Check for at least one eligible text block
    find_last_eligible_text_block_idx(s).is_some()
}

// ============================================================================
// Public: inject markers
// ============================================================================

/// Inject a `cache_control` marker into the last element of a canonical tools array.
///
/// # AD-CA-4
///
/// The net growth is exactly `MARKER_BYTES` (37 bytes).
///
/// # Idempotence (AC8)
///
/// The marker is inserted at the **canonical sorted position** of `"cache_control"`
/// within the last element's key-value pairs (`"c"` sorts before `"d"`, `"i"`, `"n"`,
/// `"t"` — the typical Anthropic tool keys). This ensures the result is already in
/// canonical key order, so a second `align()` pass sees `cache_control` already
/// present (via `last_element_has_cc_byte_scan`) and does not re-inject.
///
/// # Returns
///
/// `Some((injected_bytes, injected_element_index))` on success. The index identifies
/// which array element received the marker and is passed straight to
/// [`verify_injection`] for the AD-CA-7 injection-path self-verify.
/// `None` when the position is not eligible (not an object, `{}`, already has
/// `cache_control`, empty array, or parse failure).
pub fn inject_tools_marker(canonical_tools: &[u8]) -> Option<(Vec<u8>, usize)> {
    debug_assert_eq!(
        MARKER.len(),
        MARKER_BYTES,
        "AD-CA-4: MARKER must equal MARKER_BYTES"
    );

    let s = std::str::from_utf8(canonical_tools).ok()?;
    let target_idx = last_injectable_object_index(canonical_tools)?;
    let elements: Vec<Box<RawValue>> = serde_json::from_str(s.trim()).ok()?;

    // Rebuild the target element with cache_control at its canonical sorted position.
    let new_elem = build_element_with_cc(elements.get(target_idx)?.get().trim())?;

    Some((
        rebuild_array_with(&elements, target_idx, &new_elem),
        target_idx,
    ))
}

/// Inject a `cache_control` marker into the last eligible text block of a
/// canonical block-form system array.
///
/// # AD-CA-4
///
/// The injected bytes are exactly `MARKER` (37 bytes = `MARKER_BYTES`).
///
/// # Returns
///
/// `Some((injected_bytes, injected_element_index))` on success — the index of the
/// system block that received the marker, passed straight to [`verify_injection`].
/// `None` when the system is not block-form, has no eligible text block, or any
/// parse fails (fail-open).
pub fn inject_system_marker(canonical_system: &[u8]) -> Option<(Vec<u8>, usize)> {
    debug_assert_eq!(
        MARKER.len(),
        MARKER_BYTES,
        "AD-CA-4: MARKER must equal MARKER_BYTES"
    );

    let sys_str = std::str::from_utf8(canonical_system).ok()?;
    let trimmed = sys_str.trim();
    if !trimmed.starts_with('[') {
        return None; // string-form system
    }

    let elements: Vec<Box<RawValue>> = serde_json::from_str(trimmed).ok()?;
    let target_idx = find_last_eligible_text_block_idx(trimmed)?;

    // Rebuild the target text block with cache_control at its canonical sorted
    // position (idempotence: the rebuilt element is already key-sorted).
    let new_elem = build_element_with_cc(elements.get(target_idx)?.get().trim())?;

    Some((
        rebuild_array_with(&elements, target_idx, &new_elem),
        target_idx,
    ))
}

// ============================================================================
// Internal: element rebuild with cc at sorted position
// ============================================================================

/// Rebuild a canonical JSON object with `"cache_control":{"type":"ephemeral"}` inserted
/// at its canonical (alphabetically sorted) key position.
///
/// # Idempotence guarantee (AC8)
///
/// The returned bytes are in canonical key order: `cache_control` is positioned between
/// the last key that sorts before `"c"` and the first key that sorts after `"c"`. For
/// typical Anthropic tool elements (whose keys are `description`, `input_schema`, `name`,
/// `type` — all `> "c"`) this means `cache_control` is emitted FIRST. The result is
/// byte-identical to what `canonical_emit::canonicalize_object` would produce for the
/// same object augmented with `cache_control`, so a second `align()` pass produces
/// byte-identical output (idempotent).
///
/// # Net growth (enforced, not assumed)
///
/// The returned bytes are exactly `elem_str.len() + MARKER_BYTES` bytes, because:
/// - We add 36 bytes for `"cache_control":{"type":"ephemeral"}` (the key-value payload)
/// - We add 1 byte for the comma separator between cc and its neighbouring key
///
/// = 37 = `MARKER_BYTES` total.
///
/// This holds only for an already-canonical `elem_str` (compact, key-sorted), which is
/// all any caller passes. Rather than assume it, the function re-checks the resulting
/// length and returns `None` if it does not hold — an unmet precondition fails the
/// request open instead of emitting bytes that would trip the seam's byte gate.
fn build_element_with_cc(elem_str: &str) -> Option<Vec<u8>> {
    let pairs = parse_object_pairs(elem_str)?;

    // `{}` has no neighbouring member, so no separator comma is needed and the growth
    // would be MARKER_BYTES - 1 — breaking the exact-growth invariant verify_injection
    // enforces. `last_injectable_object_index` already rejects this position; the guard
    // keeps the invariant total even if a future caller skips that check.
    if pairs.is_empty() {
        return None;
    }

    // Find the sorted insertion index for "cache_control" ('c' sorts before 'd','i','n','t'…)
    let insert_idx = pairs.partition_point(|(k, _)| k.as_str() < CC_KEY);

    // Build each member independently, then join with ','. Keys are re-encoded with
    // `serde_json::to_string` — the same encoding `canonical_emit::canonicalize_object`
    // uses — so the untouched members reproduce their canonical bytes exactly. Writing
    // the decoded key raw would emit invalid JSON for any key containing `"` or `\`.
    let mut members: Vec<Vec<u8>> = Vec::with_capacity(pairs.len() + 1);
    for (k, v) in &pairs {
        let key_json = serde_json::to_string(k.as_str()).ok()?;
        let val = v.get().trim();
        let mut member = Vec::with_capacity(key_json.len() + 1 + val.len());
        member.extend_from_slice(key_json.as_bytes());
        member.push(b':');
        member.extend_from_slice(val.as_bytes());
        members.push(member);
    }
    members.insert(insert_idx, CC_MEMBER.to_vec());

    let mut result = Vec::with_capacity(elem_str.len() + MARKER_BYTES);
    result.push(b'{');
    for (i, member) in members.iter().enumerate() {
        if i > 0 {
            result.push(b',');
        }
        result.extend_from_slice(member);
    }
    result.push(b'}');

    // Precondition check, enforced in production (never a panic — this is business
    // logic, so an unmet invariant returns None and the caller fails the whole request
    // open). Callers only ever pass an already-canonical element, for which the growth
    // is exactly CC_MEMBER (36) + one separator comma = MARKER_BYTES (37).
    if result.len() != elem_str.len() + MARKER_BYTES {
        return None;
    }

    Some(result)
}

// ============================================================================
// Injection self-verify helpers (public, for lib.rs)
// ============================================================================

/// AD-CA-7 injection-path self-verify: prove the injection added **only** the marker.
///
/// The check strips the skim-injected `cache_control` member back out of element
/// `injected_idx` and asserts the result is value-equal to the pre-injection canonical
/// array under the **order-sensitive** comparator
/// [`rskim_contract::canonical::tools_arrays_equal`] — injection must not reorder,
/// drop, duplicate, or mutate anything.
///
/// Three independent conditions must all hold:
/// 1. `injected.len() == canonical.len() + MARKER_BYTES` (exactly one marker's growth).
/// 2. Element `injected_idx` carries exactly one `cache_control` member whose value is
///    the canonical `{"type":"ephemeral"}` — so the stripped form is well-defined.
/// 3. The stripped array is order-sensitively value-equal to `canonical`.
///
/// Returns `false` on any parse failure. The caller maps `false` to a whole-request
/// fail-open passthrough.
///
/// # Discriminating (PF-007)
///
/// This is not a size-only check: mutating a neighbouring element, reordering elements,
/// or dropping a member during the rebuild all leave the length unchanged yet fail
/// condition 3.
pub fn verify_injection(canonical: &[u8], injected: &[u8], injected_idx: usize) -> bool {
    // Condition 1: exactly one marker's worth of growth.
    if injected.len() != canonical.len() + MARKER_BYTES {
        return false;
    }
    let (Ok(canonical_str), Ok(injected_str)) = (
        std::str::from_utf8(canonical),
        std::str::from_utf8(injected),
    ) else {
        return false;
    };
    // Condition 2: strip the one skim-injected marker back out.
    let Some(stripped) = strip_injected_marker(injected_str, injected_idx) else {
        return false;
    };
    let Ok(stripped_str) = String::from_utf8(stripped) else {
        return false;
    };
    // Condition 3: order-sensitive value equality with the pre-injection bytes.
    tools_arrays_equal(canonical_str, &stripped_str)
}

/// Remove the skim-injected `cache_control` member from element `target_idx` of a
/// canonical JSON array, returning the array bytes without it.
///
/// Returns `None` unless the target element is an object carrying **exactly one**
/// `cache_control` member whose value is the canonical `{"type":"ephemeral"}` — the
/// shape [`build_element_with_cc`] writes. Every other element is copied verbatim.
fn strip_injected_marker(array_str: &str, target_idx: usize) -> Option<Vec<u8>> {
    let elements: Vec<Box<RawValue>> = serde_json::from_str(array_str.trim()).ok()?;
    let pairs = parse_object_pairs(elements.get(target_idx)?.get().trim())?;

    let mut members: Vec<Vec<u8>> = Vec::with_capacity(pairs.len());
    let mut removed = 0usize;
    for (k, v) in &pairs {
        let val = v.get().trim();
        if k == CC_KEY && val == CC_VALUE {
            removed += 1;
            continue;
        }
        let key_json = serde_json::to_string(k.as_str()).ok()?;
        let mut member = Vec::with_capacity(key_json.len() + 1 + val.len());
        member.extend_from_slice(key_json.as_bytes());
        member.push(b':');
        member.extend_from_slice(val.as_bytes());
        members.push(member);
    }
    // Exactly one skim marker must be removable; zero means nothing was injected and
    // two would mean the element was mangled.
    if removed != 1 {
        return None;
    }

    let mut elem = Vec::new();
    elem.push(b'{');
    for (i, member) in members.iter().enumerate() {
        if i > 0 {
            elem.push(b',');
        }
        elem.extend_from_slice(member);
    }
    elem.push(b'}');

    Some(rebuild_array_with(&elements, target_idx, &elem))
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Find the index of the last eligible text block in a canonical block-form system string.
///
/// An element is eligible when it is a JSON object that has a top-level `"type"` member
/// equal to `"text"` and **no** top-level `cache_control` member.
///
/// The membership tests are structural (`parse_object_pairs`), not substring scans: a
/// substring scan would match `"type":"text"` nested inside another value and would miss
/// nothing but could also mis-classify a block whose `cache_control` sits at a nested
/// level. Structural checks keep eligibility a pure function of the top-level shape.
fn find_last_eligible_text_block_idx(trimmed_sys: &str) -> Option<usize> {
    let elements: Vec<Box<RawValue>> = serde_json::from_str(trimmed_sys).ok()?;
    elements
        .iter()
        .enumerate()
        .rev()
        .find(|(_, e)| {
            let Some(pairs) = parse_object_pairs(e.get().trim()) else {
                return false; // not an object
            };
            let is_text = pairs
                .iter()
                .any(|(k, v)| k == "type" && v.get().trim() == "\"text\"");
            let has_cc = pairs.iter().any(|(k, _)| k == CC_KEY);
            is_text && !has_cc
        })
        .map(|(i, _)| i)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // ── count_cc_in_span ──────────────────────────────────────────────────────

    #[test]
    fn count_cc_empty_span() {
        assert_eq!(count_cc_in_span("[]"), 0);
    }

    #[test]
    fn count_cc_one_marker() {
        let s = r#"[{"name":"t","cache_control":{"type":"ephemeral"}}]"#;
        assert_eq!(count_cc_in_span(s), 1);
    }

    #[test]
    fn count_cc_multiple_markers() {
        let s =
            r#"[{"cache_control":{"type":"ephemeral"}},{"cache_control":{"type":"ephemeral"}}]"#;
        assert_eq!(count_cc_in_span(s), 2);
    }

    // ── count_client_markers ─────────────────────────────────────────────────

    #[test]
    fn count_client_markers_none() {
        let input = r#"{"tools":[{"name":"t"}],"messages":[]}"#;
        let spans = crate::span::locate_top_level_spans(input).unwrap();
        assert_eq!(count_client_markers(input, &spans), 0);
    }

    #[test]
    fn count_client_markers_in_tools() {
        let input =
            r#"{"tools":[{"name":"t","cache_control":{"type":"ephemeral"}}],"messages":[]}"#;
        let spans = crate::span::locate_top_level_spans(input).unwrap();
        assert_eq!(count_client_markers(input, &spans), 1);
    }

    #[test]
    fn count_client_markers_in_system_and_messages() {
        let input = r#"{"system":[{"type":"text","text":"hi","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":[{"type":"text","text":"m","cache_control":{"type":"ephemeral"}}]}]}"#;
        let spans = crate::span::locate_top_level_spans(input).unwrap();
        // 1 in system + 1 in messages content = 2
        assert_eq!(count_client_markers(input, &spans), 2);
    }

    #[test]
    fn count_client_5_markers_ac14() {
        // AC14: 5 client markers → count = 5, skim_count = 0
        // Build a body with 5 cache_control across tools (2), system (1), messages (2)
        let input = r#"{"tools":[{"name":"a","cache_control":{"type":"ephemeral"}},{"name":"b","cache_control":{"type":"ephemeral"}}],"system":[{"type":"text","text":"s","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":[{"type":"text","cache_control":{"type":"ephemeral"}},{"type":"text","cache_control":{"type":"ephemeral"}}]}]}"#;
        let spans = crate::span::locate_top_level_spans(input).unwrap();
        let count = count_client_markers(input, &spans);
        assert_eq!(count, 5, "expected 5 client markers");
        let plan = plan_injection(
            Some(b"[{\"name\":\"a\"},{\"name\":\"b\"}]"),
            Some(b"[{\"text\":\"s\",\"type\":\"text\"}]"),
            count,
        );
        assert_eq!(plan.skim_count, 0, "5 client markers → 0 skim markers");
    }

    // ── tools_position_eligible ───────────────────────────────────────────────

    #[test]
    fn tools_eligible_non_empty_object() {
        assert!(tools_position_eligible(b"[{\"name\":\"a\"}]"));
    }

    #[test]
    fn tools_not_eligible_empty_array() {
        assert!(!tools_position_eligible(b"[]"));
    }

    #[test]
    fn tools_not_eligible_already_has_cc() {
        assert!(!tools_position_eligible(
            b"[{\"name\":\"a\",\"cache_control\":{\"type\":\"ephemeral\"}}]"
        ));
    }

    // ── plan_injection ────────────────────────────────────────────────────────

    #[test]
    fn plan_inject_2_markers_both_eligible_ac4() {
        // AC4: 3 tools + block system, no client markers → skim_count = 2
        let tools = b"[{\"name\":\"a\"},{\"name\":\"b\"},{\"name\":\"c\"}]";
        let system = b"[{\"text\":\"hi\",\"type\":\"text\"}]";
        let plan = plan_injection(Some(tools), Some(system), 0);
        assert_eq!(plan.eligible_positions, 2);
        assert_eq!(plan.skim_count, 2);
        assert!(plan.inject_tools);
        assert!(plan.inject_system);
        assert_eq!(plan.client_count, 0);
    }

    #[test]
    fn plan_inject_1_marker_tools_only() {
        // tools eligible, no block system
        let tools = b"[{\"name\":\"a\"}]";
        let plan = plan_injection(Some(tools), None, 0);
        assert_eq!(plan.eligible_positions, 1);
        assert_eq!(plan.skim_count, 1);
        assert!(plan.inject_tools);
        assert!(!plan.inject_system);
    }

    #[test]
    fn plan_budget_limited_by_client_count() {
        // Client has 3 markers → budget = 1, even though eligible = 2
        let tools = b"[{\"name\":\"a\"}]";
        let system = b"[{\"text\":\"hi\",\"type\":\"text\"}]";
        let plan = plan_injection(Some(tools), Some(system), 3);
        assert_eq!(plan.eligible_positions, 2);
        assert_eq!(plan.skim_count, 1);
        // Only tools gets the marker (priority: tools first)
        assert!(plan.inject_tools);
        assert!(!plan.inject_system);
    }

    #[test]
    fn plan_zero_skim_markers_when_budget_exhausted() {
        let tools = b"[{\"name\":\"a\"}]";
        let plan = plan_injection(Some(tools), None, 4);
        assert_eq!(plan.skim_count, 0);
        assert!(!plan.inject_tools);
    }

    #[test]
    fn plan_string_system_not_eligible() {
        // String-form system → not block-form → not eligible
        let tools = b"[{\"name\":\"a\"}]";
        let system = b"\"You are helpful.\"";
        let plan = plan_injection(Some(tools), Some(system), 0);
        assert_eq!(plan.eligible_positions, 1); // only tools eligible
        assert!(plan.inject_tools);
        assert!(!plan.inject_system);
    }

    // ── inject_tools_marker ───────────────────────────────────────────────────

    #[test]
    fn inject_tools_single_element() {
        let tools = b"[{\"name\":\"a\"}]";
        let (injected, offset) = inject_tools_marker(tools).unwrap();
        let injected_str = std::str::from_utf8(&injected).unwrap();
        assert!(injected_str.contains("\"cache_control\":{\"type\":\"ephemeral\"}"));
        assert_eq!(injected.len(), tools.len() + MARKER_BYTES);
        // Self-verify: stripping MARKER at offset yields original
        assert!(verify_injection(tools, &injected, offset));
    }

    #[test]
    fn inject_tools_multiple_elements_last_gets_marker() {
        let tools = b"[{\"name\":\"a\"},{\"name\":\"b\"},{\"name\":\"c\"}]";
        let (injected, offset) = inject_tools_marker(tools).unwrap();
        let injected_str = std::str::from_utf8(&injected).unwrap();
        // The last element (name="c") should have the marker.
        // cc ('c') sorts before 'n' (name), so the rebuilt element starts with cc.
        // Verify: cc appears AFTER the 'b' element (i.e., it's in the 'c' element, not earlier).
        let last_b_name_pos = injected_str.rfind("\"name\":\"b\"").unwrap();
        let cc_pos = injected_str.rfind("\"cache_control\"").unwrap();
        assert!(
            cc_pos > last_b_name_pos,
            "marker must be in the last element (c), not in earlier elements"
        );
        // The 'a' and 'b' elements must NOT have cache_control
        let a_elem_end = injected_str.find("\"name\":\"a\"").unwrap() + 10;
        assert!(
            !injected_str[..a_elem_end].contains("cache_control"),
            "a element must not have cc"
        );
        assert!(verify_injection(tools, &injected, offset));
    }

    #[test]
    fn inject_tools_empty_array_returns_none() {
        assert!(inject_tools_marker(b"[]").is_none());
    }

    #[test]
    fn inject_tools_already_has_cc_returns_none() {
        let tools = b"[{\"cache_control\":{\"type\":\"ephemeral\"},\"name\":\"a\"}]";
        assert!(inject_tools_marker(tools).is_none());
    }

    #[test]
    fn inject_tools_nested_schema_correct_position() {
        // Element keys: input_schema (i), name (n) — both > 'c', so cc sorts FIRST.
        let tools =
            b"[{\"input_schema\":{\"properties\":{\"x\":{\"type\":\"string\"}}},\"name\":\"a\"}]";
        let (injected, offset) = inject_tools_marker(tools).unwrap();
        assert_eq!(injected.len(), tools.len() + MARKER_BYTES);
        assert!(verify_injection(tools, &injected, offset));
        let injected_str = std::str::from_utf8(&injected).unwrap();
        // cc sorts before input_schema and name, so the rebuilt element starts with cc
        assert!(
            injected_str.contains("\"cache_control\":{\"type\":\"ephemeral\"}"),
            "must contain cc key-value"
        );
        assert!(
            injected_str.contains("\"name\":\"a\""),
            "must still contain name key"
        );
        assert!(
            injected_str.contains("\"input_schema\""),
            "must still contain input_schema key"
        );
        // cc must be the first key in the last element (element starts with {" then cc)
        assert!(
            injected_str.ends_with(",{\"cache_control\":{\"type\":\"ephemeral\"},\"input_schema\":{\"properties\":{\"x\":{\"type\":\"string\"}}},\"name\":\"a\"}]")
            || injected_str.starts_with("[{\"cache_control\":{\"type\":\"ephemeral\"},"),
            "cc must be first key in element"
        );
    }

    // ── inject_system_marker ──────────────────────────────────────────────────

    #[test]
    fn inject_system_single_text_block() {
        let sys = b"[{\"text\":\"hello\",\"type\":\"text\"}]";
        let (injected, offset) = inject_system_marker(sys).unwrap();
        let injected_str = std::str::from_utf8(&injected).unwrap();
        assert!(injected_str.contains("\"cache_control\":{\"type\":\"ephemeral\"}"));
        assert_eq!(injected.len(), sys.len() + MARKER_BYTES);
        assert!(verify_injection(sys, &injected, offset));
    }

    #[test]
    fn inject_system_last_text_block_not_last_element() {
        // Text block at index 0, image block at index 1 → inject into index 0
        let sys = b"[{\"text\":\"a\",\"type\":\"text\"},{\"source\":{},\"type\":\"image\"}]";
        let (injected, _offset) = inject_system_marker(sys).unwrap();
        let injected_str = std::str::from_utf8(&injected).unwrap();
        assert!(injected_str.contains("\"cache_control\":{\"type\":\"ephemeral\"}"));
        // Image block must NOT have cache_control
        let img_start = injected_str.rfind("\"type\":\"image\"").unwrap();
        let cc_pos = injected_str.find("\"cache_control\"").unwrap();
        assert!(cc_pos < img_start, "cc must be before the image block");
    }

    #[test]
    fn inject_system_string_form_returns_none() {
        assert!(inject_system_marker(b"\"You are helpful.\"").is_none());
    }

    #[test]
    fn inject_system_no_text_blocks_returns_none() {
        let sys = b"[{\"source\":{},\"type\":\"image\"}]";
        assert!(inject_system_marker(sys).is_none());
    }

    #[test]
    fn inject_system_already_has_cc_returns_none() {
        let sys =
            b"[{\"cache_control\":{\"type\":\"ephemeral\"},\"text\":\"a\",\"type\":\"text\"}]";
        assert!(inject_system_marker(sys).is_none());
    }

    // ── verify_injection ──────────────────────────────────────────────────────

    #[test]
    fn verify_injection_correct() {
        let canonical = b"[{\"name\":\"a\"}]";
        let (injected, offset) = inject_tools_marker(canonical).unwrap();
        assert!(verify_injection(canonical, &injected, offset));
    }

    #[test]
    fn verify_injection_wrong_length_fails() {
        let canonical = b"[{\"name\":\"a\"}]";
        let (injected, offset) = inject_tools_marker(canonical).unwrap();
        // Modify injected to be wrong length
        let mut bad = injected.clone();
        bad.push(b'X');
        assert!(!verify_injection(canonical, &bad, offset));
    }

    // ── marker constant ───────────────────────────────────────────────────────

    #[test]
    fn marker_length_matches_constant() {
        // AD-CA-4: MARKER_BYTES (37) imported from rskim_contract::waiver must match MARKER
        assert_eq!(
            MARKER.len(),
            MARKER_BYTES,
            "AD-CA-4: MARKER.len() must equal the imported MARKER_BYTES constant"
        );
        assert_eq!(MARKER_BYTES, 37);
    }

    #[test]
    fn max_markers_constant_matches_import() {
        // AD-CA-4: MAX_MARKERS (4) imported from rskim_contract::waiver
        assert_eq!(MAX_MARKERS, 4);
    }

    // ── verify_injection is discriminating, not size-only (PF-007) ───────────

    #[test]
    fn verify_injection_rejects_mutated_neighbour_element() {
        // A same-length mutation of an element OTHER than the injected one must fail.
        // A size-only check would pass this.
        let canonical = b"[{\"name\":\"aa\"},{\"name\":\"bb\"}]";
        let (injected, idx) = inject_tools_marker(canonical).expect("must inject");
        assert!(verify_injection(canonical, &injected, idx));

        let tampered = String::from_utf8(injected)
            .unwrap()
            .replace("\"aa\"", "\"zz\"");
        assert!(
            !verify_injection(canonical, tampered.as_bytes(), idx),
            "AD-CA-7: a mutated neighbouring element must fail the injection self-verify"
        );
    }

    #[test]
    fn verify_injection_rejects_element_reorder() {
        // Injection must not reorder elements. Same length, same multiset — only the
        // order-sensitive comparator catches this.
        let canonical = b"[{\"name\":\"aa\"},{\"name\":\"bb\"}]";
        let reordered =
            br#"[{"cache_control":{"type":"ephemeral"},"name":"bb"},{"name":"aa"}]"#.to_vec();
        assert_eq!(
            reordered.len(),
            canonical.len() + MARKER_BYTES,
            "test fixture must have the exact injected length"
        );
        // idx 0: the marker strips cleanly, so only the ORDER-SENSITIVE comparator
        // can reject this — a set-equality comparator would accept it.
        assert!(
            !verify_injection(canonical, &reordered, 0),
            "AD-CA-7: injection must not reorder elements"
        );
        // idx 1: the element at the claimed index carries no marker at all.
        assert!(
            !verify_injection(canonical, &reordered, 1),
            "AD-CA-7: a missing marker at the claimed index must fail"
        );
    }

    #[test]
    fn verify_injection_rejects_missing_marker() {
        // Right length, but the growth came from padding rather than a marker.
        let canonical = b"[{\"name\":\"a\"}]";
        let padded = format!("[{{\"name\":\"a{}\"}}]", "p".repeat(MARKER_BYTES));
        assert_eq!(padded.len(), canonical.len() + MARKER_BYTES);
        assert!(
            !verify_injection(canonical, padded.as_bytes(), 0),
            "AD-CA-7: growth without a strippable marker must fail"
        );
    }

    // ── empty-object tool element (degenerate shape, AC24) ──────────────────

    #[test]
    fn empty_object_tool_element_is_ineligible_and_never_panics() {
        // `[{}]` used to trip a debug_assert inside build_element_with_cc (growth is
        // MARKER_BYTES - 1 with no neighbouring member). It must be a clean no-op.
        assert!(
            !tools_position_eligible(b"[{}]"),
            "an empty tool object must not be an eligible injection position"
        );
        assert!(
            inject_tools_marker(b"[{}]").is_none(),
            "injecting into an empty tool object must return None, not panic"
        );
        assert!(build_element_with_cc("{}").is_none());
    }

    #[test]
    fn last_element_eligibility_survives_braces_inside_strings() {
        // The old backwards byte scan mis-located the last element whenever a string
        // value contained a brace/bracket/comma. The last element here has NO marker,
        // so the position is eligible; a mis-scan would see the earlier marked element.
        let tools =
            br#"[{"cache_control":{"type":"ephemeral"},"name":"a"},{"description":"emit }, then ]","name":"b"}]"#;
        assert!(
            tools_position_eligible(tools),
            "a brace inside a description must not confuse eligibility"
        );
        let (injected, idx) = inject_tools_marker(tools).expect("must inject into last element");
        assert_eq!(idx, 1, "the marker must land on the last element");
        assert!(verify_injection(tools, &injected, idx));
    }
}
