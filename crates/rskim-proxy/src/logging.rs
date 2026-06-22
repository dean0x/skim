//! Structured JSON log initialisation and auth redaction for the proxy.
//!
//! ## AC13 / Invariant 7 — Auth material never in logs
//!
//! `Authorization` and `x-api-key` header values MUST NEVER appear, in whole or
//! part, in any log line at any log level.
//!
//! Redaction uses `rskim_contract::log::is_sensitive_value` (the VALUE-axis
//! classifier) — NOT `is_sensitive_key`. The key-name classifier matches
//! underscore-bearing env-style names (e.g., `ANTHROPIC_API_KEY`) but does NOT
//! match hyphenated HTTP header names like `authorization` or `x-api-key`.
//! Never rely on `is_sensitive_key` for HTTP header value redaction.
//!
//! ## Proxy clocks (AC18)
//!
//! The proxy's server layer LEGITIMATELY uses clocks (Instant::now for latency
//! measurement, lifecycle timers). Do NOT copy `rskim-contract/clippy.toml` into
//! this crate — rskim-contract bans clock usage because its transform path must
//! be deterministic (invariant 5). The proxy is NOT under that constraint.

use tracing_subscriber::{EnvFilter, fmt};

/// Initialise a structured JSON tracing subscriber for the proxy process.
///
/// Reads log level from the `RUST_LOG` environment variable (default: `info`).
/// Output is JSON-formatted to stderr. Call this once at proxy startup from
/// `serve()` / `serve_with_analytics()` before the tokio runtime starts.
///
/// ## AC13 — auth material never logged
///
/// The subscriber emits the log level, timestamp, target, and message fields.
/// Auth header VALUES are never passed to the tracing macros (the forwarding
/// path only logs header names and request_id, never values). The redaction
/// helpers [`redact_header_value`] / [`is_suppressed_header`] are available
/// for call sites in #304 that log request metadata.
///
/// Safe to call multiple times — subsequent calls are ignored (global init is
/// idempotent via `try_init()`).
pub(crate) fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // try_init() returns Err if already initialised — ignore the error (idempotent).
    let _ = fmt::Subscriber::builder()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Redact-safe representation of a header value for logging.
///
/// Returns a static sentinel `"[REDACTED]"` for sensitive header values so
/// that call sites can safely include the redacted form in log records without
/// leaking auth material.
///
/// ## Redaction rule (AC13 / AD-PXY-08)
///
/// A header is considered sensitive if its NAME is one of the auth headers OR
/// if its VALUE matches the value-axis classifier `is_sensitive_value`. The
/// name-axis check is intentional defense-in-depth: even if the value does not
/// pattern-match as sensitive, auth header names (`authorization`, `x-api-key`)
/// imply the value is always sensitive.
///
/// NOTE: `is_sensitive_key` MUST NOT be used here — it matches `_KEY`, `_TOKEN`
/// suffixes (env-var style) and does NOT match `authorization` or `x-api-key`
/// (hyphenated HTTP style).
///
/// Called by the request-logging path in #304 (forwarder). Suppressed until
/// #304 lands.
#[allow(dead_code)]
pub(crate) fn redact_header_value(name: &str, value: &str) -> &'static str {
    use rskim_contract::log::is_sensitive_value;

    // Explicit auth header name check (defense-in-depth).
    let sensitive_name = matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "proxy-authorization"
    );

    // Value-axis check: does the value look like a raw secret?
    let sensitive_value = is_sensitive_value(value);

    if sensitive_name || sensitive_value {
        "[REDACTED]"
    } else {
        // Return static lifetime — callers must store separately if needed.
        // This function is used only for logging; the value is never cached.
        "[safe]" // placeholder: real logging would use the value directly
    }
}

/// Returns `true` if a header should be excluded from log output entirely.
///
/// Some headers should not appear in logs even in redacted form (e.g., we don't
/// want to confirm the presence of `x-api-key` via a `[REDACTED]` marker in
/// production logs at max verbosity, as that leaks timing/auth-scheme information).
///
/// Currently suppresses `authorization` and `x-api-key` from any log line.
///
/// Called by the request-logging path in #304 (forwarder). Suppressed until
/// #304 lands.
#[allow(dead_code)]
pub(crate) fn is_suppressed_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "proxy-authorization"
    )
}

// ============================================================================
// Tests (AC13)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // AC13 discriminating: auth headers are redacted.
    #[test]
    fn test_authorization_header_is_redacted() {
        let result = redact_header_value("authorization", "Bearer sk-ant-api03-SENTINEL");
        assert_eq!(
            result, "[REDACTED]",
            "Authorization header must be redacted"
        );
    }

    #[test]
    fn test_x_api_key_header_is_redacted() {
        let result = redact_header_value("x-api-key", "sk-ant-api03-SENTINEL");
        assert_eq!(result, "[REDACTED]", "x-api-key header must be redacted");
    }

    #[test]
    fn test_proxy_authorization_is_redacted() {
        let result = redact_header_value("proxy-authorization", "Bearer token");
        assert_eq!(result, "[REDACTED]");
    }

    // AC13: auth headers are suppressed.
    #[test]
    fn test_auth_headers_are_suppressed() {
        assert!(is_suppressed_header("authorization"));
        assert!(is_suppressed_header("x-api-key"));
        assert!(is_suppressed_header("Authorization")); // case-insensitive
        assert!(is_suppressed_header("X-Api-Key")); // case-insensitive
    }

    #[test]
    fn test_non_auth_headers_not_suppressed() {
        assert!(!is_suppressed_header("content-type"));
        assert!(!is_suppressed_header("accept"));
        assert!(!is_suppressed_header("x-request-id"));
    }
}
