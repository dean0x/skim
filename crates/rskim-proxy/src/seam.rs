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
    /// Callers are responsible for passing a provably safe ID (e.g. a monotonic
    /// counter-derived string like `"px-{n}"`) that is never derived from any
    /// request header that could carry auth material (x-api-key echo proxy
    /// anti-pattern; #301 AC12 guard). [`rskim_contract::log::DecisionRecord`]
    /// constructors re-sanitize the ID defensively via `sanitize_request_id`.
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

    /// Maximum number of bytes by which this stage's output is permitted to exceed
    /// its input.
    ///
    /// # AD-CA-5 / AD-PXY-21 — Per-stage growth allowance for `#306`
    ///
    /// The default is `0` (never-inflate: output MUST be ≤ input bytes). This keeps
    /// `BlockRouterStage` (`#304`) and `#307` byte-identical after the `#306` seam
    /// extension — they never override this method and therefore always pass the strict
    /// zero-growth gate.
    ///
    /// `CacheAlignStage` (`#306`) overrides this to return `2 × MARKER_BYTES` (74),
    /// reflecting the maximum of two `cache_control` marker injections it may perform.
    /// The seam passes this value to `guarded_transform_with_growth` so the gate
    /// accepts the growth while the sink-failure rule (invariant 8) and the `Modified`
    /// `DecisionRecord` are preserved via the single-home implementation.
    ///
    /// # Arguments
    ///
    /// - `_input_len` — byte length of the stage's input (the pre-stage body). Provided
    ///   so growth can be expressed as a function of input size if needed; the default
    ///   ignores it (constant zero).
    ///
    /// # Non-breaking default
    ///
    /// Adding this method to the trait is backward-compatible: all existing `TransformStage`
    /// implementors receive the zero-growth default and are unaffected.
    fn max_growth(&self, _input_len: usize) -> usize {
        0
    }
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
/// [`crate::serve_with_stage`]`(config, pipeline, analytics)` without a breaking
/// API change (D1 / AD-PXY-06).
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
/// and [`TransformPipeline::from_stages`] to compose the stage list.
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
    /// ## AD-PXY-07 — Per-stage gate structurally enforced here
    ///
    /// After each stage's `apply()` returns an `Outcome`, `run()` calls
    /// `guarded_transform` on the stage output to enforce the never-inflate
    /// invariant (design constraint 2 / lib.rs). This makes the invariant a
    /// property of the SEAM, not of each individual stage's discipline. A stage
    /// that returns inflated bytes (without calling `guarded_transform` internally)
    /// will be caught here and fail-open to the pre-stage bytes.
    ///
    /// This does NOT call `whole_request_check` — that post-assembly call is #304's
    /// responsibility (D3). The per-stage gate is the structural lock; whole_request_check
    /// is the cross-stage assembly guard.
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
        use rskim_contract::guardrail::guarded_transform_with_growth;

        // AD-PXY-02: Unknown provider → bypass transform seam entirely.
        // The seam is skipped; the forward layer routes to default upstream or 502.
        if ctx.provider == ProxyProvider::Unknown {
            return Outcome::passthrough(body, ctx.request_id, "pipeline-unknown-bypass");
        }

        // Run stages in order. Each stage receives the output of the previous stage.
        // The first stage receives the original body; subsequent stages receive
        // the (possibly modified) output of the previous stage.
        //
        // ## AC9 / AD-PXY-12 — per-stage panic containment
        //
        // Each `stage.apply()` call is wrapped in `catch_unwind` so a panicking
        // stage falls back to byte-identical passthrough of the CURRENT bytes
        // (i.e. the output of the previous stage). This is the "per-transform-call"
        // boundary described in AD-PXY-12.
        //
        // ## AC4 — per-stage never-inflate gate (structural enforcement)
        //
        // After each stage, `guarded_transform` enforces that the stage output
        // is no larger than the stage input. A stage that bypasses `guarded_transform`
        // internally will still be caught here. This makes the never-inflate invariant
        // a property of the seam — not of each stage's voluntary compliance.
        let mut current = body;
        for stage in &self.stages {
            // Clone before apply so we have pre-stage bytes for:
            // (a) the catch_unwind fail-open path, and
            // (b) the guarded_transform input (required by its signature).
            let pre_stage = current.clone();

            // AC9 / AD-PXY-12 — per-stage panic containment.
            // AssertUnwindSafe: stages are stateless per-request (AC11 / AD-PXY-06).
            let stage_ref: &dyn TransformStage = stage.as_ref();
            let apply_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stage_ref.apply(&pre_stage, ctx, sink)
            }));

            let (stage_output, panicked) = match apply_result {
                Ok(outcome) => (outcome.bytes, false),
                Err(_panic) => {
                    // Stage panicked — fail-open to pre-stage bytes (AC9).
                    // Do not log here (tracing may be the panic source).
                    (pre_stage.clone(), true)
                }
            };

            // AC4 — structural never-inflate gate.
            //
            // Only call `guarded_transform` when the stage ACTUALLY changed bytes.
            // For byte-identical outputs (identity stage, panic fail-open), we move
            // `stage_output` forward directly. This preserves the passthrough
            // invariant (a passthrough outcome is never mislabelled as modified) and
            // avoids per-request allocation of a DecisionRecord + channel try_send for
            // every identity request. guarded_transform is reserved for genuinely-
            // modifying stages where the never-inflate gate is load-bearing.
            if panicked || stage_output == pre_stage {
                // Stage returned byte-identical output (or panicked to original).
                // Move bytes forward without recording a spurious Modified record.
                current = stage_output;
            } else {
                // Stage proposed a modification — run through the gate.
                //
                // AD-CA-5 / AD-PXY-21 — growth-aware gate (single call site).
                //
                // `guarded_transform_with_growth` enforces `candidate_len ≤ pre_stage.len()
                // + stage.max_growth(pre_stage.len())`, records a Modified DecisionRecord,
                // and falls back to passthrough if the gate rejects (or sink is full).
                //
                // For all stages that do not override `max_growth` (including #304
                // BlockRouterStage and #307), `max_growth` returns 0 — byte-identical
                // behaviour to the former `guarded_transform` call (AC21 regression).
                //
                // For #306 CacheAlignStage, `max_growth` returns `2 × MARKER_BYTES`
                // so the gate accepts the bounded growth while preserving invariant 8.
                let growth = stage.max_growth(pre_stage.len());
                let gated = guarded_transform_with_growth(
                    pre_stage,
                    stage_output,
                    growth,
                    ctx.request_id,
                    stage.name(),
                    sink,
                );
                current = gated.bytes;
            }
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

    // ========================================================================
    // AC21 — max_growth regression: seam preserves #304/#307 byte-identity
    //        and enforces the per-stage growth cap (AD-CA-5 / AD-PXY-21).
    // ========================================================================

    /// AC21 (NEGATIVE, seam regression): a modifying stage with default max_growth=0
    /// that returns shrunk output must pass the gate — byte-identical to before #306.
    ///
    /// Discriminating: if the seam accidentally used a non-zero growth default,
    /// a stage output that should fail could incorrectly pass.
    #[test]
    fn test_seam_default_max_growth_zero_pass() {
        // A stage that shrinks its input (always passes the strict zero-growth gate).
        struct ShrinkStage;
        impl TransformStage for ShrinkStage {
            fn name(&self) -> &'static str {
                "shrink"
            }
            fn apply(
                &self,
                body: &[u8],
                ctx: &TransformContext<'_>,
                _sink: &dyn DecisionSink,
            ) -> Outcome {
                // Return half the body (a shrinking modification).
                let half = &body[..body.len() / 2];
                Outcome::modified(half.to_vec(), body.len(), ctx.request_id, self.name())
            }
        }

        let body = b"hello world extended".to_vec(); // 20 bytes
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::ApiKey,
            request_id: "ac21-a",
            headers: &hv,
        };
        let sink = MockSink::new();
        let pipeline = TransformPipeline::from_stages(vec![Box::new(ShrinkStage)]);
        let outcome = pipeline.run(body, &ctx, &sink);
        // Half of 20 bytes = 10 bytes — well within zero-growth gate.
        assert_eq!(outcome.bytes.len(), 10, "shrink must pass the zero-growth gate");
    }

    /// AC21 (NEGATIVE, seam regression): a stage with default max_growth=0 that
    /// attempts to inflate is rejected to passthrough — the gate enforces zero-growth.
    ///
    /// Discriminating: if `guarded_transform_with_growth` is called with max_growth=0
    /// and the candidate exceeds input length, the outcome MUST be the input (passthrough).
    /// Deleting the gate call would let the inflated output through.
    #[test]
    fn test_seam_default_max_growth_zero_reject_inflate() {
        // A stage that tries to inflate its output.
        struct InflateStage;
        impl TransformStage for InflateStage {
            fn name(&self) -> &'static str {
                "inflate"
            }
            fn apply(
                &self,
                body: &[u8],
                ctx: &TransformContext<'_>,
                _sink: &dyn DecisionSink,
            ) -> Outcome {
                let mut out = body.to_vec();
                out.extend_from_slice(b"EXTRA_BYTES");
                Outcome::modified(out, body.len(), ctx.request_id, self.name())
            }
        }

        let body = b"original".to_vec(); // 8 bytes
        let original = body.clone();
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::ApiKey,
            request_id: "ac21-b",
            headers: &hv,
        };
        let sink = MockSink::new();
        let pipeline = TransformPipeline::from_stages(vec![Box::new(InflateStage)]);
        let outcome = pipeline.run(body, &ctx, &sink);
        // Gate must reject the inflation and fall back to the pre-stage input.
        assert_eq!(
            outcome.bytes, original,
            "inflate stage with max_growth=0 must be rejected to passthrough"
        );
    }

    /// AC21 (POSITIVE): a waivered stage with max_growth > 0 can inflate up to the allowance.
    ///
    /// Discriminating: if `max_growth` is ignored (always zero), a waivered stage's
    /// growth would be incorrectly rejected and forwarded as the pre-stage bytes.
    #[test]
    fn test_seam_waivered_stage_growth_accepted() {
        const GROWTH: usize = 37; // One MARKER_BYTES worth of growth

        // A stage that inflates by exactly GROWTH bytes and declares that allowance.
        struct WaivedGrowthStage;
        impl TransformStage for WaivedGrowthStage {
            fn name(&self) -> &'static str {
                "waived-grow"
            }
            fn apply(
                &self,
                body: &[u8],
                ctx: &TransformContext<'_>,
                _sink: &dyn DecisionSink,
            ) -> Outcome {
                let mut out = body.to_vec();
                // Append exactly GROWTH bytes to simulate marker injection.
                out.extend(std::iter::repeat(b'M').take(GROWTH));
                Outcome::modified(out, body.len(), ctx.request_id, self.name())
            }
            fn max_growth(&self, _input_len: usize) -> usize {
                GROWTH
            }
        }

        let body = b"input body bytes".to_vec(); // 16 bytes
        let expected_len = body.len() + GROWTH;
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::ApiKey,
            request_id: "ac21-c",
            headers: &hv,
        };
        let sink = MockSink::new();
        let pipeline = TransformPipeline::from_stages(vec![Box::new(WaivedGrowthStage)]);
        let outcome = pipeline.run(body, &ctx, &sink);
        assert_eq!(
            outcome.bytes.len(),
            expected_len,
            "waivered stage growth within allowance must pass the gate"
        );
    }

    /// AC21 (NEGATIVE, invariant 8): a waivered stage with SinkFull falls back to
    /// passthrough — the marker is NOT injected even with growth allowance.
    ///
    /// Discriminating: if the SinkFull branch of `guarded_transform_with_growth` is
    /// removed, the inflated output would be forwarded without a decision record,
    /// violating invariant 8.
    #[test]
    fn test_seam_waivered_stage_sink_full_falls_back() {
        const GROWTH: usize = 37;

        struct WaivedGrowthStage;
        impl TransformStage for WaivedGrowthStage {
            fn name(&self) -> &'static str {
                "waived-grow-sink-full"
            }
            fn apply(
                &self,
                body: &[u8],
                ctx: &TransformContext<'_>,
                _sink: &dyn DecisionSink,
            ) -> Outcome {
                let mut out = body.to_vec();
                out.extend(std::iter::repeat(b'M').take(GROWTH));
                Outcome::modified(out, body.len(), ctx.request_id, self.name())
            }
            fn max_growth(&self, _input_len: usize) -> usize {
                GROWTH
            }
        }

        let body = b"input body bytes".to_vec();
        let original = body.clone();
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::ApiKey,
            request_id: "ac21-d",
            headers: &hv,
        };
        // Set sink to full — any try_send returns SinkFull.
        let sink = MockSink::new();
        sink.set_full(true);
        let pipeline = TransformPipeline::from_stages(vec![Box::new(WaivedGrowthStage)]);
        let outcome = pipeline.run(body, &ctx, &sink);
        // SinkFull → passthrough (marker NOT injected, invariant 8 preserved).
        assert_eq!(
            outcome.bytes, original,
            "SinkFull must cause passthrough even with growth allowance (invariant 8)"
        );
    }

    /// AC21 (NEGATIVE): a waivered stage that inflates BEYOND its max_growth is
    /// rejected to passthrough.
    ///
    /// Discriminating: if the gate used a blanket allowance rather than the per-stage
    /// max_growth value, an over-inflating stage's output would be incorrectly accepted.
    #[test]
    fn test_seam_waivered_stage_over_cap_rejected() {
        const GROWTH: usize = 10; // stage declares only 10 bytes of growth

        struct OverCapStage;
        impl TransformStage for OverCapStage {
            fn name(&self) -> &'static str {
                "over-cap"
            }
            fn apply(
                &self,
                body: &[u8],
                ctx: &TransformContext<'_>,
                _sink: &dyn DecisionSink,
            ) -> Outcome {
                let mut out = body.to_vec();
                // Inflate by 50 bytes — well beyond the declared growth of 10.
                out.extend(std::iter::repeat(b'X').take(50));
                Outcome::modified(out, body.len(), ctx.request_id, self.name())
            }
            fn max_growth(&self, _input_len: usize) -> usize {
                GROWTH
            }
        }

        let body = b"input".to_vec();
        let original = body.clone();
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext {
            provider: ProxyProvider::Anthropic,
            auth_mode: AuthMode::ApiKey,
            request_id: "ac21-e",
            headers: &hv,
        };
        let sink = MockSink::new();
        let pipeline = TransformPipeline::from_stages(vec![Box::new(OverCapStage)]);
        let outcome = pipeline.run(body, &ctx, &sink);
        // Over-cap → rejected to passthrough.
        assert_eq!(
            outcome.bytes, original,
            "over-cap stage output must be rejected to passthrough"
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
