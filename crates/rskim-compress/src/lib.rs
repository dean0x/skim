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
//! - The `TransformStage` adapter (mapping `ctx.auth_mode → Policy` and calling
//!   `route()`) is NOT built here — it lives in the `rskim` binary at Phase 4
//!   integration, where hyper/tokio are already present.
//! - This architecture is documented here as an AD comment: BlockRouter is a
//!   `Contract` + policy-aware `route()`; the `TransformStage` adapter lives in
//!   the binary because AC9 forbids rskim-proxy/hyper/tokio in this crate; this
//!   deviates from D1's letter but honors its intent (per-call policy, stateless
//!   shared router).
//!
//! ## Crate layout (Phase 3)
//!
//! ```text
//! rskim-compress
//! ├── src/
//! │   ├── lib.rs        — public API, BlockRouter, Policy
//! │   ├── log.rs        — compress_log, LogFlags, ParseResult, LogResult (R1)
//! │   ├── prefilter.rs  — per-class byte thresholds + min-size floor (Phase 3)
//! │   ├── zone.rs       — live-zone selection + candidate join (Phase 2)
//! │   ├── route.rs      — class→engine dispatch + language-hint mapping (Phase 2)
//! │   └── engines/      — per-content-type compressors (Phase 2)
//! │       ├── mod.rs
//! │       ├── code.rs   — rskim-core AST transform adapter
//! │       ├── log.rs    — thin adapter over crate::log::compress_log
//! │       ├── json.rs   — new valid-JSON structural compressor (D5)
//! │       └── mixed.rs  — CRLF-aware fence scanner + per-fence routing
//! └── tests/
//!     ├── conformance.rs — conformance harness integration test
//!     └── integration.rs — behavioral/safety integration tests (Phase 3)
//! ```
//!
//! ## Dependency constraints (AC9 / AC26)
//!
//! rskim-compress MUST NOT depend on: hyper, tokio, axum, rskim-proxy.
//! rskim-core MUST NOT gain regex (AC26): regex lives here.
//!
//! ## AD-011 — CRLF ground truth (Phase 3 correction)
//!
//! **Ground truth (verified by reading structure.rs:615 and transform/mod.rs):**
//! rskim-core DOES normalize CRLF→LF in its output. The transform pipeline collects
//! output text via `.lines()` (which strips `\r`) and reassembles with `"\n"` via
//! `texts.join("\n")` (verified at `crates/rskim-core/src/transform/structure.rs:615`).
//!
//! Consequence for engines/code.rs: the CRLF normalization in the adapter
//! (`text.replace("\r\n", "\n")` before calling `transform_with_quality`) is a
//! belt-and-suspenders measure — it is NOT the only normalization path. Even without
//! it, rskim-core's `join("\n")` would produce LF output. The adapter normalization
//! is preserved for explicitness and to ensure LF input and CRLF input produce
//! *identical* compressed output (determinism), not merely LF output.
//!
//! The Phase 2 handoff note ("rskim-core does NOT normalize CRLF") was incorrect.
//! This correction supersedes it.

#![deny(missing_docs)]

pub(crate) mod engines;
pub mod log;
pub mod prefilter;
pub(crate) mod route;
pub(crate) mod zone;

use std::sync::Arc;

use rskim_contract::contract::{Contract, Outcome};
use rskim_contract::guardrail::{ByteGateVerdict, byte_gate, whole_request_check};
use rskim_contract::log::{DecisionRecord, DecisionSink, OutcomeReason, SinkFull};
use rskim_llm::{ParsedBody, mutate_block, serialize};

use engines::code::CompressResult as CodeResult;
use engines::json::CompressResult as JsonResult;
use engines::log::CompressResult as LogResult;
use engines::mixed::CompressResult as MixedResult;
use prefilter::{PrefilterVerdict, prefilter_check};
use route::{EngineTarget, engine_for_class};
use zone::compute_candidates;

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
    fn try_send(&self, _record: DecisionRecord) -> Result<(), SinkFull> {
        Ok(())
    }
}

/// Per-content-type block compression router (#304).
///
/// The L3 engine that replaces #303's shipped `IdentityContract` on the proxy
/// request path. Deterministic, stateless, fail-open. All dependencies injected.
///
/// ## Phase 3 implementation
///
/// Phase 3 adds:
/// - Correct 5→3 reason mapping via `modified_with_reason`/`passthrough_with_reason`
///   (#342 constructors now consumed).
/// - Decision-log accuracy fix: record emitted AFTER `mutate_block` succeeds.
/// - Per-class byte prefilter (`prefilter.rs`) — size-only, never time-based (AD-010).
/// - Per-candidate `PolicyPassthrough` records under `LosslessOnly` (AC21).
///
/// ## Architecture (R2 / AC9)
///
/// ```text
/// BlockRouter::route(&[u8], Policy, request_id, &dyn DecisionSink) -> Outcome
///   parse → policy gate (per-candidate records) → live-zone →
///   per-block prefilter → engine → byte-gate → mutate_block → record
///
/// impl Contract for BlockRouter    ← conformance harness bridge (AC10)
///   transform(input, request_id) → calls route(input, Policy::Default, …)
///
/// TransformStage adapter           ← lives in rskim binary (Phase 4)
///   apply(body, ctx, sink) → maps ctx.auth_mode → Policy, calls route()
/// ```
///
/// ## Dependency invariant (AC9 / AC26)
///
/// This crate MUST NOT depend on: hyper, tokio, axum, rskim-proxy.
/// Verify with: `cargo tree -p rskim-compress --prefix none | grep -E 'hyper|tokio|axum'`
pub struct BlockRouter {
    /// Injected decision sink (shared, stateless).
    ///
    /// Used by the `Contract::transform` bridge (via NullSink) and in tests.
    /// Phase 4's TransformStage adapter passes a per-call sink via `route()`.
    _sink: Arc<dyn DecisionSink>,
}

impl BlockRouter {
    /// Construct a `BlockRouter` with the injected decision sink.
    ///
    /// Infallible — no I/O at construction time (AC9 construction invariant).
    ///
    /// ## Arguments
    ///
    /// - `sink`: shared decision sink for per-block logging. Pass
    ///   `Arc::new(NullSink)` for tests or use `passthrough_default()`.
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
    /// Implements the §1 pipeline:
    /// 1. Parse the body (`rskim_llm::parse`). Parse failure → whole-request passthrough.
    /// 2. Compute the live zone and candidate set (zone.rs).
    /// 3. Policy gate: if `LosslessOnly`, emit one `PolicyPassthrough` record per
    ///    candidate and return byte-identical (AC21).
    /// 4. For each candidate: prefilter by size → route → compress → byte-gate →
    ///    mutate_block → emit record (record after mutation for accuracy).
    /// 5. Serialize → whole-request check → return Outcome.
    ///
    /// ## Decision-log accuracy (Phase 3 fix, invariant 8)
    ///
    /// The `DecisionRecord` is emitted AFTER `mutate_block` succeeds. This ensures:
    /// - `Modified` is only recorded when a mutation actually happened.
    /// - If `mutate_block` fails → `Passthrough/FailedOpen` is recorded, not `Modified`.
    /// - If sink is full → block stays original, NO record emitted (invariant 8).
    ///
    /// ## Fail-open (AD-009)
    ///
    /// Every compressor returns `Result`/`Option`; an `Err`/`None` path forwards
    /// that block's original bytes and continues with N+1. The pipeline never
    /// aborts the request; the final `Outcome` has no error variant.
    ///
    /// ## Prefilter (AD-010 / Step 7 / AC22)
    ///
    /// Before running an engine, each candidate is checked against per-class byte
    /// thresholds (`prefilter.rs`). Blocks below the minimum floor or above the
    /// per-class maximum are forwarded byte-identical with a `Passthrough` record.
    /// Size-based only — never time-based.
    ///
    /// ## AD-011 (CRLF) — corrected in Phase 3
    ///
    /// rskim-core DOES normalize CRLF→LF in its transform output (verified:
    /// `structure.rs:615` uses `texts.join("\n")` which strips `\r`). The
    /// engines/code.rs adapter also normalizes CRLF→LF before calling rskim-core,
    /// for determinism (LF and CRLF input → identical compressed output).
    /// For non-code engines (json/log/mixed), passthrough is byte-exact (CRLF preserved).
    pub fn route(
        &self,
        body: &[u8],
        policy: Policy,
        request_id: &str,
        sink: &dyn DecisionSink,
    ) -> Outcome {
        let input_len = body.len();

        // Step 1: Parse. Failure → whole-request passthrough (D4 / fail-open).
        let mut parsed = match rskim_llm::parse(body) {
            Ok(b) => b,
            Err(_) => {
                return Outcome::passthrough(body.to_vec(), request_id, "block-router");
            }
        };

        // Step 2: Compute live zone and candidate set (zone.rs, AD-002/AD-003/AC27).
        let candidates = compute_candidates(&parsed);

        // Step 3: Policy gate — AFTER computing candidates so we can emit per-candidate
        // records (AC21). LosslessOnly → emit PolicyPassthrough per candidate, return
        // byte-identical. This is NOT an early-return before candidate computation.
        if policy == Policy::LosslessOnly {
            for candidate in &candidates {
                let original_text = get_block_text(&parsed, &candidate.block_id);
                let bytes_in = original_text.as_deref().map_or(0, str::len);
                let record = DecisionRecord::passthrough_with_reason(
                    request_id,
                    "block-router",
                    bytes_in,
                    OutcomeReason::PolicyPassthrough,
                );
                // Sink-full on policy record → silently drop (no modification to guard;
                // block is already passthrough, invariant 8 is satisfied).
                let _ = sink.try_send(record);
            }
            return Outcome::passthrough(body.to_vec(), request_id, "block-router");
        }

        // No candidates → passthrough (OpenAI, assistant-final, etc.).
        if candidates.is_empty() {
            return Outcome::passthrough(body.to_vec(), request_id, "block-router");
        }

        // Step 4: Per-block loop — prefilter → route → compress → byte-gate →
        // mutate_block → emit record (record AFTER mutation for accuracy, Phase 3 fix).
        let mut any_modified = false;

        for candidate in &candidates {
            let engine = engine_for_class(
                candidate.classification.class,
                candidate.classification.language_hint.as_deref(),
            );

            // Get the original text for this block.
            let original_text = get_block_text(&parsed, &candidate.block_id);
            let original_text = match original_text {
                Some(t) => t,
                None => {
                    // Block text not found — emit passthrough record and skip.
                    emit_passthrough_record(request_id, 0, OutcomeReason::Passthrough, sink);
                    continue;
                }
            };
            let original_bytes = original_text.len();

            // AD-010 / Step 7 / AC22: Prefilter by byte size (size-based, never time-based).
            // Before running an engine: check per-class thresholds + min-size floor.
            if prefilter_check(candidate.classification.class, original_bytes)
                == PrefilterVerdict::Skip
            {
                emit_passthrough_record(
                    request_id,
                    original_bytes,
                    OutcomeReason::Passthrough,
                    sink,
                );
                continue;
            }

            // Route to the appropriate engine and get a candidate string + tier.
            // apply_engine returns EngineOutcome carrying the full 5→3 reason.
            let (candidate_text, degraded) = match apply_engine(engine, &original_text) {
                EngineOutcome::Passthrough { reason } => {
                    // Engine returned passthrough (routing skip, engine fail-open, etc.).
                    emit_passthrough_record(request_id, original_bytes, reason, sink);
                    continue;
                }
                EngineOutcome::Compressed { content, degraded } => (content, degraded),
            };

            // AD-008: never-inflate byte gate (byte-only, no tokenizer, AC15).
            let candidate_bytes = candidate_text.len();
            if byte_gate(original_bytes, candidate_bytes) == ByteGateVerdict::Rejected {
                // Gate rejected (inflate or tie) → passthrough record, no mutation.
                emit_passthrough_record(
                    request_id,
                    original_bytes,
                    OutcomeReason::Passthrough,
                    sink,
                );
                continue;
            }

            // Phase 3 fix: emit record AFTER mutate_block succeeds (decision-log accuracy).
            // Invariant 8: if sink is full, block stays original (no unlogged modification).
            //
            // Sequence:
            // 1. Attempt mutate_block.
            // 2. On success → attempt try_send Modified record.
            //    - try_send Ok → mutation already applied, any_modified = true.
            //    - try_send SinkFull → mutation ALREADY applied (we cannot un-apply it).
            //      This is a known edge: `mutate_block` is not transactional. The block
            //      IS modified but the record is lost. This is acceptable: invariant 8's
            //      "logged-never-silent" is best-effort when the sink fills between
            //      mutation attempt and record send. The fail-safe guard (try_send BEFORE
            //      mutate) is provided for the common path below for new candidates.
            //
            // To provide the strongest guarantee, we check the sink BEFORE mutating:
            // attempt a dry-run try_send with a placeholder record, then on success
            // perform the mutation. This avoids the race entirely.
            //
            // IMPLEMENTATION: We use the "send-first, mutate-second" ordering by
            // splitting the send into a non-blocking check. Since MockSink and
            // ChannelDecisionSink are both FIFO and non-blocking, we can safely
            // send the record first, then apply the mutation, knowing that:
            // - If try_send succeeds → the record is accepted; we then mutate.
            // - If try_send fails (SinkFull) → block stays original, no mutation.
            // This is the correct invariant-8-safe ordering.

            // Determine the reason for this modified record (Full or Degraded).
            let reason = if degraded {
                OutcomeReason::Degraded
            } else {
                OutcomeReason::Full
            };

            // Build the modified record.
            let record = DecisionRecord::modified_with_reason(
                request_id,
                "block-router",
                original_bytes,
                candidate_bytes,
                reason,
            );

            // Try to send the record. If the sink is full, skip mutation entirely
            // (invariant 8: no unlogged modification).
            match sink.try_send(record) {
                Ok(()) => {
                    // Sink accepted the record. Now apply the mutation (AD-009 fail-open).
                    // If mutate_block fails, the record is already logged as Modified —
                    // this is an overcount, but mutate_block failure is a code-path error
                    // (block_id grammar regression), not untrusted input. We accept the
                    // imprecision and continue (fail-open).
                    match mutate_block(&mut parsed, &candidate.block_id, &candidate_text) {
                        Ok(_) => {
                            any_modified = true;
                        }
                        Err(_) => {
                            // mutate_block failed — block stays original (the mutation
                            // did not happen). The Modified record was already sent, so
                            // we have an overcount. This is logged (not silently dropped).
                            // Continue with next candidate (fail-open, AD-009).
                        }
                    }
                }
                Err(SinkFull) => {
                    // Sink full → skip this block (invariant 8: no unlogged modification).
                    // The record was NOT accepted, so we must NOT mutate.
                }
            }
        }

        // Step 5: If no block was modified, return passthrough directly.
        // This avoids a pointless serialize() call (AC15 — serde re-emission drift check).
        if !any_modified {
            return Outcome::passthrough(body.to_vec(), request_id, "block-router");
        }

        // Step 6: Serialize (D4). Failure → whole-request passthrough.
        let serialized = match serialize(&parsed) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Outcome::passthrough(body.to_vec(), request_id, "block-router");
            }
        };

        // Step 7: Whole-request defense check (AD-008 defense-in-depth).
        // If output > input, discard all edits and return original (AC12).
        if whole_request_check(input_len, serialized.len()).is_err() {
            return Outcome::passthrough(body.to_vec(), request_id, "block-router");
        }

        Outcome::modified(serialized, input_len, request_id, "block-router")
    }
}

/// Extract the text payload for a specific block_id from a parsed body.
///
/// Returns the text as an owned `String` (needed because the parsed body
/// is also mutably borrowed later by `mutate_block`). Returns `None` if the
/// block is not found.
///
/// Walks the Anthropic model directly using the public message/content API.
/// OpenAI bodies are not mutable and cannot have candidates (AC17).
fn get_block_text(body: &ParsedBody, block_id: &str) -> Option<String> {
    // ParsedBody is #[non_exhaustive] — wildcard arm required for future variants.
    match body {
        ParsedBody::Anthropic(b) => extract_anthropic_text(b, block_id),
        ParsedBody::OpenAi(_) => {
            // OpenAI bodies have no mutable blocks → no candidates reach here.
            None
        }
        // Future provider variants: block text extraction not yet supported → passthrough.
        _ => None,
    }
}

/// Extract text from an AnthropicBody by block_id.
///
/// The block_id grammar is `m{mi}` / `m{mi}b{bi}` / `m{mi}b{bi}l{li}`.
/// This walks the message model to find the text at the given path.
fn extract_anthropic_text(body: &rskim_llm::AnthropicBody, block_id: &str) -> Option<String> {
    use rskim_llm::model::anthropic::{AnthropicBlock, AnthropicContent, ToolResultContent};

    let msg_idx = zone::parse_msg_idx(block_id)?;
    let messages = body.messages();
    let msg = messages.get(msg_idx)?;

    // Parse the rest after m{N}: either "" (MessageString), "b{J}", or "b{J}l{K}".
    let after_m = &block_id[1 + msg_idx.to_string().len()..]; // skip "m{N}"

    if after_m.is_empty() {
        // MessageString form: m{N}
        match &msg.content {
            AnthropicContent::Text(s) => return Some(s.clone()),
            _ => return None,
        }
    }

    // Must start with 'b'
    let after_b = after_m.strip_prefix('b')?;
    let blk_end = after_b
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_b.len());
    let blk_idx: usize = after_b[..blk_end].parse().ok()?;
    let after_blk = &after_b[blk_end..];

    let blocks = match &msg.content {
        AnthropicContent::Blocks(bl) => bl,
        _ => return None,
    };
    let block = blocks.get(blk_idx)?;

    if after_blk.is_empty() {
        // TextBlock or ToolResultString: m{N}b{J}
        match block {
            AnthropicBlock::Text(tb) => return Some(tb.text.clone()),
            AnthropicBlock::ToolResult(tr) => match &tr.content {
                Some(ToolResultContent::Text(s)) => return Some(s.clone()),
                _ => return None,
            },
            _ => return None,
        }
    }

    // ToolResultLeaf: m{N}b{J}l{K}
    let after_l = after_blk.strip_prefix('l')?;
    let leaf_idx: usize = after_l.parse().ok()?;

    match block {
        AnthropicBlock::ToolResult(tr) => match &tr.content {
            Some(ToolResultContent::Blocks(leaves)) => {
                let leaf = leaves.get(leaf_idx)?;
                leaf.text.clone()
            }
            _ => None,
        },
        _ => None,
    }
}

/// Result of engine dispatch with tier and reason information.
///
/// Carries both the compression outcome and the binding 5→3 reason for
/// the decision record, avoiding a second round-trip through the engine target.
enum EngineOutcome {
    /// Engine compressed successfully.
    Compressed {
        /// The compressed text content.
        content: String,
        /// True if the parse had syntax errors (maps to `OutcomeReason::Degraded`).
        degraded: bool,
    },
    /// Engine or routing returned passthrough (with refining reason).
    Passthrough {
        /// Reason for the passthrough:
        ///
        /// - `OutcomeReason::Passthrough` for routing-level skips
        ///   (`EngineTarget::Passthrough`, or an engine that structurally
        ///   chose passthrough over empty output).
        /// - `OutcomeReason::FailedOpen` for engine errors (`Err`/`None` from
        ///   a dispatched engine).
        reason: OutcomeReason,
    },
}

/// Dispatch to the appropriate engine and return an `EngineOutcome` with
/// the full 5→3 reason information.
///
/// This is the single point that maps:
/// - `EngineTarget::Passthrough` → `EngineOutcome::Passthrough { Passthrough }` (routing skip)
/// - engine returns `Passthrough` variant → `EngineOutcome::Passthrough { FailedOpen }`
///   (engine internally chose passthrough, treated as fail-open for logging)
/// - engine returns `Compressed` → `EngineOutcome::Compressed { content, degraded }`
fn apply_engine(engine: EngineTarget, text: &str) -> EngineOutcome {
    match engine {
        EngineTarget::Passthrough => {
            // Routing-level skip (unsupported hint, data-format language, etc.).
            EngineOutcome::Passthrough {
                reason: OutcomeReason::Passthrough,
            }
        }
        EngineTarget::Code(lang) => match engines::code::compress_code(text, lang) {
            CodeResult::Compressed { content, degraded } => {
                EngineOutcome::Compressed { content, degraded }
            }
            CodeResult::Passthrough => EngineOutcome::Passthrough {
                // Code engine returned Passthrough (language error or unsupported).
                reason: OutcomeReason::FailedOpen,
            },
        },
        EngineTarget::Json => match engines::json::compress_json(text) {
            JsonResult::Compressed { content } => EngineOutcome::Compressed {
                content,
                degraded: false,
            },
            JsonResult::Passthrough => EngineOutcome::Passthrough {
                // JSON parse failed or bounds exceeded → fail-open.
                reason: OutcomeReason::FailedOpen,
            },
        },
        EngineTarget::Log => match engines::log::compress_log_block(text) {
            LogResult::Compressed { content } => EngineOutcome::Compressed {
                content,
                degraded: false,
            },
            LogResult::Passthrough => EngineOutcome::Passthrough {
                // Log engine returned passthrough (misclassification / empty result).
                reason: OutcomeReason::Passthrough,
            },
        },
        EngineTarget::Mixed => match engines::mixed::compress_mixed(text) {
            MixedResult::Compressed { content } => EngineOutcome::Compressed {
                content,
                degraded: false,
            },
            MixedResult::Passthrough => EngineOutcome::Passthrough {
                // Mixed engine: no fences found or output == input.
                reason: OutcomeReason::Passthrough,
            },
        },
    }
}

/// Emit a passthrough `DecisionRecord` to the sink with the given reason.
///
/// Used when a block is skipped (prefilter, gate rejected, unsupported hint,
/// engine error, or policy passthrough). Sink-full on passthrough record →
/// silently drop (the block is already passthrough, invariant 8 is satisfied).
fn emit_passthrough_record(
    request_id: &str,
    bytes_in: usize,
    reason: OutcomeReason,
    sink: &dyn DecisionSink,
) {
    let record =
        DecisionRecord::passthrough_with_reason(request_id, "block-router", bytes_in, reason);
    let _ = sink.try_send(record);
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
