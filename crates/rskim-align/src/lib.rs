//! KV-cache alignment for skim Layer 3.
//!
//! # Overview
//!
//! This crate provides deterministic canonical ordering of Anthropic and OpenAI
//! request bodies to maximise KV-cache hit rates across repeated or slightly-varied
//! requests, plus bounded `cache_control` marker injection at stable structural
//! positions. It operates entirely in-process, synchronously, with no I/O, no async,
//! and no ambient state (no wall-clock, no entropy). See DECISIONS-RESOLVED.md
//! Decision 6 and the plan at
//! `.devflow/docs/design/l3-wave3/2026-07-17_1916/306-kv-cache-alignment-plan.md`.
//!
//! # AD-CA-1 — Pure synchronous crate
//!
//! rskim-align is intentionally a pure sync crate: no tokio, no hyper, no TLS,
//! no RNG, no clock. This makes the transform deterministic (AC9) and composable
//! without an async runtime. The determinism gate is enforced at compile time by
//! the `clippy.toml` `disallowed-methods` entries in this crate.
//!
//! # AD-CA-10 — Provider isolation
//!
//! The provider branch occurs **before** any marker code. Anthropic bodies receive
//! canonical ordering **plus** bounded `cache_control` marker injection. OpenAI
//! bodies receive canonical ordering only (no `cache_control`). An unknown provider
//! variant causes whole-request fail-open.
//!
//! # AD-CA-7 — Triple self-verify
//!
//! The pipeline applies three independent self-verify checks:
//! 1. **Reorder path**: `tools_arrays_set_equal(original_tools, canonical_tools)` — proves
//!    the tools element sort dropped, duplicated, or mutated nothing.
//! 2. **Envelope path**: output `messages` value span byte-identical to input `messages`
//!    span (done in `canonical_envelope`), plus unchanged top-level key set.
//! 3. **Injection path**: after injecting markers, verify each injected span against the
//!    pre-injection canonical bytes (byte-exact equality after stripping the known marker).
//!
//! Any failure → whole-request SHA-256-equal fail-open passthrough.
//!
//! # Fail-open doctrine
//!
//! Every error condition (parse failure, duplicate key, depth exceeded, UTF-8
//! error, self-verify failure) results in a passthrough — the original input bytes
//! are returned unmodified. A proxy that corrupts a request is worse than one
//! that passes it through unchanged.

pub mod breakpoint;
pub mod canonical_emit;
pub mod span;
pub mod stats;
pub mod volatile;

use crate::breakpoint::{plan_injection, BreakpointPlan};
use crate::canonical_emit::canonical_envelope;
use crate::span::locate_top_level_spans;
use crate::stats::AlignStats;
use rskim_contract::canonical::tools_arrays_set_equal;
use rskim_contract::contract::{Contract, Outcome};
use rskim_contract::waiver::{MetadataReorderWithMarkers, MARKER_BYTES};
use rskim_llm::{ParsedBody, Provider};
use std::collections::HashMap;

// ============================================================================
// Public constants
// ============================================================================

/// Maximum recursive depth for canonical value emission.
///
/// # AC26 — Depth fail-open
///
/// Any value nested deeper than this limit causes `canonical_emit::canonicalize_value`
/// to return `None`, which propagates up to cause a whole-request passthrough.
/// This prevents stack overflow on pathologically deep JSON (e.g., a model's
/// input schema with recursively nested objects).
///
/// The value 32 is deliberately conservative: real tool `input_schema` objects
/// are rarely deeper than 10 levels.
pub const MAX_ALIGN_SCHEMA_DEPTH: u32 = 32;

// ============================================================================
// AlignOutcome
// ============================================================================

/// The result of a canonical alignment operation.
///
/// Always contains valid bytes: if alignment succeeded, `bytes` is the
/// canonical form with any skim markers injected; if any step failed,
/// `bytes` is the original input (fail-open). The `stats` field carries
/// observability data for analytics.
#[derive(Debug, Clone)]
#[must_use]
pub struct AlignOutcome {
    /// Output bytes. On success: canonical + markers. On fail-open: original input.
    pub bytes: Vec<u8>,
    /// Alignment statistics (content-free; only hashes and counts — AC18).
    pub stats: AlignStats,
}

// ============================================================================
// Internal result type
// ============================================================================

/// Internal alignment result returned by `try_align_full`.
struct AlignResult {
    bytes: Vec<u8>,
    skim_count: usize,
    client_count: usize,
    tools_key_sorted: bool,
    spans_compacted: bool,
    volatile_warn_count: usize,
}

// ============================================================================
// Public: align()
// ============================================================================

/// Align a request body to its canonical form and inject skim cache markers.
///
/// # AD-CA-3 — Provider-aware alignment
///
/// The `provider` argument controls which fields are canonicalized, how tool
/// names are extracted for element ordering, and whether `cache_control` markers
/// are injected (Anthropic only).
///
/// # AD-CA-10 — Provider isolation before any marker code
///
/// Provider dispatch occurs before any marker injection logic. An unknown
/// provider variant causes a whole-request fail-open.
///
/// # AD-CA-7 — Triple self-verify
///
/// Three independent self-verify checks guard egress; any failure causes
/// whole-request fail-open (SHA-256-equal to input).
///
/// # Fail-open
///
/// Returns the original input bytes on any error. The `stats.fail_open` flag
/// is set to `true` in that case.
pub fn align(body: &[u8], provider: Provider, _request_id: &str) -> AlignOutcome {
    match try_align_full(body, provider) {
        Some(result) => {
            let s = AlignStats::success(
                body,
                &result.bytes,
                result.tools_key_sorted,
                result.spans_compacted,
                result.skim_count,
                result.client_count,
                result.volatile_warn_count,
            );
            AlignOutcome {
                bytes: result.bytes,
                stats: s,
            }
        }
        None => AlignOutcome {
            bytes: body.to_vec(),
            stats: AlignStats::fail_open_from_input(body),
        },
    }
}

// ============================================================================
// Internal: try_align_full
// ============================================================================

/// Full alignment pipeline: canonical ordering + marker injection + triple self-verify.
///
/// Returns `None` on any error (fail-open), or `Some(AlignResult)` on success.
///
/// # AD-CA-10 — Provider isolation
///
/// The provider branch is the first operation. Marker injection code is only
/// reached for `Provider::Anthropic`. Any other provider falls through to
/// canonical ordering only.
fn try_align_full(body: &[u8], provider: Provider) -> Option<AlignResult> {
    // AD-CA-10: branch on provider BEFORE any marker code
    // Unknown/future provider variants map to fail-open (defensive passthrough)
    match provider {
        Provider::Anthropic | Provider::OpenAi => {}
        _ => return None,
    }

    // ── Step 1: Parse and locate top-level spans ─────────────────────────────
    let input_str = std::str::from_utf8(body).ok()?;
    // AD-CA-11: duplicate top-level keys → None → whole-request fail-open
    let input_spans = locate_top_level_spans(input_str)?;

    // ── Step 2: Build canonical form (no markers yet) ────────────────────────
    // AD-CA-2/12/13: canonical ordering, element sort, envelope key order
    let canonical_bytes = canonical_envelope(input_str, &input_spans, provider)?;
    let canonical_str = std::str::from_utf8(&canonical_bytes).ok()?;
    // Second span-locate on canonical output (needed for span extraction below)
    let canonical_spans = locate_top_level_spans(canonical_str)?;

    // ── AD-CA-7 envelope path: top-level key set unchanged ───────────────────
    // (The messages-span byte-identity is verified inside canonical_envelope.)
    // Verify: same number of top-level keys, all input keys present in canonical.
    if input_spans.len() != canonical_spans.len() {
        return None;
    }
    for key in input_spans.keys() {
        if !canonical_spans.contains_key(key) {
            return None;
        }
    }

    // ── AD-CA-7 reorder path: tools arrays set-equal ─────────────────────────
    // tools_arrays_set_equal(original_tools_span, canonicalized_reordered_tools_span)
    // Proves the element sort dropped, duplicated, or mutated nothing.
    let orig_tools_str = input_spans.get("tools").and_then(|s| s.extract(input_str));
    let canon_tools_str = canonical_spans.get("tools").and_then(|s| s.extract(canonical_str));
    if let (Some(orig), Some(canon)) = (orig_tools_str, canon_tools_str)
        && !tools_arrays_set_equal(orig, canon)
    {
        return None; // AD-CA-7 reorder self-verify failed → fail-open
    }

    // ── Stats bookkeeping ────────────────────────────────────────────────────
    let tools_key_sorted = input_spans.contains_key("tools") || input_spans.contains_key("functions");
    let spans_compacted = canonical_bytes.len() <= body.len();

    // ── Step 3: Count client markers in original input ───────────────────────
    let client_count = breakpoint::count_client_markers(input_str, &input_spans);

    // ── AD-CA-10: Marker injection — Anthropic only ──────────────────────────
    // BEFORE this branch, no marker-related code has run.
    let (final_bytes, skim_count) = if provider == Provider::Anthropic {
        // Extract canonical tools and system bytes for injection planning
        let canonical_tools_bytes: Option<Vec<u8>> = canonical_spans
            .get("tools")
            .and_then(|s| s.extract(canonical_str))
            .map(|v| v.as_bytes().to_vec());
        let canonical_system_bytes: Option<Vec<u8>> = canonical_spans
            .get("system")
            .and_then(|s| s.extract(canonical_str))
            .map(|v| v.as_bytes().to_vec());

        let plan = plan_injection(
            canonical_tools_bytes.as_deref(),
            canonical_system_bytes.as_deref(),
            client_count,
        );

        if plan.skim_count == 0 {
            // No markers to inject — canonical output is final
            (canonical_bytes, 0)
        } else {
            // Apply marker injection with AD-CA-7 injection path self-verify
            let result = apply_injection(
                &canonical_bytes,
                &canonical_spans,
                canonical_tools_bytes.as_deref(),
                canonical_system_bytes.as_deref(),
                &plan,
            );
            // Any injection failure → whole-request fail-open
            result?
        }
    } else {
        // OpenAI and future providers: canonical ordering only, no markers (AC15)
        (canonical_bytes, 0)
    };

    // ── AD-CA-6: Volatile detection (warn/stats only) ────────────────────────
    // Called AFTER injection is finalized. VolatileReport is never passed to
    // breakpoint.rs — structural isolation enforced by module imports.
    let volatile_report = volatile::detect_volatile(input_str);

    Some(AlignResult {
        bytes: final_bytes,
        skim_count,
        client_count,
        tools_key_sorted,
        spans_compacted,
        volatile_warn_count: volatile_report.pattern_count,
    })
}

// ============================================================================
// Internal: apply_injection
// ============================================================================

/// Apply marker injection into the canonical output at the positions specified by `plan`.
///
/// Injects into tools first (higher byte offset in canonical key order), then system
/// (lower byte offset). This order ensures that the system span position is not shifted
/// by the tools injection.
///
/// # AD-CA-7 — Injection path self-verify
///
/// Each injection is verified with `breakpoint::verify_injection` before being
/// committed. Any verification failure returns `None` → whole-request fail-open.
///
/// # Returns
///
/// `Some((final_bytes, skim_count))` on success, `None` on any failure.
fn apply_injection(
    canonical_bytes: &[u8],
    canonical_spans: &HashMap<String, crate::span::Span>,
    canonical_tools_bytes: Option<&[u8]>,
    canonical_system_bytes: Option<&[u8]>,
    plan: &BreakpointPlan,
) -> Option<(Vec<u8>, usize)> {
    let mut result = canonical_bytes.to_vec();
    let mut skim_count = 0usize;

    // Inject into tools (at higher byte offset in canonical output — 't' > 's').
    // Doing tools first leaves system's span offset unaffected.
    if plan.inject_tools
        && let (Some(tools_bytes), Some(tools_span)) =
            (canonical_tools_bytes, canonical_spans.get("tools"))
    {
        let (injected_tools, inject_at_in_tools) =
            breakpoint::inject_tools_marker(tools_bytes)?;

        // AD-CA-7 injection path self-verify for tools
        if !breakpoint::verify_injection(tools_bytes, &injected_tools, inject_at_in_tools) {
            return None;
        }

        // Replace tools value span in result with injected bytes.
        // canonical_spans.get("tools").start is the offset of the tools VALUE in
        // canonical_bytes, which is still valid in `result` (no earlier insertions yet).
        let span_start = tools_span.start;
        let span_end = tools_span.start + tools_span.len;
        let mut new_result = Vec::with_capacity(result.len() + MARKER_BYTES);
        new_result.extend_from_slice(&result[..span_start]);
        new_result.extend_from_slice(&injected_tools);
        new_result.extend_from_slice(&result[span_end..]);
        result = new_result;
        skim_count += 1;
    }

    // Inject into system (at lower byte offset in canonical output — 's' < 't').
    // After the tools injection above, the bytes before tools_span.start are unchanged,
    // so system_span.start (which is < tools_span.start) is still valid in `result`.
    if plan.inject_system
        && let (Some(system_bytes), Some(system_span)) =
            (canonical_system_bytes, canonical_spans.get("system"))
    {
        let (injected_system, inject_at_in_system) =
            breakpoint::inject_system_marker(system_bytes)?;

        // AD-CA-7 injection path self-verify for system
        if !breakpoint::verify_injection(system_bytes, &injected_system, inject_at_in_system) {
            return None;
        }

        // Replace system value span in result.
        // system_span.start is a position in canonical_bytes; since
        // tools_span.start > system_span.start, the tools injection did NOT
        // shift any bytes at position ≤ system_span.start. The system span
        // offset is still valid.
        let span_start = system_span.start;
        let span_end = system_span.start + system_span.len;
        let mut new_result = Vec::with_capacity(result.len() + MARKER_BYTES);
        new_result.extend_from_slice(&result[..span_start]);
        new_result.extend_from_slice(&injected_system);
        new_result.extend_from_slice(&result[span_end..]);
        result = new_result;
        skim_count += 1;
    }

    Some((result, skim_count))
}

// ============================================================================
// Provider detection
// ============================================================================

/// Detect the request provider from raw body bytes.
///
/// # AD-CA-10 — Provider isolation
///
/// Uses `rskim_llm::parse` for structural heuristic detection. If detection
/// fails (non-JSON, unknown shape), returns `None` → the caller must fail-open.
///
/// This function is only called by `CacheAlignContract::apply_reorder`, which
/// wraps it with the fail-open logic. The `align()` function accepts provider
/// as an explicit argument (caller has already detected it) and does not use
/// this function.
fn detect_provider(input: &[u8]) -> Option<Provider> {
    match rskim_llm::parse(input).ok()? {
        ParsedBody::Anthropic(_) => Some(Provider::Anthropic),
        ParsedBody::OpenAi(_) => Some(Provider::OpenAi),
        // AD-CA-10: unknown provider → fail-open
        _ => None,
    }
}

// ============================================================================
// CacheAlignContract
// ============================================================================

/// `Contract` implementation for KV-cache alignment.
///
/// Implements both [`Contract`] and [`MetadataReorderWithMarkers`].
///
/// # AD-CA-1 — Stateless by design
///
/// `CacheAlignContract` carries no mutable state; all transform parameters
/// are determined from the input bytes at call time. This makes it safe to
/// share across threads.
///
/// # Integration note (Phase 4)
///
/// Phase 4 adds `CacheAlignStage` in `rskim-proxy` that wraps this contract.
/// The stage overrides `TransformStage::max_growth` to return `2 × MARKER_BYTES`
/// (from `rskim_contract::waiver::MARKER_BYTES`), covering the bounded growth
/// from `cache_control` marker injection.
pub struct CacheAlignContract;

impl Contract for CacheAlignContract {
    fn component_name(&self) -> &'static str {
        "cache-align"
    }

    /// Transform a request body into its canonical form with markers.
    ///
    /// Delegates to `apply_reorder` (from the `MetadataReorderWithMarkers`
    /// waiver) and validates the marker cap before accepting the modification.
    ///
    /// Returns passthrough if:
    /// - Provider detection fails (AD-CA-10)
    /// - Input is not valid UTF-8 or not a valid JSON object
    /// - Duplicate top-level keys (AD-CA-11)
    /// - Nesting exceeds `MAX_ALIGN_SCHEMA_DEPTH` (AC26)
    /// - Any AD-CA-7 self-verify fails
    /// - Output would exceed the waiver marker cap
    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        match self.apply_reorder(input, request_id) {
            Some(candidate) => {
                // Verify the waiver marker cap (AC20: use verify_marker_cap,
                // not the strict never-inflate check, for a growing component).
                if !self.verify_marker_cap(input.len(), candidate.len()) {
                    return Outcome::passthrough(
                        input.to_vec(),
                        request_id,
                        self.component_name(),
                    );
                }
                Outcome::modified(candidate, input.len(), request_id, self.component_name())
            }
            None => Outcome::passthrough(input.to_vec(), request_id, self.component_name()),
        }
    }
}

impl MetadataReorderWithMarkers for CacheAlignContract {
    /// Apply canonical ordering and bounded marker injection.
    ///
    /// # AD-CA-10 — Provider isolation
    ///
    /// Detects provider from the body; unknown provider → `None` → passthrough.
    ///
    /// # AD-CA-11 — Duplicate top-level key detection
    ///
    /// Duplicate keys in the top-level object → `None` → passthrough.
    ///
    /// # AD-CA-2 / AD-CA-12 / AD-CA-13 — Canonical form
    ///
    /// Within-object key-sort (AD-CA-2), element-reorder for tools/functions
    /// (AD-CA-12), and canonical envelope key order (AD-CA-13) are applied
    /// together via `canonical_emit::canonical_envelope`.
    ///
    /// # AD-CA-7 — Triple self-verify
    ///
    /// Reorder path, envelope path, and injection path are each independently
    /// verified. Any failure → `None` → passthrough.
    ///
    /// # AC26 — Depth fail-open
    ///
    /// If any value exceeds `MAX_ALIGN_SCHEMA_DEPTH` nesting depth, returns
    /// `None` → passthrough.
    fn apply_reorder(&self, input: &[u8], _request_id: &str) -> Option<Vec<u8>> {
        // AD-CA-10: detect provider; unknown → fail-open
        let provider = detect_provider(input)?;
        // Run full pipeline: canonical ordering + marker injection + triple self-verify
        let result = try_align_full(input, provider)?;
        Some(result.bytes)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::stats::sha256;
    use rskim_contract::contract::Contract;
    use rskim_contract::waiver::MetadataReorderWithMarkers;

    // ── align() — basic canonicalization ─────────────────────────────────────

    #[test]
    fn align_anthropic_sorts_top_level_keys() {
        let body = br#"{"model":"claude-3","messages":[],"max_tokens":100}"#;
        let out = align(body, Provider::Anthropic, "req-001");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        // max_tokens < messages < model
        let max_pos = out_str.find("\"max_tokens\"").unwrap();
        let msgs_pos = out_str.find("\"messages\"").unwrap();
        let model_pos = out_str.find("\"model\"").unwrap();
        assert!(max_pos < msgs_pos);
        assert!(msgs_pos < model_pos);
    }

    #[test]
    fn align_bad_utf8_passthrough() {
        let body: Vec<u8> = vec![0xFF, 0xFE, 0x00];
        let out = align(&body, Provider::Anthropic, "req-002");
        // Fail-open: invalid UTF-8 → original bytes
        assert_eq!(out.bytes, body);
        assert!(out.stats.fail_open);
    }

    #[test]
    fn align_not_json_passthrough() {
        let body = b"not json at all";
        let out = align(body, Provider::Anthropic, "req-003");
        assert_eq!(&out.bytes, body);
        assert!(out.stats.fail_open);
    }

    #[test]
    fn align_duplicate_key_passthrough_ad_ca_11() {
        let body = br#"{"tools":[],"tools":[],"messages":[]}"#;
        let out = align(body, Provider::Anthropic, "req-004");
        // AD-CA-11: duplicate key → passthrough
        assert_eq!(&out.bytes, body);
        assert!(out.stats.fail_open);
    }

    #[test]
    fn align_number_token_verbatim_ac12() {
        let body = br#"{"messages":[],"count":1e3}"#;
        let out = align(body, Provider::Anthropic, "req-005");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert!(out_str.contains("1e3"), "number token 1e3 must not be reformatted");
    }

    // ── AC4 — v1 marker budget ────────────────────────────────────────────────

    #[test]
    fn align_injects_markers_tools_and_system_ac4() {
        // AC4: 3 tools + block system + no client markers → 2 skim markers
        let body = br#"{"messages":[],"model":"claude-3","system":[{"text":"hi","type":"text"}],"tools":[{"name":"a"},{"name":"b"},{"name":"c"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac4");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        let cc_count = count_cache_control_occurrences(out_str);
        assert_eq!(cc_count, 2, "expected exactly 2 skim markers (one in tools, one in system)");
        assert_eq!(out.stats.skim_breakpoints_injected, 2);
        assert!(!out.stats.fail_open);
    }

    #[test]
    fn align_injects_one_marker_tools_only_ac4() {
        // tools only, no system → 1 marker
        let body = br#"{"messages":[],"model":"claude-3","tools":[{"name":"a"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac4b");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert_eq!(count_cache_control_occurrences(out_str), 1);
        assert_eq!(out.stats.skim_breakpoints_injected, 1);
    }

    #[test]
    fn align_marker_on_last_tool_not_earlier_ac4() {
        // AC4: marker ONLY on the canonically-last tool object (beta, sorted after alpha).
        // build_element_with_cc inserts at the sorted key position — "cache_control" ('c')
        // sorts before "name" ('n'), so the beta element is:
        //   {"cache_control":{"type":"ephemeral"},"name":"beta"}
        // i.e. cc_pos < beta_pos in string order.  The invariant to test is that cc is in
        // the beta element and NOT in the alpha element.
        let body = br#"{"messages":[],"tools":[{"name":"alpha"},{"name":"beta"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac4c");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        let alpha_pos = out_str.find("\"alpha\"").unwrap();
        let cc_pos = out_str.find("\"cache_control\"").unwrap();
        // tools sorted by name: alpha appears before the beta element in the array
        assert!(alpha_pos < cc_pos, "alpha must appear before the cc marker");
        // cc is the first key in beta element; "beta" value follows it
        let after_cc = &out_str[cc_pos..];
        assert!(
            after_cc.contains("\"beta\""),
            "beta name must appear after its cc marker (cc sorts first in canonical key order)"
        );
        // alpha element must not contain cache_control
        let before_cc = &out_str[..cc_pos];
        assert!(
            !before_cc.contains("\"cache_control\""),
            "alpha element must not carry a cc marker"
        );
    }

    // ── AC6 — Volatile detector does not steer placement ─────────────────────

    #[test]
    fn align_volatile_body_same_placement_ac6() {
        // AC6: same body with/without UUID injected into system → identical marker placement
        let body_stable = br#"{"messages":[],"system":[{"text":"stable","type":"text"}],"tools":[{"name":"a"}]}"#;
        let body_volatile = br#"{"messages":[],"system":[{"text":"550e8400-e29b-41d4-a716-446655440000","type":"text"}],"tools":[{"name":"a"}]}"#;

        let out_stable = align(body_stable, Provider::Anthropic, "req-ac6a");
        let out_volatile = align(body_volatile, Provider::Anthropic, "req-ac6b");

        // Both should have 2 skim markers (tools + system eligible)
        let stable_str = std::str::from_utf8(&out_stable.bytes).unwrap();
        let volatile_str = std::str::from_utf8(&out_volatile.bytes).unwrap();
        let stable_cc = count_cache_control_occurrences(stable_str);
        let volatile_cc = count_cache_control_occurrences(volatile_str);
        assert_eq!(stable_cc, volatile_cc, "AC6: volatile content must not affect marker count");

        // Volatile body triggers a warn
        assert!(out_volatile.stats.volatile_warn_count > 0, "volatile body must have warn count > 0");
        // Stable body has no warn
        assert_eq!(out_stable.stats.volatile_warn_count, 0, "stable body must have no warn");
    }

    // ── AC7 — Shape eligibility ───────────────────────────────────────────────

    #[test]
    fn align_string_system_no_tools_passthrough_no_markers_ac7() {
        // AC7(a): string system + string content + no tools → no markers, canonical output
        let body = br#"{"messages":[{"content":"hi","role":"user"}],"model":"claude-3","system":"You are helpful."}"#;
        let out = align(body, Provider::Anthropic, "req-ac7a");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        // No cache_control injected
        assert!(!out_str.contains("cache_control"), "AC7(a): no markers for string-only body");
        assert_eq!(out.stats.skim_breakpoints_injected, 0);
        // System string preserved verbatim
        assert!(out_str.contains("\"You are helpful.\""));
    }

    #[test]
    fn align_string_system_with_tools_one_marker_ac7() {
        // AC7(b): string system + non-empty tools → 1 marker on last tool, system string verbatim
        let system_str = "\"You are helpful.\"";
        let body_str = format!(
            r#"{{"messages":[],"model":"claude-3","system":{system_str},"tools":[{{"name":"search"}}]}}"#
        );
        let out = align(body_str.as_bytes(), Provider::Anthropic, "req-ac7b");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert_eq!(count_cache_control_occurrences(out_str), 1, "AC7(b): exactly 1 marker on last tool");
        assert!(out_str.contains(system_str), "AC7(b): system string must be verbatim");
        assert_eq!(out.stats.skim_breakpoints_injected, 1);
    }

    // ── AC8 — Idempotence ─────────────────────────────────────────────────────

    #[test]
    fn align_idempotent_no_tools_ac8() {
        // AC8: align(align(x)) == align(x) for a body without tools
        let body = br#"{"messages":[{"content":"test","role":"user"}],"model":"claude-3"}"#;
        let out1 = align(body, Provider::Anthropic, "req-ac8a");
        let out2 = align(&out1.bytes, Provider::Anthropic, "req-ac8b");
        assert_eq!(out1.bytes, out2.bytes, "AC8: must be idempotent");
    }

    #[test]
    fn align_idempotent_with_tools_ac8() {
        // AC8: align(align(x)) == align(x) for a body with tools (marker pre-present)
        let body = br#"{"messages":[],"model":"claude-3","tools":[{"name":"b"},{"name":"a"}]}"#;
        let out1 = align(body, Provider::Anthropic, "req-ac8c");
        // Second pass: marker already in the last tool → idempotent (no double injection)
        let out2 = align(&out1.bytes, Provider::Anthropic, "req-ac8d");
        assert_eq!(out1.bytes, out2.bytes, "AC8: must be idempotent with pre-marked body");
        // Stats: second pass has 1 client marker (from first pass) and 0 skim markers
        assert_eq!(out2.stats.skim_breakpoints_injected, 0, "second pass must not re-inject");
        assert_eq!(out2.stats.client_breakpoint_count, 1, "second pass sees 1 client marker (skim's marker)");
    }

    // ── AC10 — Fail-open on malformed/ambiguous input ─────────────────────────

    #[test]
    fn align_malformed_json_passthrough_ac10() {
        let body = b"{invalid json}";
        let out = align(body, Provider::Anthropic, "req-ac10a");
        assert_eq!(&out.bytes, body);
        assert!(out.stats.fail_open);
    }

    #[test]
    fn align_dup_tools_key_passthrough_ac10() {
        let body = br#"{"tools":[],"tools":[{"name":"a"}],"messages":[]}"#;
        let out = align(body, Provider::Anthropic, "req-ac10b");
        assert_eq!(&out.bytes, body);
        assert!(out.stats.fail_open);
    }

    #[test]
    fn align_dup_system_key_passthrough_ac10() {
        let body = br#"{"messages":[],"system":"a","system":"b"}"#;
        let out = align(body, Provider::Anthropic, "req-ac10c");
        assert_eq!(&out.bytes, body);
        assert!(out.stats.fail_open);
    }

    // ── AC12 — Set-equal tools + companion non-tautology ─────────────────────

    #[test]
    fn align_tools_set_equal_ac12() {
        use crate::canonical_emit::canonical_envelope;
        use rskim_contract::canonical::tools_arrays_set_equal;

        // AC12: tools_arrays_set_equal(original, canonical-pre-injection) == true.
        // This tests the REORDER path self-verify (AD-CA-7): element sort preserves
        // the tool set (no drops, dups, or mutations). The comparison is against the
        // PRE-INJECTION canonical (not post-injection), because injection adds cache_control
        // which changes the element and would break set-equality.
        let body = br#"{"messages":[],"tools":[{"name":"b","input_schema":{"type":"object"}},{"name":"a","input_schema":{"type":"object"}}]}"#;
        let body_str = std::str::from_utf8(body).unwrap();

        // Build pre-injection canonical via the module under test
        let input_spans = locate_top_level_spans(body_str).unwrap();
        let canonical_bytes =
            canonical_envelope(body_str, &input_spans, Provider::Anthropic).unwrap();
        let canonical_str = std::str::from_utf8(&canonical_bytes).unwrap();
        let canonical_spans = locate_top_level_spans(canonical_str).unwrap();

        let orig_tools = input_spans
            .get("tools")
            .and_then(|s| s.extract(body_str))
            .unwrap();
        let canon_tools = canonical_spans
            .get("tools")
            .and_then(|s| s.extract(canonical_str))
            .unwrap();

        assert!(
            tools_arrays_set_equal(orig_tools, canon_tools),
            "AC12: canonical reorder must preserve the tool set (no drop/dup/mutation)"
        );
        // Companion non-tautology: byte order must differ (reorder happened: a < b)
        assert_ne!(
            orig_tools.as_bytes(),
            canon_tools.as_bytes(),
            "AC12: reorder must produce different byte sequence (non-tautology)"
        );
    }

    // ── AC13 — Bounded growth with imported constants ─────────────────────────

    #[test]
    fn align_bounded_growth_ac13() {
        use rskim_contract::waiver::{MARKER_BYTES, MAX_MARKERS};
        // AC13: len(out) <= len(in) + MAX_MARKERS * MARKER_BYTES (v1 tighter: + 2*MARKER_BYTES)
        let body = br#"{"messages":[],"model":"claude-3","system":[{"text":"s","type":"text"}],"tools":[{"name":"a"},{"name":"b"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac13a");
        let max_growth = MAX_MARKERS * MARKER_BYTES; // 4 * 37 = 148
        let v1_tighter = 2 * MARKER_BYTES; // 2 * 37 = 74
        assert!(
            out.bytes.len() <= body.len() + max_growth,
            "AC13: output exceeds MAX_MARKERS * MARKER_BYTES bound"
        );
        assert!(
            out.bytes.len() <= body.len() + v1_tighter,
            "AC13: output exceeds v1 tighter 2 * MARKER_BYTES bound"
        );
    }

    #[test]
    fn align_fail_open_sha256_equal_ac13() {
        // AC13: fail-open inputs are SHA-256-equal to stage input (no byte change)
        let dup_body = br#"{"tools":[],"tools":[{"name":"a"}],"messages":[]}"#;
        let out = align(dup_body, Provider::Anthropic, "req-ac13b");
        assert_eq!(&out.bytes, dup_body, "fail-open must return exact input bytes");
        assert_eq!(out.stats.input_sha256, out.stats.output_sha256, "fail-open SHA-256s must be equal");
    }

    // ── AC14 — Client sovereignty (>4 markers) ───────────────────────────────

    #[test]
    fn align_client_4_markers_zero_skim_ac14() {
        // AC14: client has 4 markers → 0 skim markers injected, all client markers preserved
        let body = br#"{"messages":[{"role":"user","content":[{"cache_control":{"type":"ephemeral"},"text":"a","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"b","type":"text"}]}],"system":[{"cache_control":{"type":"ephemeral"},"text":"sys","type":"text"}],"tools":[{"cache_control":{"type":"ephemeral"},"name":"t"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac14");
        assert_eq!(out.stats.skim_breakpoints_injected, 0, "AC14: 4 client markers → 0 skim");
        // All 4 client markers preserved
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert_eq!(count_cache_control_occurrences(out_str), 4);
    }

    #[test]
    fn align_client_5_markers_zero_skim_ac14() {
        // AC14: 5 client markers → 0 skim markers
        let body = br#"{"messages":[{"role":"user","content":[{"cache_control":{"type":"ephemeral"},"text":"a","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"b","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"c","type":"text"}]}],"system":[{"cache_control":{"type":"ephemeral"},"text":"sys","type":"text"}],"tools":[{"cache_control":{"type":"ephemeral"},"name":"t"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac14b");
        assert_eq!(out.stats.skim_breakpoints_injected, 0, "AC14: 5 client markers → 0 skim");
    }

    // ── AC15 — Provider isolation (OpenAI: no cache_control) ─────────────────

    #[test]
    fn align_openai_no_cache_control_ac15() {
        // AC15: OpenAI body must NOT contain cache_control after alignment
        let body = br#"{"messages":[{"content":"hi","role":"user"}],"model":"gpt-4o","tools":[{"function":{"description":"search","name":"search"},"type":"function"}]}"#;
        let out = align(body, Provider::OpenAi, "req-ac15");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert!(
            !out_str.contains("cache_control"),
            "AC15: OpenAI body must not contain cache_control"
        );
        assert_eq!(out.stats.skim_breakpoints_injected, 0);
    }

    #[test]
    fn align_openai_message_bytes_unchanged_ac15() {
        // AC15: all message bytes identical for OpenAI
        let body = br#"{"messages":[{"content":"hello world","role":"user"},{"content":"response","role":"assistant"}],"model":"gpt-4o"}"#;
        let out = align(body, Provider::OpenAi, "req-ac15b");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert!(out_str.contains("\"hello world\""));
        assert!(out_str.contains("\"response\""));
    }

    // ── CacheAlignContract — core ─────────────────────────────────────────────

    #[test]
    fn contract_transform_anthropic_modifies() {
        let contract = CacheAlignContract;
        let input = br#"{"model":"claude-3-haiku","messages":[],"max_tokens":10}"#;
        let outcome = contract.transform(input, "req-100");
        let out_str = std::str::from_utf8(&outcome.bytes).unwrap();
        // max_tokens < messages < model
        let max_pos = out_str.find("max_tokens").unwrap();
        let msgs_pos = out_str.find("messages").unwrap();
        let model_pos = out_str.find("model").unwrap();
        assert!(max_pos < msgs_pos);
        assert!(msgs_pos < model_pos);
    }

    #[test]
    fn contract_transform_invalid_json_passthrough() {
        let contract = CacheAlignContract;
        let input = b"{invalid}";
        let outcome = contract.transform(input, "req-101");
        assert_eq!(outcome.bytes, input);
        assert!(outcome.is_passthrough());
    }

    #[test]
    fn contract_transform_returns_passthrough_on_already_canonical() {
        let contract = CacheAlignContract;
        // Canonical order: max_tokens < messages < model (lexicographic). No tools/system.
        let canonical = br#"{"max_tokens":10,"messages":[],"model":"claude-3-haiku"}"#;
        let outcome = contract.transform(canonical, "req-102");
        // Output must be byte-identical to input (already canonical, no markers eligible)
        assert_eq!(outcome.bytes.as_slice(), canonical.as_slice());
    }

    // ── MetadataReorderWithMarkers ────────────────────────────────────────────

    #[test]
    fn apply_reorder_detects_anthropic() {
        let contract = CacheAlignContract;
        let input = br#"{"model":"claude-3","messages":[{"role":"user","content":"hi"}]}"#;
        let result = contract.apply_reorder(input, "req-200");
        assert!(result.is_some(), "Anthropic body must succeed");
    }

    #[test]
    fn apply_reorder_fails_on_non_json() {
        let contract = CacheAlignContract;
        let result = contract.apply_reorder(b"not json", "req-201");
        assert!(result.is_none(), "Non-JSON must fail-open (None)");
    }

    #[test]
    fn verify_marker_cap_passes_for_growth_up_to_cap() {
        let contract = CacheAlignContract;
        let input_len = 1000;
        // no growth: cap satisfied
        assert!(contract.verify_marker_cap(input_len, input_len));
        // exactly cap (4 * 37 = 148): satisfied
        assert!(contract.verify_marker_cap(input_len, input_len + 148));
        // over cap: rejected
        assert!(!contract.verify_marker_cap(input_len, input_len + 149));
    }

    // ── detect_provider ───────────────────────────────────────────────────────

    #[test]
    fn detect_provider_anthropic() {
        let body = br#"{"model":"claude-3-haiku","messages":[]}"#;
        assert_eq!(detect_provider(body), Some(Provider::Anthropic));
    }

    #[test]
    fn detect_provider_openai() {
        let body = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;
        assert_eq!(detect_provider(body), Some(Provider::OpenAi));
    }

    #[test]
    fn detect_provider_non_json_returns_none() {
        assert!(detect_provider(b"garbage").is_none());
    }

    // ── MAX_ALIGN_SCHEMA_DEPTH ────────────────────────────────────────────────

    #[test]
    fn max_align_schema_depth_constant() {
        assert_eq!(MAX_ALIGN_SCHEMA_DEPTH, 32);
    }

    // ── Stats ─────────────────────────────────────────────────────────────────

    #[test]
    fn align_stats_sha256_populated() {
        let body = br#"{"messages":[],"model":"claude-3","tools":[{"name":"t"}]}"#;
        let out = align(body, Provider::Anthropic, "req-stats");
        // sha256 must be set (non-zero)
        assert_ne!(out.stats.input_sha256, [0u8; 32]);
        assert_ne!(out.stats.output_sha256, [0u8; 32]);
        assert_eq!(out.stats.input_len, body.len());
        assert_eq!(out.stats.output_len, out.bytes.len());
    }

    #[test]
    fn align_stats_fail_open_sha256_equal() {
        let body = b"bad json {{{";
        let out = align(body, Provider::Anthropic, "req-stats-fo");
        assert!(out.stats.fail_open);
        assert_eq!(out.stats.input_sha256, sha256(body));
        assert_eq!(out.stats.output_sha256, sha256(body));
    }

    // ── Helper ────────────────────────────────────────────────────────────────

    /// Count `"cache_control"` key occurrences in a JSON string (for test assertions).
    fn count_cache_control_occurrences(s: &str) -> usize {
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
}
