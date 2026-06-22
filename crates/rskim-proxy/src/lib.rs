//! # rskim-proxy — HTTP reverse proxy foundation for skim Layer 3
//!
//! This crate is the **carrier and trust foundation** for skim's Layer-3 LLM
//! request proxy. It ships a resident loopback HTTP/1.1 reverse proxy built on
//! hyper + tokio that provably never alters, delays, inflates, or drops traffic.
//!
//! ## Design constraints
//!
//! 1. **Fail-open on every request path** — parse error, transform panic, analytics
//!    failure, and sink-full all resolve to byte-identical forward + structured
//!    warning. Never a proxy-originated 4xx/5xx on the forwarding path (except
//!    the named carve-outs: 502 for unknown provider with no default upstream,
//!    and 504 for upstream timeout). The connection cap uses bounded-accept TCP
//!    backpressure (AD-PXY-13) — the proxy does NOT emit 503+Retry-After.
//!
//! 2. **Never-inflate** — the transform seam composes #301's `guarded_transform`
//!    gate; output bytes ≤ input bytes per stage (invariant 2). The identity stage
//!    ships with this ticket; #304/#306/#307 plug into the [`seam::TransformStage`]
//!    trait.
//!
//! 3. **Byte-identical header forward** — allowed-list only: RFC 9110 hop-by-hop
//!    headers, Host/SNI rewrite, and framing. No `Via`. See `forward.rs`.
//!
//! 4. **Auth material never logged** — `Authorization` and `x-api-key` forwarded
//!    byte-identical; never appear in any log record. Redaction uses the
//!    value-axis classifier `rskim_contract::log::is_sensitive_value` (the
//!    key-name classifier `is_sensitive_key` does NOT match hyphenated HTTP header
//!    names — do not rely on it for auth headers). See AC13.
//!
//! 5. **No ML, no tokenizer, no blocking on the request path** — detection is
//!    path-first + bounded JSON shape fallback. Analytics fires synchronously via
//!    `AnalyticsHook::on_request` (catch_unwind-guarded). The recommended
//!    implementation is [`analytics::ChannelAnalyticsHook`] which hands off via a
//!    bounded channel and is non-blocking. #305 wires the channel hook into `serve()`.
//!
//! 6. **Single static binary** — rustls + webpki-roots (no system-CA dependency).
//!    OS trust-store option is a follow-up tracked in #346.
//!
//! ## AD-PXY-01: Separate crate rationale
//!
//! hyper, tokio, and rustls are heavy dependencies and introduce the first async
//! runtime in the workspace. A separate crate isolates incremental-rebuild churn:
//! changes to proxy internals do not re-link all other crates. NOTE: `rskim`
//! currently depends on `rskim-proxy` unconditionally (not feature-gated), so
//! hyper/tokio/rustls ARE compiled into every `skim` build and flow through release
//! LTO. A `proxy` feature gate is a future cleanup (#todo). This follows the
//! `rskim-search` precedent: depends on `rskim-core`/`rskim-contract`
//! without changing their APIs.
//!
//! ## Public API surface
//!
//! - [`config::ProxyConfig`] — validated proxy configuration
//! - [`seam::TransformStage`] — the seam trait #304/#306/#307 implement
//! - [`seam::TransformContext`] — per-request read-only context (non-exhaustive)
//! - [`seam::TransformPipeline`] — ordered stage composition (canonical order fixed here)
//! - [`detect::ProxyProvider`] — self-contained provider enum (non-exhaustive)
//! - [`authmode::AuthMode`] — header-shape auth classification (non-exhaustive)
//! - [`analytics::AnalyticsHook`] — fire-and-forget hook trait
//! - [`analytics::ProxyEvent`] — non-exhaustive event payload
//! - [`errors::ProxyError`] — construction/startup errors only

#![deny(missing_docs)]

pub mod analytics;
pub mod authmode;
pub mod config;
pub mod detect;
pub mod errors;
pub mod seam;

// Internal modules — not part of the public API surface.
// Exposed pub(crate) for integration test access where needed.
pub(crate) mod health;
pub(crate) mod logging;
pub(crate) mod server;

/// Forward-path utilities exposed for integration tests (AC12 header-diff test).
///
/// Only the committed const and helpers used by integration tests are re-exported.
pub mod forward;

/// Test utilities for integration testing the running proxy server.
///
/// These utilities expose the async server entry point so integration tests
/// (in `tests/`) can spin up a real proxy on an ephemeral port, drive requests
/// through it against a fake upstream, and verify byte-identity on the wire.
///
/// Available only when compiling under `cfg(test)` or as a `dev-dependency`
/// with the `testing` feature.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::sync::Arc;

    use crate::analytics::AnalyticsHook;
    use crate::config::ProxyConfig;
    use crate::errors::ProxyError;
    use crate::seam::TransformPipeline;

    /// Run the proxy server asynchronously inside an existing tokio runtime.
    ///
    /// Unlike [`crate::serve_with_stage`] (which creates its own runtime), this
    /// function is `async` and is meant to be spawned as a `tokio::task` inside
    /// a test's `#[tokio::test]` runtime:
    ///
    /// ```ignore
    /// let handle = tokio::spawn(rskim_proxy::testing::run_server_async(config, pipeline, analytics));
    /// // drive requests…
    /// handle.abort(); // shut down when done
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ProxyError`] for bind/TLS failures. Forwarding-path errors
    /// are never surfaced — they resolve to fail-open.
    pub async fn run_server_async(
        config: ProxyConfig,
        pipeline: TransformPipeline,
        analytics: Arc<dyn AnalyticsHook>,
    ) -> Result<(), ProxyError> {
        crate::server::run_server(config, pipeline, analytics).await
    }
}

use std::sync::Arc;

/// Serve the proxy with the given configuration and transform pipeline.
///
/// This is the primary entry point called by `rskim`'s `cmd/proxy.rs` handler.
/// The function creates a tokio runtime and blocks until the server shuts down
/// (SIGINT/SIGTERM received and drain completes).
///
/// # Errors
///
/// Returns [`errors::ProxyError`] for startup failures: bind failure, TLS
/// configuration error, or invalid configuration. Forwarding-path errors are
/// never surfaced here — they resolve to fail-open per the design constraints.
///
/// # AD-PXY-01
///
/// The `rskim_proxy` crate is a separate optional workspace member so that
/// hyper/tokio/rustls compile cost is isolated from users who never run the proxy.
pub fn serve(config: config::ProxyConfig) -> Result<(), errors::ProxyError> {
    // Initialise structured JSON logging (AC13). Safe to call multiple times.
    logging::init_logging();
    serve_with_analytics(config, Arc::new(analytics::NoopAnalyticsHook))
}

/// Serve the proxy with a custom analytics hook and the identity pipeline.
///
/// Used for testing and for callers that want to capture analytics events.
/// The `analytics` hook is fired once per completed request (AC6 / AC15).
///
/// # Errors
///
/// Same as [`serve`].
pub fn serve_with_analytics(
    config: config::ProxyConfig,
    analytics: Arc<dyn analytics::AnalyticsHook>,
) -> Result<(), errors::ProxyError> {
    serve_with_stage(config, seam::TransformPipeline::identity(), analytics)
}

/// Serve the proxy with a custom transform pipeline and analytics hook.
///
/// This is the injection point for #304's `BlockRouter`:
/// ```ignore
/// rskim_proxy::serve_with_stage(config, TransformPipeline::from_stages(vec![
///     Box::new(BlockRouter::new(…)),
/// ]), analytics_hook);
/// ```
///
/// The identity pipeline (no compression) is the #303 default. #304 replaces
/// it by injecting `BlockRouter` at this construction point (D1 / AD-PXY-06).
///
/// # Errors
///
/// Same as [`serve`].
pub fn serve_with_stage(
    config: config::ProxyConfig,
    pipeline: seam::TransformPipeline,
    analytics: Arc<dyn analytics::AnalyticsHook>,
) -> Result<(), errors::ProxyError> {
    // Initialise structured JSON logging (AC13). Safe to call multiple times —
    // subsequent calls are ignored via try_init().
    logging::init_logging();

    // Build a tokio runtime and run the async server.
    // The runtime blocks until the server shuts down (AC23).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| errors::ProxyError::InvalidConfig(format!("tokio runtime init: {e}")))?;

    rt.block_on(server::run_server(config, pipeline, analytics))
}
