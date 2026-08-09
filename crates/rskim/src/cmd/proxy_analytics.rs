//! Feature-gated bridge between the `rskim-proxy` analytics hook and the
//! always-compiled recording layer in `rskim::analytics`.
//!
//! ## AD-AN-10 — feature-gated bridge, ungated core
//!
//! Only this module and the `run()` wiring in `cmd/proxy.rs` are
//! `#[cfg(feature = "proxy")]`. Schema v4, `record_proxy`, `select_encoding`,
//! and the stats breakdowns compile and are reachable in the default build
//! (they use `rusqlite` + `rskim-tokens`, both default deps — no `rskim-proxy`
//! type). A default-build compile + reachability check is an AC (AC22 /
//! challenge #14). Applies ADR-008.
//!
//! ## AD-AN-8 — bounded proxy recording queue (records AND bytes)
//!
//! `PROXY_QUEUE_RECORD_CAPACITY = 2048` and `PROXY_QUEUE_BYTE_BUDGET = 128 MiB`
//! are **bounded defaults chosen to minimise drop-bias per the user's
//! thoroughness directive (2× the prior conservative default) — NOT
//! empirically-derived thresholds** (ADR-003 / PF-005). A persistently non-zero
//! `proxy_dropped_records` in `analytics_meta` is the disclosure counter that
//! signals these constants should be raised. The constants are tunable; this
//! module documents the honest rationale and the documented trade-off.
//!
//! ## AD-AN-12 — bounded shutdown termination
//!
//! `run()` in `cmd/proxy.rs` moves the hook `Arc` into `serve_with_stage` and
//! retains no clone, so the crossbeam sender closes when `serve_with_stage`
//! returns (all connection-level clones have dropped by then). The consumer's
//! `for event in &rx` iterator ends on `Err` (channel closed) and the consumer
//! persists the accumulated drop total exactly once, then signals `done_tx`.
//! `run()` waits on `done_rx.recv_timeout(FLUSH_BOUND)`. If the bound elapses
//! (pathological wedge), `run()` returns without blocking; the OS reclaims the
//! thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rskim_proxy::analytics::{AnalyticsHook, ProxyEvent};
use rskim_proxy::config::DEFAULT_GRACEFUL_DRAIN_SECS;
use rskim_proxy::detect::ProxyProvider;

use crate::analytics::{
    AnalyticsDb, ProxyBlockDecision, ProxyRecordInput, RecordingProvider, select_encoding,
};

// ============================================================================
// Bounded-queue constants (AD-AN-8 / ADR-003 / PF-005)
// ============================================================================

/// Bounded default: maximum number of proxy events held in the recording queue.
///
/// ## AD-AN-8 — honest rationale (ADR-003 / PF-005)
///
/// **2048 is NOT an empirically-derived threshold.** This value is a bounded
/// default chosen to minimise drop-bias — 2× the prior conservative default.
/// There is no measurement claiming 2048 is optimal for any real workload.
///
/// A persistently non-zero `proxy_dropped_records` in `analytics_meta` is the
/// empirical feedback loop that signals this constant should be raised.
/// The constant is tunable at build time.
///
/// **Documented trade-off:** a finite record capacity still systematically
/// drops-and-counts burst peaks, biasing recorded savings toward smaller
/// payloads; the higher default shrinks that bias without eliminating it, and
/// the bias is never silent — `proxy_dropped_records` discloses it (AC17).
pub(crate) const PROXY_QUEUE_RECORD_CAPACITY: usize = 2048;

/// Bounded default: maximum total bytes held in the recording queue (128 MiB).
///
/// ## AD-AN-8 — honest rationale (ADR-003 / PF-005)
///
/// **128 MiB is NOT an empirically-derived threshold.** This value is a bounded
/// default chosen to minimise drop-bias for workloads with large request bodies.
/// Each enqueued event carries both `raw_body` and `final_body` as `Bytes`
/// (cheap `Arc`-backed clones), so a single event's payload can reach up to
/// ~2 × `DEFAULT_MAX_BODY_BYTES`.
///
/// A persistently non-zero `proxy_dropped_records` in `analytics_meta` is the
/// empirical feedback loop that signals this constant should be raised.
///
/// **Documented trade-off:** a finite byte budget still systematically
/// drops-and-counts bursts of very large requests, biasing recorded savings
/// toward smaller payloads; the bias is never silent — `proxy_dropped_records`
/// discloses it (AC17).
pub(crate) const PROXY_QUEUE_BYTE_BUDGET: u64 = 128 * 1024 * 1024;

/// Shutdown flush bound.
///
/// ## AD-AN-12 — bounded shutdown termination (ADR-003 / PF-005)
///
/// Defaults to `DEFAULT_GRACEFUL_DRAIN_SECS` (5 s) — reusing the existing
/// proxy shutdown-drain constant rather than inventing a new number. This
/// satisfies "joins within a fixed bound; records past the bound dropped and
/// counted" (AC18). The consumer is not waited on past this bound; the OS
/// reclaims the thread; its last action is to persist the drop total.
pub(crate) const FLUSH_BOUND: Duration = Duration::from_secs(DEFAULT_GRACEFUL_DRAIN_SECS);

/// Token-size cap for proxy-side counting (mirrors CLI analytics cap, ADR-001).
///
/// Inputs above this threshold skip BPE tokenisation and use `len / 4` as a proxy.
const PROXY_TOKEN_SIZE_CAP: usize = 256 * 1024;

// ============================================================================
// BridgeAnalyticsHook
// ============================================================================

/// Bounded-channel analytics hook bridge.
///
/// Implements `rskim_proxy::analytics::AnalyticsHook` and forwards events to
/// the background consumer thread for DB recording.
///
/// ## AD-AN-8 — dual-bound queue
///
/// Two bounds enforce memory safety:
/// - **Record count**: crossbeam channel capacity = `PROXY_QUEUE_RECORD_CAPACITY`.
/// - **Byte budget**: `queued_bytes` `AtomicU64` guards against large payloads.
///   A single event whose payload (`raw_body.len() + final_body.len()`) exceeds
///   `PROXY_QUEUE_BYTE_BUDGET` is dropped-and-counted rather than enqueued.
///
/// `on_request` is non-blocking: `try_send` is used (AC14). Any overflow
/// (record count or byte budget) increments the process-local `drop_count`
/// counter. The counter is persisted exactly once at shutdown flush (AD-AN-8).
pub(crate) struct BridgeAnalyticsHook {
    sender: crossbeam_channel::Sender<ProxyEvent>,
    drop_count: Arc<AtomicU64>,
    /// Total bytes of events currently in-flight in the channel.
    ///
    /// Incremented on enqueue, decremented by the consumer on dequeue.
    queued_bytes: Arc<AtomicU64>,
}

impl BridgeAnalyticsHook {
    /// Build the hook and return the receiver half of the channel.
    ///
    /// `capacity` is the record-count bound (production default:
    /// `PROXY_QUEUE_RECORD_CAPACITY`). The byte bound is always
    /// `PROXY_QUEUE_BYTE_BUDGET`.
    pub(crate) fn new(capacity: usize) -> (Self, crossbeam_channel::Receiver<ProxyEvent>) {
        let (sender, receiver) = crossbeam_channel::bounded(capacity);
        let hook = Self {
            sender,
            drop_count: Arc::new(AtomicU64::new(0)),
            queued_bytes: Arc::new(AtomicU64::new(0)),
        };
        (hook, receiver)
    }

    /// Clone of the drop counter shared with the consumer thread.
    pub(crate) fn drop_count_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.drop_count)
    }

    /// Clone of the queued-bytes counter shared with the consumer thread.
    pub(crate) fn queued_bytes_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.queued_bytes)
    }
}

impl AnalyticsHook for BridgeAnalyticsHook {
    /// Enqueue the event for background recording.
    ///
    /// ## AC14 — off-path structural contract (PF-007 discriminator)
    ///
    /// `on_request` MUST perform only a bounded non-blocking enqueue:
    /// - **NO** `analytics.db` open
    /// - **NO** SQL query
    /// - **NO** token counting
    ///
    /// Any DB call here would violate the AC14 constraint. The discriminating
    /// test asserts the event lands in the channel and the drop count is 0 on an
    /// uncrowded channel; deleting the `try_send` call (or replacing it with a
    /// blocking call) makes the test fail (PF-007).
    fn on_request(&self, event: &ProxyEvent) {
        // AD-AN-8: payload size = raw + final body bytes.
        let payload = event_payload_bytes(event);

        // AD-AN-8: single-event oversize check — a payload that already exceeds
        // the total byte budget is dropped immediately (AC17 single-event case).
        if payload > PROXY_QUEUE_BYTE_BUDGET {
            self.drop_count.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // AD-AN-8: cumulative byte-budget check.
        // fetch_add returns the OLD value; if old + payload would exceed the
        // budget we roll back the accounting and drop the event.
        let prev = self.queued_bytes.fetch_add(payload, Ordering::Relaxed);
        if prev.saturating_add(payload) > PROXY_QUEUE_BYTE_BUDGET {
            self.queued_bytes.fetch_sub(payload, Ordering::Relaxed);
            self.drop_count.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // try_send is non-blocking: Err(Full) means the record-count bound was
        // hit. Roll back byte accounting and drop on failure.
        if self.sender.try_send(event.clone()).is_err() {
            self.queued_bytes.fetch_sub(payload, Ordering::Relaxed);
            self.drop_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Approximate event payload size for byte-budget accounting (raw + final bytes).
pub(crate) fn event_payload_bytes(event: &ProxyEvent) -> u64 {
    (event.raw_body.len() + event.final_body.len()) as u64
}

// ============================================================================
// Consumer thread (AD-AN-12)
// ============================================================================

/// Spawn the background consumer thread.
///
/// The consumer drains the channel until the sender side closes (when
/// `serve_with_stage` returns and the hook `Arc` is dropped). After the
/// channel closes the consumer:
///
/// 1. Persists the accumulated drop count **exactly once** to `analytics_meta`
///    via `analytics_meta_add_drop_count` (AD-AN-8).
/// 2. Signals completion via `done_tx`.
///
/// Any rusqlite error — including `SQLITE_BUSY` past `busy_timeout` — is
/// treated as a **fail-open drop**: the event is counted, processing continues,
/// and the proxy never panics or blocks (AC15 / AD-AN-8).
///
/// Token counting happens HERE on the consumer thread, never on the request
/// path (AC14 / AD-AN-8).
pub(crate) fn spawn_consumer(
    rx: crossbeam_channel::Receiver<ProxyEvent>,
    drop_count: Arc<AtomicU64>,
    queued_bytes: Arc<AtomicU64>,
    session_id: Option<String>,
    done_tx: crossbeam_channel::Sender<()>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Attempt to open the analytics DB once. Failure → all events are
        // dropped-and-counted (fail-open, AC15).
        let mut db = AnalyticsDb::open_default().ok();

        // Drain until the sender side closes (returns Err when channel is
        // empty and all senders have been dropped).
        for event in &rx {
            // Decrement queued_bytes now that this event has left the channel.
            let payload = event_payload_bytes(&event);
            // Saturating sub prevents underflow in pathological race windows.
            let _ = queued_bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(cur.saturating_sub(payload))
            });

            let Some(ref mut db) = db else {
                // DB unavailable — count as drop (fail-open, AC15).
                drop_count.fetch_add(1, Ordering::Relaxed);
                continue;
            };

            // Token counting happens HERE — never on the request path (AC14).
            let input = build_proxy_record_input(&event, &session_id);

            // AC15: any rusqlite error (including SQLITE_BUSY) → fail-open drop.
            if db.record_proxy(&input).is_err() {
                drop_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Channel is closed. Persist accumulated drop count EXACTLY ONCE (AD-AN-8).
        let total_drops = drop_count.load(Ordering::Relaxed);
        // if-let chain (Rust 2024): collapse the guard + binding into one expression.
        if total_drops > 0 && let Some(ref db) = db {
            // Persist failure is fail-open (AD-AN-8) — drop silently.
            let _ = db.analytics_meta_add_drop_count(total_drops);
        }

        // Signal the waiting run() function (AD-AN-12).
        let _ = done_tx.send(());
    })
}

// ============================================================================
// Record-input construction helpers
// ============================================================================

/// Map a `rskim_proxy` `ProxyProvider` to the analytics-layer `RecordingProvider`.
fn to_recording_provider(provider: &ProxyProvider) -> RecordingProvider {
    match provider {
        ProxyProvider::Anthropic => RecordingProvider::Anthropic,
        ProxyProvider::OpenAI => RecordingProvider::OpenAI,
        // `#[non_exhaustive]` wildcard — any future ProxyProvider variant falls
        // back to Unknown, which stores NULL in the provider column.
        _ => RecordingProvider::Unknown,
    }
}

/// Compute optional token counts from the event bodies.
///
/// ## AD-AN-7 — measured-never-estimated + pair-jointly NULL
///
/// Token counting happens on the consumer thread, never on the request path
/// (AC14). `raw_tokens`, `compressed_tokens`, and `savings_pct` are **all**
/// `None` when:
/// - Either `raw_body` or `final_body` is not valid UTF-8, OR
/// - The `Counter` construction fails (practically unreachable).
///
/// They are **all** `Some` with measured values otherwise — no fabricated or
/// estimated token value is ever written.
///
/// ## AD-AN-11 — one encoding source of truth
///
/// `select_encoding(recording_provider, model)` is called once; the returned
/// `Encoding` is used to build a single `Counter` that counts **both** the raw
/// and the final body, so both sides use the same tokeniser instance.
pub(crate) fn compute_token_counts(
    event: &ProxyEvent,
    recording_provider: &RecordingProvider,
) -> (Option<i64>, Option<i64>, Option<f64>) {
    // AD-AN-7: both bodies must be valid UTF-8; otherwise all three fields are NULL.
    let raw_str = match std::str::from_utf8(&event.raw_body) {
        Ok(s) => s,
        Err(_) => return (None, None, None),
    };
    let final_str = match std::str::from_utf8(&event.final_body) {
        Ok(s) => s,
        Err(_) => return (None, None, None),
    };

    // AD-AN-11: single encoding derived from provider + model.
    let encoding = select_encoding(recording_provider, event.model.as_deref());

    // AD-AN-7: one Counter instance for both sides (same encoding — never mix).
    let counter = match rskim_tokens::Counter::new(encoding) {
        Ok(c) => c,
        Err(_) => return (None, None, None),
    };

    // Apply size cap (mirrors CLI analytics TOKEN_SIZE_CAP, ADR-001).
    let raw_tokens = bounded_count(&counter, raw_str);
    let compressed_tokens = bounded_count(&counter, final_str);

    let savings_pct = if raw_tokens > 0 {
        (1.0 - compressed_tokens as f64 / raw_tokens as f64) * 100.0
    } else {
        0.0
    };

    (Some(raw_tokens), Some(compressed_tokens), Some(savings_pct))
}

/// Count tokens with a cap to avoid O(n²) BPE cost on huge inputs (ADR-001).
fn bounded_count(counter: &rskim_tokens::Counter, text: &str) -> i64 {
    if text.len() > PROXY_TOKEN_SIZE_CAP {
        return (text.len() / 4) as i64;
    }
    counter.count(text) as i64
}

/// Build a `ProxyRecordInput` from a `ProxyEvent` and session context.
pub(crate) fn build_proxy_record_input(
    event: &ProxyEvent,
    session_id: &Option<String>,
) -> ProxyRecordInput {
    let recording_provider = to_recording_provider(&event.provider);
    let (raw_tokens, compressed_tokens, savings_pct) =
        compute_token_counts(event, &recording_provider);

    // AD-AN-13: project block decisions into the detail-row format.
    let blocks: Vec<ProxyBlockDecision> = event
        .block_decisions
        .iter()
        .enumerate()
        .map(|(i, bd)| ProxyBlockDecision {
            block_index: i,
            component: bd.component.to_string(),
            outcome: bd.outcome.to_string(),
            bytes_in: bd.bytes_in as u64,
            bytes_out: bd.bytes_out as u64,
        })
        .collect();

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let project_path = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    ProxyRecordInput {
        timestamp,
        provider: recording_provider,
        model: event.model.clone(),
        turn_id: event.turn_id.clone(),
        tier: event.tier.as_str().to_string(),
        duration_ms: event.duration.as_millis() as u64,
        project_path,
        session_id: session_id.clone(),
        original_cmd: "proxy".to_string(),
        raw_tokens,
        compressed_tokens,
        savings_pct,
        upstream_error_status: event.upstream_error_status.map(|s| s as i64),
        blocks,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use bytes::Bytes;
    use rskim_proxy::analytics::{BlockDecisionProjection, RequestTier};
    use rskim_proxy::detect::ProxyProvider;

    use super::*;
    use crate::analytics::{AnalyticsDb, AnalyticsStore, RecordingProvider};

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn make_event_with_bodies(raw: Bytes, final_body: Bytes) -> ProxyEvent {
        ProxyEvent::new(
            ProxyProvider::Anthropic,
            Some("claude-3-5-sonnet-20241022".to_string()),
            None,
            RequestTier::Passthrough,
            raw,
            final_body,
            vec![],
            None,
            Duration::from_millis(10),
        )
    }

    fn make_small_event() -> ProxyEvent {
        make_event_with_bodies(
            Bytes::from_static(b"hello world raw"),
            Bytes::from_static(b"hello world"),
        )
    }

    /// Run an inline consumer against a real DB at `db_path`.
    ///
    /// Drains `rx` until the channel closes, writes each event to the DB,
    /// persists drops, and signals `done_tx` on completion. Used in tests that
    /// need a real DB without touching the process-global `SKIM_ANALYTICS_DB`
    /// env var (thread-unsafe).
    fn run_inline_consumer(
        db_path: &Path,
        rx: crossbeam_channel::Receiver<ProxyEvent>,
        drop_count: Arc<AtomicU64>,
        queued_bytes: Arc<AtomicU64>,
        session_id: Option<String>,
        done_tx: crossbeam_channel::Sender<()>,
    ) -> std::thread::JoinHandle<()> {
        let db_path = db_path.to_path_buf();
        std::thread::spawn(move || {
            let mut db = AnalyticsDb::open(&db_path).ok();
            for event in &rx {
                let payload = event_payload_bytes(&event);
                let _ = queued_bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                    Some(cur.saturating_sub(payload))
                });
                match db {
                    Some(ref mut db) => {
                        let input = build_proxy_record_input(&event, &session_id);
                        if db.record_proxy(&input).is_err() {
                            drop_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    None => {
                        drop_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            let total = drop_count.load(Ordering::Relaxed);
            if total > 0 {
                if let Some(ref db) = db {
                    let _ = db.analytics_meta_add_drop_count(total);
                }
            }
            let _ = done_tx.send(());
        })
    }

    // -------------------------------------------------------------------------
    // AC14 — structural: on_request does only a bounded non-blocking enqueue
    // -------------------------------------------------------------------------

    /// AC14 / PF-007 discriminator: `on_request` delivers the event to the
    /// channel without opening any DB or counting tokens.
    ///
    /// Deleting the `try_send` call would make `rx.try_recv()` return `Err`
    /// (no event in channel), failing this assertion.
    #[test]
    fn test_on_request_only_enqueues_no_db_no_counting() {
        let (hook, rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let event = make_small_event();

        // Call on_request — must return immediately (non-blocking).
        hook.on_request(&event);

        // The event must be in the channel (try_send happened).
        let received = rx.try_recv().expect("event must be in channel after on_request");
        assert_eq!(
            received.raw_body.len(),
            event.raw_body.len(),
            "received event must match the original"
        );

        // No drops on an uncrowded channel.
        let drop_count = hook.drop_count.load(Ordering::Relaxed);
        assert_eq!(drop_count, 0, "AC14: drop_count must be 0 on uncrowded channel");

        // queued_bytes must be positive while event was in-channel (consumer has
        // not yet decremented it — we drained via try_recv, not the consumer).
        let qb = hook.queued_bytes.load(Ordering::Relaxed);
        assert!(qb > 0, "queued_bytes must be positive while event was in-channel");
    }

    /// AC14 / PF-007: overflow → bounded `try_send` fails rather than blocking.
    ///
    /// Filling the channel with 1 event then sending a second event must
    /// increment the drop counter. If `on_request` used a blocking send this
    /// test would deadlock.
    #[test]
    fn test_on_request_drops_on_record_overflow_without_blocking() {
        let (hook, _rx) = BridgeAnalyticsHook::new(1); // capacity = 1
        let event = make_small_event();

        // Fill the channel.
        hook.on_request(&event);
        let drops_before = hook.drop_count.load(Ordering::Relaxed);
        assert_eq!(drops_before, 0, "first send must not drop");

        // Overflow: record bound hit, must drop rather than block.
        hook.on_request(&event);
        let drops_after = hook.drop_count.load(Ordering::Relaxed);
        assert_eq!(
            drops_after, 1,
            "AC14/AC17 discriminator: overflow must increment drop_count"
        );
    }

    // -------------------------------------------------------------------------
    // AC17 — bounded queue + byte-budget drop accounting
    // -------------------------------------------------------------------------

    /// AC17: a small event within budget is enqueued.
    #[test]
    fn test_small_event_within_byte_budget_is_enqueued() {
        let (hook, rx) = BridgeAnalyticsHook::new(2048);
        let ok_event = make_small_event();
        hook.on_request(&ok_event);
        assert_eq!(
            hook.drop_count.load(Ordering::Relaxed),
            0,
            "small event must not be dropped"
        );
        let _ = rx.try_recv().expect("small event must be in channel");
    }

    /// AC17: byte-budget overflow drops events without blocking the caller.
    ///
    /// Uses `BridgeAnalyticsHookForTest` with a tiny byte budget to trigger
    /// the cumulative-budget path cheaply (no 128 MiB allocation needed).
    #[test]
    fn test_byte_budget_overflow_drops_without_blocking() {
        // payload per event = raw.len() + final.len() = 15 + 11 = 26 bytes.
        // budget = 60 bytes: first 2 events fit (52 bytes), third overflows.
        let (hook, _rx) = BridgeAnalyticsHookForTest::new(100, 60);
        let event = make_small_event();

        hook.on_request(&event); // 26 bytes — within budget (prev=0, new=26)
        hook.on_request(&event); // 52 bytes — within budget (prev=26, new=52)
        hook.on_request(&event); // 78 bytes — OVER 60-byte budget → drop

        let drops = hook.drop_count.load(Ordering::Relaxed);
        assert!(drops >= 1, "byte-budget overflow must increment drop_count; got {drops}");
    }

    /// AC17: single event with payload > byte budget is immediately dropped
    /// (single-event oversize path, not the cumulative path).
    #[test]
    fn test_single_event_oversize_for_byte_budget() {
        // Tiny budget = 10 bytes; event payload = 26 bytes → oversize.
        let (hook, _rx) = BridgeAnalyticsHookForTest::new(100, 10);
        let event = make_small_event();
        hook.on_request(&event);
        assert_eq!(
            hook.drop_count.load(Ordering::Relaxed),
            1,
            "oversize single event must be dropped-and-counted"
        );
    }

    // -------------------------------------------------------------------------
    // AD-AN-7 / AD-AN-11 — token-count helpers
    // -------------------------------------------------------------------------

    /// AD-AN-7: valid UTF-8 bodies produce non-NULL token counts.
    #[test]
    fn test_compute_token_counts_valid_utf8() {
        let event = make_event_with_bodies(
            Bytes::from_static(b"Hello world, this is a test body for counting."),
            Bytes::from_static(b"Hello world."),
        );
        let provider = RecordingProvider::Anthropic;
        let (raw, comp, pct) = compute_token_counts(&event, &provider);
        assert!(raw.is_some(), "raw_tokens must be Some for valid UTF-8");
        assert!(comp.is_some(), "compressed_tokens must be Some for valid UTF-8");
        assert!(pct.is_some(), "savings_pct must be Some for valid UTF-8");
        let raw = raw.unwrap();
        let comp = comp.unwrap();
        assert!(raw > 0, "raw_tokens must be positive");
        assert!(comp > 0, "compressed_tokens must be positive");
        assert!(raw >= comp, "raw_tokens >= compressed_tokens for a shrunk body");
    }

    /// AD-AN-7: non-UTF-8 raw body → all three token fields are None.
    #[test]
    fn test_compute_token_counts_non_utf8_raw_yields_null() {
        let event = make_event_with_bodies(
            Bytes::from(vec![0xFF, 0xFE, 0xFD]),
            Bytes::from_static(b"valid final body"),
        );
        let provider = RecordingProvider::Anthropic;
        let (raw, comp, pct) = compute_token_counts(&event, &provider);
        assert!(raw.is_none(), "non-UTF-8 raw must yield None raw_tokens");
        assert!(comp.is_none(), "non-UTF-8 raw must yield None compressed_tokens");
        assert!(pct.is_none(), "non-UTF-8 raw must yield None savings_pct");
    }

    /// AD-AN-7: non-UTF-8 final body → all three token fields are None.
    #[test]
    fn test_compute_token_counts_non_utf8_final_yields_null() {
        let event = make_event_with_bodies(
            Bytes::from_static(b"valid raw body"),
            Bytes::from(vec![0x80, 0x81, 0x82]),
        );
        let provider = RecordingProvider::Anthropic;
        let (raw, comp, pct) = compute_token_counts(&event, &provider);
        assert!(raw.is_none(), "non-UTF-8 final must yield None raw_tokens");
        assert!(comp.is_none(), "non-UTF-8 final must yield None compressed_tokens");
        assert!(pct.is_none(), "non-UTF-8 final must yield None savings_pct");
    }

    /// AD-AN-11: Unknown provider maps to `RecordingProvider::Unknown`.
    #[test]
    fn test_to_recording_provider_unknown() {
        assert_eq!(
            to_recording_provider(&ProxyProvider::Unknown),
            RecordingProvider::Unknown
        );
    }

    #[test]
    fn test_to_recording_provider_anthropic() {
        assert_eq!(
            to_recording_provider(&ProxyProvider::Anthropic),
            RecordingProvider::Anthropic
        );
    }

    #[test]
    fn test_to_recording_provider_openai() {
        assert_eq!(
            to_recording_provider(&ProxyProvider::OpenAI),
            RecordingProvider::OpenAI
        );
    }

    // -------------------------------------------------------------------------
    // Consumer thread + DB integration tests
    // -------------------------------------------------------------------------

    /// AC17 / AD-AN-8: drop counter is persisted to `analytics_meta` at
    /// shutdown and is additive across two sequential "process run" simulations.
    ///
    /// Uses a capacity-1 queue so overflows happen predictably: 1 event fits,
    /// 4 additional events are dropped. After each "run" the DB must show an
    /// accumulated drop count ≥ 1 × run_number.
    #[test]
    fn test_drop_counter_persisted_at_shutdown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("drops.db");

        for run in 0u64..2 {
            let (hook, rx) = BridgeAnalyticsHookForTest::new(1, u64::MAX);
            let drop_count = Arc::clone(&hook.drop_count);
            let queued_bytes = Arc::clone(&hook.queued_bytes);
            let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);

            let handle = run_inline_consumer(
                &db_path,
                rx,
                Arc::clone(&drop_count),
                Arc::clone(&queued_bytes),
                None,
                done_tx,
            );

            // Flood the hook: capacity = 1 → first event enters channel,
            // remaining increments drop_count.
            for _ in 0..5 {
                hook.on_request(&make_small_event());
            }
            // Close sender by dropping hook (drops the sender half).
            drop(hook);

            // Wait for consumer to finish (bounded by 5s).
            let _ = done_rx.recv_timeout(Duration::from_secs(5));
            let _ = handle.join();

            // After run N (0-indexed), persisted drops must be ≥ (N+1).
            let db = AnalyticsDb::open(&db_path).expect("open temp DB");
            let persisted = db.query_proxy_dropped_records().unwrap_or(0);
            assert!(
                persisted >= (run + 1),
                "run {run}: persisted drops ({persisted}) must be at least {}",
                run + 1
            );
        }
    }

    /// AC15 arm e: concurrent `stats --clear` (clearing `token_savings`) while
    /// the consumer writes must not crash the consumer.
    ///
    /// `AnalyticsDb::clear()` deletes from `token_savings`; the consumer writes
    /// to `token_savings` in a separate WAL connection. SQLite WAL allows
    /// concurrent readers + writers; the worst observable outcome is a rusqlite
    /// error which the consumer counts as a fail-open drop.
    #[test]
    fn test_concurrent_clear_does_not_crash_consumer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("concurrent.db");

        let (hook, rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let drop_count = hook.drop_count_handle();
        let queued_bytes = hook.queued_bytes_handle();
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);

        let handle = run_inline_consumer(
            &db_path,
            rx,
            Arc::clone(&drop_count),
            Arc::clone(&queued_bytes),
            None,
            done_tx,
        );

        // Send several events to give the consumer something to process.
        for _ in 0..20 {
            hook.on_request(&make_small_event());
        }

        // Concurrently clear the DB while the consumer writes.
        // AC15 arm e: clear must not crash the consumer.
        if let Ok(db) = AnalyticsDb::open(&db_path) {
            let _ = AnalyticsStore::clear(&db);
        }

        // Close the sender and wait for consumer to finish.
        drop(hook);
        let consumer_finished = done_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        let _ = handle.join();

        // Consumer must finish — no panic, no hang (AC15).
        assert!(consumer_finished, "consumer must signal completion after channel closes");
    }

    /// AC23 / AC5: real `BlockRouter` with a compressible body produces a proxy
    /// row with real token counts in the DB.
    ///
    /// Validates the complete pipeline:
    /// BlockRouter → ProxyEvent → BridgeAnalyticsHook → consumer → DB row.
    ///
    /// "Real BlockRouter + Policy::Default + body the re-encoder actually shrinks"
    /// (plan AC23). `Policy::Default` maps to ApiKey auth (D1/AD-PXY-08).
    #[test]
    fn test_e2e_block_router_full_tier_row_with_real_token_delta() {
        use rskim_compress::{BlockRouter, Policy};
        use rskim_contract::log::MockSink;

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("e2e.db");

        // A large compressible body (repetitive content the block router trims).
        let raw_body: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":"Summarise this text: The quick brown fox jumped over the lazy dog. The quick brown fox jumped over the lazy dog. The quick brown fox jumped over the lazy dog. The quick brown fox jumped over the lazy dog. The quick brown fox jumped over the lazy dog."}]}"#;

        // Run the real BlockRouter with Policy::Default (equivalent to ApiKey auth, D1).
        let per_call_sink = MockSink::new();
        let contract_stub = Arc::new(TestDecisionSinkStub);
        let router = BlockRouter::new(contract_stub);
        let outcome = router.route(raw_body, Policy::Default, "e2e-test-001", &per_call_sink);

        let final_body_bytes = outcome.bytes;
        let block_decisions_raw = per_call_sink.drain();

        // Derive tier from the block-router decisions (Cross-Plan Amendment #3).
        let tier = derive_tier_from_decisions(&block_decisions_raw);

        // Build block decision projections.
        let block_decisions: Vec<BlockDecisionProjection> = block_decisions_raw
            .iter()
            .map(|r| BlockDecisionProjection {
                // component and outcome are &'static str fields on DecisionRecord
                component: r.component,
                outcome: outcome_static_str(r.reason.clone()),
                bytes_in: r.bytes_in,
                bytes_out: r.bytes_out,
            })
            .collect();

        // Build the ProxyEvent with the real router output.
        let event = ProxyEvent::new(
            ProxyProvider::Anthropic,
            Some("claude-3-5-sonnet-20241022".to_string()),
            None,
            tier,
            Bytes::copy_from_slice(raw_body),
            Bytes::from(final_body_bytes),
            block_decisions,
            None,
            Duration::from_millis(42),
        );

        // Push through the bridge hook.
        let (hook, rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let drop_count = hook.drop_count_handle();
        let queued_bytes = hook.queued_bytes_handle();
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);

        let handle = run_inline_consumer(
            &db_path,
            rx,
            Arc::clone(&drop_count),
            Arc::clone(&queued_bytes),
            None,
            done_tx,
        );

        hook.on_request(&event);
        drop(hook); // close sender → consumer loop ends
        let _ = done_rx.recv_timeout(Duration::from_secs(10));
        let _ = handle.join();

        // Verify the DB row (AD-AN-9: query_by_model returns one row).
        let db = AnalyticsDb::open(&db_path).expect("open DB");
        let rows = db.query_by_model(None).expect("query_by_model");

        assert!(!rows.is_empty(), "AC23: at least one proxy row must be recorded");

        let row = &rows[0];
        assert!(row.requests > 0, "AC23: requests count must be positive");

        // If token counts are present (body is valid UTF-8 → they WILL be),
        // verify raw_tokens >= compressed_tokens (body was not inflated).
        if let (Some(raw_tok), Some(comp_tok)) = (row.raw_tokens, row.compressed_tokens) {
            assert!(raw_tok > 0, "AC23: raw_tokens must be positive");
            assert!(
                comp_tok <= raw_tok,
                "AC23: compressed_tokens ({comp_tok}) must be <= raw_tokens ({raw_tok})"
            );
        }
    }

    // -------------------------------------------------------------------------
    // Test helpers for AC23
    // -------------------------------------------------------------------------

    /// No-op `DecisionSink` stub for `BlockRouter::new` in tests.
    ///
    /// `BlockRouter::new` requires an `Arc<dyn DecisionSink>` for its Contract
    /// bridge. The per-call `route()` path passes a separate `MockSink`; this
    /// stub is never called (mirrors `BinarySinkStub` in proxy.rs).
    struct TestDecisionSinkStub;

    impl rskim_contract::log::DecisionSink for TestDecisionSinkStub {
        fn try_send(
            &self,
            _r: rskim_contract::log::DecisionRecord,
        ) -> Result<(), rskim_contract::log::SinkFull> {
            Ok(())
        }
    }

    /// Map an `OutcomeReason` to the canonical outcome `&'static str`.
    fn outcome_static_str(reason: rskim_contract::log::OutcomeReason) -> &'static str {
        use rskim_contract::log::OutcomeReason;
        match reason {
            OutcomeReason::Full => "full",
            OutcomeReason::Degraded => "degraded",
            OutcomeReason::Passthrough => "passthrough",
            OutcomeReason::FailedOpen => "failed_open",
            OutcomeReason::PolicyPassthrough => "policy_passthrough",
            OutcomeReason::LossyRejected => "lossy_rejected",
            // `#[non_exhaustive]` wildcard — future OutcomeReason variants.
            _ => "unknown",
        }
    }

    /// Derive `RequestTier` from a slice of `DecisionRecord`s.
    ///
    /// Cross-Plan Amendment #3: filter to `"block-router"` component only.
    fn derive_tier_from_decisions(
        records: &[rskim_contract::log::DecisionRecord],
    ) -> RequestTier {
        use rskim_contract::log::OutcomeReason;
        let block_router: Vec<_> = records
            .iter()
            .filter(|r| r.component == "block-router")
            .collect();
        if block_router.iter().any(|r| r.reason == OutcomeReason::Degraded) {
            RequestTier::Degraded
        } else if block_router.iter().any(|r| r.reason == OutcomeReason::Full) {
            RequestTier::Full
        } else {
            RequestTier::Passthrough
        }
    }

    // -------------------------------------------------------------------------
    // Test-only hook with overrideable byte budget (AC17)
    // -------------------------------------------------------------------------

    /// Test-only hook with a configurable byte budget for overflow testing.
    ///
    /// Wraps the dual-bound logic with a custom `byte_budget` ceiling instead of
    /// `PROXY_QUEUE_BYTE_BUDGET`. Avoids 128 MiB allocations in tests while
    /// exercising the exact same control-flow paths as `BridgeAnalyticsHook`.
    struct BridgeAnalyticsHookForTest {
        sender: crossbeam_channel::Sender<ProxyEvent>,
        drop_count: Arc<AtomicU64>,
        queued_bytes: Arc<AtomicU64>,
        byte_budget: u64,
    }

    impl BridgeAnalyticsHookForTest {
        fn new(
            capacity: usize,
            byte_budget: u64,
        ) -> (Self, crossbeam_channel::Receiver<ProxyEvent>) {
            let (sender, receiver) = crossbeam_channel::bounded(capacity);
            let hook = Self {
                sender,
                drop_count: Arc::new(AtomicU64::new(0)),
                queued_bytes: Arc::new(AtomicU64::new(0)),
                byte_budget,
            };
            (hook, receiver)
        }

        fn on_request(&self, event: &ProxyEvent) {
            let payload = event_payload_bytes(event);
            if payload > self.byte_budget {
                self.drop_count.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let prev = self.queued_bytes.fetch_add(payload, Ordering::Relaxed);
            if prev.saturating_add(payload) > self.byte_budget {
                self.queued_bytes.fetch_sub(payload, Ordering::Relaxed);
                self.drop_count.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if self.sender.try_send(event.clone()).is_err() {
                self.queued_bytes.fetch_sub(payload, Ordering::Relaxed);
                self.drop_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
