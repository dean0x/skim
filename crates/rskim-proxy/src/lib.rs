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
//!    503+Retry-After when connection cap is chosen as the cap behavior, and 504
//!    for upstream timeout).
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
//!    path-first + bounded JSON shape fallback. Analytics is fire-and-forget on a
//!    bounded channel.
//!
//! 6. **Single static binary** — rustls + webpki-roots (no system-CA dependency).
//!    OS trust-store option is a follow-up tracked in #346.
//!
//! ## AD-PXY-01: Separate crate rationale
//!
//! hyper, tokio, and rustls are heavy dependencies and introduce the first async
//! runtime in the workspace. A separate optional crate isolates compile cost from
//! users who never run the proxy, and from `release` LTO of the main binary.
//! This follows the `rskim-search` precedent: depends on `rskim-core`/`rskim-contract`
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
pub(crate) mod logging;

/// Serve the proxy with the given configuration and transform pipeline.
///
/// This is the primary entry point called by `rskim`'s `cmd/proxy.rs` handler.
/// The function takes ownership of the runtime and blocks until the server
/// shuts down (SIGINT/SIGTERM received and drain completes).
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
pub fn serve(_config: config::ProxyConfig) -> Result<(), errors::ProxyError> {
    // Phase 1 (this ticket): config + CLI wiring only. The actual server
    // implementation lands in a later phase (Steps 7-9 of the plan).
    // This stub validates that the crate skeleton, config, and wiring compile
    // end-to-end before the server internals are built.
    //
    // AC1 (Functionality): `skim proxy --port <P>` starts and binds — wired in
    // cmd/proxy.rs; the serve() function here will block on the tokio runtime
    // once the server internals (server.rs) are implemented in the next phase.
    Err(errors::ProxyError::NotImplemented(
        "proxy server not yet implemented — skeleton phase only".to_string(),
    ))
}
