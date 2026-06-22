//! Upstream HTTP/1.1 forwarding over rustls (hyper direct — no reqwest).
//!
//! ## AD-PXY-04 — Upstream client stack
//!
//! hyper 1.x directly over hyper-rustls (rustls 0.23 + webpki-roots). HTTP/1.1
//! on both listener and upstream sides. HTTP/2-to-upstream is a follow-up tracked
//! in #347. The single-static-binary + supply-chain stance matches `ureq 3.3` in
//! the workspace (no system-CA dependency; OS trust-store is #346).
//!
//! ## AD-PXY-15 — Header allowed-list (Via deliberately absent)
//!
//! On the forward path, ONLY the following header mutations are applied:
//!
//! 1. Strip RFC 9110 hop-by-hop headers:
//!    `connection`, `keep-alive`, `proxy-authenticate`, `proxy-authorization`,
//!    `te`, `trailer`, `transfer-encoding`, `upgrade`.
//! 2. Rewrite `host` to the upstream authority (AC12 / AC13).
//! 3. Pass `content-length` / `transfer-encoding` as required by hyper framing.
//! 4. ALL other headers — including `authorization`, `x-api-key`, and any custom
//!    headers — are forwarded byte-identical.
//! 5. **`Via` is intentionally NOT added** (deviation from RFC 9110 §7.6.3).
//!    Rationale: (a) skim is a local AI proxy, not an internet intermediary;
//!    (b) adding `Via` leaks proxy infrastructure to the upstream; (c) this is a
//!    transparent passthrough contract — adding headers outside the allowed-list
//!    violates AC12. #303 commits this deviation; it is NOT a bug.
//!
//! ## Response streaming (AC5, AC7)
//!
//! Responses are streamed chunk-by-chunk. The proxy NEVER buffers the full response
//! before forwarding. Each frame is written to the client as it arrives from
//! upstream. Backpressure propagates: a slow client slows the upstream read,
//! which applies TCP flow control to the upstream connection (AC7).
//!
//! SSE (`text/event-stream`) and chunked-transfer responses are handled identically
//! at this layer — both are opaque byte streams forwarded without interpretation.

use http_body_util::Full;
use hyper::header::HeaderValue;
use hyper::{Request, Response, Uri};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::TokioExecutor;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

// ============================================================================
// RFC 9110 hop-by-hop header names (AD-PXY-15)
// ============================================================================

/// RFC 9110 §7.6.1 hop-by-hop headers that MUST be stripped on every forward.
///
/// These headers are connection-scoped and MUST NOT be forwarded to the upstream.
/// They are defined here as a committed const (AC12 requires the allowed-list is
/// a named const, not inline strings) so the assertion in tests can compare
/// against it directly.
///
/// Note: `transfer-encoding` is managed by hyper's framing layer when chunked;
/// we strip it here so hyper can set the correct value for the upstream request.
pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

// ============================================================================
// UpstreamClient
// ============================================================================

/// hyper-over-rustls HTTP/1.1 upstream client.
///
/// One instance is created per-proxy at startup and shared across all
/// connection-handling tasks (Send + Sync via Arc). The client maintains
/// a connection pool internally (hyper-util legacy client).
///
/// ## AD-PXY-04 — HTTP/1.1 only, rustls + webpki-roots
///
/// HTTP/2-to-upstream is a follow-up tracked in #347. The client uses
/// webpki-roots for hermetic certificate validation (no OS trust-store
/// dependency; follow-up tracked in #346).
pub struct UpstreamClient {
    inner: Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<bytes::Bytes>>,
}

impl UpstreamClient {
    /// Construct a new upstream client with rustls + webpki-roots.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS configuration cannot be built (e.g., if
    /// webpki-roots cannot be loaded — should not happen in practice).
    pub fn new() -> Result<Self, crate::errors::ProxyError> {
        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let tls_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1()
            .build();

        let client = Client::builder(TokioExecutor::new()).build(https);
        Ok(Self { inner: client })
    }
}

// ============================================================================
// Header rewrite (AD-PXY-15 allowed-list)
// ============================================================================

/// Determine if a header name is a RFC 9110 hop-by-hop header that must be
/// stripped from the forwarded request.
///
/// Case-insensitive. Called once per request header in the forward path.
pub fn is_hop_by_hop(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    HOP_BY_HOP_HEADERS.contains(&lower.as_str())
}

/// Build the upstream URI from the original request URI and the configured
/// upstream base URL.
///
/// The upstream base URL (`https://api.anthropic.com`) is combined with the
/// path and query from the original request. The host:port from the original
/// request is discarded — the upstream authority is always the configured one.
///
/// # Errors
///
/// Returns `None` if the resulting URI cannot be parsed (e.g., if the upstream
/// base URL or the original path is malformed). The caller treats `None` as a
/// 502 (no upstream available / configuration error).
pub fn build_upstream_uri(original_uri: &Uri, upstream_base: &str) -> Option<Uri> {
    // Strip trailing slash from the upstream base so we don't get double slashes.
    let base = upstream_base.trim_end_matches('/');

    // Preserve path and query from the original request.
    let path_and_query = original_uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let full = format!("{base}{path_and_query}");
    full.parse::<Uri>().ok()
}

/// Extract the host:port authority from the upstream base URL.
///
/// Used to rewrite the `host` header in the forwarded request (AC12).
/// Returns `None` if the upstream base URL has no host component or no scheme.
///
/// A bare string like `"not-a-url"` has no scheme and is rejected even if the
/// `Uri` parser can parse it as a path-relative URI — we only accept absolute
/// URIs with an `http` or `https` scheme (AD-PXY-02).
pub fn upstream_authority(upstream_base: &str) -> Option<String> {
    let uri = upstream_base.parse::<Uri>().ok()?;
    // Reject relative URIs (no scheme → no absolute upstream authority).
    let scheme = uri.scheme_str()?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = uri.authority()?;
    Some(authority.as_str().to_owned())
}

// ============================================================================
// Forward function
// ============================================================================

/// Forward `request` to the upstream specified by `upstream_url`.
///
/// ## Header rewrite (AD-PXY-15)
///
/// 1. Hop-by-hop headers are stripped.
/// 2. `host` is rewritten to the upstream authority.
/// 3. All other headers are forwarded byte-identical (including auth headers —
///    they MUST NOT be logged but MUST be forwarded).
/// 4. `Via` is NOT added (deliberate deviation, documented at module level).
///
/// ## Streaming (AC5, AC7)
///
/// The response body is NOT buffered. The caller is responsible for streaming
/// `ForwardResult::response` to the client chunk-by-chunk.
///
/// ## Timeout (AC20, AD-PXY-14)
///
/// The caller wraps this call in `tokio::time::timeout(config.upstream_timeout)`
/// and converts `Elapsed` into a 504 response.
///
/// ## Auth headers (AC13)
///
/// Auth header VALUES are forwarded byte-identical to upstream. They are NOT
/// inspected or logged at this layer. The logging path (logging.rs) handles
/// redaction separately.
pub async fn forward_request(
    mut req_parts: hyper::http::request::Parts,
    body: bytes::Bytes,
    upstream_url: &str,
    client: &UpstreamClient,
) -> Result<Response<hyper::body::Incoming>, ForwardError> {
    // Build the upstream URI — path/query from original, authority from config.
    let upstream_uri = build_upstream_uri(&req_parts.uri, upstream_url)
        .ok_or_else(|| ForwardError::BadUpstreamUrl(upstream_url.to_owned()))?;

    // Rewrite headers: strip hop-by-hop, rewrite host.
    // Auth headers (authorization, x-api-key) are forwarded byte-identical.
    // (AD-PXY-15: no Via, no extra headers outside the allowed-list.)
    let mut new_headers = hyper::HeaderMap::new();
    for (name, value) in &req_parts.headers {
        if is_hop_by_hop(name.as_str()) {
            // Strip hop-by-hop (connection, upgrade, te, etc.)
            continue;
        }
        if name.as_str().eq_ignore_ascii_case("host") {
            // Rewrite host to the upstream authority below.
            continue;
        }
        new_headers.insert(name.clone(), value.clone());
    }

    // Rewrite host to upstream authority (AC12).
    if let Some(authority) = upstream_authority(upstream_url)
        && let Ok(host_value) = HeaderValue::from_str(&authority)
    {
        new_headers.insert(hyper::header::HOST, host_value);
    }

    req_parts.uri = upstream_uri;
    req_parts.headers = new_headers;

    let upstream_req = Request::from_parts(req_parts, Full::from(body));

    client
        .inner
        .request(upstream_req)
        .await
        .map_err(|e| ForwardError::Upstream(e.to_string()))
}

// ============================================================================
// ForwardError
// ============================================================================

/// Errors that can occur during the forwarding step.
///
/// These are translated by the server layer to clean HTTP error responses
/// (502, 504) so the client socket is never dropped without a response (AC10).
#[derive(Debug)]
pub enum ForwardError {
    /// The upstream URL is malformed or the authority cannot be extracted.
    BadUpstreamUrl(String),
    /// The upstream connection failed or the request was rejected.
    ///
    /// This covers: connect-refused, TCP reset, HTTP protocol errors.
    Upstream(String),
}

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardError::BadUpstreamUrl(u) => write!(f, "bad upstream URL: {u}"),
            ForwardError::Upstream(e) => write!(f, "upstream error: {e}"),
        }
    }
}

// ============================================================================
// Tests (AC12, AC13)
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // AC12: hop-by-hop detection is case-insensitive and covers all RFC 9110 headers.
    #[test]
    fn test_hop_by_hop_detection() {
        // All RFC 9110 hop-by-hop headers must be detected.
        for &header in HOP_BY_HOP_HEADERS {
            assert!(
                is_hop_by_hop(header),
                "should detect lowercase hop-by-hop: {header}"
            );
            assert!(
                is_hop_by_hop(&header.to_ascii_uppercase()),
                "should detect uppercase hop-by-hop: {header}"
            );
        }
    }

    // AC12: non-hop-by-hop headers are NOT stripped.
    #[test]
    fn test_non_hop_by_hop_not_stripped() {
        let headers = [
            "content-type",
            "authorization",
            "x-api-key",
            "accept",
            "anthropic-version",
            "x-request-id",
            "content-length",
        ];
        for header in &headers {
            assert!(
                !is_hop_by_hop(header),
                "should NOT strip content header: {header}"
            );
        }
    }

    // AC12: Via is NOT in the hop-by-hop list — it's intentionally not added.
    // This test documents that Via is absent (not stripped, just never added).
    #[test]
    fn test_via_not_in_hop_by_hop() {
        assert!(
            !is_hop_by_hop("via"),
            "Via is not stripped — it is never added (AD-PXY-15)"
        );
    }

    // Upstream URI construction: path/query preserved, authority replaced.
    #[test]
    fn test_build_upstream_uri_preserves_path() {
        let original: Uri = "http://localhost:41322/v1/messages?stream=true"
            .parse()
            .unwrap();
        let result = build_upstream_uri(&original, "https://api.anthropic.com");
        assert_eq!(
            result.unwrap().to_string(),
            "https://api.anthropic.com/v1/messages?stream=true"
        );
    }

    // Upstream URI: Azure-style base path with custom prefix.
    #[test]
    fn test_build_upstream_uri_azure_base_path() {
        let original: Uri = "http://localhost:41322/v1/messages".parse().unwrap();
        let result = build_upstream_uri(&original, "https://myresource.openai.azure.com/openai");
        assert_eq!(
            result.unwrap().to_string(),
            "https://myresource.openai.azure.com/openai/v1/messages"
        );
    }

    // Upstream URI: no trailing slash double-slash.
    #[test]
    fn test_build_upstream_uri_no_double_slash() {
        let original: Uri = "http://localhost:41322/v1/messages".parse().unwrap();
        let result = build_upstream_uri(&original, "https://api.anthropic.com/");
        let uri_str = result.unwrap().to_string();
        assert!(
            !uri_str.contains("//v1"),
            "no double-slash between base and path: {uri_str}"
        );
    }

    // Upstream authority extraction.
    #[test]
    fn test_upstream_authority() {
        assert_eq!(
            upstream_authority("https://api.anthropic.com"),
            Some("api.anthropic.com".to_owned())
        );
        assert_eq!(
            upstream_authority("http://localhost:8080"),
            Some("localhost:8080".to_owned())
        );
        assert_eq!(upstream_authority("not-a-url"), None);
    }

    // AC12: HOP_BY_HOP_HEADERS const covers exactly the RFC 9110 §7.6.1 set.
    #[test]
    fn test_hop_by_hop_const_completeness() {
        let expected = [
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
        ];
        // Every expected header must be in the const.
        for h in &expected {
            assert!(
                HOP_BY_HOP_HEADERS.contains(h),
                "missing hop-by-hop header in const: {h}"
            );
        }
        // Const length must equal the expected set (no extras).
        assert_eq!(
            HOP_BY_HOP_HEADERS.len(),
            expected.len(),
            "HOP_BY_HOP_HEADERS has unexpected extra entries"
        );
    }

    // AC13: auth header names are NOT in the hop-by-hop list (they must be forwarded).
    #[test]
    fn test_auth_headers_not_hop_by_hop() {
        assert!(
            !HOP_BY_HOP_HEADERS.contains(&"authorization"),
            "authorization must NOT be stripped"
        );
        assert!(
            !HOP_BY_HOP_HEADERS.contains(&"x-api-key"),
            "x-api-key must NOT be stripped"
        );
    }
}
