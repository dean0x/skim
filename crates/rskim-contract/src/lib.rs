//! # rskim-contract — L3 safety invariant contract and fail-open guardrail
//!
//! This crate is the **safety substrate** for skim's Layer-3 LLM request proxy.
//! It codifies eight binding invariants as a *typed contract* — most invariants
//! are made unrepresentable at the type level, a few are runtime gates, and the
//! rest are proven by the conformance harness in CI.
//!
//! ## Eight binding invariants
//!
//! 1. **Fail-open** — The transform path has no error variant. Every code path
//!    terminates with `Outcome { bytes, decision }`. Passthrough is a success
//!    variant, never `Err`. This makes fail-open the *only* shape the output type
//!    can express.
//!
//! 2. **Never-inflate** — Per-transform-unit and whole-request output bytes ≤
//!    input bytes. The gate is a byte-length comparison only — no tokenizer in the
//!    accept/reject path. No tiny-payload exemption (unlike the L2 guardrail).
//!
//! 3. **Hot-zone byte-identity** — System prompt, tools array, and every message
//!    up to and including the last assistant message are re-emitted from the
//!    *original buffer by splice* (never re-serialized). Thinking/reasoning blocks
//!    are byte-identical in both zones.
//!
//! 4. **Append-only turns** — Turn count must not decrease, turn order must not
//!    change, and no turns may be merged. Budget overflow resolves to passthrough
//!    + log, never truncation.
//!
//! 5. **Determinism** — No wall-clock time, no entropy in the transform path.
//!    `SystemTime::now`, `Instant::now`, and `rand`/`getrandom` are statically
//!    banned by the clippy `disallowed-methods` gate (AC10). The `Contract::transform`
//!    signature carries no clock parameter — enforcement is structural absence, not
//!    dependency injection.
//!
//! 6. **Canonical tool equality** — Any waivered tools-array reorder must be
//!    deep-equal to the original. Numbers are compared as **raw source-token bytes
//!    via `serde_json::value::RawValue`** — never re-serialized via JCS/RFC-8785
//!    (which would change number representation). This relies on the `raw_value`
//!    feature already enabled workspace-wide.
//!
//! 7. **Sacrosanct-field passthrough** — `model`, `metadata`, transport headers,
//!    and provider-opaque fields pass through byte-identical. Auth material
//!    (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, and the scrub list declared in
//!    [`log::SENSITIVE_EXACT`] / [`log::SENSITIVE_SUFFIXES`]) must never appear
//!    unredacted in any log record. The scrub list is declared in this crate —
//!    not imported from the binary crate — so it is available to downstream
//!    consumers without pulling in the `rskim` binary crate.
//!
//! 8. **Logged-never-silent** — Every modification (non-passthrough transform)
//!    must produce exactly one structured JSON [`log::DecisionRecord`] accepted
//!    by the [`log::DecisionSink`]. If the sink is at capacity (`try_send` returns
//!    [`log::SinkFull`]), the transform emits byte-faithful passthrough instead —
//!    never an unlogged modification.
//!
//! ## Design decisions recorded here
//!
//! ### Byte-first gating (invariant 2)
//!
//! The L3 never-inflate gate is a *byte-length comparison only*, unlike the L2
//! guardrail in `crates/rskim/src/output/guardrail.rs` which has a token slow-path
//! and a 256-byte tiny-payload exemption. These are deliberately different:
//!
//! - L2 guards whole-output rendering, where token savings matter and tiny files
//!   are genuinely expected to have overhead.
//! - L3 guards per-transform-unit LLM request modification, where *any* inflation
//!   breaks the cache-key assumption. There is no "acceptable overhead" category.
//!
//! The L2 guardrail migration to share the L3 implementation is tracked in #325.
//!
//! ### Fail-open as a success variant (invariant 1)
//!
//! The `transform` method returns `Outcome` (no `Result`). This generalises the
//! proven `ParseResult<T>` precedent where `Passthrough(String)` is a typed
//! *success* variant, not an `Err`. Failure-to-modify is not an error — it is
//! the conservative correct behaviour. `Result` appears only at construction
//! boundaries (constructors for contract impls), in harness assertion APIs (where a
//! broken impl is the expected error case), and in defense-in-depth helpers
//! such as [`guardrail::whole_request_check`] reserved for the #302 consumer.
//!
//! ### Typed waivers over capability tokens (invariants 3, 4, 6)
//!
//! The two sanctioned exceptions to the default-deny invariants are modeled as
//! *narrowed traits*, not as runtime capability tokens:
//!
//! - [`waiver::MetadataReorderWithMarkers`] — metadata-only reorder + bounded
//!   `cache_control` marker injection. The trait's method signature encodes the
//!   narrowed rule (`len(output) ≤ len(input) + 4 × MARKER_BYTES`).
//! - [`waiver::SameSlotShrink`] — same-array-slot byte-shrinking turn edit.
//!
//! The core [`contract::Contract`] trait has no surface that can grow bytes or
//! touch the hot zone. Absence of a waiver trait *is* the default deny. This
//! makes the carve-outs self-documenting and harness-checkable without runtime
//! branching where it is easy to forget.
//!
//! ### Number comparison via RawValue (invariant 6)
//!
//! The `arbitrary_precision` serde_json feature is NOT enabled workspace-wide
//! (Decision 1). Invariant 6's number-faithful canonical equality instead
//! compares numbers as **raw source-token bytes** via `serde_json::value::RawValue`
//! (the `raw_value` feature, already enabled). JCS/RFC-8785 remains forbidden
//! because it re-serializes numbers per ECMAScript rules, which can silently
//! change values (e.g., `1e3` → `1000`) and produce cache misses.
//!
//! ### DecisionSink backed by crossbeam-channel (invariant 8)
//!
//! The sink trait method is `fn try_send(&self, r: DecisionRecord) -> Result<(), SinkFull>`
//! — non-blocking by type, never `async`, never `await`. The crate is entirely
//! sync. The concrete [`log::ChannelDecisionSink`] wraps a bounded
//! `crossbeam_channel::Sender` and maps `TrySendError::Full` to `SinkFull`.
//!
//! ### Compatibility with #302 byte-stable serialization (invariant 6)
//!
//! Canonical equality (this crate) is *compatible with* #302's byte-stable
//! cache-key serialization, not strictly coincident. Two inputs deemed
//! canonically-equal by this crate's deep-equality check must not produce
//! different #302 cache keys. This keeps `Depends on: none` honest and allows
//! tickets to land in any order.
//!
//! ## Related tickets
//!
//! - #302 `rskim-llm` — typed LLM request model; registers with this harness.
//! - #305 — `DecisionSink` persistent implementation (trait defined here).
//! - #306 — `cache_control` mutation layer using `MetadataReorderWithMarkers`.
//! - #307 — same-slot byte-shrink using `SameSlotShrink`.
//! - #308 — marker-immutability extension invariant registration.
//! - #309 — Wave-1 tracking; no `l3-` infix on crate names.
//! - #323 — cross-OS CI matrix (consumes this crate's harness).
//! - #325 — L2 guardrail migration follow-up (tracked, not done here).
//! - #328 — conformance-harness registration for `rskim-llm`.

#![deny(missing_docs)]

pub mod canonical;
pub mod contract;
pub mod extension;
pub mod guardrail;
pub mod log;
pub mod request;
pub mod waiver;
pub mod zone;

#[cfg(feature = "harness")]
pub mod harness;

pub use contract::{Contract, Outcome};
pub use log::{ChannelDecisionSink, DecisionRecord, DecisionSink, SinkFull};

/// Crate-level `Result` alias for construction/configuration errors.
///
/// `Result` appears only at construction boundaries and in harness assertion
/// APIs. The transform path itself is infallible (`Outcome`, no `Result`).
pub type Result<T> = std::result::Result<T, ContractError>;

/// Errors that can occur during construction or configuration.
///
/// `ContractError` is the only error type visible to callers. The transform
/// path (`Contract::transform`) never returns an error — it returns `Outcome`,
/// where passthrough is a success variant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ContractError {
    /// The request body is structurally invalid JSON.
    ///
    /// The caller should fall back to passing the body through unmodified.
    #[error("invalid JSON in request body: {0}")]
    InvalidJson(#[from] serde_json::Error),

    /// A configuration value is out of range or otherwise invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}
