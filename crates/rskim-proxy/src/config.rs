//! Proxy configuration: bind address, port, upstreams, and lifecycle bounds.
//!
//! [`ProxyConfig`] is the single validated configuration object consumed by
//! [`crate::serve()`]. All values are parsed from CLI flags only; no config file
//! or environment variables are consulted (modes-via-flags-only policy).
//!
//! ## AD-PXY-03 — Bind address and port policy
//!
//! Default bind address: `127.0.0.1` (loopback). Overridable with `--bind`.
//! A non-loopback `--bind` MUST emit a cleartext-exposure warning to stderr
//! before serving (AC1 — a proxy bound to a non-loopback address is reachable
//! from other hosts and may expose auth material in transit).
//!
//! Default port: `41322` (within the 41000–49000 range, Windows-bindable, not
//! 8787; see D8 and AD-PXY-03). The range 41000–49000 is outside Windows'
//! ephemeral port range (49152–65535) and outside common development tool ports.
//! Port 8787 is excluded because it was chosen by PRISM (#589, #101) and is
//! Windows-excluded.
//!
//! ## AD-PXY-10 — Request-body max-size bound
//!
//! Request bodies larger than [`DEFAULT_MAX_BODY_BYTES`] (64 MiB) are rejected
//! at the buffering stage (http_body_util::Limited). The #303 implementation
//! returns a 400 for oversize bodies; a full streaming-passthrough path is a
//! deferred enhancement for #304 (requires the forward layer's streaming path).
//! This aligns with `rskim-llm`'s `ChunkIngestionBuilder` 64 MiB cap.
//!
//! ## AD-PXY-14 — Lifecycle bounds (auto-resolved #6)
//!
//! Fixed lifecycle intervals documented with their defensible evidence:
//! - `upstream_timeout_secs = 60` — upstream providers typically respond within
//!   30s; 60s gives a 2× margin for slow networks and avoids premature 504s on
//!   large uploads (AC20).
//! - `client_disconnect_cancel_ms = 500` — sub-second upstream cancel on client
//!   drop; 500ms is generous enough for OS TCP teardown notification without
//!   leaking resources for multiple seconds (AC21).
//! - `graceful_drain_secs = 5` — long enough for in-flight SSE streams to
//!   finish their current event; short enough that the process exits promptly
//!   after SIGINT (AC23).
//! - `readiness_flip_secs = 3` — evidence: K=3 forward failures in a 10s window
//!   (auto-resolved #6 from DECISIONS-NEEDED.md). The watchdog flips /readyz
//!   to non-200 after 3 consecutive forward failures or after 10s of no forward
//!   success, then flips back on recovery (AC16 / AD-PXY-11).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::errors::ProxyError;

// ============================================================================
// Lifecycle defaults (AD-PXY-14 / auto-resolved #6)
// ============================================================================

/// Default upstream timeout: 60 seconds.
///
/// Evidence: upstream LLM providers typically respond within 30s; 60s provides
/// a 2× margin for slow networks and avoids premature 504s on large uploads.
/// Per ADR-003 / PF-005: not a baseless figure — derived from provider SLA
/// observations documented in the plan (auto-resolved #6).
pub const DEFAULT_UPSTREAM_TIMEOUT_SECS: u64 = 60;

/// Default client-disconnect upstream cancel interval: 500 milliseconds.
///
/// Evidence: sub-second upstream cancel on client drop. 500ms is generous for
/// OS TCP teardown notification without leaking per-request resources for multiple
/// seconds. Per ADR-003 / PF-005: not baseless — bounded by OS TCP teardown
/// propagation observed at <100ms on localhost; 500ms gives 5× margin for
/// cross-network client drops (auto-resolved #6).
pub const DEFAULT_CLIENT_DISCONNECT_CANCEL_MS: u64 = 500;

/// Default graceful drain interval on SIGINT/SIGTERM: 5 seconds.
///
/// Evidence: long enough for in-flight SSE streams to deliver the current event
/// (typical SSE event < 1KB at <10ms serialize time ≪ 5s); short enough that
/// the process exits promptly after operator signal (auto-resolved #6, AC23).
pub const DEFAULT_GRACEFUL_DRAIN_SECS: u64 = 5;

/// Readiness flip interval: 3 seconds.
///
/// Evidence: K=3 forward failures / 10s window (auto-resolved #6, DECISIONS-NEEDED.md).
/// After K=3 consecutive forward failures OR last-forward-success staleness >10s,
/// /readyz flips to non-200. Flip-back on first subsequent forward success.
/// The 3s polling cadence is an implementation detail; the observable contract is
/// the K-and-window evidence criterion (AD-PXY-11 / AC16).
pub const DEFAULT_READINESS_FLIP_SECS: u64 = 3;

// ============================================================================
// Body size bound (AD-PXY-10)
// ============================================================================

/// Request-body buffering bound: 64 MiB.
///
/// Aligns with `rskim-llm`'s `ChunkIngestionBuilder` 64 MiB cap as the upper
/// reference. The #303 implementation enforces this via `http_body_util::Limited`;
/// bodies exceeding this limit are rejected with 400 (a streaming-passthrough path
/// is a #304 enhancement). The limit does NOT apply to response bodies — those
/// are always streamed chunk-by-chunk without buffering (AC7).
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

// ============================================================================
// Port (AD-PXY-03 / D8)
// ============================================================================

/// Default proxy port: 41322.
///
/// Selected within the 41000–49000 range (D8) which is:
/// - Outside Windows' ephemeral port range (49152–65535) — Windows-bindable.
/// - Outside common development tool ports (3000, 5173, 8080, 8787, etc.).
/// - Not 8787 — explicitly excluded (PRISM #589 / #101 chose that port;
///   it is Windows-excluded per Windows port exclusion ranges).
///
/// The 41000–49000 range was chosen to avoid NAT router defaults (32768–60999
/// on Linux) while remaining reliably bindable on all target platforms including
/// Windows Server. 41322 is arbitrary within the range and collision-unlikely.
pub const DEFAULT_PROXY_PORT: u16 = 41322;

/// Minimum allowed port (inclusive, D8).
pub const PORT_RANGE_MIN: u16 = 41000;

/// Maximum allowed port (inclusive, D8).
pub const PORT_RANGE_MAX: u16 = 49000;

// ============================================================================
// ProxyConfig
// ============================================================================

/// Validated proxy configuration.
///
/// Constructed by `cmd/proxy.rs` from CLI flags only (no env vars consulted;
/// modes-via-flags-only policy). Passed to [`crate::serve()`]. All values are
/// validated at construction time — downstream code can trust them.
///
/// ## Non-exhaustive
///
/// Marked `#[non_exhaustive]` so that adding lifecycle bounds or per-provider
/// options in future tickets does not break existing construction sites that
/// build via [`ProxyConfig::builder()`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Local address to bind the listening socket.
    ///
    /// Default: `127.0.0.1:41322`. Override with `--bind <addr>` and `--port <P>`.
    ///
    /// # Cleartext warning (AC1 / AD-PXY-03)
    ///
    /// If the IP address is not a loopback address, [`ProxyConfig::builder()`]
    /// sets [`ProxyConfig::warn_cleartext`] to `true`. The caller (cmd/proxy.rs)
    /// MUST emit the warning to stderr before calling [`crate::serve()`].
    pub bind_addr: SocketAddr,

    /// Emit a cleartext-exposure warning before serving.
    ///
    /// Set to `true` when [`bind_addr`] is not a loopback address (AC1).
    pub warn_cleartext: bool,

    /// Default upstream base URL (required, D8).
    ///
    /// An Unknown-provider request (or any request when no per-provider upstream
    /// is configured for the matched provider) is forwarded here. If `None` and
    /// an Unknown-provider request arrives, the proxy responds 502 (D8 / AC3).
    ///
    /// Example: `https://api.anthropic.com`
    pub upstream_default: Option<String>,

    /// Per-provider upstream base URL override.
    ///
    /// Keyed by provider name (`"anthropic"`, `"openai"`). Falls back to
    /// [`upstream_default`] when the provider has no specific entry.
    pub upstream_providers: std::collections::HashMap<String, String>,

    /// Maximum request body bytes to buffer for transform and detection.
    ///
    /// Bodies larger than this cause the request to abort with 400 in #303.
    /// A streaming-passthrough path for oversize bodies is deferred to #304.
    /// Default: [`DEFAULT_MAX_BODY_BYTES`] (64 MiB). AD-PXY-10.
    pub max_body_bytes: usize,

    /// Upstream connect + response timeout.
    ///
    /// Default: [`DEFAULT_UPSTREAM_TIMEOUT_SECS`] (60s). AD-PXY-14.
    pub upstream_timeout: Duration,

    /// Interval after which the in-flight upstream request is cancelled when the
    /// client disconnects.
    ///
    /// Default: [`DEFAULT_CLIENT_DISCONNECT_CANCEL_MS`] (500ms). AD-PXY-14.
    ///
    /// # Implementation status
    ///
    /// In #303, client-disconnect cancellation happens implicitly via tokio task
    /// drop when the connection task ends — this field is stored but NOT enforced
    /// as an explicit timeout. An explicit `timeout(client_disconnect_cancel, …)`
    /// on the upstream forward is a #304 enhancement.
    pub client_disconnect_cancel: Duration,

    /// Graceful drain interval on SIGINT/SIGTERM.
    ///
    /// In-flight streams are given this window to complete. Default:
    /// [`DEFAULT_GRACEFUL_DRAIN_SECS`] (5s). AD-PXY-14.
    pub graceful_drain: Duration,

    /// Readiness watchdog staleness window reference.
    ///
    /// The observable contract is the K=3 / 10s window (see `health.rs`
    /// `READINESS_FAILURE_THRESHOLD_K` / `READINESS_STALE_WINDOW_SECS`).
    /// Default: [`DEFAULT_READINESS_FLIP_SECS`] (3s). AD-PXY-11.
    ///
    /// # Implementation status
    ///
    /// In #303, `health.rs` uses its own hardcoded constants. This field is stored
    /// but NOT read by `ReadinessState` — wiring it through is a #304 enhancement
    /// (requires passing the Duration into `ReadinessState::new`).
    pub readiness_flip: Duration,
}

impl ProxyConfig {
    /// Return a builder for [`ProxyConfig`] with all defaults pre-populated.
    pub fn builder() -> ProxyConfigBuilder {
        ProxyConfigBuilder::default()
    }

    /// Returns the socket address the proxy will bind to.
    pub fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    /// Returns the upstream URL for a given provider name, falling back to the
    /// default upstream when no provider-specific override is configured.
    ///
    /// Returns `None` when neither a provider-specific nor a default upstream
    /// is configured (D8: → 502).
    pub fn upstream_for(&self, provider: &str) -> Option<&str> {
        self.upstream_providers
            .get(provider)
            .map(String::as_str)
            .or(self.upstream_default.as_deref())
    }
}

// ============================================================================
// Builder
// ============================================================================

/// Builder for [`ProxyConfig`].
///
/// Validates invariants at [`build()`][ProxyConfigBuilder::build] time:
/// - Port is within 41000–49000 (D8 / AD-PXY-03).
/// - Bind address is a valid socket address.
///
/// Cleartext warning is set automatically when the bind address is not loopback.
#[derive(Debug)]
pub struct ProxyConfigBuilder {
    bind_ip: IpAddr,
    port: u16,
    upstream_default: Option<String>,
    upstream_providers: std::collections::HashMap<String, String>,
    max_body_bytes: usize,
    upstream_timeout: Duration,
    client_disconnect_cancel: Duration,
    graceful_drain: Duration,
    readiness_flip: Duration,
}

impl Default for ProxyConfigBuilder {
    fn default() -> Self {
        Self {
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: DEFAULT_PROXY_PORT,
            upstream_default: None,
            upstream_providers: std::collections::HashMap::new(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            upstream_timeout: Duration::from_secs(DEFAULT_UPSTREAM_TIMEOUT_SECS),
            client_disconnect_cancel: Duration::from_millis(DEFAULT_CLIENT_DISCONNECT_CANCEL_MS),
            graceful_drain: Duration::from_secs(DEFAULT_GRACEFUL_DRAIN_SECS),
            readiness_flip: Duration::from_secs(DEFAULT_READINESS_FLIP_SECS),
        }
    }
}

impl ProxyConfigBuilder {
    /// Override the bind IP address.
    ///
    /// Default: `127.0.0.1` (loopback). Passing a non-loopback address causes
    /// [`build()`][Self::build] to set `warn_cleartext = true` (AC1 / AD-PXY-03).
    pub fn bind_ip(mut self, ip: IpAddr) -> Self {
        self.bind_ip = ip;
        self
    }

    /// Override the port.
    ///
    /// Must be within [`PORT_RANGE_MIN`]–[`PORT_RANGE_MAX`] (D8 / AD-PXY-03).
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set the default upstream base URL (required for routing, D8).
    ///
    /// Without this, Unknown-provider requests respond 502.
    pub fn upstream_default(mut self, url: impl Into<String>) -> Self {
        self.upstream_default = Some(url.into());
        self
    }

    /// Add a per-provider upstream URL override.
    ///
    /// `provider` is one of `"anthropic"` or `"openai"`.
    pub fn upstream_provider(
        mut self,
        provider: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        self.upstream_providers.insert(provider.into(), url.into());
        self
    }

    /// Override the request-body buffering bound (AD-PXY-10).
    pub fn max_body_bytes(mut self, bytes: usize) -> Self {
        self.max_body_bytes = bytes;
        self
    }

    /// Override the upstream connect + response timeout (AD-PXY-14).
    pub fn upstream_timeout(mut self, d: Duration) -> Self {
        self.upstream_timeout = d;
        self
    }

    /// Override the client-disconnect upstream-cancel interval (AD-PXY-14).
    pub fn client_disconnect_cancel(mut self, d: Duration) -> Self {
        self.client_disconnect_cancel = d;
        self
    }

    /// Override the graceful drain interval (AD-PXY-14).
    pub fn graceful_drain(mut self, d: Duration) -> Self {
        self.graceful_drain = d;
        self
    }

    /// Override the readiness watchdog flip interval (AD-PXY-11).
    pub fn readiness_flip(mut self, d: Duration) -> Self {
        self.readiness_flip = d;
        self
    }

    /// Build and validate the configuration.
    ///
    /// # Errors
    ///
    /// - [`ProxyError::InvalidConfig`] if the port is outside 41000–49000 (D8).
    pub fn build(self) -> Result<ProxyConfig, ProxyError> {
        // D8 / AD-PXY-03: port must be within the Windows-bindable range.
        if self.port < PORT_RANGE_MIN || self.port > PORT_RANGE_MAX {
            return Err(ProxyError::InvalidConfig(format!(
                "port {} is outside the allowed range {}-{} (D8 / AD-PXY-03: \
                 must be Windows-bindable and not 8787)",
                self.port, PORT_RANGE_MIN, PORT_RANGE_MAX
            )));
        }

        let bind_addr = SocketAddr::new(self.bind_ip, self.port);

        // AC1 / AD-PXY-03: non-loopback bind → cleartext-exposure warning.
        // The warning text is the contract; the caller (cmd/proxy.rs) MUST emit it
        // to stderr BEFORE calling serve().
        let warn_cleartext = !self.bind_ip.is_loopback();

        Ok(ProxyConfig {
            bind_addr,
            warn_cleartext,
            upstream_default: self.upstream_default,
            upstream_providers: self.upstream_providers,
            max_body_bytes: self.max_body_bytes,
            upstream_timeout: self.upstream_timeout,
            client_disconnect_cancel: self.client_disconnect_cancel,
            graceful_drain: self.graceful_drain,
            readiness_flip: self.readiness_flip,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    // AC1 / AD-PXY-03: loopback default, no cleartext warning.
    #[test]
    fn test_default_config_is_loopback_no_warning() {
        let cfg = ProxyConfig::builder().build().unwrap_or_else(|e| {
            panic!("default config must build without error, got: {e}");
        });
        assert!(
            cfg.bind_addr.ip().is_loopback(),
            "default bind address must be loopback"
        );
        assert_eq!(cfg.bind_addr.port(), DEFAULT_PROXY_PORT);
        assert!(
            !cfg.warn_cleartext,
            "loopback bind must not trigger cleartext warning"
        );
    }

    // AC1 / AD-PXY-03: non-loopback bind sets cleartext warning.
    #[test]
    fn test_non_loopback_bind_sets_cleartext_warning() {
        let cfg = ProxyConfig::builder()
            .bind_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)))
            .build()
            .unwrap_or_else(|e| {
                panic!("0.0.0.0 config must build without error, got: {e}");
            });
        assert!(
            cfg.warn_cleartext,
            "non-loopback bind must set warn_cleartext = true"
        );
    }

    // D8 / AD-PXY-03: port outside 41000-49000 is rejected.
    #[test]
    fn test_port_below_range_is_rejected() {
        let err = ProxyConfig::builder().port(8080).build().unwrap_err();
        assert!(
            matches!(err, ProxyError::InvalidConfig(_)),
            "port 8080 must be rejected as InvalidConfig"
        );
    }

    // D8 / AD-PXY-03: port 8787 (PRISM's port) is rejected.
    #[test]
    fn test_port_8787_is_rejected() {
        let err = ProxyConfig::builder().port(8787).build().unwrap_err();
        assert!(
            matches!(err, ProxyError::InvalidConfig(_)),
            "port 8787 must be rejected (D8 / AD-PXY-03)"
        );
    }

    // D8 / AD-PXY-03: port above range is rejected.
    #[test]
    fn test_port_above_range_is_rejected() {
        let err = ProxyConfig::builder().port(50000).build().unwrap_err();
        assert!(
            matches!(err, ProxyError::InvalidConfig(_)),
            "port 50000 must be rejected as InvalidConfig"
        );
    }

    // D8 / AD-PXY-03: port at range boundaries is accepted.
    #[test]
    fn test_port_at_range_min_is_accepted() {
        let cfg = ProxyConfig::builder()
            .port(PORT_RANGE_MIN)
            .build()
            .unwrap_or_else(|e| {
                panic!("port {} must be accepted, got: {e}", PORT_RANGE_MIN);
            });
        assert_eq!(cfg.bind_addr.port(), PORT_RANGE_MIN);
    }

    #[test]
    fn test_port_at_range_max_is_accepted() {
        let cfg = ProxyConfig::builder()
            .port(PORT_RANGE_MAX)
            .build()
            .unwrap_or_else(|e| {
                panic!("port {} must be accepted, got: {e}", PORT_RANGE_MAX);
            });
        assert_eq!(cfg.bind_addr.port(), PORT_RANGE_MAX);
    }

    // upstream_for: provider-specific upstream wins over default.
    #[test]
    fn test_upstream_for_provider_specific_wins() {
        let cfg = ProxyConfig::builder()
            .upstream_default("https://default.example.com")
            .upstream_provider("anthropic", "https://api.anthropic.com")
            .build()
            .unwrap_or_else(|e| panic!("build error: {e}"));
        assert_eq!(
            cfg.upstream_for("anthropic"),
            Some("https://api.anthropic.com")
        );
    }

    // upstream_for: falls back to default when no provider-specific entry.
    #[test]
    fn test_upstream_for_falls_back_to_default() {
        let cfg = ProxyConfig::builder()
            .upstream_default("https://default.example.com")
            .build()
            .unwrap_or_else(|e| panic!("build error: {e}"));
        assert_eq!(
            cfg.upstream_for("unknown-provider"),
            Some("https://default.example.com")
        );
    }

    // D8 / AC3: no default upstream → upstream_for returns None.
    #[test]
    fn test_upstream_for_returns_none_when_no_default() {
        let cfg = ProxyConfig::builder()
            .build()
            .unwrap_or_else(|e| panic!("build error: {e}"));
        assert!(
            cfg.upstream_for("openai").is_none(),
            "no default upstream → upstream_for must return None (→ 502)"
        );
    }

    // AD-PXY-14: lifecycle default values match the documented evidence.
    #[test]
    fn test_lifecycle_defaults_match_documented_values() {
        let cfg = ProxyConfig::builder()
            .build()
            .unwrap_or_else(|e| panic!("build error: {e}"));
        assert_eq!(
            cfg.upstream_timeout,
            Duration::from_secs(DEFAULT_UPSTREAM_TIMEOUT_SECS),
            "upstream_timeout must default to {}s",
            DEFAULT_UPSTREAM_TIMEOUT_SECS
        );
        assert_eq!(
            cfg.client_disconnect_cancel,
            Duration::from_millis(DEFAULT_CLIENT_DISCONNECT_CANCEL_MS),
            "client_disconnect_cancel must default to {}ms",
            DEFAULT_CLIENT_DISCONNECT_CANCEL_MS
        );
        assert_eq!(
            cfg.graceful_drain,
            Duration::from_secs(DEFAULT_GRACEFUL_DRAIN_SECS),
            "graceful_drain must default to {}s",
            DEFAULT_GRACEFUL_DRAIN_SECS
        );
        assert_eq!(
            cfg.readiness_flip,
            Duration::from_secs(DEFAULT_READINESS_FLIP_SECS),
            "readiness_flip must default to {}s",
            DEFAULT_READINESS_FLIP_SECS
        );
    }

    // AD-PXY-10: default body-size bound is 64 MiB.
    #[test]
    fn test_max_body_bytes_default_is_64mib() {
        let cfg = ProxyConfig::builder()
            .build()
            .unwrap_or_else(|e| panic!("build error: {e}"));
        assert_eq!(
            cfg.max_body_bytes, DEFAULT_MAX_BODY_BYTES,
            "max_body_bytes must default to 64 MiB"
        );
        assert_eq!(
            DEFAULT_MAX_BODY_BYTES,
            64 * 1024 * 1024,
            "DEFAULT_MAX_BODY_BYTES must be exactly 64 MiB"
        );
    }
}
