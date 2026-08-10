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
//! 1. **Reorder path**: `tools_arrays_set_equal(original, canonical)` over BOTH the
//!    `tools` and the OpenAI legacy `functions` spans — proves the element sort dropped,
//!    duplicated, or mutated nothing in either array.
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

use crate::breakpoint::{BreakpointPlan, plan_injection};
use crate::canonical_emit::canonical_envelope;
use crate::span::locate_top_level_spans;
use crate::stats::AlignStats;
use rskim_contract::canonical::tools_arrays_set_equal;
use rskim_contract::contract::{Contract, Outcome};
use rskim_contract::waiver::MetadataReorderWithMarkers;
use rskim_llm::ParsedBody;
/// Re-exported from `rskim_llm` so callers can construct a provider value
/// without adding `rskim-llm` as a separate direct dependency.
pub use rskim_llm::Provider;
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
pub fn align(body: &[u8], provider: Provider, request_id: &str) -> AlignOutcome {
    match try_align_full(body, provider) {
        Some(result) => {
            // AC7/AD-CA-1: emit debug log when the alignment is a genuine no-op
            // (already-canonical body with no eligible injection positions). The log
            // crate is a pure synchronous facade — no clock, no entropy, no I/O.
            if result.bytes == body {
                log::debug!(
                    "rskim-align: canonical no-op request_id={request_id} len={}",
                    body.len()
                );
            }
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
        None => {
            log::debug!(
                "rskim-align: fail-open passthrough request_id={request_id} len={}",
                body.len()
            );
            AlignOutcome {
                bytes: body.to_vec(),
                stats: AlignStats::fail_open_from_input(body),
            }
        }
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

    // ── AD-CA-7 reorder path: element-reordered arrays set-equal ─────────────
    // `canonical_envelope` element-reorders BOTH `tools` and the OpenAI legacy
    // `functions` array (AD-CA-12), so both need the multiset gate — AC29 names both.
    // Gating only `tools` would leave a legacy-`functions` body's reorder unverified,
    // i.e. an ungated egress mutation (ADR-007).
    //
    // tools_arrays_set_equal(original_span, canonicalized_reordered_span) proves the
    // element sort dropped, duplicated, and mutated nothing while permitting the
    // deliberate order change.
    for key in ["tools", "functions"] {
        let before = input_spans.get(key).and_then(|s| s.extract(input_str));
        let after = canonical_spans
            .get(key)
            .and_then(|s| s.extract(canonical_str));
        if let (Some(before), Some(after)) = (before, after)
            && !tools_arrays_set_equal(before, after)
        {
            return None; // AD-CA-7 reorder self-verify failed → fail-open
        }
    }

    // ── Stats bookkeeping ────────────────────────────────────────────────────
    //
    // These are *measured*, not inferred from key presence (ADR-003): report what the
    // canonicalization actually did. `tools_key_sorted` is true only when a tools or
    // functions span was genuinely rewritten; `spans_compacted` only when the canonical
    // body is strictly smaller than the input.
    let tools_key_sorted = ["tools", "functions"].into_iter().any(|key| {
        match (
            input_spans.get(key).and_then(|s| s.extract(input_str)),
            canonical_spans
                .get(key)
                .and_then(|s| s.extract(canonical_str)),
        ) {
            (Some(before), Some(after)) => before != after,
            _ => false,
        }
    });
    let spans_compacted = canonical_bytes.len() < body.len();

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

    // Precondition (AD-CA-13): the canonical envelope emits top-level keys in sorted
    // order, so the `system` value always precedes the `tools` value. The splice order
    // below (tools first, then system) depends on it — injecting into the LATER span
    // first leaves the EARLIER span's offset valid. Verify rather than assume: if the
    // ordering ever changed, splicing at a stale offset would corrupt the body.
    if let (Some(sys), Some(tools)) = (canonical_spans.get("system"), canonical_spans.get("tools"))
        && sys.start >= tools.start
    {
        return None; // fail-open rather than splice at a stale offset
    }

    // Inject into tools (at higher byte offset in canonical output — 't' > 's').
    // Doing tools first leaves system's span offset unaffected.
    if plan.inject_tools
        && let (Some(tools_bytes), Some(tools_span)) =
            (canonical_tools_bytes, canonical_spans.get("tools"))
    {
        let (injected_tools, tools_elem_idx) = breakpoint::inject_tools_marker(tools_bytes)?;

        // AD-CA-7 injection path self-verify for tools
        if !breakpoint::verify_injection(tools_bytes, &injected_tools, tools_elem_idx) {
            return None;
        }

        // Replace tools value span in result with injected bytes.
        // canonical_spans.get("tools").start is the offset of the tools VALUE in
        // canonical_bytes, which is still valid in `result` (no earlier insertions yet).
        // Bounds are resolved with checked slicing: an out-of-range span fails the
        // request open rather than panicking inside the proxy's forwarding path.
        result = splice_span(&result, tools_span.start, tools_span.len, &injected_tools)?;
        skim_count += 1;
    }

    // Inject into system (at lower byte offset in canonical output — 's' < 't').
    // After the tools injection above, the bytes before tools_span.start are unchanged,
    // so system_span.start (which is < tools_span.start) is still valid in `result`.
    if plan.inject_system
        && let (Some(system_bytes), Some(system_span)) =
            (canonical_system_bytes, canonical_spans.get("system"))
    {
        let (injected_system, system_elem_idx) = breakpoint::inject_system_marker(system_bytes)?;

        // AD-CA-7 injection path self-verify for system
        if !breakpoint::verify_injection(system_bytes, &injected_system, system_elem_idx) {
            return None;
        }

        // Replace system value span in result.
        // system_span.start is a position in canonical_bytes; since
        // tools_span.start > system_span.start, the tools injection did NOT
        // shift any bytes at position ≤ system_span.start. The system span
        // offset is still valid.
        result = splice_span(
            &result,
            system_span.start,
            system_span.len,
            &injected_system,
        )?;
        skim_count += 1;
    }

    Some((result, skim_count))
}

/// Replace `buf[start..start + len]` with `replacement`, returning the new buffer.
///
/// Returns `None` when the span is out of bounds — the caller maps that to a
/// whole-request fail-open. Index-based slicing would panic instead, and a panic
/// inside the proxy's forwarding path is never an acceptable failure mode.
fn splice_span(buf: &[u8], start: usize, len: usize, replacement: &[u8]) -> Option<Vec<u8>> {
    let end = start.checked_add(len)?;
    let head = buf.get(..start)?;
    let tail = buf.get(end..)?;
    let mut out = Vec::with_capacity(head.len() + replacement.len() + tail.len());
    out.extend_from_slice(head);
    out.extend_from_slice(replacement);
    out.extend_from_slice(tail);
    Some(out)
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
                    return Outcome::passthrough(input.to_vec(), request_id, self.component_name());
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
        assert!(
            out_str.contains("1e3"),
            "number token 1e3 must not be reformatted"
        );
    }

    // ── AC4 — v1 marker budget ────────────────────────────────────────────────

    #[test]
    fn align_injects_markers_tools_and_system_ac4() {
        // AC4: 3 tools + block system + no client markers → 2 skim markers
        let body = br#"{"messages":[],"model":"claude-3","system":[{"text":"hi","type":"text"}],"tools":[{"name":"a"},{"name":"b"},{"name":"c"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac4");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        let cc_count = count_cache_control_occurrences(out_str);
        assert_eq!(
            cc_count, 2,
            "expected exactly 2 skim markers (one in tools, one in system)"
        );
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
        let body_stable =
            br#"{"messages":[],"system":[{"text":"stable","type":"text"}],"tools":[{"name":"a"}]}"#;
        let body_volatile = br#"{"messages":[],"system":[{"text":"550e8400-e29b-41d4-a716-446655440000","type":"text"}],"tools":[{"name":"a"}]}"#;

        let out_stable = align(body_stable, Provider::Anthropic, "req-ac6a");
        let out_volatile = align(body_volatile, Provider::Anthropic, "req-ac6b");

        // Both should have 2 skim markers (tools + system eligible)
        let stable_str = std::str::from_utf8(&out_stable.bytes).unwrap();
        let volatile_str = std::str::from_utf8(&out_volatile.bytes).unwrap();
        let stable_cc = count_cache_control_occurrences(stable_str);
        let volatile_cc = count_cache_control_occurrences(volatile_str);
        assert_eq!(
            stable_cc, volatile_cc,
            "AC6: volatile content must not affect marker count"
        );

        // Volatile body triggers a warn
        assert!(
            out_volatile.stats.volatile_warn_count > 0,
            "volatile body must have warn count > 0"
        );
        // Stable body has no warn
        assert_eq!(
            out_stable.stats.volatile_warn_count, 0,
            "stable body must have no warn"
        );
    }

    // ── AC7 — Shape eligibility ───────────────────────────────────────────────

    #[test]
    fn align_string_system_no_tools_passthrough_no_markers_ac7() {
        // AC7(a): string system + string content + no tools → no markers, canonical output.
        // Body keys are already in canonical order (messages < model < system) and compact,
        // so the output must be byte-identical to the input (genuine no-op).
        let body = br#"{"messages":[{"content":"hi","role":"user"}],"model":"claude-3","system":"You are helpful."}"#;
        let out = align(body, Provider::Anthropic, "req-ac7a");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        // No cache_control injected
        assert!(
            !out_str.contains("cache_control"),
            "AC7(a): no markers for string-only body"
        );
        assert_eq!(out.stats.skim_breakpoints_injected, 0);
        // System string preserved verbatim
        assert!(out_str.contains("\"You are helpful.\""));
        // AC7(a): SHA-256(output) == SHA-256(stage input) — byte-identical genuine no-op
        assert_eq!(
            &out.bytes, body,
            "AC7(a): already-canonical body with no eligible positions must be byte-identical to input"
        );
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
        assert_eq!(
            count_cache_control_occurrences(out_str),
            1,
            "AC7(b): exactly 1 marker on last tool"
        );
        assert!(
            out_str.contains(system_str),
            "AC7(b): system string must be verbatim"
        );
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
        assert_eq!(
            out1.bytes, out2.bytes,
            "AC8: must be idempotent with pre-marked body"
        );
        // Stats: second pass has 1 client marker (from first pass) and 0 skim markers
        assert_eq!(
            out2.stats.skim_breakpoints_injected, 0,
            "second pass must not re-inject"
        );
        assert_eq!(
            out2.stats.client_breakpoint_count, 1,
            "second pass sees 1 client marker (skim's marker)"
        );
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
        assert_eq!(
            &out.bytes, dup_body,
            "fail-open must return exact input bytes"
        );
        assert_eq!(
            out.stats.input_sha256, out.stats.output_sha256,
            "fail-open SHA-256s must be equal"
        );
    }

    // ── AC14 — Client sovereignty (>4 markers) ───────────────────────────────

    #[test]
    fn align_client_4_markers_zero_skim_ac14() {
        // AC14: client has 4 markers → 0 skim markers injected, all client markers preserved
        let body = br#"{"messages":[{"role":"user","content":[{"cache_control":{"type":"ephemeral"},"text":"a","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"b","type":"text"}]}],"system":[{"cache_control":{"type":"ephemeral"},"text":"sys","type":"text"}],"tools":[{"cache_control":{"type":"ephemeral"},"name":"t"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac14");
        assert_eq!(
            out.stats.skim_breakpoints_injected, 0,
            "AC14: 4 client markers → 0 skim"
        );
        // All 4 client markers preserved
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert_eq!(count_cache_control_occurrences(out_str), 4);
    }

    #[test]
    fn align_client_5_markers_zero_skim_ac14() {
        // AC14: 5 client markers → 0 skim markers
        let body = br#"{"messages":[{"role":"user","content":[{"cache_control":{"type":"ephemeral"},"text":"a","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"b","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"c","type":"text"}]}],"system":[{"cache_control":{"type":"ephemeral"},"text":"sys","type":"text"}],"tools":[{"cache_control":{"type":"ephemeral"},"name":"t"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac14b");
        assert_eq!(
            out.stats.skim_breakpoints_injected, 0,
            "AC14: 5 client markers → 0 skim"
        );
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
    fn align_stats_report_measured_work_not_key_presence() {
        // `tools_key_sorted` must reflect that the tools span ACTUALLY changed, and
        // `spans_compacted` that the body ACTUALLY shrank (ADR-003: never report a
        // number/flag that was not measured).
        let non_canonical = br#"{"messages":[],"tools":[ {"name":"b"} , {"name":"a"} ]}"#;
        let out = align(non_canonical, Provider::Anthropic, "req-stats-measured");
        assert!(
            out.stats.tools_key_sorted,
            "a reordered + compacted tools span must report tools_key_sorted"
        );
        assert!(
            out.stats.spans_compacted,
            "removing whitespace must report spans_compacted"
        );

        // Already-canonical body with no tools: nothing was rewritten, nothing shrank.
        let canonical = br#"{"max_tokens":10,"messages":[],"model":"claude-3"}"#;
        let out = align(canonical, Provider::Anthropic, "req-stats-noop");
        assert_eq!(out.bytes.as_slice(), canonical.as_slice());
        assert!(
            !out.stats.tools_key_sorted,
            "no tools span means no key sort happened"
        );
        assert!(
            !out.stats.spans_compacted,
            "a byte-identical output did not compact anything"
        );
    }

    #[test]
    fn align_trailing_content_passthrough() {
        // ADR-007: a body with bytes after the top-level object must NOT be silently
        // truncated to the first object — it must pass through byte-identical.
        let body = br#"{"messages":[],"model":"claude-3"}TRAILING"#;
        let out = align(body, Provider::Anthropic, "req-trailing");
        assert_eq!(&out.bytes, body, "trailing bytes must not be dropped");
        assert!(out.stats.fail_open);
    }

    #[test]
    fn align_empty_tool_object_no_panic_ac24() {
        // AC24 structural edge shape: `tools:[{}]` must resolve deterministically with
        // no panic. The empty object is an ineligible marker position, so the body is
        // canonicalized without any injection.
        let body = br#"{"messages":[],"tools":[{}]}"#;
        let out = align(body, Provider::Anthropic, "req-empty-tool");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert!(!out_str.contains("cache_control"));
        assert_eq!(out.stats.skim_breakpoints_injected, 0);
        assert!(!out.stats.fail_open, "an empty tool object is not an error");
        // Determinism: a second pass produces the same bytes.
        let out2 = align(&out.bytes, Provider::Anthropic, "req-empty-tool-2");
        assert_eq!(out.bytes, out2.bytes);
    }

    #[test]
    fn align_stats_fail_open_sha256_equal() {
        let body = b"bad json {{{";
        let out = align(body, Provider::Anthropic, "req-stats-fo");
        assert!(out.stats.fail_open);
        assert_eq!(out.stats.input_sha256, sha256(body));
        assert_eq!(out.stats.output_sha256, sha256(body));
    }

    // ── AC2 — OpenAI convergence + legacy functions ───────────────────────────

    #[test]
    fn align_openai_convergence_two_key_orders_ac2() {
        // AC2: two OpenAI bodies identical except top-level key order + tool key order
        // must converge to the same canonical output bytes.
        let body_a = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"tools":[{"type":"function","function":{"description":"search","name":"search"}}]}"#;
        let body_b = br#"{"tools":[{"type":"function","function":{"name":"search","description":"search"}}],"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;
        let out_a = align(body_a, Provider::OpenAi, "req-ac2a");
        let out_b = align(body_b, Provider::OpenAi, "req-ac2b");
        assert_eq!(
            out_a.bytes, out_b.bytes,
            "AC2: OpenAI bodies differing only in key/element order must converge"
        );
        // No markers injected (OpenAI path)
        let out_str = std::str::from_utf8(&out_a.bytes).unwrap();
        assert!(
            !out_str.contains("cache_control"),
            "AC2: OpenAI output must not contain cache_control"
        );
    }

    #[test]
    fn align_openai_legacy_functions_sorted_ac2() {
        // AC2: OpenAI legacy `functions` array is sorted by top-level "name" field.
        // Verifies ToolArrayKind::OpenAiLegacyFunctions path through align().
        let body = br#"{"model":"gpt-4","messages":[],"functions":[{"name":"zoo","description":"z"},{"name":"alpha","description":"a"}]}"#;
        let out = align(body, Provider::OpenAi, "req-ac2-legacy");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        let alpha_pos = out_str.find("\"alpha\"").unwrap_or(usize::MAX);
        let zoo_pos = out_str.find("\"zoo\"").unwrap_or(0);
        assert!(
            alpha_pos < zoo_pos,
            "AC2 legacy: alpha must appear before zoo in sorted functions array"
        );
        assert!(
            !out_str.contains("cache_control"),
            "AC2 legacy: legacy functions body must not get cache_control (OpenAI path)"
        );
    }

    // ── AC3b/AC26 — Depth fail-open end-to-end through align() ───────────────

    #[test]
    fn align_depth_exceeded_fail_open_ac3b_ac26() {
        // AC3b/AC26: a tools body with a schema nested deeper than MAX_ALIGN_SCHEMA_DEPTH
        // must fail-open (passthrough) with SHA-256-equal bytes.
        let leaf = r#"{"type":"string"}"#;
        // Build a JSON object nested MAX_ALIGN_SCHEMA_DEPTH + 1 levels deep
        let mut deep = leaf.to_string();
        for _ in 0..=(MAX_ALIGN_SCHEMA_DEPTH + 1) {
            deep = format!(r#"{{"nested":{deep}}}"#);
        }
        let body_str = format!(
            r#"{{"messages":[],"model":"claude-3","tools":[{{"name":"t","input_schema":{deep}}}]}}"#
        );
        let body = body_str.as_bytes();
        let out = align(body, Provider::Anthropic, "req-ac26-depth");
        // AC26: fail-open → byte-identical to input
        assert_eq!(
            &out.bytes, body,
            "AC26: depth-exceeded body must fail-open to byte-identical passthrough"
        );
        assert!(
            out.stats.fail_open,
            "AC26: depth-exceeded must set fail_open=true"
        );
        // SHA-256 equality (passthrough assertion)
        assert_eq!(
            out.stats.input_sha256, out.stats.output_sha256,
            "AC26: fail-open SHA-256s must be equal"
        );
    }

    // ── AC11 — JSONPath-restricted verbatim checks ────────────────────────────

    #[test]
    fn align_messages_span_byte_identical_ac11() {
        // AC11: the messages value must be byte-identical in the output regardless
        // of what else gets canonicalized. Use a complex messages value with
        // array-form content including unicode and nested structure.
        let messages_json = r#"[{"role":"user","content":[{"text":"Hello world","type":"text"},{"text":"foo\nbar","type":"text"}]},{"role":"assistant","content":"response 123"}]"#;
        let body_str = format!(
            r#"{{"model":"claude-3","tools":[{{"name":"b"}},{{"name":"a"}}],"messages":{messages_json}}}"#
        );
        let body = body_str.as_bytes();
        let out = align(body, Provider::Anthropic, "req-ac11a");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();

        // Extract the messages value from the output using span-location
        let out_spans =
            crate::span::locate_top_level_spans(out_str).expect("AC11: output must be valid JSON");
        let out_messages = out_spans
            .get("messages")
            .and_then(|s| s.extract(out_str))
            .expect("AC11: output must have messages key");
        assert_eq!(
            out_messages, messages_json,
            "AC11: messages value must be byte-identical to input value"
        );
    }

    #[test]
    fn align_non_tools_system_values_verbatim_ac11() {
        // AC11: non-tools, non-system top-level values (model, max_tokens, stream)
        // must be byte-verbatim in the output (only tools/system are canonicalized).
        let body =
            br#"{"messages":[],"max_tokens":1e3,"model":"claude-3","stream":false,"tools":[{"name":"t"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac11b");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        // These values must be present verbatim
        assert!(
            out_str.contains("\"max_tokens\":1e3"),
            "AC11: number token 1e3 must be verbatim (not reformatted)"
        );
        assert!(
            out_str.contains("\"stream\":false"),
            "AC11: stream value must be verbatim"
        );
        assert!(
            out_str.contains("\"model\":\"claude-3\""),
            "AC11: model value must be verbatim"
        );
    }

    // ── AC24 — Structural edge shapes ────────────────────────────────────────

    #[test]
    fn align_tools_empty_array_no_marker_ac24() {
        // AC24: tools:[] (empty array) → no markers, no panic, idempotent
        let body = br#"{"messages":[{"role":"user","content":"hi"}],"model":"m","tools":[]}"#;
        let out = align(body, Provider::Anthropic, "req-ac24-empty");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert!(
            !out_str.contains("cache_control"),
            "AC24: empty tools array must not get a marker"
        );
        assert_eq!(out.stats.skim_breakpoints_injected, 0);
        assert!(
            !out.stats.fail_open,
            "AC24: empty tools array is not an error"
        );
    }

    #[test]
    fn align_tools_non_array_fail_open_ac24() {
        // AC24: tools:5 (non-array scalar) → fail-open (sort_tools_array requires array input)
        let body = br#"{"messages":[],"model":"m","tools":5}"#;
        let out = align(body, Provider::Anthropic, "req-ac24-nonarray");
        assert_eq!(
            &out.bytes, body,
            "AC24: non-array tools must fail-open to byte-identical passthrough"
        );
        assert!(
            out.stats.fail_open,
            "AC24: non-array tools must set fail_open"
        );
    }

    #[test]
    fn align_system_absent_tools_present_one_marker_ac24() {
        // AC24: system key absent, tools present → 1 marker on last tool only
        let body = br#"{"messages":[],"model":"m","tools":[{"name":"x"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac24-nosys");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert_eq!(
            count_cache_control_occurrences(out_str),
            1,
            "AC24: tools without system must inject exactly 1 marker"
        );
    }

    #[test]
    fn align_system_array_empty_no_extra_markers_ac24() {
        // AC24: system:[] (empty block array) → no system marker, 1 tool marker if tools present.
        // An empty system array has no last block, so it is ineligible for injection.
        let body = br#"{"messages":[],"model":"m","system":[],"tools":[{"name":"x"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac24-emptysys");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        // No system marker (empty array → no last block); 1 tool marker
        assert_eq!(
            count_cache_control_occurrences(out_str),
            1,
            "AC24: empty system array yields 1 tool marker only"
        );
    }

    // ── AC29 — AD-CA-7 reorder gate fault-injection tests ────────────────────

    #[test]
    fn ac29_set_equal_gate_detects_element_drop() {
        // AC29 NEGATIVE (PF-007 discriminating): tools_arrays_set_equal returns false
        // when the canonical output is missing an element. This proves the gate in
        // try_align_full would fire if sort_tools_array ever dropped an element.
        use rskim_contract::canonical::tools_arrays_set_equal;
        let original = r#"[{"name":"a"},{"name":"b"}]"#;
        // Mutant: element "b" dropped
        let drop_mutant = r#"[{"name":"a"}]"#;
        assert!(
            !tools_arrays_set_equal(original, drop_mutant),
            "AC29: gate must detect element drop"
        );
    }

    #[test]
    fn ac29_set_equal_gate_detects_element_duplication() {
        // AC29 NEGATIVE (PF-007): gate must fire when an element is duplicated.
        use rskim_contract::canonical::tools_arrays_set_equal;
        let original = r#"[{"name":"a"},{"name":"b"}]"#;
        let dup_mutant = r#"[{"name":"a"},{"name":"a"}]"#;
        assert!(
            !tools_arrays_set_equal(original, dup_mutant),
            "AC29: gate must detect element duplication"
        );
    }

    #[test]
    fn ac29_set_equal_gate_detects_element_mutation() {
        // AC29 NEGATIVE (PF-007): gate must fire when a tool name is mutated.
        use rskim_contract::canonical::tools_arrays_set_equal;
        let original = r#"[{"name":"alpha"},{"name":"beta"}]"#;
        let mutation_mutant = r#"[{"name":"alpha"},{"name":"beta_MUTATED"}]"#;
        assert!(
            !tools_arrays_set_equal(original, mutation_mutant),
            "AC29: gate must detect element mutation"
        );
    }

    #[test]
    fn ac29_set_equal_gate_passes_correct_sort() {
        // AC29 POSITIVE companion: gate passes for a correctly sorted output.
        // Non-tautology: the sorted output is NOT byte-equal to the input (reorder happened).
        use crate::canonical_emit::ToolArrayKind;
        use crate::canonical_emit::sort_tools_array;
        use rskim_contract::canonical::tools_arrays_set_equal;
        let original = r#"[{"name":"beta","x":1},{"name":"alpha","x":2}]"#;
        let sorted = sort_tools_array(original, ToolArrayKind::AnthropicTools).unwrap();
        let sorted_str = std::str::from_utf8(&sorted).unwrap();
        assert!(
            tools_arrays_set_equal(original, sorted_str),
            "AC29: correct sort must pass the set-equal gate"
        );
        assert_ne!(
            original.as_bytes(),
            sorted.as_slice(),
            "AC29: sort must have actually reordered elements (non-tautology)"
        );
    }

    // ── AC8 / AC5 — placement is structural, not marker-dependent ─────────────

    /// AC8 (idempotence) + AC5 (placement stability) for a MULTI-block system.
    ///
    /// Regression: `system_position_eligible` used to scan backwards for the last text
    /// block *without* `cache_control`, so pass 1 marked block 1 and pass 2 marked
    /// block 0 as well — markers accumulated instead of converging, and the static-zone
    /// marker moved between turns.
    ///
    /// DISCRIMINATING (PF-007): reverting `last_injectable_text_block_index` to a
    /// "last block lacking cc" scan makes pass 2 emit a second marker and this fails.
    #[test]
    fn align_idempotent_multi_block_system_ac8() {
        let body = br#"{"messages":[],"model":"claude-3","system":[{"text":"one","type":"text"},{"text":"two","type":"text"}]}"#;
        let out1 = align(body, Provider::Anthropic, "req-ac8-multi-1");
        let out2 = align(&out1.bytes, Provider::Anthropic, "req-ac8-multi-2");
        assert_eq!(
            out1.bytes, out2.bytes,
            "AC8: multi-block system must be idempotent (no second marker on an earlier block)"
        );
        let out1_str = std::str::from_utf8(&out1.bytes).unwrap();
        assert_eq!(
            count_cache_control_occurrences(out1_str),
            1,
            "exactly one marker, on the LAST system text block"
        );
        assert_eq!(out2.stats.skim_breakpoints_injected, 0, "no re-injection");
    }

    /// AC5: a client marker on the LAST system block must not push skim's marker onto
    /// an earlier block — the position is structural, so the position is simply
    /// ineligible and skim injects nothing there.
    ///
    /// DISCRIMINATING (PF-007): with the old "last block lacking cc" scan, skim injected
    /// into block 0 here, moving the static-zone marker.
    #[test]
    fn align_client_marked_last_system_block_blocks_injection_ac5() {
        let body = br#"{"messages":[],"model":"claude-3","system":[{"text":"one","type":"text"},{"cache_control":{"type":"ephemeral"},"text":"two","type":"text"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac5-sys");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert_eq!(
            count_cache_control_occurrences(out_str),
            1,
            "the client's marker is the only one; skim must not mark an earlier block"
        );
        assert_eq!(out.stats.skim_breakpoints_injected, 0);
        assert_eq!(out.stats.client_breakpoint_count, 1);
    }

    // ── AC14 — client marker counting tolerates JSON whitespace ───────────────

    /// AC14: client markers written as `"cache_control" : {…}` (space before the colon —
    /// legal JSON) MUST be counted, or skim's budget is inflated and the total marker
    /// count can exceed `MAX_MARKERS`, which the provider rejects.
    ///
    /// DISCRIMINATING (PF-007): with the literal `"cache_control":` needle this body
    /// counted 0 client markers and skim injected 2, for a total of 6 > MAX_MARKERS.
    #[test]
    fn align_counts_whitespaced_client_markers_ac14() {
        let body = br#"{"messages":[{"content":[{"cache_control" : {"type":"ephemeral"},"text":"a","type":"text"},{"cache_control" : {"type":"ephemeral"},"text":"b","type":"text"},{"cache_control" : {"type":"ephemeral"},"text":"c","type":"text"},{"cache_control" : {"type":"ephemeral"},"text":"d","type":"text"}],"role":"user"}],"system":[{"text":"s","type":"text"}],"tools":[{"name":"a"}]}"#;
        let out = align(body, Provider::Anthropic, "req-ac14-ws");
        assert_eq!(
            out.stats.client_breakpoint_count, 4,
            "AC14: whitespace between the key and `:` must not hide a client marker"
        );
        assert_eq!(
            out.stats.skim_breakpoints_injected, 0,
            "AC14: budget is exhausted by 4 client markers — skim must inject none"
        );
    }

    // ── AC29 — reorder gate covers the OpenAI legacy `functions` array ────────

    /// AC29: the legacy `functions` array is element-reordered exactly like `tools`,
    /// so it must satisfy the same multiset gate.
    ///
    /// Non-tautology: the companion assert proves the reorder actually happened, so a
    /// passing set-equality check is not vacuous.
    #[test]
    fn align_openai_functions_reorder_is_set_equal_ac29() {
        let body = br#"{"functions":[{"name":"b","description":"B"},{"name":"a","description":"A"}],"messages":[],"model":"gpt-4o"}"#;
        let out = align(body, Provider::OpenAi, "req-ac29-fns");
        assert!(!out.stats.fail_open, "a well-formed body must align");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();

        let orig_spans = locate_top_level_spans(std::str::from_utf8(body).unwrap()).unwrap();
        let out_spans = locate_top_level_spans(out_str).unwrap();
        let before = orig_spans["functions"]
            .extract(std::str::from_utf8(body).unwrap())
            .unwrap();
        let after = out_spans["functions"].extract(out_str).unwrap();

        assert!(
            tools_arrays_set_equal(before, after),
            "AC29: the `functions` reorder must be multiset-equal to its input"
        );
        assert_ne!(
            before, after,
            "AC29 non-tautology: the `functions` array must actually have been reordered"
        );
        assert!(
            !out_str.contains("cache_control"),
            "AC15: no Anthropic marker on an OpenAI body"
        );
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
