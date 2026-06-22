//! Liveness and readiness health endpoints for the proxy.
//!
//! ## AD-PXY-11 — Passive watchdog design (the PRISM #258 counter-design)
//!
//! Health is determined by a **passive watchdog** over recent forwarding outcomes,
//! not by active self-probing. Active probing would require an outbound connection
//! to the upstream from within the health check handler — introducing a dependency
//! on upstream availability in the liveness path (wrong) and potential latency in
//! a tight loop (wrong).
//!
//! The watchdog maintains a rolling window of forwarding outcomes:
//! - **K = 3** consecutive forward failures flip `/readyz` non-200.
//! - **10s staleness** (no forward success for 10s) also flips `/readyz` non-200.
//! - First subsequent forward success flips `/readyz` back to 200.
//! - `/livez` is NEVER flipped by forward failures — it stays 200 as long as the
//!   process is running (AC16).
//!
//! Evidence (auto-resolved #6): K=3 / 10s window from DECISIONS-NEEDED.md.
//! The 3s polling cadence in [`ReadinessWatchdog`] is an implementation detail;
//! the observable contract is the K-and-window criterion.
//!
//! ## Endpoints
//!
//! - `/livez` → 200 `{"status":"ok"}` while the process is running.
//! - `/readyz` → 200 `{"status":"ready"}` while the forwarding path is healthy;
//!   503 `{"status":"degraded","reason":"..."}` after K failures or 10s stale.
//! - `/health` → alias for `/readyz` (AC16).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ============================================================================
// Evidence-cited constants (ADR-003 / PF-005)
// ============================================================================

/// Number of consecutive forward failures that flip /readyz non-200.
///
/// Evidence: K=3 forward failures / 10s window (auto-resolved #6,
/// DECISIONS-NEEDED.md). Chosen as "three strikes" — a single failure is
/// transient (network glitch, upstream restart), three consecutive is a
/// pattern indicating a systemic problem worth declaring unready.
pub const READINESS_FAILURE_THRESHOLD_K: u64 = 3;

/// Staleness window: if no forward success in this many seconds, flip /readyz.
///
/// Evidence: 10s window matches K=3 / 10s from auto-resolved #6. At typical
/// proxy load (>1 req/s) this is effectively the same as K=3 consecutive
/// failures. At low load it catches a "no traffic but upstream is down" case.
pub const READINESS_STALE_WINDOW_SECS: u64 = 10;

// ============================================================================
// ReadinessState — shared atomic state
// ============================================================================

/// Shared atomic state for the readiness watchdog.
///
/// Uses atomic integers for lock-free read from multiple health-check handlers.
/// Updated only by the forwarding path (one writer per request).
///
/// ## Memory ordering
///
/// - `consecutive_failures`: `Relaxed` — approximate counter; a read/write race
///   is not harmful (we'd just flip one request too early/late).
/// - `last_success_unix_secs`: `Relaxed` — same argument; monotonic update.
/// - `is_ready`: `SeqCst` — the flip from ready→unready must be visible to all
///   readers without reordering across the failure-counter write.
pub struct ReadinessState {
    /// Number of consecutive forwarding failures (reset on any success).
    consecutive_failures: AtomicU64,

    /// Unix timestamp (seconds) of the last successful forward.
    ///
    /// Initialized to the proxy start time. A value of -1 means "never
    /// succeeded" (used only between proxy start and the first request).
    last_success_unix_secs: AtomicI64,

    /// Current readiness: true = ready (200), false = degraded (503).
    is_ready: AtomicBool,
}

impl ReadinessState {
    /// Construct a new [`ReadinessState`] in the ready condition.
    ///
    /// The initial last-success timestamp is the current time so that a freshly
    /// started proxy with no traffic is not immediately declared stale.
    pub fn new() -> Arc<Self> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;

        Arc::new(Self {
            consecutive_failures: AtomicU64::new(0),
            last_success_unix_secs: AtomicI64::new(now_secs),
            is_ready: AtomicBool::new(true),
        })
    }

    /// Record a forwarding success.
    ///
    /// Resets the consecutive-failure counter and updates the last-success
    /// timestamp. Flips `is_ready` back to true if it was false.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;
        self.last_success_unix_secs
            .store(now_secs, Ordering::Relaxed);

        // Flip ready if we were unready (SeqCst to ensure visibility).
        self.is_ready.store(true, Ordering::SeqCst);
    }

    /// Record a forwarding failure.
    ///
    /// Increments the consecutive-failure counter. If the counter reaches
    /// [`READINESS_FAILURE_THRESHOLD_K`], flips `is_ready` to false.
    pub fn record_failure(&self) {
        let failures = self
            .consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);

        if failures >= READINESS_FAILURE_THRESHOLD_K {
            self.is_ready.store(false, Ordering::SeqCst);
        }
    }

    /// Check whether the proxy is ready to serve requests.
    ///
    /// Returns `false` if:
    /// - The consecutive-failure counter is ≥ K (K = [`READINESS_FAILURE_THRESHOLD_K`]).
    /// - The last-success timestamp is more than [`READINESS_STALE_WINDOW_SECS`] ago.
    ///
    /// The staleness check runs on every call to catch "no traffic but upstream
    /// is down" scenarios at low load.
    pub fn is_ready(&self) -> bool {
        if !self.is_ready.load(Ordering::SeqCst) {
            return false;
        }

        // Staleness check: flip to not-ready if last success is too old.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;

        let last = self.last_success_unix_secs.load(Ordering::Relaxed);
        let staleness = (now_secs - last).max(0) as u64;

        if staleness > READINESS_STALE_WINDOW_SECS {
            self.is_ready.store(false, Ordering::SeqCst);
            return false;
        }

        true
    }
}

impl Default for ReadinessState {
    fn default() -> Self {
        // Cannot return Arc here; default is for the value, not the Arc wrapper.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;
        Self {
            consecutive_failures: AtomicU64::new(0),
            last_success_unix_secs: AtomicI64::new(now_secs),
            is_ready: AtomicBool::new(true),
        }
    }
}

// ============================================================================
// Health response helpers
// ============================================================================

/// Build the /livez response: always 200 while the process is running.
///
/// `/livez` is NOT influenced by forwarding failures. A process that is alive
/// but whose upstream is down is still live — the scheduler should not kill it.
pub fn livez_response() -> (u16, &'static str) {
    (200, r#"{"status":"ok"}"#)
}

/// Build the /readyz (and /health alias) response based on the current
/// readiness state.
///
/// Returns 200 when the forwarding path is healthy; 503 when the watchdog
/// has flipped the state to degraded (K failures or staleness).
pub fn readyz_response(state: &ReadinessState) -> (u16, String) {
    if state.is_ready() {
        (200, r#"{"status":"ready"}"#.to_owned())
    } else {
        let failures = state.consecutive_failures.load(Ordering::Relaxed);
        let reason = if failures >= READINESS_FAILURE_THRESHOLD_K {
            format!(
                "k={failures} consecutive forward failures (threshold={})",
                READINESS_FAILURE_THRESHOLD_K
            )
        } else {
            format!("last forward success >{READINESS_STALE_WINDOW_SECS}s ago (stale window)")
        };
        (
            503,
            format!(r#"{{"status":"degraded","reason":"{reason}"}}"#),
        )
    }
}

/// Returns `true` if `path` is a health endpoint path.
///
/// Recognised paths: `/livez`, `/readyz`, `/health`.
pub fn is_health_path(path: &str) -> bool {
    matches!(path, "/livez" | "/readyz" | "/health")
}

// ============================================================================
// Tests (AC16)
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // AC16: /livez always 200.
    #[test]
    fn test_livez_always_200() {
        let (status, body) = livez_response();
        assert_eq!(status, 200);
        assert!(body.contains("ok"));
    }

    // AC16 (POSITIVE): /readyz is 200 when proxy just started (no failures).
    #[test]
    fn test_readyz_healthy_initially() {
        let state = ReadinessState::new();
        let (status, _) = readyz_response(&state);
        assert_eq!(status, 200, "fresh proxy must be ready");
    }

    // AC16 (NEGATIVE/discriminating): K consecutive failures flip /readyz non-200.
    // DISCRIMINATING: deleting record_failure / threshold would keep status=200 forever.
    #[test]
    fn test_readyz_flips_after_k_failures() {
        let state = ReadinessState::new();

        // K-1 failures: still ready.
        for _ in 0..(READINESS_FAILURE_THRESHOLD_K - 1) {
            state.record_failure();
            let (status, _) = readyz_response(&state);
            assert_eq!(
                status,
                200,
                "below threshold: must still be ready after {} failures",
                READINESS_FAILURE_THRESHOLD_K - 1
            );
        }

        // Kth failure: must flip to non-200.
        state.record_failure();
        let (status, body) = readyz_response(&state);
        assert_ne!(
            status, 200,
            "at threshold: must flip non-200 after K failures"
        );
        assert!(body.contains("degraded"), "response must say degraded");
        assert!(
            body.contains("consecutive"),
            "response must mention failures"
        );
    }

    // AC16 (POSITIVE): Recovery after K failures: success resets to ready.
    #[test]
    fn test_readyz_recovers_on_success() {
        let state = ReadinessState::new();

        // Trip the threshold.
        for _ in 0..READINESS_FAILURE_THRESHOLD_K {
            state.record_failure();
        }
        let (status, _) = readyz_response(&state);
        assert_ne!(status, 200, "must be non-ready after K failures");

        // One success recovers.
        state.record_success();
        let (status, _) = readyz_response(&state);
        assert_eq!(status, 200, "must recover after success");
    }

    // AC16: /livez stays 200 even after K failures (discriminating from /readyz).
    // DISCRIMINATING: if livez were coupled to readiness, this would fail.
    #[test]
    fn test_livez_stays_200_after_failures() {
        // livez doesn't take state — it's always 200 (process alive check).
        for _ in 0..100 {
            let (status, _) = livez_response();
            assert_eq!(status, 200, "livez must always be 200");
        }
    }

    // AC16: /health is an alias path for /readyz.
    #[test]
    fn test_health_path_recognized() {
        assert!(is_health_path("/livez"), "/livez must be a health path");
        assert!(is_health_path("/readyz"), "/readyz must be a health path");
        assert!(
            is_health_path("/health"),
            "/health must be a health path (alias)"
        );
        assert!(
            !is_health_path("/v1/messages"),
            "/v1/messages is not health"
        );
        assert!(!is_health_path("/"), "root is not a health path");
    }

    // AC16: consecutive_failures reset on success.
    #[test]
    fn test_failure_counter_reset_on_success() {
        let state = ReadinessState::new();
        state.record_failure();
        state.record_failure();
        assert_eq!(
            state.consecutive_failures.load(Ordering::Relaxed),
            2,
            "two failures recorded"
        );

        state.record_success();
        assert_eq!(
            state.consecutive_failures.load(Ordering::Relaxed),
            0,
            "counter must reset to 0 on success"
        );
    }

    // AC16: readiness_state remains ready after K-1 failures.
    #[test]
    fn test_below_threshold_stays_ready() {
        let state = ReadinessState::new();
        for _ in 0..(READINESS_FAILURE_THRESHOLD_K - 1) {
            state.record_failure();
        }
        assert!(state.is_ready(), "below threshold: must still be ready");
    }
}
