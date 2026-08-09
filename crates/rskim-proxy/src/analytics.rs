//! Fire-and-forget analytics hook for per-request proxy telemetry.
//!
//! ## AD-PXY-17 — Analytics must not block the request path (AC15 / AC6)
//!
//! Note: AD-PXY-15 is reserved for the header rewrite allowed-list decision in
//! `forward.rs`. This analytics decision is AD-PXY-17.
//!
//! The proxy calls `AnalyticsHook::on_request` SYNCHRONOUSLY (catch_unwind-guarded)
//! on the request path. The non-blocking guarantee is therefore a property of the
//! HOOK IMPLEMENTATION, not of the proxy itself. The recommended implementation is
//! [`ChannelAnalyticsHook`], which uses `try_send` on a bounded crossbeam channel —
//! non-blocking and lossy on overflow. A hook that sleeps or blocks WILL delay the
//! request; callers must use `ChannelAnalyticsHook` (or similar) to satisfy AC15.
//!
//! When the channel is full, the event is dropped and `drop_count` is incremented
//! (AC15: events MUST be observably dropped, not silently blocked on overflow).
//! #305 connects [`ChannelAnalyticsHook`] into `serve()` with a spawned consumer.
//!
//! The concrete [`ChannelAnalyticsHook`] ships a bounded `crossbeam_channel`
//! sender. The struct is `#[non_exhaustive]`.
//!
//! ## AC6 — ProxyEvent is non-exhaustive and fires exactly once
//!
//! External construction of [`ProxyEvent`] requires `..` (non-exhaustive struct
//! literal). The hook fires exactly once per completed request. A completing
//! no-op sink is the default — no analytics overhead in tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;

use crate::detect::ProxyProvider;

// ============================================================================
// RequestTier — per-request compression tier (AD-PXY-21)
// ============================================================================

/// Per-request compression tier, derived from the collected per-block
/// [`rskim_contract::log::DecisionRecord`]s after the transform pipeline runs.
///
/// ## AD-PXY-21 — tier derivation rule
///
/// Derived by filtering collected records to the `"block-router"` component
/// (Cross-Plan Amendment #3, to exclude future CacheAlignStage records) and
/// then applying: any `Degraded` → `Degraded`; else any `Full` → `Full`; else
/// `Passthrough`. This derivation is pure in-memory with no I/O and no token
/// counting on the request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestTier {
    /// All blocks forwarded byte-identically (passthrough, fail-open, or
    /// lossless-only policy). Matches the passthrough-family
    /// [`rskim_contract::log::OutcomeReason`] variants.
    #[default]
    Passthrough,
    /// At least one block was compressed with a clean parse (no degraded
    /// blocks). Maps to `OutcomeReason::Full`.
    Full,
    /// At least one block was compressed but with parse errors. Maps to
    /// `OutcomeReason::Degraded`. Takes precedence over `Full`.
    Degraded,
}

impl RequestTier {
    /// Stable string representation used in analytics DB rows.
    pub fn as_str(self) -> &'static str {
        match self {
            RequestTier::Passthrough => "passthrough",
            RequestTier::Full => "full",
            RequestTier::Degraded => "degraded",
        }
    }
}

// ============================================================================
// BlockDecisionProjection — compact per-block decision for the detail table
// ============================================================================

/// Compact projection of one [`rskim_contract::log::DecisionRecord`] for the
/// `proxy_block_decisions` detail table.
///
/// ## AD-AN-13 / AD-PXY-21
///
/// The unit is **bytes** (not tokens): `bytes_in` / `bytes_out` are always
/// present and exactly additive across blocks. Token counting is non-additive
/// across block boundaries; the parent `token_savings` row carries the whole-body
/// single-`Counter` counts. The byte-reconciliation invariant:
/// `Σ bytes_in == raw_content_len` and `Σ bytes_out == forwarded_content_len`.
#[derive(Debug, Clone)]
pub struct BlockDecisionProjection {
    /// Stage component name (e.g., `"block-router"`, `"identity"`).
    pub component: &'static str,
    /// Outcome string derived from [`rskim_contract::log::OutcomeReason`]:
    /// one of `"full"`, `"degraded"`, `"passthrough"`, `"failed_open"`,
    /// `"policy_passthrough"`, `"lossy_rejected"`, or `"unknown"`.
    pub outcome: &'static str,
    /// Input byte count for this block.
    pub bytes_in: usize,
    /// Output byte count for this block.
    pub bytes_out: usize,
}

// ============================================================================
// ProxyEvent
// ============================================================================

/// Per-request analytics payload.
///
/// Fired exactly once per completed request (AC6). `#[non_exhaustive]` so future
/// fields can be added without breaking external match/construction patterns.
///
/// ## AD-PXY-21 / AD-PXY-22 / AD-PXY-23
///
/// - `model` — verbatim model string extracted by `detect_model`; no
///   normalization (AD-PXY-22).
/// - `tier` — derived from collected `DecisionRecord`s filtered to the
///   `"block-router"` component after the transform pipeline (AD-PXY-21).
/// - `raw_body` / `final_body` — original and transformed request bodies as
///   `Bytes` (cheap Arc-clone) for background token counting in the consumer
///   thread.
/// - `upstream_error_status` — set only on the distinct transformed-but-
///   upstream-errored path (AD-PXY-25); `None` for a normally relayed request.
/// - `duration` — set at fire time (AD-PXY-23), measuring request receipt →
///   final relayed response frame / stream end.
///
/// # Non-exhaustive construction
///
/// External crates must use `..` in struct literals. In-crate construction
/// uses [`ProxyEvent::new`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ProxyEvent {
    /// Provider classification for this request.
    pub provider: ProxyProvider,

    /// Bytes received from the client (request body).
    ///
    /// Derived from `raw_body.len()` — equals the client-sent body length.
    pub request_bytes: u64,

    /// Bytes received from the upstream (response body).
    ///
    /// Currently always `0` (deferred OQ3 — response byte counting requires a
    /// separate response-body wrapping layer not yet implemented).
    pub response_bytes: u64,

    /// Wall-clock duration from first request byte to last response byte
    /// (stream end), set at fire time by [`crate::server::AnalyticsCompletionBody`].
    ///
    /// AD-PXY-23: `duration` is computed as `start.elapsed()` at the moment
    /// the completion wrapper fires, not at response-header time.
    pub duration: Duration,

    /// Verbatim model string extracted from the request body's top-level
    /// `"model"` key (AD-PXY-22). `None` when undetected or non-UTF-8.
    pub model: Option<String>,

    /// Turn-level attribution. Always `None` in #305; live derivation is
    /// owned by #344 (filed per ADR-004).
    pub turn_id: Option<String>,

    /// Per-request compression tier derived from block-router DecisionRecords
    /// (AD-PXY-21). `Passthrough` when no modification occurred.
    pub tier: RequestTier,

    /// Original (untransformed) request body.
    ///
    /// Cheap `Bytes` clone (Arc-based). Used by the background consumer thread
    /// for token counting (AD-AN-7 / AD-AN-8).
    pub raw_body: Bytes,

    /// Transformed (forwarded) request body.
    ///
    /// Cheap `Bytes` clone. Used together with `raw_body` for token delta
    /// counting on the background consumer thread.
    pub final_body: Bytes,

    /// Compact per-block decision projection for the `proxy_block_decisions`
    /// detail table (AD-AN-13 / AD-PXY-21).
    pub block_decisions: Vec<BlockDecisionProjection>,

    /// Set only on the distinct transformed-but-upstream-errored path
    /// (AD-PXY-25): `Some(502)` for no-upstream / bad-URL / connection-error,
    /// `Some(504)` for upstream timeout. `None` for a normally relayed row.
    pub upstream_error_status: Option<u16>,
}

impl ProxyEvent {
    /// Construct a [`ProxyEvent`] (in-crate constructor).
    ///
    /// External consumers use the `#[non_exhaustive]` struct literal with `..`.
    ///
    /// `request_bytes` is derived from `raw_body.len()`. `response_bytes` is
    /// always `0` (deferred OQ3).
    ///
    /// ## AD-PXY-21 / AD-PXY-22 / AD-PXY-23
    ///
    /// `tier`, `block_decisions`, and `model` are derived on the request path
    /// before this constructor is called (see `server.rs::handle_request`).
    /// `duration` is set at fire time (stream end or Drop).
    /// `upstream_error_status` is `None` for normal relayed rows and `Some(502|504)`
    /// for the distinct transformed-but-upstream-errored path (AD-PXY-25).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: ProxyProvider,
        model: Option<String>,
        turn_id: Option<String>,
        tier: RequestTier,
        raw_body: Bytes,
        final_body: Bytes,
        block_decisions: Vec<BlockDecisionProjection>,
        upstream_error_status: Option<u16>,
        duration: Duration,
    ) -> Self {
        let request_bytes = raw_body.len() as u64;
        Self {
            provider,
            request_bytes,
            response_bytes: 0, // deferred OQ3
            duration,
            model,
            turn_id,
            tier,
            raw_body,
            final_body,
            block_decisions,
            upstream_error_status,
        }
    }
}

// ============================================================================
// AnalyticsHook trait
// ============================================================================

/// Fire-and-forget per-request analytics hook.
///
/// The proxy calls `on_request` SYNCHRONOUSLY on the request path (wrapped in
/// `catch_unwind`). Implementations MUST NOT block; use [`ChannelAnalyticsHook`]
/// (or a similar bounded-channel wrapper) so the call returns immediately. A
/// panicking implementation does not fail the request (AC9 / AD-PXY-12).
///
/// The default impl is [`NoopAnalyticsHook`] — a no-op sink with zero overhead.
pub trait AnalyticsHook: Send + Sync {
    /// Called exactly once per completed request.
    ///
    /// MUST NOT block. MUST NOT panic (panics are caught at the call site via
    /// `std::panic::catch_unwind`, per AC9 / AD-PXY-12).
    fn on_request(&self, event: &ProxyEvent);
}

// ============================================================================
// No-op default sink
// ============================================================================

/// Default analytics sink — discards all events with zero overhead.
///
/// This is the [`AnalyticsHook`] implementation used when no analytics hook is
/// configured. It satisfies AC6 (hook fires exactly once) because it is called,
/// it just does nothing (the no-op hook is the legitimate "I don't care" case).
#[derive(Debug, Clone, Default)]
pub struct NoopAnalyticsHook;

impl AnalyticsHook for NoopAnalyticsHook {
    fn on_request(&self, _event: &ProxyEvent) {
        // Intentional no-op. Zero allocation, zero blocking.
    }
}

// ============================================================================
// Channel-based fire-and-forget sink (AC15 / AD-PXY-17)
// ============================================================================

/// Bounded-channel analytics hook: non-blocking, lossy on overflow.
///
/// Uses `crossbeam_channel::try_send` — the `on_request` call returns immediately
/// without blocking the request path (AC15 / AD-PXY-17). When the channel is at
/// capacity, the event is dropped and `drop_count` is incremented (AC15 discriminator:
/// events MUST be observably dropped, not silently blocked on overflow).
///
/// The caller must spawn a consumer on the returned `Receiver` to process events
/// asynchronously. Dropping the receiver ends the channel; subsequent sends are
/// counted as drops.
pub struct ChannelAnalyticsHook {
    sender: crossbeam_channel::Sender<ProxyEvent>,
    drop_count: Arc<AtomicU64>,
}

impl ChannelAnalyticsHook {
    /// Create a bounded-channel hook with the given capacity.
    ///
    /// Returns the hook and the receiver half of the channel. The caller is
    /// responsible for spawning a consumer on the receiver.
    pub fn new(capacity: usize) -> (Self, crossbeam_channel::Receiver<ProxyEvent>) {
        let (sender, receiver) = crossbeam_channel::bounded(capacity);
        let hook = Self {
            sender,
            drop_count: Arc::new(AtomicU64::new(0)),
        };
        (hook, receiver)
    }

    /// Returns the number of events dropped due to channel overflow.
    ///
    /// AC15 discriminator: this counter MUST increment under saturation,
    /// proving events are dropped (not blocked) when the channel is full.
    pub fn drop_count(&self) -> u64 {
        self.drop_count.load(Ordering::Relaxed)
    }

    /// Returns a clone of the drop counter for sharing with the consumer side.
    pub fn drop_count_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.drop_count)
    }
}

impl AnalyticsHook for ChannelAnalyticsHook {
    fn on_request(&self, event: &ProxyEvent) {
        // try_send is non-blocking: Err(Full) → drop the event, increment counter.
        // AC15: lossy fire-and-forget is the contract.
        if self.sender.try_send(event.clone()).is_err() {
            self.drop_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ============================================================================
// Tests (AC6, AC15)
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::detect::ProxyProvider;

    fn make_event() -> ProxyEvent {
        // raw_body of 1024 bytes so request_bytes == 1024 (AC6 field check).
        ProxyEvent::new(
            ProxyProvider::Anthropic,
            None,
            None,
            RequestTier::Passthrough,
            Bytes::from(vec![0u8; 1024]),
            Bytes::from(vec![0u8; 1024]),
            vec![],
            None,
            Duration::from_millis(42),
        )
    }

    // AC6: noop hook fires once without panicking.
    #[test]
    fn test_noop_hook_fires_without_panic() {
        let hook = NoopAnalyticsHook;
        let event = make_event();
        hook.on_request(&event); // must not panic
    }

    // AC6: event fields are populated correctly.
    #[test]
    fn test_event_fields_populated() {
        let event = make_event();
        assert_eq!(event.provider, ProxyProvider::Anthropic);
        assert_eq!(
            event.request_bytes, 1024,
            "request_bytes derived from raw_body.len()"
        );
        assert_eq!(
            event.response_bytes, 0,
            "response_bytes always 0 (deferred OQ3)"
        );
        assert_eq!(event.duration, Duration::from_millis(42));
        assert_eq!(event.tier, RequestTier::Passthrough);
        assert!(event.model.is_none());
        assert!(event.turn_id.is_none());
        assert!(event.upstream_error_status.is_none());
        assert!(event.block_decisions.is_empty());
    }

    // RequestTier: as_str returns stable strings.
    #[test]
    fn test_request_tier_as_str() {
        assert_eq!(RequestTier::Passthrough.as_str(), "passthrough");
        assert_eq!(RequestTier::Full.as_str(), "full");
        assert_eq!(RequestTier::Degraded.as_str(), "degraded");
    }

    // AC15: channel hook is non-blocking; event is received by consumer.
    #[test]
    fn test_channel_hook_delivers_event() {
        let (hook, rx) = ChannelAnalyticsHook::new(16);
        let event = make_event();
        hook.on_request(&event);
        let received = rx.try_recv().expect("event must be delivered to channel");
        assert_eq!(received.request_bytes, 1024);
        assert_eq!(hook.drop_count(), 0, "no drops on uncrowded channel");
    }

    // AC15 discriminating: channel overflow → event dropped, counter increments.
    #[test]
    fn test_channel_hook_drops_on_overflow() {
        let (hook, rx) = ChannelAnalyticsHook::new(2);

        // Fill channel to capacity.
        hook.on_request(&make_event());
        hook.on_request(&make_event());
        assert_eq!(hook.drop_count(), 0, "no drops yet");

        // Overflow: third event must be dropped, not block.
        hook.on_request(&make_event());
        assert_eq!(
            hook.drop_count(),
            1,
            "overflow must increment drop_count (AC15 discriminator)"
        );

        // Channel still has the first two events.
        assert_eq!(rx.len(), 2);
    }

    // AD-PXY-25: upstream_error_status is set correctly for error events.
    #[test]
    fn test_upstream_error_status_field() {
        let event = ProxyEvent::new(
            ProxyProvider::Unknown,
            None,
            None,
            RequestTier::Full,
            Bytes::from_static(b"raw"),
            Bytes::from_static(b"final"),
            vec![],
            Some(502),
            Duration::from_millis(10),
        );
        assert_eq!(event.upstream_error_status, Some(502));
        assert_eq!(event.tier, RequestTier::Full);
        assert_eq!(event.request_bytes, 3, "raw.len() == 3");
    }

    // AD-PXY-21: block_decisions are stored correctly.
    #[test]
    fn test_block_decisions_stored() {
        let decisions = vec![BlockDecisionProjection {
            component: "block-router",
            outcome: "full",
            bytes_in: 100,
            bytes_out: 80,
        }];
        let event = ProxyEvent::new(
            ProxyProvider::Anthropic,
            Some("claude-3-5-sonnet-20241022".to_owned()),
            None,
            RequestTier::Full,
            Bytes::from(vec![0u8; 100]),
            Bytes::from(vec![0u8; 80]),
            decisions.clone(),
            None,
            Duration::from_millis(5),
        );
        assert_eq!(event.block_decisions.len(), 1);
        assert_eq!(event.block_decisions[0].component, "block-router");
        assert_eq!(event.block_decisions[0].outcome, "full");
        assert_eq!(event.block_decisions[0].bytes_in, 100);
        assert_eq!(event.block_decisions[0].bytes_out, 80);
        assert_eq!(event.model.as_deref(), Some("claude-3-5-sonnet-20241022"));
    }

    // AD-PXY-24: ProxyEvent is non-exhaustive — compile-time check via struct literal
    // with `..`. This cannot be asserted at runtime; the type system enforces it.
    // The comment is the acceptance criterion documentation.
    //
    // Proof: the following would not compile without `..`:
    //   let _ = ProxyEvent { provider: ProxyProvider::Unknown, request_bytes: 0,
    //                        response_bytes: 0, duration: Duration::ZERO, ... };
    // External crates must use struct-update syntax.
    // (This is enforced by #[non_exhaustive] on the struct.)
    #[test]
    fn test_proxy_event_non_exhaustive_marker() {
        // Use the constructor — cannot use struct literal without `..` from outside.
        let event = ProxyEvent::new(
            ProxyProvider::Unknown,
            None,
            None,
            RequestTier::default(),
            Bytes::new(),
            Bytes::new(),
            vec![],
            None,
            Duration::ZERO,
        );
        assert!(event.duration.is_zero());
    }
}
