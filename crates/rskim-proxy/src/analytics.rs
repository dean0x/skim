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
//! sender. #305 will extend [`ProxyEvent`] with usage counters (token fields)
//! without a breaking change, because the struct is `#[non_exhaustive]`.
//!
//! ## AC6 — ProxyEvent is non-exhaustive and fires exactly once
//!
//! External construction of [`ProxyEvent`] requires `..` (non-exhaustive struct
//! literal). The hook fires exactly once per completed request. A completing
//! no-op sink is the default — no analytics overhead in tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::detect::ProxyProvider;

// ============================================================================
// ProxyEvent
// ============================================================================

/// Per-request analytics payload.
///
/// Fired exactly once per completed request (AC6). Extensible: #305 adds
/// usage-counter fields (tokens in/out, model name) without a breaking change
/// because the struct is `#[non_exhaustive]`.
///
/// # Non-exhaustive construction
///
/// External crates must use `..` in struct literals (AC24). In-crate construction
/// uses [`ProxyEvent::new`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ProxyEvent {
    /// Provider classification for this request.
    pub provider: ProxyProvider,

    /// Bytes received from the client (request body).
    ///
    /// Equal to the client-sent body length. The proxy MUST NOT modify this —
    /// byte-identity is the forwarding invariant.
    pub request_bytes: u64,

    /// Bytes received from the upstream (response body).
    ///
    /// Measured at the proxy, not at the client. For streaming responses this
    /// is the total bytes forwarded to the client.
    pub response_bytes: u64,

    /// Wall-clock duration from first request byte to last response byte.
    ///
    /// Includes upstream latency + forwarding overhead. Does NOT subtract
    /// upstream time; the absolute figure is more useful for analytics.
    /// Per ADR-003 / PF-005: not used as a CI gate — latency regression
    /// detection uses criterion bench baselines (AD-PXY-16).
    ///
    /// Note: [`crate::seam::TransformStage`] transforms are deterministic
    /// (no clocks per #301 invariant 5). The proxy's server layer LEGITIMATELY
    /// uses clocks (AC18 — do not copy rskim-contract's disallowed-methods gate).
    pub duration: Duration,
}

impl ProxyEvent {
    /// Construct a [`ProxyEvent`] (in-crate constructor).
    ///
    /// External consumers use the `#[non_exhaustive]` struct literal with `..`.
    ///
    /// Called by the request-completion path in #304 (block-router + forwarder).
    /// Suppressed until #304 lands.
    #[allow(dead_code)]
    pub(crate) fn new(
        provider: ProxyProvider,
        request_bytes: u64,
        response_bytes: u64,
        duration: Duration,
    ) -> Self {
        Self {
            provider,
            request_bytes,
            response_bytes,
            duration,
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
        ProxyEvent::new(
            ProxyProvider::Anthropic,
            1024,
            2048,
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
        assert_eq!(event.request_bytes, 1024);
        assert_eq!(event.response_bytes, 2048);
        assert_eq!(event.duration, Duration::from_millis(42));
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

    // AC24: ProxyEvent is non-exhaustive — compile-time check via struct literal
    // with `..`. This cannot be asserted at runtime; the type system enforces it.
    // The comment is the acceptance criterion documentation.
    //
    // Proof: the following would not compile without `..`:
    //   let _ = ProxyEvent { provider: ProxyProvider::Unknown, request_bytes: 0,
    //                        response_bytes: 0, duration: Duration::ZERO };
    // External crates must write:
    //   ProxyEvent { provider: ..., request_bytes: ..., ..
    //                rskim_proxy::analytics::ProxyEvent::new(...) }
    // (This is enforced by #[non_exhaustive] on the struct.)
    #[test]
    fn test_proxy_event_non_exhaustive_marker() {
        // Use the constructor — cannot use struct literal without `..` from outside.
        let event = ProxyEvent::new(ProxyProvider::Unknown, 0, 0, Duration::ZERO);
        assert!(event.duration.is_zero());
    }
}
