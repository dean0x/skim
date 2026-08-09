//! KV-cache alignment for skim Layer 3.
//!
//! # Overview
//!
//! This crate provides deterministic canonical ordering of Anthropic and OpenAI
//! request bodies to maximise KV-cache hit rates across repeated or slightly-varied
//! requests. It operates entirely in-process, synchronously, with no I/O, no async,
//! and no ambient state (no wall-clock, no entropy). See DECISIONS-RESOLVED.md
//! Decision 6 and the plan at
//! `.devflow/docs/design/l3-wave3/2026-07-17_1916/306-kv-cache-alignment-plan.md`.
//!
//! # AD-CA-1 — Pure synchronous crate
//!
//! rskim-align is intentionally a pure sync crate: no tokio, no hyper, no TLS,
//! no RNG, no clock. This makes the transform deterministic (AC10) and composable
//! without an async runtime. The determinism gate is enforced at compile time by
//! the `clippy.toml` `disallowed-methods` entries in this crate.
//!
//! # Fail-open doctrine
//!
//! Every error condition (parse failure, duplicate key, depth exceeded, UTF-8
//! error) results in a passthrough — the original input bytes are returned
//! unmodified. A proxy that corrupts a request is worse than one that passes it
//! through unchanged.
//!
//! # Integration
//!
//! The `CacheAlignContract` type implements [`rskim_contract::Contract`] and
//! [`rskim_contract::waiver::MetadataReorderWithMarkers`]. Phase 4 will create
//! a `CacheAlignStage` in `rskim-proxy` that delegates to this contract.

pub mod canonical_emit;
pub mod span;

use crate::canonical_emit::canonical_envelope;
use crate::span::locate_top_level_spans;
use rskim_contract::contract::{Contract, Outcome};
use rskim_contract::waiver::MetadataReorderWithMarkers;
use rskim_llm::{ParsedBody, Provider};

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
/// canonical form; if any step failed, `bytes` is the original input
/// (fail-open). The caller cannot distinguish these cases from `AlignOutcome`
/// alone — use `CacheAlignContract::transform` for a decision-recorded outcome.
#[derive(Debug, Clone)]
#[must_use]
pub struct AlignOutcome {
    /// Output bytes. May equal the input bytes (passthrough on error or
    /// when the input is already canonical).
    pub bytes: Vec<u8>,
}

// ============================================================================
// Public: align()
// ============================================================================

/// Align a request body to its canonical form.
///
/// # AD-CA-3 — Provider-aware alignment
///
/// The `provider` argument controls which fields are canonicalized and how
/// tool names are extracted for element ordering. The caller (typically the
/// proxy seam) is responsible for provider detection; `align` does not
/// attempt re-detection.
///
/// # AD-CA-10 — Provider isolation
///
/// Each provider branch is entirely independent. A new provider variant
/// must add its own branch before any canonicalization code runs.
///
/// # Fail-open
///
/// Returns `AlignOutcome { bytes: body.to_vec() }` on any error:
/// - `body` is not valid UTF-8
/// - `body` is not a valid JSON object
/// - Duplicate top-level keys (AD-CA-11)
/// - Any value exceeds `MAX_ALIGN_SCHEMA_DEPTH` levels of nesting (AC26)
pub fn align(body: &[u8], provider: Provider, _request_id: &str) -> AlignOutcome {
    let canonical = try_align(body, provider);
    AlignOutcome {
        bytes: canonical.unwrap_or_else(|| body.to_vec()),
    }
}

/// Attempt canonical alignment; returns `None` on any error (fail-open).
fn try_align(body: &[u8], provider: Provider) -> Option<Vec<u8>> {
    let input_str = std::str::from_utf8(body).ok()?;
    let spans = locate_top_level_spans(input_str)?; // AD-CA-11: dup keys → None
    canonical_envelope(input_str, &spans, provider) // AD-CA-2/12/13
}

// ============================================================================
// Provider detection
// ============================================================================

/// Detect the request provider from raw body bytes.
///
/// # AD-CA-10 — Provider isolation
///
/// Uses `rskim_llm::parse` for structural heuristic detection. If detection
/// fails (non-JSON, unknown shape, or non-messages-array), returns `None` →
/// the caller must fail-open.
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
/// share across threads (where `Send + Sync` bounds permit).
///
/// # Integration note (Phase 4)
///
/// Phase 4 adds `CacheAlignStage` in `rskim-proxy` that wraps this contract.
/// The stage overrides `TransformStage::max_growth` to return `2 × MARKER_BYTES`
/// (from `rskim_contract::waiver::MARKER_BYTES`), covering the bounded growth
/// from Phase-3 cache_control marker injection (not yet implemented in Phase 2).
pub struct CacheAlignContract;

impl Contract for CacheAlignContract {
    fn component_name(&self) -> &'static str {
        "cache-align"
    }

    /// Transform a request body into its canonical form.
    ///
    /// Delegates to `apply_reorder` (from the `MetadataReorderWithMarkers`
    /// waiver) and validates the marker cap before accepting the modification.
    ///
    /// Returns passthrough if:
    /// - Provider detection fails (AD-CA-10)
    /// - Input is not valid UTF-8 or not a valid JSON object
    /// - Duplicate top-level keys (AD-CA-11)
    /// - Nesting exceeds `MAX_ALIGN_SCHEMA_DEPTH` (AC26)
    /// - Output would exceed the waiver marker cap (defensive; shouldn't happen
    ///   in Phase 2 since no markers are injected yet)
    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        match self.apply_reorder(input, request_id) {
            Some(candidate) => {
                // Defensive: verify the waiver cap even in Phase 2 (no markers yet).
                // In Phase 3, marker injection will push output up to input_len
                // + MAX_MARKERS × MARKER_BYTES = input_len + 148.
                if !self.verify_marker_cap(input.len(), candidate.len()) {
                    // Cap exceeded → fail-open
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
    /// Apply canonical ordering to the request body.
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
    /// # AC26 — Depth fail-open
    ///
    /// If any value exceeds `MAX_ALIGN_SCHEMA_DEPTH` nesting depth, returns
    /// `None` → passthrough.
    ///
    /// # Phase 3 note
    ///
    /// Phase 3 will extend this method to inject `cache_control` markers at
    /// the sanctioned structural positions (tool-definition objects and
    /// block-form system text blocks; never inside the `messages` span).
    /// In Phase 2, this method performs canonicalization only — no markers.
    fn apply_reorder(&self, input: &[u8], _request_id: &str) -> Option<Vec<u8>> {
        let provider = detect_provider(input)?; // AD-CA-10
        try_align(input, provider)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use rskim_contract::contract::Contract;
    use rskim_contract::waiver::MetadataReorderWithMarkers;

    // ── align() ──────────────────────────────────────────────────────────────

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
    }

    #[test]
    fn align_not_json_passthrough() {
        let body = b"not json at all";
        let out = align(body, Provider::Anthropic, "req-003");
        assert_eq!(&out.bytes, body);
    }

    #[test]
    fn align_duplicate_key_passthrough_ad_ca_11() {
        let body = br#"{"tools":[],"tools":[],"messages":[]}"#;
        let out = align(body, Provider::Anthropic, "req-004");
        // AD-CA-11: duplicate key → passthrough
        assert_eq!(&out.bytes, body);
    }

    #[test]
    fn align_number_token_verbatim_ac12() {
        let body = br#"{"messages":[],"count":1e3}"#;
        let out = align(body, Provider::Anthropic, "req-005");
        let out_str = std::str::from_utf8(&out.bytes).unwrap();
        assert!(out_str.contains("1e3"), "number token 1e3 must not be reformatted");
    }

    // ── CacheAlignContract::transform() ──────────────────────────────────────

    #[test]
    fn contract_transform_anthropic_modifies() {
        let contract = CacheAlignContract;
        // Valid Anthropic body with keys in non-canonical order
        let input = br#"{"model":"claude-3-haiku","messages":[],"max_tokens":10}"#;
        let outcome = contract.transform(input, "req-100");
        // Should be modified (keys reordered to canonical)
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
        // Canonical order: max_tokens < messages < model (lexicographic)
        let canonical = br#"{"max_tokens":10,"messages":[],"model":"claude-3-haiku"}"#;
        let outcome = contract.transform(canonical, "req-102");
        // Output must be byte-identical to input (already canonical)
        assert_eq!(outcome.bytes.as_slice(), canonical.as_slice());
    }

    // ── MetadataReorderWithMarkers ────────────────────────────────────────────

    #[test]
    fn apply_reorder_detects_anthropic() {
        let contract = CacheAlignContract;
        // Anthropic body (has "messages" array and no "choices")
        let input = br#"{"model":"claude-3","messages":[{"role":"user","content":"hi"}]}"#;
        // Should succeed (detected as Anthropic)
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
    fn verify_marker_cap_passes_for_zero_growth() {
        let contract = CacheAlignContract;
        let input_len = 1000;
        // canonical form with no markers: no growth, cap easily satisfied
        assert!(contract.verify_marker_cap(input_len, input_len));
        // even a few hundred bytes of growth: still within cap (4 × 37 = 148)
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
}
