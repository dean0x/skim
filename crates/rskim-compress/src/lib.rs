//! # rskim-compress — per-content-type block compression router (#304)
//!
//! This crate is the L3 block-compression engine for skim's Layer-3 LLM request
//! proxy. It hosts the [`BlockRouter`] and the promoted `compress_log` function.
//!
//! ## Deviation from §2 of 304-plan.md (R1 / AC26)
//!
//! The plan §2 originally said "move `compress_log` into rskim-core." That
//! VIOLATES AC26 (`rskim-core` MUST NOT gain `regex` — it is the pure transform
//! lib with zero regex refs today; verified at rskim-core/Cargo.toml). Instead,
//! `compress_log` + `LogFlags` + `LogResult` + `ParseResult` are hosted HERE in
//! `rskim-compress` where `regex` is an allowed dependency. The `rskim` binary's
//! `cmd/log.rs` handler is re-pointed to call `rskim_compress::log::compress_log`
//! (no behavior change; AC25 regression-free). This deviation is documented
//! citing AC26 + #327 (log-rule library extraction ticket).
//!
//! ## Deviation from plan D1 / AC9 (R2)
//!
//! The finalized plan §3 says `BlockRouter` implements `TransformStage` from
//! `rskim-proxy`. However, `TransformStage`/`TransformContext` live in
//! `rskim-proxy`, which has NON-OPTIONAL hyper/tokio/axum as dependencies. AC9
//! forbids rskim-compress from depending on hyper/tokio/axum (this is a pure
//! sync library crate). Resolution (R2, per the binding task brief):
//!
//! - `BlockRouter` lives in rskim-compress and implements
//!   `rskim_contract::contract::Contract` directly (the bare trait, no proxy deps).
//! - For Phase 1, `BlockRouter::route()` returns a whole-request PASSTHROUGH.
//! - The `TransformStage` adapter (mapping `ctx.auth_mode → Policy` and calling
//!   `route()`) is NOT built here — it lives in the `rskim` binary at Phase 4
//!   integration, where hyper/tokio are already present.
//! - This architecture is documented here as an AD comment: BlockRouter is a
//!   `Contract` + policy-aware `route()`; the `TransformStage` adapter lives in
//!   the binary because AC9 forbids rskim-proxy/hyper/tokio in this crate; this
//!   deviates from D1's letter but honors its intent (per-call policy, stateless
//!   shared router).
//!
//! ## Crate layout
//!
//! ```text
//! rskim-compress
//! ├── src/
//! │   ├── lib.rs        — public API, BlockRouter, Policy
//! │   └── log.rs        — compress_log, LogFlags, ParseResult, LogResult (R1)
//! └── tests/
//!     └── conformance.rs — conformance harness integration test
//! ```
//!
//! ## Dependency constraints (AC9 / AC26)
//!
//! rskim-compress MUST NOT depend on: hyper, tokio, axum, rskim-proxy.
//! rskim-core MUST NOT gain regex (AC26): regex lives here.

#![deny(missing_docs)]

pub mod log;

use std::sync::Arc;

use rskim_contract::contract::{Contract, Outcome};
use rskim_contract::log::DecisionSink;

/// Auth-derived compression policy for the block router.
///
/// Derived per call from `ctx.auth_mode` in the TransformStage adapter
/// (which lives in the rskim binary at Phase 4 — not here, per AC9/R2).
///
/// ## AD-PXY (R2)
///
/// `Policy` is defined here (not in rskim-proxy) so the router can be tested
/// independently of the proxy crate. The TransformStage adapter maps
/// `ctx.auth_mode → Policy` per call; the router itself is stateless.
///
/// `#[non_exhaustive]` so future policies (e.g., `DebugDump`) can be added
/// without breaking existing match arms in dependents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Policy {
    /// Default compression: apply all registered engines.
    Default,
    /// Lossless-only mode: forward every block byte-identical.
    ///
    /// Applied when `ctx.auth_mode` indicates a subscription/OAuth flow.
    /// Conservative map: `Ambiguous → Default` (per D1 / DECISIONS-NEEDED.md).
    LosslessOnly,
}

/// A null (no-op) [`DecisionSink`] that discards every record.
///
/// Used by `BlockRouter`'s `Contract::transform` bridge so the conformance
/// harness can drive the router without a real sink. Records are accepted
/// without blocking.
struct NullSink;

impl DecisionSink for NullSink {
    fn try_send(
        &self,
        _record: rskim_contract::log::DecisionRecord,
    ) -> Result<(), rskim_contract::log::SinkFull> {
        Ok(())
    }
}

/// Per-content-type block compression router (#304).
///
/// The L3 engine that replaces #303's shipped `IdentityContract` on the proxy
/// request path. Deterministic, stateless, fail-open. All dependencies injected.
///
/// ## Phase 1 (this implementation)
///
/// Phase 1 ships only the conformance baseline: `route()` returns
/// `Outcome::passthrough` for every request. The per-class engines (code, log,
/// JSON, mixed) land in Phase 2.
///
/// ## Architecture (R2 / AC9)
///
/// ```text
/// BlockRouter::route(&[u8], Policy, request_id, &dyn DecisionSink) -> Outcome
///   ↑ Phase 1: whole-request PASSTHROUGH
///   ↑ Phase 2: parse → live-zone → per-block engines
///
/// impl Contract for BlockRouter    ← conformance harness bridge (AC10)
///   transform(input, request_id) → calls route(input, Policy::Default, …)
///
/// TransformStage adapter           ← lives in rskim binary (Phase 4)
///   apply(body, ctx, sink) → maps ctx.auth_mode → Policy, calls route()
/// ```
///
/// The `TransformStage` adapter is NOT in this crate because rskim-proxy
/// (which defines `TransformStage`) depends on hyper/tokio/axum, violating
/// AC9. This deviates from plan D1's letter but honours its intent: per-call
/// policy, stateless shared router, clean separation.
///
/// ## Dependency invariant (AC9 / AC26)
///
/// This crate MUST NOT depend on: hyper, tokio, axum, rskim-proxy.
/// Verify with: `cargo tree -p rskim-compress --prefix none | grep -E 'hyper|tokio|axum'`
pub struct BlockRouter {
    /// Injected decision sink (shared, stateless).
    ///
    /// In Phase 1, only the `Contract::transform` bridge uses this via `NullSink`.
    /// In Phase 2, the `TransformStage` adapter passes a per-call sink instead.
    _sink: Arc<dyn DecisionSink>,
}

impl BlockRouter {
    /// Construct a `BlockRouter` with the injected decision sink.
    ///
    /// Infallible — no I/O at construction time (AC9 construction invariant).
    ///
    /// ## Arguments
    ///
    /// - `sink`: shared decision sink for per-block logging. Phase 1 accepts any
    ///   `DecisionSink`; pass `Arc::new(NullSink)` for tests or use the
    ///   `BlockRouter::passthrough_default()` convenience constructor.
    pub fn new(sink: Arc<dyn DecisionSink>) -> Self {
        Self { _sink: sink }
    }

    /// Convenience constructor for tests: wraps a `NullSink`.
    ///
    /// Suitable for the conformance harness and unit tests where decision
    /// logging is not under test.
    pub fn passthrough_default() -> Self {
        Self::new(Arc::new(NullSink))
    }

    /// Policy-aware per-request entry point.
    ///
    /// ## Phase 1 baseline
    ///
    /// Returns `Outcome::passthrough(body.to_vec(), request_id, "block-router")`
    /// unconditionally. This matches the `IdentityContract` baseline and passes
    /// the #301 conformance harness (AC10).
    ///
    /// ## Phase 2 (next)
    ///
    /// Will route through: parse → policy gate → live-zone selection →
    /// per-block engines (code/log/JSON/mixed) → never-inflate gate →
    /// serialize → whole-request check.
    ///
    /// ## Arguments
    ///
    /// - `body`: raw request body bytes (bounded by proxy's max-body limit).
    /// - `policy`: compression policy derived from `ctx.auth_mode` per call.
    /// - `request_id`: caller-assigned request identifier (no entropy added).
    /// - `sink`: per-call decision sink; on `SinkFull` → block stays original.
    ///
    /// ## AD-010 (Determinism)
    ///
    /// No `SystemTime::now`, `Instant::now`, `rand`, or `getrandom` anywhere in
    /// this path. Enforced by the crate's `clippy.toml` `disallowed-methods` gate
    /// (copied from rskim-contract, per AD-010).
    pub fn route(
        &self,
        body: &[u8],
        _policy: Policy,
        request_id: &str,
        _sink: &dyn DecisionSink,
    ) -> Outcome {
        // Phase 1: whole-request PASSTHROUGH baseline.
        // Phase 2 will: parse → live-zone → per-block → serialize.
        //
        // AC10: parity with IdentityContract — the conformance harness must pass.
        Outcome::passthrough(body.to_vec(), request_id, "block-router")
    }
}

/// `Contract` implementation bridges `BlockRouter` to the #301 conformance
/// harness (AC10 / R2).
///
/// Uses `Policy::Default` and a `NullSink` so the harness can call
/// `transform(input, request_id)` without constructing a full proxy context.
///
/// ## Non-tautology requirement (AC10)
///
/// This `impl` exercises `BlockRouter::route` (the type the proxy will actually
/// use), NOT the pre-existing `rskim_contract::contract::IdentityContract`.
/// Replacing `BlockRouter::route` with a mutating implementation and running
/// `run_conformance_suite` against this impl MUST fail — proving the harness
/// tests `BlockRouter`'s actual behaviour.
impl Contract for BlockRouter {
    fn component_name(&self) -> &'static str {
        "block-router"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        // Use NullSink for the harness bridge — no decision logging in this path.
        // The TransformStage adapter (Phase 4) passes a real per-call sink.
        self.route(input, Policy::Default, request_id, &NullSink)
    }
}
