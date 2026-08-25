//! Feature-gated bridge between the `rskim-proxy` analytics hook and the
//! always-compiled recording layer in `rskim::analytics`.
//!
//! ## AD-AN-10 — feature-gated bridge, ungated core
//!
//! Only this module and the `run()` wiring in `cmd/proxy.rs` are
//! `#[cfg(feature = "proxy")]`. Schema v4, `record_proxy`,
//! `encoding_for_provider_model`, and the stats breakdowns compile and are
//! reachable in the default build (they use `rusqlite` + `rskim-tokens`, both
//! default deps — no `rskim-proxy` type). A default-build compile +
//! reachability check is an AC (AC22 / challenge #14). Applies ADR-008.
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

use crate::analytics::{AnalyticsDb, ProxyBlockDecision, ProxyRecordInput, RecordingProvider};

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
///
/// Cross-Plan Amendment #4 (fulfilled): #305 swaps:
/// - `NullSink` → `ChannelDecisionSink` (collector) in `server.rs:422`
/// - `NoopAnalyticsHook` → real `BridgeAnalyticsHook` here in `proxy.rs:238`
///
/// #306's `AlignmentRecorder` consumer adopts the same bounded lifecycle:
/// `ChannelAlignmentRecorder::new_boxed` returns the thread handle and done
/// channel in `AlignmentRecorderBundle` so `proxy.rs` joins it under the
/// same `recv_timeout(FLUSH_BOUND)` pattern (ADR-003 / Cross-Plan Amendment #4).
///
/// `db_path` is the injected database location: `None` resolves through
/// `AnalyticsDb::open_default()` (the production path, honouring
/// `SKIM_ANALYTICS_DB` / `SKIM_CACHE_DIR`), `Some(p)` opens `p` directly. The
/// parameter exists so tests can drive **this** function against a temp DB
/// instead of re-implementing the drain loop — a duplicated loop silently drifts
/// from production and would leave the real consumer untested — and without
/// racing on `std::env::set_var` between parallel tests.
pub(crate) fn spawn_consumer(
    rx: crossbeam_channel::Receiver<ProxyEvent>,
    drop_count: Arc<AtomicU64>,
    queued_bytes: Arc<AtomicU64>,
    session_id: Option<String>,
    done_tx: crossbeam_channel::Sender<()>,
    db_path: Option<std::path::PathBuf>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Attempt to open the analytics DB once. Failure → all events are
        // dropped-and-counted (fail-open, AC15).
        let mut db = match db_path {
            Some(ref p) => AnalyticsDb::open(p).ok(),
            None => AnalyticsDb::open_default().ok(),
        };

        // AC15: cap per-call SQLITE_BUSY waits to 100 ms so a locked DB does not
        // stall the drain loop for the full open() default (5 s per attempt).
        // The consumer is a fail-open background thread — it counts BUSY hits as
        // drops and must not delay forwarding (AD-AN-8). 100 ms is enough for
        // brief writer contention between concurrent skim CLI invocations.
        if let Some(ref db) = db {
            let _ = db.set_busy_timeout(Duration::from_millis(100));
        }

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
        if total_drops > 0
            && let Some(ref db) = db
        {
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
        // `#[non_exhaustive]` wildcard: `ProxyProvider` is a foreign
        // `#[non_exhaustive]` enum, so exhaustive matching is impossible here.
        // A future `ProxyProvider::Google` therefore silently maps to Unknown
        // (→ `Provider::Unknown` → `Encoding::Heuristic`, basis = "heuristic").
        // See the `Provider` rustdoc in `rskim-tokens/src/encoding.rs` for the
        // full gap analysis and why this cannot be made a compile error.
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
/// AC6: no size cap — any valid UTF-8 body is counted regardless of length.
/// Token counting runs on the consumer thread (off the request path), so
/// counting large bodies does not add latency to forwarding.  A per-body size
/// cap would NULL out all large L3 transcripts, defeating the ticket's
/// primary deliverable.
///
/// ## AD-AN-11 — one encoding source of truth
///
/// `rskim_tokens::encoding_for_provider_model(recording_provider, model)` is
/// called once; the returned `Encoding` is used to build a single `Counter`
/// that counts **both** the raw and the final body, so both sides use the same
/// tokeniser instance.
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
    let encoding = rskim_tokens::encoding_for_provider_model(
        (*recording_provider).into(),
        event.model.as_deref(),
    );

    // AD-AN-7: one Counter instance for both sides (same encoding — never mix).
    let counter = match rskim_tokens::Counter::new(encoding) {
        Ok(c) => c,
        Err(_) => return (None, None, None),
    };

    // Both sides are counted with the SAME `Counter` instance (AD-AN-7).
    let raw_tokens = counter.count(raw_str) as i64;
    let compressed_tokens = counter.count(final_str) as i64;

    let savings_pct = if raw_tokens > 0 {
        (1.0 - compressed_tokens as f64 / raw_tokens as f64) * 100.0
    } else {
        0.0
    };

    (Some(raw_tokens), Some(compressed_tokens), Some(savings_pct))
}

/// Store the model string verbatim as supplied by the caller (AD-PXY-22).
///
/// AD-PXY-22 mandates exact-string grouping with no casing/alias folding.
/// Truncation at any character boundary would silently merge long model
/// identifiers that differ only in suffix, breaking the `GROUP BY` key
/// guarantee.  The proxy already rejects bodies above `max_body_bytes`
/// (64 MiB by default), so the model string is bounded by the sniff window
/// (`SHAPE_SNIFF_LIMIT` = 8 KiB in detect.rs) — no additional cap is needed.
fn store_model(model: Option<&str>) -> Option<String> {
    model.map(str::to_string)
}

/// Build a `ProxyRecordInput` from a `ProxyEvent` and session context.
pub(crate) fn build_proxy_record_input(
    event: &ProxyEvent,
    session_id: &Option<String>,
) -> ProxyRecordInput {
    let recording_provider = to_recording_provider(&event.provider);

    // AC25 / AD-PXY-25: a transformed-but-upstream-errored request is NOT a
    // successful savings row.  Its token columns are pair-jointly NULL so it can
    // never contribute to a token sum or a savings %, and it stays
    // distinguishable from a NULL-token passthrough row via
    // `upstream_error_status IS NOT NULL`.  Counting is skipped entirely — the
    // response never reached the client, so there is no relayed saving to
    // measure.  This is what `server.rs::fire_upstream_error_event` documents.
    let (raw_tokens, compressed_tokens, savings_pct) = if event.upstream_error_status.is_some() {
        (None, None, None)
    } else {
        compute_token_counts(event, &recording_provider)
    };

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
        model: store_model(event.model.as_deref()),
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
    use rskim_contract::contract::Outcome;
    use rskim_contract::log::DecisionSink;
    use rskim_proxy::analytics::{BlockDecisionProjection, RequestTier};
    use rskim_proxy::authmode::AuthMode;
    use rskim_proxy::config::ProxyConfig;
    use rskim_proxy::detect::ProxyProvider;
    use rskim_proxy::seam::{TransformContext, TransformPipeline, TransformStage};

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

    /// Drive the **production** [`spawn_consumer`] against a test-scoped DB path.
    ///
    /// Drains `rx` until the channel closes, writes each event to the DB at
    /// `db_path`, persists drops, and signals `done_tx` on completion.
    ///
    /// This is deliberately a thin delegation rather than a re-implementation of
    /// the drain loop: a copied loop would let the tests keep passing while
    /// `spawn_consumer` regressed. The injected path also avoids the
    /// thread-unsafe `std::env::set_var` races that `open_default()` would
    /// otherwise force between parallel tests.
    fn run_inline_consumer(
        db_path: &Path,
        rx: crossbeam_channel::Receiver<ProxyEvent>,
        drop_count: Arc<AtomicU64>,
        queued_bytes: Arc<AtomicU64>,
        session_id: Option<String>,
        done_tx: crossbeam_channel::Sender<()>,
    ) -> std::thread::JoinHandle<()> {
        spawn_consumer(
            rx,
            drop_count,
            queued_bytes,
            session_id,
            done_tx,
            Some(db_path.to_path_buf()),
        )
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
        let received = rx
            .try_recv()
            .expect("event must be in channel after on_request");
        assert_eq!(
            received.raw_body.len(),
            event.raw_body.len(),
            "received event must match the original"
        );

        // No drops on an uncrowded channel.
        let drop_count = hook.drop_count.load(Ordering::Relaxed);
        assert_eq!(
            drop_count, 0,
            "AC14: drop_count must be 0 on uncrowded channel"
        );

        // queued_bytes must be positive while event was in-channel (consumer has
        // not yet decremented it — we drained via try_recv, not the consumer).
        let qb = hook.queued_bytes.load(Ordering::Relaxed);
        assert!(
            qb > 0,
            "queued_bytes must be positive while event was in-channel"
        );
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

    /// AC14 / PF-007 timing discriminator: `on_request` completes near-instantly
    /// for 1000 consecutive calls, proving no synchronous I/O (DB open, SQL
    /// query, token counting) is on the enqueue path.
    ///
    /// If someone adds a synchronous `AnalyticsDb::open()` call inside
    /// `on_request`, each of the 1000 calls would incur disk I/O (≥ 1ms each
    /// = ≥ 1000ms total), blowing past the 500ms wall-clock budget.
    #[test]
    fn test_on_request_is_bounded_non_blocking() {
        let (hook, _rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let event = make_small_event();
        let start = std::time::Instant::now();
        for _ in 0..1_000 {
            hook.on_request(&event);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "AC14 / PF-007: 1000 on_request calls must complete in < 500ms \
             (no synchronous I/O on the enqueue path); took {}ms — a synchronous \
             DB open per call would take ≥ 1000ms",
            elapsed.as_millis()
        );
    }

    // -------------------------------------------------------------------------
    // AC15 (a)-(d) — hostile DB states: consumer must never panic or block
    // -------------------------------------------------------------------------

    /// AC15 arm (a): analytics DB deleted mid-run.
    ///
    /// Consumer opens the DB, records a batch of events, then the DB file is
    /// deleted. Subsequent events are fail-open dropped. The consumer must
    /// neither panic nor hang.
    #[test]
    fn test_consumer_db_deleted_mid_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("deleted.db");

        let (hook, rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let drop_count = hook.drop_count_handle();
        let queued_bytes = hook.queued_bytes_handle();
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);

        // Open and pre-populate the DB so it exists before the consumer opens it.
        {
            let _db = AnalyticsDb::open(&db_path).expect("pre-create db");
        }

        let handle = run_inline_consumer(
            &db_path,
            rx,
            Arc::clone(&drop_count),
            Arc::clone(&queued_bytes),
            None,
            done_tx,
        );

        // Send a few events before deleting the DB.
        for _ in 0..5 {
            hook.on_request(&make_small_event());
        }
        // Brief wait for consumer to open and process some events.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Delete the DB file mid-run.
        let _ = std::fs::remove_file(&db_path);

        // Send more events — consumer must fail-open (not panic, not block).
        for _ in 0..5 {
            hook.on_request(&make_small_event());
        }

        // Close the sender and wait for consumer to finish.
        drop(hook);
        let consumer_finished = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        let _ = handle.join();

        assert!(
            consumer_finished,
            "AC15 arm (a): consumer must finish after DB deletion — no panic or hang"
        );
    }

    /// AC15 arm (b): DB locked past busy_timeout by a second connection.
    ///
    /// Holds an exclusive lock while the consumer tries to write. With WAL
    /// mode `busy_timeout` (5000ms), the consumer will eventually fail with
    /// `SQLITE_BUSY` and treat it as a fail-open drop. The proxy request path
    /// is not blocked (consumer is separate thread).
    ///
    /// NOTE: we use a very short busy_timeout for test speed by using a
    /// separate exclusive connection that holds a BEGIN EXCLUSIVE lock. The
    /// consumer's internal `busy_timeout` is set in `AnalyticsDb::open`;
    /// in practice the consumer will fail quickly on the WAL path. The test
    /// only asserts "no panic/hang", not the drop count, since timing varies.
    ///
    /// Wall-clock discrimination of `set_busy_timeout(100ms)` is in the
    /// companion test `test_consumer_busy_timeout_cap_discriminates_set_busy_timeout`,
    /// which holds the lock until after the drain completes so that each BUSY wait
    /// runs to the full cap.
    #[test]
    fn test_consumer_db_locked_by_exclusive_connection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("locked.db");

        // Pre-create the DB so the consumer can open it.
        {
            let _db = AnalyticsDb::open(&db_path).expect("pre-create db");
        }

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

        // Acquire an exclusive lock on the DB via a raw rusqlite connection,
        // holding it while the consumer attempts to write.
        let lock_conn = rusqlite::Connection::open(&db_path).expect("lock conn");
        let _ = lock_conn.execute_batch("BEGIN EXCLUSIVE");

        // Send events while the lock is held.
        for _ in 0..10 {
            hook.on_request(&make_small_event());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Release the lock.
        let _ = lock_conn.execute_batch("ROLLBACK");
        drop(lock_conn);

        // Close the sender and verify consumer finishes (fail-open, no panic/hang).
        drop(hook);
        let consumer_finished = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        let _ = handle.join();

        assert!(
            consumer_finished,
            "AC15 arm (b): consumer must finish despite DB lock — no panic or hang"
        );
    }

    /// PF-007 discriminating: `spawn_consumer` calls `db.set_busy_timeout(100ms)`
    /// (proxy_analytics.rs line 262) to cap each blocked write to at most 100ms.
    /// Without that call, `AnalyticsDb::open` leaves the timeout at 5000ms, and
    /// draining N events under an exclusive lock takes N × 5000ms ≈ 15s for N=3.
    /// With the cap, it takes N × 100ms ≈ 300ms. The wall-clock assertion below
    /// (< 2s) passes with the cap and fails without it.
    ///
    /// SQLite / WAL notes that apply to this code path:
    /// - `Connection::transaction()` issues `BEGIN DEFERRED` — no lock is acquired
    ///   until the first operation.
    /// - The first `INSERT` in `record_proxy` attempts to acquire the WAL WRITER
    ///   lock from scratch. This is NOT a "read-to-write promotion" (where the busy
    ///   handler is sometimes skipped), so the busy handler IS invoked and
    ///   `busy_timeout` controls the wait duration.
    ///
    /// Setup contract: a small sleep after spawning the consumer ensures its
    /// `AnalyticsDb::open()` completes before the exclusive lock is acquired.
    /// Without that sleep, `open()` races the lock acquisition; if it loses,
    /// `db = None` (a different code path) and the test becomes vacuous.
    #[test]
    fn test_consumer_busy_timeout_cap_discriminates_set_busy_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("busycap.db");

        // Pre-create the DB so the consumer can open it successfully.
        {
            let _db = AnalyticsDb::open(&db_path).expect("pre-create db");
        }

        let (hook, rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let drop_count = hook.drop_count_handle();
        let queued_bytes = hook.queued_bytes_handle();
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);

        // Spawn the production consumer — it will open the DB and call
        // set_busy_timeout(100ms).  The sleep below lets AnalyticsDb::open()
        // finish before we acquire the exclusive lock.
        let handle = run_inline_consumer(
            &db_path,
            rx,
            Arc::clone(&drop_count),
            Arc::clone(&queued_bytes),
            None,
            done_tx,
        );
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Hold an exclusive lock for the entire duration of the drain so that
        // every record_proxy call blocks for the full busy_timeout window before
        // failing open.
        let lock_conn = rusqlite::Connection::open(&db_path).expect("lock conn");
        let _ = lock_conn.execute_batch("BEGIN EXCLUSIVE");

        const N_EVENTS: u64 = 3;
        for _ in 0..N_EVENTS {
            hook.on_request(&make_small_event());
        }
        drop(hook); // close sender so consumer exits after draining all events

        // Measure wall-clock time from channel-close to consumer exit.
        let started = std::time::Instant::now();
        let consumer_finished = done_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .is_ok();
        let elapsed = started.elapsed();

        // Release the exclusive lock now that the consumer is done.
        let _ = lock_conn.execute_batch("ROLLBACK");
        drop(lock_conn);
        let _ = handle.join();

        assert!(consumer_finished, "consumer must finish — no panic or hang");

        // PF-007/ADR-003 discriminating: with set_busy_timeout(100ms) each BUSY wait is
        // capped at ~100ms → 3 × 100ms ≈ 300ms total.  Without the cap the
        // fallback from AnalyticsDb::open is 5000ms/event → ~15s total.
        // 5s bound: observed 2.115s on CI (run 32871027659) with ~2.4× headroom.
        // The discriminating property is the ~3× gap to the ~15s uncapped path,
        // not the absolute value.  Deleting proxy_analytics.rs line 262 must fail.
        assert!(
            elapsed < std::time::Duration::from_millis(5_000),
            "AC15 busy_timeout cap: drained {N_EVENTS} events under exclusive lock in \
             {elapsed:?}; expected < 5s (each BUSY capped at 100ms). Without \
             set_busy_timeout(100ms) the fallback is 5000ms/event (~15s total)."
        );

        // All events must have been fail-open dropped (lock never released).
        assert_eq!(
            drop_count.load(Ordering::Relaxed),
            N_EVENTS,
            "all {N_EVENTS} events must be fail-open dropped when DB is exclusively locked"
        );
    }

    /// AC15 arm (c): DB path in a read-only directory.
    ///
    /// Consumer tries to open (or create) analytics.db in a directory where
    /// it has no write permission. `AnalyticsDb::open` fails → `db = None` →
    /// all subsequent events are fail-open dropped. Consumer must not panic.
    #[cfg(unix)]
    #[test]
    fn test_consumer_db_in_readonly_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("noperm.db");

        // Make the directory read-only before the consumer opens the DB.
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o555); // r-xr-xr-x
        std::fs::set_permissions(dir.path(), perms.clone()).unwrap();

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

        // Send events — all should be fail-open dropped (DB can't be created).
        for _ in 0..10 {
            hook.on_request(&make_small_event());
        }

        // Close sender and verify consumer finishes without panic.
        drop(hook);
        let consumer_finished = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        let _ = handle.join();

        // Restore permissions so tempdir cleanup can succeed.
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(dir.path(), perms);

        assert!(
            consumer_finished,
            "AC15 arm (c): consumer must finish even in read-only directory — no panic or hang"
        );
    }

    /// AC15 arm (d): DB file replaced with garbage content.
    ///
    /// Writes random bytes to the DB path so rusqlite opens a non-database
    /// file. The consumer's `AnalyticsDb::open` fails (not a valid SQLite DB
    /// → `db = None`) and all events are fail-open dropped. Consumer must
    /// not panic or hang.
    #[test]
    fn test_consumer_db_is_garbage_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("garbage.db");

        // Write garbage bytes — definitely not a valid SQLite DB.
        std::fs::write(&db_path, b"NOT A DATABASE\x00\xff\xfe\x00garbage garbage").unwrap();

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

        for _ in 0..10 {
            hook.on_request(&make_small_event());
        }

        drop(hook);
        let consumer_finished = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        let _ = handle.join();

        assert!(
            consumer_finished,
            "AC15 arm (d): consumer must finish with a garbage DB file — no panic or hang"
        );
    }

    // -------------------------------------------------------------------------
    // AC15 proxy-side: proxy forwards under every hostile DB state
    // -------------------------------------------------------------------------
    //
    // The four consumer-side arms (a)–(d) above prove the consumer never panics
    // or hangs. These proxy-side tests add the complementary assertion: even when
    // analytics recording fails (consumer sees a hostile DB), the HTTP forwarding
    // path is completely unaffected — requests reach the upstream and responses
    // return to the client.
    //
    // PF-012: proxy and upstream both bind via :0 (OS-assigned ephemeral ports).
    // Shared infrastructure: `start_fake_upstream_tcp`, `send_proxy_request_raw_tcp`,
    // `LocalBlockRouterStage` — all defined in the helpers section below.

    /// AC15 proxy-side arm (a): absent DB path (consumer cannot create the DB file
    /// because the parent directory does not exist). Proxy must still forward.
    #[test]
    fn test_proxy_forwards_with_absent_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Path in a non-existent sub-directory → AnalyticsDb::open fails.
        let db_path = dir.path().join("nonexistent_subdir").join("analytics.db");
        proxy_hostile_db_assert(&db_path, "absent DB directory");
    }

    /// AC15 proxy-side arm (b): garbage DB file. Consumer's `AnalyticsDb::open`
    /// sees a non-SQLite file and fails with a parse error. Proxy must still forward.
    #[test]
    fn test_proxy_forwards_with_garbage_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("garbage.db");
        std::fs::write(&db_path, b"NOT A DATABASE\x00\xff\xfe\x00garbage garbage")
            .expect("write garbage");
        proxy_hostile_db_assert(&db_path, "garbage DB file");
    }

    /// AC15 proxy-side arm (c): DB locked by an exclusive connection before the
    /// consumer starts. `AnalyticsDb::open()` fails (the `PRAGMA user_version`
    /// read needs a SHARED lock, denied by the EXCLUSIVE), so `db = None` and
    /// all events are fail-open dropped without any BUSY wait. The proxy must
    /// still forward — that is the assertion here.
    ///
    /// Note: because `AnalyticsDb::open()` fails before `set_busy_timeout(100ms)`
    /// is reached, this test does **not** discriminate the busy_timeout cap; wall-
    /// clock discrimination lives in
    /// `test_consumer_busy_timeout_cap_discriminates_set_busy_timeout`.
    #[test]
    fn test_proxy_forwards_with_locked_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("locked.db");
        // Pre-create the DB so the consumer can attempt to open it.
        {
            let _ = AnalyticsDb::open(&db_path).expect("pre-create locked db");
        }
        // Acquire an exclusive lock that the consumer will hit.
        let lock_conn = rusqlite::Connection::open(&db_path).expect("lock conn");
        let _ = lock_conn.execute_batch("BEGIN EXCLUSIVE");

        proxy_hostile_db_assert(&db_path, "locked DB");

        // Release the lock after the assertion so the tempdir cleanup can succeed.
        let _ = lock_conn.execute_batch("ROLLBACK");
    }

    /// AC15 proxy-side arm (d): read-only directory. Consumer cannot create or open
    /// the DB because the directory is unwritable. Proxy must still forward.
    #[cfg(unix)]
    #[test]
    fn test_proxy_forwards_with_readonly_db_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("noperm.db");

        // Make the directory read-only before the consumer starts.
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(dir.path(), perms.clone()).unwrap();

        proxy_hostile_db_assert(&db_path, "read-only DB directory");

        // Restore write permission so tempdir cleanup succeeds.
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(dir.path(), perms);
    }

    /// Shared helper for the four AC15 proxy-side tests.
    ///
    /// Wires up a `BridgeAnalyticsHook` + consumer against the given (hostile)
    /// `db_path`, then starts a real proxy server (with `LocalBlockRouterStage`)
    /// and a raw-TCP fake upstream, sends one request, and asserts:
    ///
    /// 1. The proxy returned a non-empty response (forwarding succeeded).
    /// 2. The consumer thread finished without panic or hang.
    ///
    /// The consumer's DB writes fail-open under hostile conditions (AD-AN-8 /
    /// AC15); the proxy forwarding path is completely independent of the consumer.
    fn proxy_hostile_db_assert(db_path: &std::path::Path, scenario: &str) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        let (hook, rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let drop_count = hook.drop_count_handle();
        let queued_bytes = hook.queued_bytes_handle();
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);
        let consumer_handle = run_inline_consumer(
            db_path,
            rx,
            Arc::clone(&drop_count),
            Arc::clone(&queued_bytes),
            None,
            done_tx,
        );
        let hook_arc: Arc<dyn AnalyticsHook> = Arc::new(hook);

        let request_body: &[u8] = br#"{"msg":"hostile-db-test"}"#;

        let (response, abort_handle) = rt.block_on(async {
            let (upstream_url, _upstream_abort) = start_fake_upstream_tcp().await;

            let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("proxy bind :0");
            let proxy_addr = proxy_listener.local_addr().expect("proxy local_addr");

            // port(0) would fail ProxyConfig::build() range check (41000-49000).
            // For run_server_with_listener the config port is a placeholder — the
            // actual binding uses the pre-bound TcpListener, not config.bind_addr.
            let config = ProxyConfig::builder()
                .upstream_default(upstream_url)
                .build()
                .expect("proxy config");

            let stage = LocalBlockRouterStage::new();
            let pipeline = TransformPipeline::from_stages(vec![Box::new(stage)]);

            let task = tokio::spawn(rskim_proxy::testing::run_server_with_listener(
                proxy_listener,
                config,
                pipeline,
                Arc::clone(&hook_arc),
            ));

            tokio::time::sleep(Duration::from_millis(100)).await;

            let response = send_proxy_request_raw_tcp(proxy_addr, request_body).await;

            tokio::time::sleep(Duration::from_millis(30)).await;

            (response, task.abort_handle())
        });

        abort_handle.abort();
        drop(hook_arc);
        let consumer_finished = done_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        let _ = consumer_handle.join();

        assert!(
            !response.is_empty(),
            "AC15 proxy-side ({scenario}): proxy must forward the request and return \
             a non-empty response; analytics DB failure must not block forwarding"
        );
        assert!(
            consumer_finished,
            "AC15 proxy-side ({scenario}): consumer must finish without panic or hang"
        );
    }

    // -------------------------------------------------------------------------
    // AC22 — SKIM_DISABLE_ANALYTICS wiring: NoopAnalyticsHook stores nothing
    // -------------------------------------------------------------------------

    /// AC22: when analytics is disabled (`SKIM_DISABLE_ANALYTICS=1`), the
    /// proxy wires `NoopAnalyticsHook` (no consumer thread, no DB writes).
    ///
    /// This test verifies the structural consequence: events routed through
    /// `NoopAnalyticsHook::on_request` leave zero rows in any database —
    /// because `NoopAnalyticsHook` discards events immediately without
    /// enqueuing them to a consumer thread.
    ///
    /// PF-007 discriminating: replacing `NoopAnalyticsHook` with a
    /// `BridgeAnalyticsHook` (real hook) causes the consumer to write rows;
    /// the DB-row count would then be > 0, failing this assertion.
    #[test]
    fn test_noop_hook_writes_zero_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("noop.db");

        // Create a clean DB so we can verify zero rows afterward.
        let db = AnalyticsDb::open(&db_path).expect("open db");

        // Route events through NoopAnalyticsHook — analogous to
        // SKIM_DISABLE_ANALYTICS=1 path in proxy.rs run().
        let noop = rskim_proxy::analytics::NoopAnalyticsHook;
        for _ in 0..50 {
            noop.on_request(&make_small_event());
        }

        // Zero rows must exist — NoopAnalyticsHook never calls record_proxy.
        let summary = db.query_summary(None).expect("query_summary");
        assert_eq!(
            summary.invocations, 0,
            "AC22: NoopAnalyticsHook must leave zero rows in the DB (got {} invocations)",
            summary.invocations
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
        assert!(
            drops >= 1,
            "byte-budget overflow must increment drop_count; got {drops}"
        );
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
        assert!(
            comp.is_some(),
            "compressed_tokens must be Some for valid UTF-8"
        );
        assert!(pct.is_some(), "savings_pct must be Some for valid UTF-8");
        let raw = raw.unwrap();
        let comp = comp.unwrap();
        assert!(raw > 0, "raw_tokens must be positive");
        assert!(comp > 0, "compressed_tokens must be positive");
        assert!(
            raw >= comp,
            "raw_tokens >= compressed_tokens for a shrunk body"
        );
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
        assert!(
            comp.is_none(),
            "non-UTF-8 raw must yield None compressed_tokens"
        );
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
        assert!(
            comp.is_none(),
            "non-UTF-8 final must yield None compressed_tokens"
        );
        assert!(pct.is_none(), "non-UTF-8 final must yield None savings_pct");
    }

    /// AC6 (PF-007 discriminating): a large but valid UTF-8 body IS counted.
    ///
    /// After removing the unplanned 256 KiB size cap (alignment fix), any valid
    /// UTF-8 body is measured regardless of length. Token counting runs on the
    /// consumer thread (off the request path) so large bodies do not add
    /// forwarding latency.
    ///
    /// Discriminator: reinstating a size cap makes large bodies return None and
    /// fails this assertion — proving no size-cap is present (PF-007).
    #[test]
    fn test_compute_token_counts_large_utf8_body_is_counted() {
        // ~512 KiB of valid ASCII — well above any plausible size cap.
        let big = Bytes::from(b"hello ".repeat(512 * 1024 / 6));
        let small = Bytes::from_static(b"hi");
        let event = make_event_with_bodies(big, small);
        let provider = RecordingProvider::Anthropic;
        let (raw, comp, pct) = compute_token_counts(&event, &provider);
        assert!(
            raw.is_some() && comp.is_some() && pct.is_some(),
            "AC6: large valid UTF-8 body must be counted (no size cap); \
             got raw={raw:?}, comp={comp:?}, pct={pct:?}"
        );
    }

    /// AC25 (PF-007 discriminating): a transformed-but-upstream-errored event is
    /// recorded with pair-jointly NULL token columns.
    ///
    /// Discriminator: deleting the `upstream_error_status.is_some()` branch in
    /// `build_proxy_record_input` makes the token columns `Some(...)` and fails
    /// the assertion, which is exactly the "errored request wrongly counted as a
    /// successful savings row" defect AC25 forbids.
    #[test]
    fn test_upstream_errored_event_has_null_token_columns() {
        let errored = ProxyEvent::new(
            ProxyProvider::Anthropic,
            Some("claude-3-5-sonnet-20241022".to_string()),
            None,
            RequestTier::Full,
            Bytes::from_static(b"{\"model\":\"claude-3-5-sonnet-20241022\"}"),
            Bytes::from_static(b"{\"model\":\"claude\"}"),
            vec![BlockDecisionProjection {
                component: "block-router",
                outcome: "full",
                bytes_in: 37,
                bytes_out: 18,
            }],
            Some(502),
            Duration::from_millis(7),
        );
        let input = build_proxy_record_input(&errored, &None);
        assert_eq!(input.upstream_error_status, Some(502));
        assert!(
            input.raw_tokens.is_none()
                && input.compressed_tokens.is_none()
                && input.savings_pct.is_none(),
            "AC25: an upstream-errored row must never carry token counts (got \
             raw={:?}, comp={:?}, pct={:?})",
            input.raw_tokens,
            input.compressed_tokens,
            input.savings_pct
        );
        assert_eq!(
            input.blocks.len(),
            1,
            "AC24: per-block detail rows are still recorded for an errored row"
        );

        // Control: the same event WITHOUT an upstream error is counted, proving
        // the NULLing is scoped to the error outcome and not a blanket change.
        let ok = make_small_event();
        let ok_input = build_proxy_record_input(&ok, &None);
        assert!(
            ok_input.raw_tokens.is_some(),
            "a normally relayed row must still carry measured token counts"
        );
    }

    /// AC24 / AD-AN-13: proxy_block_decisions rows ARE written to the DB for a
    /// transformed-but-upstream-errored event, and satisfy the byte-reconciliation
    /// invariant.
    ///
    /// The existing `test_upstream_errored_event_has_null_token_columns` verifies the
    /// in-memory `ProxyRecordInput.blocks` count. This test verifies the **DB rows**:
    /// the consumer writes `proxy_block_decisions` for errored events (AD-AN-13),
    /// and their byte sums exactly equal the raw and final body lengths.
    ///
    /// **Byte-reconciliation invariant (AD-AN-13):**
    ///   `Σ proxy_block_decisions.bytes_in  == raw_body.len()`
    ///   `Σ proxy_block_decisions.bytes_out == final_body.len()`
    ///
    /// **Discriminating assertion (PF-007):** removing the
    /// `proxy_block_decisions` INSERT from `record_proxy` leaves the table empty,
    /// failing `assert!(!decisions.is_empty(), ...)`.
    /// Introducing a byte-accounting bug in `BlockRouter` (e.g. wrong `bytes_in`)
    /// fails the sum-equality assertions.
    ///
    /// Driven through the real `BlockRouter` with `Policy::Default` on a
    /// compressible body, with `upstream_error_status = Some(502)` to simulate
    /// the transformed-but-errored path (AD-PXY-25).
    #[test]
    fn test_upstream_errored_event_proxy_block_decisions_in_db() {
        use rskim_compress::{BlockRouter, Policy};
        use rskim_contract::log::MockSink;

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("errored_blocks.db");

        // A compressible body the BlockRouter will transform (real decision records).
        let raw_body_bytes: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":"Summarise: The quick brown fox jumped over the lazy dog. The quick brown fox jumped over the lazy dog. The quick brown fox jumped over the lazy dog."}]}"#;

        // Run the real BlockRouter to get real per-block decision records.
        let per_call_sink = MockSink::new();
        let router = BlockRouter::new(Arc::new(TestDecisionSinkStub));
        let outcome = router.route(raw_body_bytes, Policy::Default, "ac24-test", &per_call_sink);
        let final_body = outcome.bytes.clone();
        let block_decisions_raw = per_call_sink.drain();

        // Verify body was actually compressed (invariant needed for non-trivial assertion).
        assert!(
            !block_decisions_raw.is_empty(),
            "AC24: BlockRouter must produce at least one decision for a compressible body"
        );

        // Build BlockDecisionProjections from real decisions.
        let block_decisions: Vec<BlockDecisionProjection> = block_decisions_raw
            .iter()
            .map(|r| BlockDecisionProjection {
                component: r.component,
                outcome: outcome_static_str(r.reason.clone()),
                bytes_in: r.bytes_in,
                bytes_out: r.bytes_out,
            })
            .collect();

        // Build a ProxyEvent as if the upstream returned 502 after the transformation.
        // upstream_error_status=Some(502) marks this as "transformed-but-upstream-errored"
        // (AD-PXY-25): token columns will be NULL, but block decisions are still written.
        let event = ProxyEvent::new(
            ProxyProvider::Anthropic,
            Some("claude-3-5-sonnet-20241022".to_string()),
            None,
            derive_tier_from_decisions(&block_decisions_raw),
            Bytes::copy_from_slice(raw_body_bytes),
            Bytes::from(final_body.clone()),
            block_decisions,
            Some(502), // upstream error status
            Duration::from_millis(15),
        );

        // Push through the production consumer.
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
        drop(hook); // close sender → consumer drains and exits
        let _ = done_rx.recv_timeout(Duration::from_secs(10));
        let _ = handle.join();

        // Verify proxy_block_decisions rows in the DB.
        let db = AnalyticsDb::open(&db_path).expect("AC24: open DB");
        let decisions = db.all_block_decisions().expect("AC24: all_block_decisions");

        // Discriminating assertion: rows must be present (AD-AN-13).
        assert!(
            !decisions.is_empty(),
            "AC24/AD-AN-13: proxy_block_decisions must have rows for an errored event; \
             deleting the INSERT from record_proxy leaves the table empty and fails here"
        );

        // AD-AN-13: byte-reconciliation invariant.
        //
        // IMPORTANT: bytes_in / bytes_out in DecisionRecord are PER-BLOCK content
        // lengths (the text string length of each processed candidate block), NOT
        // the full raw/final body lengths.  `Σ bytes_in` therefore equals the sum
        // of content-string lengths across all candidate blocks — not raw_body.len().
        //
        // The correct invariant is:
        //   Σ DB.bytes_in  == Σ BlockRouter.DecisionRecord.bytes_in
        //   Σ DB.bytes_out == Σ BlockRouter.DecisionRecord.bytes_out
        //
        // This proves that the recording pipeline faithfully stores what the
        // BlockRouter reported, without truncation, corruption, or omission.
        //
        // Discriminating (PF-007): introducing a bug in record_proxy that stores 0
        // instead of the actual bytes_in/out values makes these equal fail.
        let expected_bytes_in: u64 = block_decisions_raw.iter().map(|d| d.bytes_in as u64).sum();
        let expected_bytes_out: u64 = block_decisions_raw.iter().map(|d| d.bytes_out as u64).sum();

        let sum_bytes_in: u64 = decisions.iter().map(|d| d.bytes_in).sum();
        let sum_bytes_out: u64 = decisions.iter().map(|d| d.bytes_out).sum();

        assert!(
            expected_bytes_in > 0,
            "AC24: BlockRouter must process some bytes (expected_bytes_in=0 indicates \
             the test body was not compressed — use a compressible body)"
        );

        assert_eq!(
            sum_bytes_in, expected_bytes_in,
            "AC24/AD-AN-13: Σ DB.bytes_in ({sum_bytes_in}) must equal \
             Σ BlockRouter.bytes_in ({expected_bytes_in}); \
             a byte-accounting bug in record_proxy (e.g. storing 0) causes divergence"
        );
        assert_eq!(
            sum_bytes_out, expected_bytes_out,
            "AC24/AD-AN-13: Σ DB.bytes_out ({sum_bytes_out}) must equal \
             Σ BlockRouter.bytes_out ({expected_bytes_out}); \
             a byte-accounting bug in record_proxy (e.g. storing 0) causes divergence"
        );
        assert!(
            sum_bytes_out <= sum_bytes_in,
            "AC24/AD-AN-13: compressed bytes ({sum_bytes_out}) must not exceed raw bytes \
             ({sum_bytes_in}); the never-inflate byte gate must have been respected"
        );

        // AD-PXY-25: the parent row must be counted as an upstream error (not as savings).
        let upstream_error_count = db
            .query_by_upstream_error(None)
            .expect("AC24: query_by_upstream_error");
        assert_eq!(
            upstream_error_count, 1,
            "AC24/AD-PXY-25: the errored event must appear in query_by_upstream_error"
        );
    }

    /// AD-PXY-22: `store_model` stores the model string exactly as supplied.
    ///
    /// After removing the unplanned MAX_MODEL_LEN truncation (alignment fix),
    /// the model string is stored verbatim — no truncation, no folding.
    /// Grouping is by exact string match (AD-PXY-22).
    ///
    /// PF-007 discriminating: reinstating truncation would make the long-model
    /// assertion fail, proving the verbatim path is load-bearing.
    #[test]
    fn test_store_model_is_verbatim() {
        assert_eq!(store_model(None), None, "None model stays None");
        assert_eq!(
            store_model(Some("claude-3-5-sonnet-20241022")).as_deref(),
            Some("claude-3-5-sonnet-20241022"),
            "AD-PXY-22: model identifier stored verbatim"
        );

        // A string longer than any plausible cap — must survive untruncated.
        let long_model = "a".repeat(512);
        assert_eq!(
            store_model(Some(&long_model)).as_deref(),
            Some(long_model.as_str()),
            "AD-PXY-22: long model stored verbatim (no truncation)"
        );

        // Multi-byte model string — no character-boundary truncation occurs.
        let unicode_model = "claude-日-3".to_string();
        assert_eq!(
            store_model(Some(&unicode_model)).as_deref(),
            Some(unicode_model.as_str()),
            "AD-PXY-22: multi-byte model stored verbatim"
        );
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
        assert!(
            consumer_finished,
            "consumer must signal completion after channel closes"
        );
    }

    /// AC23 / PF-007: a **real proxy server** with `LocalBlockRouterStage` produces
    /// a DB row with real measured token counts. No `if let` escape hatch.
    ///
    /// Drives the complete stack end-to-end through a live HTTP proxy:
    ///
    /// ```text
    /// raw-TCP client → proxy (LocalBlockRouterStage / Policy::Default)
    ///   → raw-TCP fake upstream → BridgeAnalyticsHook → consumer thread → AnalyticsDb
    /// ```
    ///
    /// **Discriminating assertions (PF-007 — deleting any guarded path fails the test):**
    ///
    /// - `tier_full_pct > 0 || tier_degraded_pct > 0` — the body is highly
    ///   compressible; block-router must fire. Removing `LocalBlockRouterStage` from
    ///   the pipeline causes all traffic to be passthrough, zeroing both percentages.
    /// - `.expect("...")` on `raw_tokens` / `compressed_tokens` — no `if let` guard;
    ///   removing token counting from the consumer leaves these `None` and panics here.
    /// - `raw_tok > comp_tok` — actual bytes were saved. A no-op transform produces
    ///   equal counts, failing this strict-greater-than assertion.
    ///
    /// **PF-012:** both proxy and upstream bind via `:0` (OS-assigned ephemeral
    /// ports); inter-test port collisions are impossible.
    #[test]
    fn test_e2e_block_router_full_tier_row_with_real_token_delta() {
        // AC23: the user message content must be JSON (start with `{` so the classifier
        // routes it to Class::Json → JSON engine → OutcomeReason::Full).
        //
        // Plain English text is Class::Text → Passthrough (prefilter rejects it;
        // class_max returns 0 for Text). Use a spaced-out JSON object instead:
        // the JSON engine minifies whitespace → bytes_out < bytes_in → tier=full.
        let raw_body: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":"{ \"results\": [ { \"id\": 1, \"score\": 0.95, \"text\": \"alpha result\" }, { \"id\": 2, \"score\": 0.87, \"text\": \"beta result\" }, { \"id\": 3, \"score\": 0.72, \"text\": \"gamma result\" }, { \"id\": 4, \"score\": 0.61, \"text\": \"delta result\" }, { \"id\": 5, \"score\": 0.55, \"text\": \"epsilon result\" } ] }"}]}"#;

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("e2e.db");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("AC23: tokio runtime");

        // Wire up the consumer thread BEFORE the proxy starts so events are not
        // lost between proxy startup and consumer launch.
        let (hook, rx) = BridgeAnalyticsHook::new(PROXY_QUEUE_RECORD_CAPACITY);
        let drop_count = hook.drop_count_handle();
        let queued_bytes = hook.queued_bytes_handle();
        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);
        let consumer_handle = run_inline_consumer(
            &db_path,
            rx,
            Arc::clone(&drop_count),
            Arc::clone(&queued_bytes),
            None,
            done_tx,
        );
        // Wrap in Arc<dyn AnalyticsHook> for run_server_with_listener.
        let hook_arc: Arc<dyn AnalyticsHook> = Arc::new(hook);

        // Start fake upstream + real proxy, send the compressible request.
        let abort_handle = rt.block_on(async {
            // PF-012: fake upstream on OS-assigned port.
            let (upstream_url, _upstream_abort) = start_fake_upstream_tcp().await;

            // PF-012: proxy on OS-assigned port.
            let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("AC23: proxy bind :0");
            let proxy_addr = proxy_listener.local_addr().expect("proxy local_addr");

            // port(0) would fail ProxyConfig::build() range check (41000-49000).
            // For run_server_with_listener the config port is a placeholder — the
            // actual binding uses the pre-bound TcpListener, not config.bind_addr.
            let config = ProxyConfig::builder()
                .upstream_default(upstream_url)
                .build()
                .expect("AC23: proxy config");

            // LocalBlockRouterStage routes ApiKey requests via Policy::Default (D1).
            let stage = LocalBlockRouterStage::new();
            let pipeline = TransformPipeline::from_stages(vec![Box::new(stage)]);

            let task = tokio::spawn(rskim_proxy::testing::run_server_with_listener(
                proxy_listener,
                config,
                pipeline,
                Arc::clone(&hook_arc),
            ));

            // Give the proxy time to bind and accept connections.
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Send the compressible body via raw TCP (x-api-key → ApiKey → Policy::Default).
            let response = send_proxy_request_raw_tcp(proxy_addr, raw_body).await;
            assert!(
                !response.is_empty(),
                "AC23: proxy must return a response (forwarding itself failed)"
            );

            // Brief pause so on_request has enqueued the event before abort.
            tokio::time::sleep(Duration::from_millis(50)).await;

            task.abort_handle()
        });

        // Aborting the task drops its Arc<dyn AnalyticsHook> clone.
        // Dropping hook_arc too → Arc refcount reaches 0 → hook drops → sender closes
        // → consumer drains remaining events → done_tx fires.
        abort_handle.abort();
        drop(hook_arc);

        let consumer_finished = done_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        let _ = consumer_handle.join();

        assert!(
            consumer_finished,
            "AC23: consumer must finish after proxy abort"
        );

        // Assert DB row — unconditionally (no `if let` escape hatch — PF-007).
        let db = AnalyticsDb::open(&db_path).expect("AC23: open DB");
        let rows = db.query_by_model(None).expect("AC23: query_by_model");

        assert!(
            !rows.is_empty(),
            "AC23: at least one proxy row must be recorded"
        );

        let row = &rows[0];

        // Tier: block-router fired → must be full or degraded, never passthrough only.
        // Deleting LocalBlockRouterStage from the pipeline zeroes both percentages.
        assert!(
            row.tier_full_pct > 0.0 || row.tier_degraded_pct > 0.0,
            "AC23: tier must be full or degraded for a compressible body \
             (full={:.1}%, degraded={:.1}%, passthrough={:.1}%)",
            row.tier_full_pct,
            row.tier_degraded_pct,
            row.tier_passthrough_pct
        );

        // Token counts must be Some (no NULL) and strictly positive.
        // `.expect()` is the discriminating assertion — no `if let` guard.
        // Removing token counting leaves raw_tokens=None and panics here.
        let raw_tok = row.raw_tokens.expect(
            "AC23: raw_tokens must be Some — NULL means token counting was omitted (PF-007)",
        );
        let comp_tok = row
            .compressed_tokens
            .expect("AC23: compressed_tokens must be Some — NULL means token counting was omitted");

        assert!(raw_tok > 0, "AC23: raw_tokens ({raw_tok}) must be positive");
        assert!(
            comp_tok > 0,
            "AC23: compressed_tokens ({comp_tok}) must be positive"
        );
        assert!(
            raw_tok > comp_tok,
            "AC23: raw_tokens ({raw_tok}) must exceed compressed_tokens ({comp_tok}); \
             a no-op transform produces equal counts and would fail this assertion"
        );
        assert!(
            row.avg_savings_pct.is_some(),
            "AC23: avg_savings_pct must be Some for a compressed row"
        );
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
    fn derive_tier_from_decisions(records: &[rskim_contract::log::DecisionRecord]) -> RequestTier {
        use rskim_contract::log::OutcomeReason;
        let block_router: Vec<_> = records
            .iter()
            .filter(|r| r.component == "block-router")
            .collect();
        if block_router
            .iter()
            .any(|r| r.reason == OutcomeReason::Degraded)
        {
            RequestTier::Degraded
        } else if block_router.iter().any(|r| r.reason == OutcomeReason::Full) {
            RequestTier::Full
        } else {
            RequestTier::Passthrough
        }
    }

    // -------------------------------------------------------------------------
    // Async proxy-server infrastructure (AC23 / AC15 proxy-side)
    // -------------------------------------------------------------------------

    /// Test-local BlockRouter adapter: mirrors the production `BlockRouterStage`
    /// in `cmd/proxy.rs` (private struct, cannot be imported as a library).
    ///
    /// `Send + Sync` is auto-derived because `BlockRouter` is `Send + Sync`
    /// (its only field is `Arc<dyn DecisionSink>` where `DecisionSink: Send + Sync`).
    ///
    /// Auth mode → Policy mapping (D1 / AD-PXY-08):
    /// - `ApiKey` / `Ambiguous` → `Policy::Default` (full compression allowed)
    /// - `Subscription`         → `Policy::LosslessOnly` (conservative)
    struct LocalBlockRouterStage {
        router: rskim_compress::BlockRouter,
    }

    impl LocalBlockRouterStage {
        fn new() -> Self {
            Self {
                router: rskim_compress::BlockRouter::new(Arc::new(TestDecisionSinkStub)),
            }
        }
    }

    impl TransformStage for LocalBlockRouterStage {
        fn name(&self) -> &'static str {
            "block-router"
        }

        fn apply(
            &self,
            body: &[u8],
            ctx: &TransformContext<'_>,
            sink: &dyn DecisionSink,
        ) -> Outcome {
            use rskim_compress::Policy;
            let policy = match ctx.auth_mode {
                AuthMode::Subscription => Policy::LosslessOnly,
                // ApiKey, Ambiguous → Policy::Default (D1)
                _ => Policy::Default,
            };
            self.router.route(body, policy, ctx.request_id, sink)
        }
    }

    /// Start a raw-TCP fake upstream server. Returns `(upstream_url, abort_handle)`.
    ///
    /// Accepts each connection, drains up to 64 KiB of incoming bytes (the proxy's
    /// forwarded request), then sends a minimal HTTP/1.1 200 response and closes.
    /// `Connection: close` tells the proxy-client that the response body ends when
    /// the connection closes.
    ///
    /// **PF-012:** binds to `127.0.0.1:0`; OS assigns an ephemeral port.
    async fn start_fake_upstream_tcp() -> (String, tokio::task::AbortHandle) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake upstream bind :0");
        let addr = listener.local_addr().expect("fake upstream local_addr");
        let url = format!("http://{addr}");

        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    // Drain the incoming request (necessary so the proxy-client
                    // doesn't get a broken-pipe writing the body).
                    let mut buf = vec![0u8; 65536];
                    let _ = stream.read(&mut buf).await;
                    // Respond with a minimal HTTP/1.1 200.
                    let resp = b"HTTP/1.1 200 OK\r\n\
                        Content-Type: application/json\r\n\
                        Content-Length: 2\r\n\
                        Connection: close\r\n\
                        \r\n{}";
                    let _ = stream.write_all(resp).await;
                    // Drop stream → TCP FIN → proxy-client reads EOF → response done.
                });
            }
        });

        (url, task.abort_handle())
    }

    /// Send a raw HTTP/1.1 POST to the proxy and return the response bytes.
    ///
    /// Sets `x-api-key` so the proxy classifies the request as
    /// `AuthMode::ApiKey → Policy::Default` (D1 / AD-PXY-08).
    /// Uses `Connection: close` so `read_to_end` terminates when the proxy closes.
    async fn send_proxy_request_raw_tcp(proxy_addr: std::net::SocketAddr, body: &[u8]) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let headers = format!(
            "POST /v1/messages HTTP/1.1\r\n\
             Host: {proxy_addr}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             x-api-key: test-api-key-not-real\r\n\
             Connection: close\r\n\
             \r\n",
            body.len()
        );
        let mut stream = tokio::net::TcpStream::connect(proxy_addr)
            .await
            .expect("connect to proxy");
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("write request headers");
        stream.write_all(body).await.expect("write request body");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        response
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
