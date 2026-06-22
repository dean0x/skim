//! Proxy construction and startup errors.
//!
//! [`ProxyError`] is for **construction and startup paths only** — it MUST NOT
//! appear on the forwarding path. Every forwarding-path failure resolves to a
//! fail-open [`rskim_contract::contract::Outcome`] (byte-identical forward +
//! structured warning).

use thiserror::Error;

/// Errors that can occur during proxy construction or startup.
///
/// This type covers bind failure, TLS configuration error, invalid configuration,
/// and other setup-time failures. It does NOT cover forwarding-path errors, which
/// are encoded as [`rskim_contract::contract::Outcome::passthrough`] (fail-open).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProxyError {
    /// The proxy failed to bind to the configured address and port.
    ///
    /// Common causes: port already in use, insufficient permissions.
    #[error("bind failed on {addr}: {source}")]
    BindFailed {
        /// The address that failed to bind.
        addr: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The proxy configuration is invalid.
    ///
    /// Returned when the validated [`crate::config::ProxyConfig`] contains
    /// contradictory or out-of-range values that were not caught at parse time.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// The TLS configuration failed to initialise.
    ///
    /// Returned when rustls cannot load the root certificate store.
    #[error("TLS configuration error: {0}")]
    TlsConfig(String),

    /// A required upstream URL is missing.
    ///
    /// Returned when a provider route is requested but no upstream URL is
    /// configured and no `--upstream-default` was provided (D8 / AC3).
    #[error("no upstream configured for provider '{provider}' and no default upstream set")]
    NoUpstream {
        /// The provider name that has no configured upstream.
        provider: String,
    },

    /// A placeholder for the skeleton phase — removed once the server is implemented.
    ///
    /// This variant exists only during Phase 1 (crate skeleton + CLI wiring) to
    /// allow the crate to compile and the CLI path to exercise the entry point.
    /// It will be replaced by the actual server startup logic in Phase 2.
    #[doc(hidden)]
    #[error("not yet implemented: {0}")]
    NotImplemented(String),
}
