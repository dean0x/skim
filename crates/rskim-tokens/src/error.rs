//! Error types for `rskim-tokens`.

use thiserror::Error;

/// Errors that can occur during counter construction.
///
/// Note: tiktoken-backed counters embed their vocabularies at compile time via
/// `include_str!` in `tiktoken-rs`. The only fallible step at runtime is the
/// internal base64 decode of a bundled asset that ships valid. This means
/// [`TokenError::TiktokenInit`] is practically unreachable at runtime, but is
/// surfaced as `Err` so callers never see a panic (constraint 4 / AC10).
#[derive(Debug, Error)]
pub enum TokenError {
    /// Failed to initialise a tiktoken BPE encoder.
    ///
    /// This is practically unreachable at runtime because the vocab is
    /// embedded at compile time and decoded from a known-valid asset.
    /// It is surfaced here to satisfy the no-panic contract (AC10) and
    /// to allow fault-injection testing via `Counter::from_raw_bpe`
    /// (available in `#[cfg(test)]` only).
    #[error("tiktoken initialisation failed for {encoding}: {source}")]
    TiktokenInit {
        /// Human-readable encoding name (e.g. "cl100k_base").
        encoding: &'static str,
        /// Underlying error from tiktoken-rs.
        #[source]
        source: anyhow::Error,
    },

    /// Network counter is disabled — the `net-anthropic` feature is not enabled,
    /// or the `ANTHROPIC_API_KEY` environment variable is missing.
    #[error("network counter unavailable: {reason}")]
    NetworkUnavailable {
        /// Human-readable reason.
        reason: &'static str,
    },

    /// The `ANTHROPIC_API_KEY` environment variable is not set.
    #[error("ANTHROPIC_API_KEY is not set")]
    MissingApiKey,

    /// Network request failed (timeout, connection refused, non-2xx response).
    ///
    /// Only reachable when the `net-anthropic` feature is enabled.
    #[cfg(feature = "net-anthropic")]
    #[error("network request failed: {0}")]
    NetworkRequest(String),

    /// The Anthropic API returned an unexpected response body.
    ///
    /// Only reachable when the `net-anthropic` feature is enabled.
    #[cfg(feature = "net-anthropic")]
    #[error("unexpected API response: {0}")]
    ApiResponse(String),
}
