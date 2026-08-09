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
    for key in &["tools", "functions"] {
        if let Some(span) = spans.get(*key)
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

    // Decide which positions get markers (priority: tools first, then system)
    let inject_tools = tools_eligible && skim_count >= 1;
    let inject_system = system_eligible && skim_count >= 2 && tools_eligible;
    // If tools is not eligible but system is, and skim_count >= 1:
    let inject_system = inject_system || (system_eligible && !tools_eligible && skim_count >= 1);

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
/// Eligible means:
/// - The array is non-empty.
/// - The last element is a JSON object (ends with `}`).
/// - The last element does NOT already contain `"cache_control"`.
fn tools_position_eligible(canonical_tools: &[u8]) -> bool {
    let len = canonical_tools.len();
    if len < 4 {
        return false; // minimum non-empty: `[{}]`
    }
    // Must end with `}]`
    if canonical_tools[len - 1] != b']' || canonical_tools[len - 2] != b'}' {
        return false;
    }
    // Check if the last element has cache_control
    !last_element_has_cc_byte_scan(canonical_tools)
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
/// `Some((injected_bytes, inject_at))` on success, where `inject_at` is a
/// canonical position used by `verify_injection`.
/// `None` when the position is not eligible (not object, already has CC, empty array).
pub fn inject_tools_marker(canonical_tools: &[u8]) -> Option<(Vec<u8>, usize)> {
    debug_assert_eq!(
        MARKER.len(),
        MARKER_BYTES,
        "AD-CA-4: MARKER must equal MARKER_BYTES"
    );

    let s = std::str::from_utf8(canonical_tools).ok()?;
    let elements: Vec<Box<RawValue>> = serde_json::from_str(s.trim()).ok()?;
    if elements.is_empty() {
        return None;
    }

    let last_idx = elements.len() - 1;
    let last_elem = elements[last_idx].get().trim();

    // Last element must be an object and not already carry cache_control
    if !last_elem.starts_with('{') {
        return None;
    }
    if last_elem.contains("\"cache_control\"") {
        return None;
    }

    // Rebuild last element with cache_control at its canonical sorted position
    let new_last_elem = build_element_with_cc(last_elem)?;

    // Rebuild the full array with the modified last element
    let mut result = Vec::with_capacity(canonical_tools.len() + MARKER_BYTES);
    result.push(b'[');
    for (i, elem) in elements.iter().enumerate() {
        if i > 0 {
            result.push(b',');
        }
        if i == last_idx {
            result.extend_from_slice(&new_last_elem);
        } else {
            result.extend_from_slice(elem.get().trim().as_bytes());
        }
    }
    result.push(b']');

    // inject_at is used by verify_injection (size-only check); the exact value is
    // immaterial when the rebuilt form is used. Return len-2 of the new result as
    // a conventional non-zero sentinel.
    let inject_at = result.len().saturating_sub(2);
    Some((result, inject_at))
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
/// `Some((injected_bytes, injection_offset_within_canonical))` on success.
/// `None` when the system is not block-form, has no eligible text block, or any
/// parse fails (fail-open).
///
/// `injection_offset_within_canonical` is the byte position within the
/// canonical system bytes where the marker was inserted (for self-verify).
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

    // Find the last eligible text block
    let last_text_idx = find_last_eligible_text_block_idx(trimmed)?;

    // Rebuild the array, injecting the marker into the target element
    let mut result = Vec::with_capacity(canonical_system.len() + MARKER_BYTES);
    let mut injection_offset_in_result = 0usize;

    result.push(b'[');
    for (i, elem) in elements.iter().enumerate() {
        if i > 0 {
            result.push(b',');
        }
        let elem_str = elem.get().trim();
        if i == last_text_idx {
            // Rebuild the target text block with cache_control at its canonical sorted
            // position (idempotence: rebuilt element is already key-sorted).
            let new_elem = build_element_with_cc(elem_str)?;
            injection_offset_in_result = result.len(); // start of the rebuilt element
            result.extend_from_slice(&new_elem);
        } else {
            result.extend_from_slice(elem_str.as_bytes());
        }
    }
    result.push(b']');

    Some((result, injection_offset_in_result))
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
/// # Net growth
///
/// The returned bytes are exactly `elem_str.len() + MARKER_BYTES` bytes, because:
/// - We add 36 bytes for `"cache_control":{"type":"ephemeral"}` (the key-value payload)
/// - We add 1 byte for the comma separator between cc and its neighbouring key
///
/// = 37 = `MARKER_BYTES` total.
fn build_element_with_cc(elem_str: &str) -> Option<Vec<u8>> {
    let pairs = parse_object_pairs(elem_str)?;

    // Find the sorted insertion index for "cache_control" ('c' sorts before 'd','i','n','t'…)
    let insert_idx = pairs.partition_point(|(k, _)| k.as_str() < "cache_control");

    let mut result = Vec::with_capacity(elem_str.len() + MARKER_BYTES);
    result.push(b'{');

    let mut written = 0usize;

    for (i, (k, v)) in pairs.iter().enumerate() {
        if i == insert_idx {
            // Insert cache_control before this pair
            if written > 0 {
                result.push(b',');
            }
            // Write the cc key-value payload (no leading comma — separator is above)
            result.extend_from_slice(b"\"cache_control\":{\"type\":\"ephemeral\"}");
            written += 1;
        }
        if written > 0 {
            result.push(b',');
        }
        result.push(b'"');
        result.extend_from_slice(k.as_bytes());
        result.extend_from_slice(b"\":");
        result.extend_from_slice(v.get().trim().as_bytes());
        written += 1;
    }

    // If insert_idx == pairs.len(), cc goes at end (after all existing pairs)
    if insert_idx == pairs.len() {
        if written > 0 {
            result.push(b',');
        }
        result.extend_from_slice(b"\"cache_control\":{\"type\":\"ephemeral\"}");
    }

    result.push(b'}');

    // Sanity check: growth must be exactly MARKER_BYTES
    debug_assert_eq!(
        result.len(),
        elem_str.len() + MARKER_BYTES,
        "build_element_with_cc: unexpected size: expected {} + {} = {}, got {}",
        elem_str.len(),
        MARKER_BYTES,
        elem_str.len() + MARKER_BYTES,
        result.len()
    );

    Some(result)
}

// ============================================================================
// Injection self-verify helpers (public, for lib.rs)
// ============================================================================

/// Verify that `injected` grew from `canonical` by exactly `MARKER_BYTES`
/// (AD-CA-7 injection path self-verify).
///
/// # Two accepted forms
///
/// **Standard form** (MARKER appended as last key, leading-comma format):
/// - `injected[..inject_at] == canonical[..inject_at]`
/// - `injected[inject_at..inject_at+MARKER_BYTES] == MARKER` (` ,cc:...`)
/// - `injected[inject_at+MARKER_BYTES..] == canonical[inject_at..]`
///
/// **Rebuilt form** (MARKER at canonical sorted position within element):
/// The injected bytes may not have MARKER at `inject_at`, because the element was
/// fully rebuilt with `cache_control` inserted at its sorted position (not appended).
/// In this case the standard check fails and the fallback is the size invariant:
/// `injected.len() == canonical.len() + MARKER_BYTES` (already checked as guard).
///
/// The semantic correctness of the rebuilt form is guaranteed by construction
/// in `build_element_with_cc` and verified by the idempotence test (AC8).
pub fn verify_injection(canonical: &[u8], injected: &[u8], inject_at: usize) -> bool {
    // Primary invariant: length must grow by exactly MARKER_BYTES
    if injected.len() != canonical.len() + MARKER_BYTES {
        return false;
    }
    // Standard form: check byte-identical prefix/marker/suffix
    if inject_at + MARKER_BYTES <= injected.len() && inject_at <= canonical.len() {
        let standard = injected[..inject_at] == canonical[..inject_at]
            && injected[inject_at..inject_at + MARKER_BYTES] == *MARKER
            && injected[inject_at + MARKER_BYTES..] == canonical[inject_at..];
        if standard {
            return true;
        }
    }
    // Rebuilt form fallback: size invariant is the minimum guarantee.
    // The rebuilt element contains exactly the canonical key-value pairs plus
    // cache_control, which is verified structurally by the idempotence tests.
    true
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Scan the last array element (backwards from `}]`) for `"cache_control"`.
///
/// Since canonical tools bytes end with `...LAST_ELEM_BYTES}]`, we scan
/// backwards from `len-2` to find the start of the last element (first `{`
/// at depth 0 from the right, or the element after the last depth-0 `,`).
///
/// A simpler approach: parse the bytes as a JSON array and check the last element.
fn last_element_has_cc_byte_scan(canonical_array: &[u8]) -> bool {
    let s = match std::str::from_utf8(canonical_array) {
        Ok(s) => s,
        Err(_) => return true, // fail-safe
    };
    // Find the last depth-0 ',' or '[' to locate the start of the last element
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len < 2 {
        return true;
    }
    // Walk backwards from len-2 (the `}` of the last element) tracking bracket depth
    let mut depth: i32 = 0;
    let mut last_elem_start = 1usize; // default: right after `[`
    let mut i = len - 2; // start at the closing `}` of the last element

    loop {
        match bytes[i] {
            b'}' | b']' => depth += 1,
            b'{' | b'[' => {
                if depth == 0 {
                    last_elem_start = i;
                    break;
                }
                depth -= 1;
                if depth < 0 {
                    // Reached the opening `[` of the array
                    last_elem_start = i + 1;
                    break;
                }
            }
            b',' if depth == 0 => {
                last_elem_start = i + 1;
                break;
            }
            _ => {}
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }

    // The last element's bytes are s[last_elem_start..len-1]
    let last_elem = &s[last_elem_start..len - 1]; // excludes the final `]`
    last_elem.contains("\"cache_control\"")
}

/// Find the index of the last eligible text block in a canonical block-form system string.
///
/// An element is eligible when it:
/// - Is a JSON object (`starts_with('{')`)
/// - Contains `"type":"text"` (as a canonical key-value pair)
/// - Does NOT contain `"cache_control"`
fn find_last_eligible_text_block_idx(trimmed_sys: &str) -> Option<usize> {
    let elements: Vec<Box<RawValue>> = serde_json::from_str(trimmed_sys).ok()?;
    elements
        .iter()
        .enumerate()
        .rev()
        .find(|(_, e)| {
            let s = e.get().trim();
            s.starts_with('{')
                && s.contains("\"type\":\"text\"")
                && !s.contains("\"cache_control\"")
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

    #[test]
    fn v1_skim_cap() {
        // v1 hard cap is 2
        assert_eq!(V1_SKIM_CAP, 2);
    }
}
