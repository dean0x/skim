//! Network-backed Anthropic token counter.
//!
//! This module is only compiled when the `net-anthropic` Cargo feature is
//! enabled. It is **not** part of the default build and is never reachable
//! without explicitly opting in (AC9 / constraint 15).
//!
//! # Usage
//!
//! ```no_run
//! # fn example() -> Result<(), rskim_tokens::TokenError> {
//! use rskim_tokens::net::AnthropicNetworkCounter;
//!
//! let counter = AnthropicNetworkCounter::from_env("claude-sonnet-4-5")?;
//! let n = counter.count("Hello, world!")?;
//! println!("{n} tokens");
//! # Ok(())
//! # }
//! # #[cfg(feature = "net-anthropic")]
//! # example().ok();
//! ```
//!
//! # Important caveats
//!
//! - Counts include the `/v1/messages/count_tokens` request envelope overhead
//!   and are **not** comparable to bare-text offline counts.
//! - The API key is read from `ANTHROPIC_API_KEY` at construction time and is
//!   **never logged** (constraint 15). Verified by the
//!   `net_security_key_absent_from_errors` test in `tests/net_integration.rs`.
//! - Requests use a fixed timeout and a bounded retry count (constraint 3).
//! - Only transient errors (transport / 5xx / 429) are retried; permanent 4xx
//!   client errors short-circuit immediately to avoid burning the retry budget
//!   on a definitively failed request.

use crate::TokenError;
use std::io::Read as _;
use std::thread;
use std::time::Duration;

/// Request timeout for each Anthropic API call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of retries on **transient** failure (transport errors, 5xx, 429).
/// Permanent 4xx errors bypass retries and return immediately.
const MAX_RETRIES: u32 = 2;

/// Exponential backoff base delay between retry attempts.
const BACKOFF_BASE_MS: u64 = 250;

/// Maximum body bytes to read from a response, guarding against unbounded allocation
/// from a buggy or hostile server.
const MAX_BODY_BYTES: u64 = 64 * 1024; // 64 KiB

/// Maximum characters of a response body included verbatim in an error message.
/// Bounds `TokenError::ApiResponse` strings (parse-at-boundary discipline).
const MAX_ERROR_BODY_LEN: usize = 512;

/// Endpoint for the Anthropic token-counting API.
const COUNT_TOKENS_ENDPOINT: &str = "https://api.anthropic.com/v1/messages/count_tokens";

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A network-backed token counter that calls the Anthropic count-tokens API.
///
/// Constructed via [`AnthropicNetworkCounter::from_env`] (reads `ANTHROPIC_API_KEY`
/// from environment) or [`AnthropicNetworkCounter::new`] (explicit key).
///
/// # Security
///
/// The API key is stored in memory for the lifetime of this struct but is
/// **never logged**, printed, or included in error messages (constraint 15).
/// The struct intentionally does **not** derive `Debug` to prevent accidental
/// key exposure via `{:?}` formatting.
pub struct AnthropicNetworkCounter {
    /// The Anthropic model ID to pass in the count-tokens request.
    model: String,
    /// Pre-built ureq agent with timeout configured.
    agent: ureq::Agent,
    /// API key — stored but never logged.
    api_key: String,
    /// Endpoint URL. Production code always uses `COUNT_TOKENS_ENDPOINT`;
    /// tests may override via `new_for_test` to point at an unreachable local address.
    endpoint: &'static str,
}

impl AnthropicNetworkCounter {
    /// Construct a counter by reading `ANTHROPIC_API_KEY` from the environment.
    ///
    /// Returns [`TokenError::MissingApiKey`] if the variable is absent or empty.
    ///
    /// # Errors
    ///
    /// - [`TokenError::MissingApiKey`] — `ANTHROPIC_API_KEY` not set.
    pub fn from_env(model_id: &str) -> Result<Self, TokenError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| TokenError::MissingApiKey)?;
        if api_key.is_empty() {
            return Err(TokenError::MissingApiKey);
        }
        Ok(Self::with_endpoint(
            model_id,
            api_key,
            COUNT_TOKENS_ENDPOINT,
        ))
    }

    /// Construct a counter with an explicit API key.
    ///
    /// The key is **never logged** or included in error output.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::MissingApiKey`] if `api_key` is empty.
    pub fn new(model_id: &str, api_key: String) -> Result<Self, TokenError> {
        if api_key.is_empty() {
            return Err(TokenError::MissingApiKey);
        }
        Ok(Self::with_endpoint(
            model_id,
            api_key,
            COUNT_TOKENS_ENDPOINT,
        ))
    }

    fn with_endpoint(model_id: &str, api_key: String, endpoint: &'static str) -> Self {
        let agent = ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build();
        Self {
            model: model_id.to_owned(),
            agent,
            api_key,
            endpoint,
        }
    }

    /// Test-seam constructor that overrides the endpoint URL with a short timeout.
    ///
    /// Used by the `net_security_key_absent_from_errors` integration test to point
    /// the counter at a local unreachable address without making real network requests,
    /// so the test does not depend on the Anthropic API being reachable in CI.
    ///
    /// This is a test-only seam. It is exposed publicly (not `pub(crate)`) because
    /// it is called from `tests/net_integration.rs` (an external integration test binary).
    /// The `rskim-tokens` crate is `publish = false`, so this does not widen the
    /// published API surface.
    #[doc(hidden)]
    pub fn new_for_test(model_id: &str, api_key: &str, endpoint: &'static str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(500))
            .build();
        Self {
            model: model_id.to_owned(),
            agent,
            api_key: api_key.to_owned(),
            endpoint,
        }
    }

    /// Count tokens by calling the Anthropic `/v1/messages/count_tokens` API.
    ///
    /// Wraps `text` as a single user message. The returned count includes the
    /// request envelope overhead and is **not** comparable to bare-text offline
    /// counts.
    ///
    /// Retries up to `MAX_RETRIES` (2) times on **transient** failure (transport
    /// errors, 5xx, 429) with exponential backoff. A fixed per-request timeout of
    /// `REQUEST_TIMEOUT` (10s) is enforced. Permanent 4xx errors (auth failure,
    /// bad request) short-circuit immediately without retrying.
    ///
    /// # Errors
    ///
    /// - [`TokenError::NetworkRequest`] — connection failed, timed out, or got a non-2xx response.
    /// - [`TokenError::ApiResponse`] — response body was not valid JSON or lacked `input_tokens`.
    pub fn count(&self, text: &str) -> Result<usize, TokenError> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{"role": "user", "content": text}]
        });
        let body_str = body.to_string();

        let mut last_err: Option<String> = None;

        // Bounded retry loop (constraint 3: no unbounded loops/retries).
        // Only transient failures (transport / 5xx / 429) are retried.
        // Permanent 4xx client errors return immediately to avoid burning
        // the retry budget on a definitively non-retriable failure.
        for attempt in 0..=MAX_RETRIES {
            // Exponential backoff before each retry (not before the first attempt).
            if attempt > 0 {
                let delay_ms = BACKOFF_BASE_MS * (1u64 << (attempt - 1));
                thread::sleep(Duration::from_millis(delay_ms));
            }

            let result = self
                .agent
                .post(self.endpoint)
                .set("x-api-key", &self.api_key)
                .set("anthropic-version", ANTHROPIC_VERSION)
                .set("content-type", "application/json")
                .send_string(&body_str);

            match result {
                Ok(response) => {
                    // Read with a bounded reader to guard against unbounded allocation
                    // from a buggy or hostile server (reliability: every resource bounded).
                    let reader = response.into_reader();
                    let mut raw = Vec::new();
                    reader
                        .take(MAX_BODY_BYTES)
                        .read_to_end(&mut raw)
                        .map_err(|e| {
                            TokenError::ApiResponse(format!("response read error: {e}"))
                        })?;

                    let json: serde_json::Value = serde_json::from_slice(&raw)
                        .map_err(|e| TokenError::ApiResponse(format!("JSON parse error: {e}")))?;

                    let input_tokens = json
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            // Truncate embedded body to bound error-message length
                            // (parse-at-boundary discipline — avoids unbounded strings in Err).
                            let body_repr = json.to_string();
                            let truncated = if body_repr.len() > MAX_ERROR_BODY_LEN {
                                format!("{}…", &body_repr[..MAX_ERROR_BODY_LEN])
                            } else {
                                body_repr
                            };
                            TokenError::ApiResponse(format!(
                                "missing 'input_tokens' field in response: {truncated}"
                            ))
                        })?;

                    // Saturating cast: u64 → usize.
                    // Token counts exceeding usize::MAX are astronomically unlikely,
                    // but we use checked conversion + saturation for correctness on
                    // 32-bit targets (avoids PF-004 silent narrowing).
                    let count = usize::try_from(input_tokens).unwrap_or(usize::MAX);
                    return Ok(count);
                }
                Err(ureq::Error::Status(code, _)) if (400..500).contains(&code) && code != 429 => {
                    // Permanent 4xx client error (e.g. 401 invalid key, 400 bad request,
                    // 403 forbidden, 413 payload too large). Do NOT retry — the error is
                    // non-transient and retrying only burns latency and request quota.
                    return Err(TokenError::NetworkRequest(format!(
                        "permanent client error: HTTP {code}"
                    )));
                }
                Err(e) => {
                    // Transient: transport error, 5xx, or 429 rate-limit.
                    last_err = Some(format!("attempt {attempt}: {e}"));
                }
            }
        }

        Err(TokenError::NetworkRequest(
            last_err.unwrap_or_else(|| "unknown network error".to_owned()),
        ))
    }
}
