//! Transform-seam contract: [`TransformStage`] trait, [`TransformContext`],
//! identity stage, and [`TransformPipeline`].
//!
//! ## D1 — TransformStage is the canonical seam
//!
//! The seam is `TransformStage` (per-request `ctx` + `sink` are explicit call
//! params), composing #301's `Outcome`/`guarded_transform`. #304's `BlockRouter`
//! implements `TransformStage`; a thin `impl Contract for <adapter>` bridges
//! a stage to the #301 conformance harness (AC19a).
//!
//! ## AD-PXY-05 — Reuse #301 Outcome, not a parallel Result<Option<bytes>>
//!
//! The plan's "contract sketch" used `Result<Option<bytes>>`. The #301 crate
//! ships the canonical L3 transform contract as `Outcome` with no error variant —
//! fail-open is encoded as `Outcome::passthrough` (a success variant). Re-deriving
//! a parallel gate would duplicate #301 and risk drift. This seam reuses `Outcome`
//! and `guarded_transform` directly.
//!
//! ## AD-PXY-06 — Canonical pipeline stage order (fixed here)
//!
//! The ordering that downstream tickets MUST honour:
//!
//! ```text
//! #307 (stale-compaction) → #304 (content) → #306 (cache-alignment) LAST
//! ```
//!
//! Cache-alignment (#306) MUST be the final stage so the bytes actually forwarded
//! are cache-aligned. This ticket ships only the `IdentityStage` placeholder;
//! successors declare their slot against this canonical order.
//!
//! ## AD-PXY-07 — Per-stage gate only; #303 does NOT call `whole_request_check`
//!
//! Each stage routes modifications through `guarded_transform` (the per-stage
//! never-inflate + sink rule). Calling `whole_request_check` under an identity
//! pipeline is a PF-007 tautology (`out_len == in_len` always). #304 owns the
//! post-assembly `whole_request_check` call; #307 owns the zone-assembly path.
//!
//! ## AD-PXY-09 — `turn_id` reserved
//!
//! `turn_id` is intentionally absent from [`TransformContext`]. The derivation
//! spec is tracked in #344 (filed per ADR-004; see DECISIONS-NEEDED.md). It will
//! be added to `TransformContext` by #305 before turn-level tests land.

use rskim_contract::contract::{Contract, Outcome};
use rskim_contract::log::DecisionSink;

use crate::authmode::AuthMode;
use crate::detect::ProxyProvider;

// ============================================================================
// HeaderView — read-only header accessor (no value logging)
// ============================================================================

/// Read-only view over the request headers.
///
/// Provides iteration over header name-value pairs. Values MUST NOT be logged
/// (AC13 / AD-PXY-08). The view carries a lifetime tied to the request lifetime
/// so no allocation is needed for header access.
pub struct HeaderView<'a> {
    headers: &'a [(String, String)],
}

impl<'a> HeaderView<'a> {
    /// Construct a `HeaderView` from a slice of name-value pairs.
    pub fn new(headers: &'a [(String, String)]) -> Self {
        Self { headers }
    }

    /// Iterate over header name-value pairs.
    ///
    /// Names are lowercase-normalised. Values MUST NOT be logged (AC13).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.headers.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Check whether a header name is present (case-insensitive).
    pub fn contains(&self, name: &str) -> bool {
        self.headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case(name))
    }
}

// ============================================================================
// TransformContext
// ============================================================================

/// Read-only per-request context handed to every transform stage.
///
/// `#[non_exhaustive]` so successors (#305 usage extraction, etc.) can add
/// fields without breaking existing stage implementations (AC24 / D1).
///
/// ## Auth material is NEVER in this context
///
/// `headers` is a read-only view. Auth header VALUES must never be read for
/// logging, stored in decision records, or exposed to stages. The redaction
/// contract is enforced in `logging.rs` using `rskim_contract::log::is_sensitive_value`.
///
/// ## AD-PXY-09 — turn_id is intentionally absent
///
/// `turn_id` derivation spec is tracked in #344. It will be added here by #305.
#[non_exhaustive]
pub struct TransformContext<'a> {
    /// Provider classified by the self-contained detection pipeline.
    ///
    /// `ProxyProvider::Unknown` means the transform seam is bypassed entirely
    /// (the pipeline's `run()` method returns a passthrough without calling any
    /// stage). See [`TransformPipeline::run`].
    pub provider: ProxyProvider,

    /// Header-shape auth classification.
    ///
    /// Shape-only: whether `x-api-key` or `Authorization: Bearer` is present.
    /// #304 selects `Policy` per call from `ctx.auth_mode` (D1 / AD-PXY-08).
    /// Conservative map: `Ambiguous → ApiKey (Policy::Default)`.
    pub auth_mode: AuthMode,

    /// Caller-assigned request identifier (opaque correlator).
    ///
    /// Sanitized via `rskim_contract::log::sanitize_request_id` before being
    /// placed here. MUST NOT be derived from any request header that could carry
    /// auth material (x-api-key echo proxy anti-pattern; #301 AC12 guard).
    pub request_id: &'a str,

    /// Read-only view over the request headers.
    ///
    /// Values MUST NOT be logged (AC13 / invariant 7). Exposed so stages can
    /// inspect custom headers (e.g., `anthropic-version`) without copying bytes.
    pub headers: &'a HeaderView<'a>,
}

impl<'a> TransformContext<'a> {
    /// Construct a [`TransformContext`] from its required fields.
    ///
    /// This constructor exists so external crates (including integration test crates
    /// in `tests/`) can build a context without relying on struct literal syntax,
    /// which is forbidden for `#[non_exhaustive]` structs outside the defining crate.
    ///
    /// # AD-PXY-09
    ///
    /// `turn_id` is intentionally absent (spec in #344). The constructor signature
    /// will be extended non-breakingly when #305 adds `turn_id`.
    pub fn new(
        provider: ProxyProvider,
        auth_mode: AuthMode,
        request_id: &'a str,
        headers: &'a HeaderView<'a>,
    ) -> Self {
        Self {
            provider,
            auth_mode,
            request_id,
            headers,
        }
    }
}

// ============================================================================
// TransformStage trait
// ============================================================================

/// A single ordered transform stage in the proxy pipeline.
///
/// The identity stage is the only implementation this ticket ships. Successors
/// implement this trait; the pipeline composes them in the canonical order fixed
/// by `AD-PXY-06` (see module doc).
///
/// ## Fail-open contract
///
/// `apply` returns [`Outcome`] — no error variant. Any error condition (parse
/// failure, logic error, sink-full) MUST resolve to `Outcome::passthrough`.
/// A stage that panics is caught at the per-transform call site by the server
/// layer (`catch_unwind` — AC9 / AD-PXY-12), not here.
///
/// ## AD-PXY-05
///
/// `Outcome` is reused from #301 (not a parallel `Result<Option<bytes>>`). The
/// identity stage returns `Outcome::passthrough(body.to_vec(), ctx.request_id,
/// "identity")`. A modifying successor SHOULD call `guarded_transform(…)` which
/// already runs the never-inflate byte gate + sink rule (invariant 2 + 8).
pub trait TransformStage: Send + Sync {
    /// Human-readable name used in decision log records.
    fn name(&self) -> &'static str;

    /// Apply this stage to the request body.
    ///
    /// # Arguments
    ///
    /// - `body` — the buffered request body bytes (bounded by
    ///   [`crate::config::DEFAULT_MAX_BODY_BYTES`]; oversize bodies were already
    ///   routed around the pipeline as passthrough by the caller).
    /// - `ctx` — read-only per-request context.
    /// - `sink` — decision record sink. If `try_send` returns `SinkFull`, the
    ///   stage MUST emit passthrough (invariant 8 via `guarded_transform`).
    ///
    /// # Returns
    ///
    /// Always returns `Outcome` (no error variant). Passthrough when the stage
    /// cannot or should not modify the body.
    fn apply(&self, body: &[u8], ctx: &TransformContext<'_>, sink: &dyn DecisionSink) -> Outcome;
}

// ============================================================================
// IdentityStage — the only stage this ticket ships
// ============================================================================

/// Identity transform stage: returns every body byte-identical.
///
/// This is the only stage shipped by #303. It is a correctly-wired passthrough:
/// it calls `Outcome::passthrough` (the #301 fail-open success variant) which
/// sets `bytes == input` and produces a `DecisionRecord::passthrough` record.
///
/// The identity stage is the default pipeline; #304 injects `BlockRouter` via
/// `serve(config, stage)` without a breaking API change (D1 / AD-PXY-06).
///
/// ## AC19a — Conformance harness adapter
///
/// `IdentityStage` implements [`Contract`] via [`IdentityStageContractAdapter`]
/// so it can be driven through `run_conformance_suite`. The adapter is the test
/// seam — not the pre-existing `rskim_contract::contract::IdentityContract` (which
/// would be a tautology re-asserting #301's test).
pub struct IdentityStage;

impl TransformStage for IdentityStage {
    fn name(&self) -> &'static str {
        "identity"
    }

    fn apply(&self, body: &[u8], ctx: &TransformContext<'_>, _sink: &dyn DecisionSink) -> Outcome {
        // AD-PXY-05: passthrough is the correct fail-open success variant.
        // body.to_vec() is a necessary allocation (the owned buffer is the Outcome).
        Outcome::passthrough(body.to_vec(), ctx.request_id, self.name())
    }
}

// ============================================================================
// Contract adapter for AC19a conformance harness
// ============================================================================

/// Adapter wrapping [`IdentityStage`] to implement [`Contract`] for the #301
/// conformance harness (AC19a).
///
/// The harness calls `transform(&[u8], request_id)` — the full `TransformContext`
/// is not available in that interface. This adapter constructs a minimal context
/// with dummy values and delegates to `IdentityStage::apply`. The `Contract::transform`
/// result is the fail-open byte-identity property the harness verifies.
///
/// ## Non-tautology requirement (AC19a)
///
/// This adapter exercises `IdentityStage::apply` (the type the proxy actually
/// forwards through), NOT the pre-existing `rskim_contract::contract::IdentityContract`.
/// Replacing `IdentityStage` with a mutating stage and running `run_conformance_suite`
/// against this adapter MUST fail — proving the harness tests `IdentityStage`'s
/// actual behavior.
pub struct IdentityStageContractAdapter;

impl Contract for IdentityStageContractAdapter {
    fn component_name(&self) -> &'static str {
        "proxy-identity-adapter"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Construct a minimal TransformContext sufficient for the identity stage.
        // The identity stage ignores all context fields except `request_id`.
        // Use an empty static slice — no allocation needed, no header access in the identity path.
        let header_view = HeaderView::new(&[]);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::Ambiguous,
            request_id,
            headers: &header_view,
        };

        // Use the null sink for the harness adapter — no decision logging in
        // the conformance test path (harness has its own assertion layer).
        use rskim_contract::log::MockSink;
        let sink = MockSink::new();
        IdentityStage.apply(input, &ctx, &sink)
    }
}

// ============================================================================
// TransformPipeline
// ============================================================================

/// Ordered transform pipeline with the canonical stage order fixed by AD-PXY-06.
///
/// Stages run in declaration order. The canonical order is:
/// ```text
/// #307 (stale-compaction) → #304 (content) → #306 (cache-alignment) LAST
/// ```
///
/// This ticket ships only `IdentityStage`. Successors add their stage at the
/// declared slot position using [`TransformPipeline::identity()`] as the baseline
/// and [`TransformPipeline::with_stage`] to insert.
pub struct TransformPipeline {
    stages: Vec<Box<dyn TransformStage>>,
}

impl TransformPipeline {
    /// Construct the identity pipeline (single `IdentityStage`).
    ///
    /// This is the default pipeline shipped by #303. #304 replaces the identity
    /// stage by injecting `BlockRouter` at the construction point in `serve()`.
    pub fn identity() -> Self {
        Self {
            stages: vec![Box::new(IdentityStage)],
        }
    }

    /// Construct a pipeline from an arbitrary ordered set of stages.
    ///
    /// Caller is responsible for maintaining the canonical order (AD-PXY-06):
    /// `#307 → #304 → #306`. Used by #304 to inject `BlockRouter` in place of
    /// `IdentityStage`.
    pub fn from_stages(stages: Vec<Box<dyn TransformStage>>) -> Self {
        Self { stages }
    }

    /// Run all stages in order on the given body.
    ///
    /// ## AD-PXY-07 — Per-stage gate only (no whole_request_check here)
    ///
    /// Each modifying stage routes through `guarded_transform` internally (the
    /// per-stage never-inflate gate + sink rule). This method does NOT call
    /// `whole_request_check` on the composed output — #304 owns that post-assembly
    /// call (D3). Calling it here under an identity pipeline would be a PF-007
    /// tautology (`out_len == in_len` always for the identity stage).
    ///
    /// ## Unknown provider bypass
    ///
    /// When `ctx.provider` is `ProxyProvider::Unknown`, the pipeline is bypassed
    /// entirely and returns `Outcome::passthrough`. Forwarding to the default
    /// upstream (or 502 if none configured) is the caller's responsibility
    /// (D8 / AC3 / AD-PXY-02).
    pub fn run(
        &self,
        body: Vec<u8>,
        ctx: &TransformContext<'_>,
        sink: &dyn DecisionSink,
    ) -> Outcome {
        // AD-PXY-02: Unknown provider → bypass transform seam entirely.
        // The seam is skipped; the forward layer routes to default upstream or 502.
        if ctx.provider == ProxyProvider::Unknown {
            return Outcome::passthrough(body, ctx.request_id, "pipeline-unknown-bypass");
        }

        // Run stages in order. Each stage receives the output of the previous stage.
        // The first stage receives the original body; subsequent stages receive
        // the (possibly modified) output of the previous stage.
        let mut current = body;
        for stage in &self.stages {
            let outcome = stage.apply(&current, ctx, sink);
            // Always take the outcome bytes as the input to the next stage.
            // If a stage fails open (passthrough), current bytes are unchanged.
            current = outcome.bytes;
        }

        // The pipeline output is the final `current` bytes. Wrap in a passthrough
        // outcome with the pipeline-level component name. If any stage modified the
        // body, the modification is already recorded in the sink by that stage.
        // This wrapper outcome carries no new decision record — it is for the
        // pipeline-level result only.
        //
        // AD-PXY-07: no whole_request_check call here. #304 owns that.
        Outcome::passthrough(current, ctx.request_id, "pipeline")
    }

    /// Returns the number of stages in the pipeline.
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }
}

// ============================================================================
// Tests (AC4, AC19a)
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use rskim_contract::log::MockSink;

    // AC4 (POSITIVE): identity stage returns byte-identical output.
    #[test]
    fn test_identity_stage_byte_identical() {
        let body = b"hello world";
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::ApiKey,
            request_id: "req-001",
            headers: &hv,
        };
        let sink = MockSink::new();
        let outcome = IdentityStage.apply(body, &ctx, &sink);
        assert_eq!(
            outcome.bytes.as_slice(),
            body,
            "identity stage must return byte-identical output"
        );
        assert!(
            outcome.is_passthrough(),
            "identity stage must produce a passthrough outcome"
        );
    }

    // AC4 (POSITIVE): pipeline with identity stage is byte-identical.
    #[test]
    fn test_pipeline_identity_byte_identical() {
        let body = b"arbitrary request body bytes".to_vec();
        let original = body.clone();
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::ApiKey,
            request_id: "req-002",
            headers: &hv,
        };
        let sink = MockSink::new();
        let pipeline = TransformPipeline::identity();
        let outcome = pipeline.run(body, &ctx, &sink);
        assert_eq!(
            outcome.bytes.as_slice(),
            original.as_slice(),
            "identity pipeline must produce byte-identical output"
        );
    }

    // AC4 / AD-PXY-02 (NEGATIVE): Unknown provider bypasses the pipeline.
    // DISCRIMINATING: deleting the Unknown bypass would cause stages to run,
    // proving this test actually guards the bypass.
    #[test]
    fn test_pipeline_unknown_provider_bypasses_seam() {
        // Stage that "modifies" (appends) output — only used for the discriminating test.
        // In production, stages go through guarded_transform; here we just want to
        // prove the bypass fires before the stage is called.
        struct AppendStage;
        impl TransformStage for AppendStage {
            fn name(&self) -> &'static str {
                "test-append"
            }
            fn apply(
                &self,
                body: &[u8],
                ctx: &TransformContext<'_>,
                _sink: &dyn DecisionSink,
            ) -> Outcome {
                let mut out = body.to_vec();
                out.extend_from_slice(b"MODIFIED");
                Outcome::passthrough(out, ctx.request_id, self.name())
            }
        }

        let body = b"original body".to_vec();
        let original = body.clone();
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Unknown, // <-- Unknown bypasses seam
            auth_mode: AuthMode::Ambiguous,
            request_id: "req-unknown",
            headers: &hv,
        };
        let sink = MockSink::new();
        let pipeline = TransformPipeline::from_stages(vec![Box::new(AppendStage)]);
        let outcome = pipeline.run(body, &ctx, &sink);
        assert_eq!(
            outcome.bytes.as_slice(),
            original.as_slice(),
            "Unknown provider must bypass the pipeline: output must equal original body"
        );
    }

    // AC19a: IdentityStageContractAdapter implements Contract correctly.
    // Verified indirectly: the adapter must return byte-identical output.
    #[test]
    fn test_identity_stage_contract_adapter_byte_identical() {
        let adapter = IdentityStageContractAdapter;
        let input = b"test body bytes for contract adapter";
        let outcome = adapter.transform(input, "req-adapter-001");
        assert_eq!(
            outcome.bytes.as_slice(),
            input,
            "contract adapter must return byte-identical output"
        );
        assert!(
            outcome.is_passthrough(),
            "contract adapter must return passthrough outcome"
        );
    }

    // HeaderView: contains() is case-insensitive.
    #[test]
    fn test_header_view_contains_case_insensitive() {
        let headers = vec![("X-Api-Key".to_string(), "sk-test".to_string())];
        let hv = HeaderView::new(&headers);
        assert!(hv.contains("x-api-key"), "must match lowercase");
        assert!(hv.contains("X-API-KEY"), "must match uppercase");
        assert!(
            !hv.contains("authorization"),
            "absent header must return false"
        );
    }
}
