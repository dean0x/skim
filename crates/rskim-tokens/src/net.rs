//! Network-backed Anthropic token counter.
//!
//! This module is only compiled when the `net-anthropic` Cargo feature is
//! enabled. It is **not** part of the default build and is never reachable
//! without explicitly opting in (AC9 / constraint 15).
//!
//! # Usage
//!
//! ```no_run
//! # #[cfg(feature = "net-anthropic")]
//! # {
//! use rskim_tokens::net::AnthropicNetworkCounter;
//!
//! let counter = AnthropicNetworkCounter::from_env("claude-sonnet-4-5")?;
//! let n = counter.count("Hello, world!")?;
//! println!("{n} tokens");
//! # Ok::<(), rskim_tokens::TokenError>(())
//! # }
//! ```
//!
//! # Important caveats
//!
//! - Counts include the `/v1/messages/count_tokens` request envelope overhead
//!   and are **not** comparable to bare-text offline counts.
//! - The API key is read from `ANTHROPIC_API_KEY` at construction time and is
//!   **never logged** (constraint 15).
//! - Requests use a fixed timeout and a bounded retry count (constraint 3).

#[cfg(feature = "net-anthropic")]
mod inner {
    use crate::TokenError;
    use std::time::Duration;

    /// Request timeout for each Anthropic API call.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    /// Maximum number of retries on transient failure.
    const MAX_RETRIES: u32 = 2;

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
    /// **never logged**, printed, or included in error messages.
    pub struct AnthropicNetworkCounter {
        /// The Anthropic model ID to pass in the count-tokens request.
        model: String,
        /// Pre-built ureq agent with timeout configured.
        agent: ureq::Agent,
        /// API key — stored but never logged.
        api_key: String,
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
            let api_key =
                std::env::var("ANTHROPIC_API_KEY").map_err(|_| TokenError::MissingApiKey)?;
            if api_key.is_empty() {
                return Err(TokenError::MissingApiKey);
            }
            Ok(Self::new_with_key(model_id, api_key))
        }

        /// Construct a counter with an explicit API key (for testing).
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
            Ok(Self::new_with_key(model_id, api_key))
        }

        fn new_with_key(model_id: &str, api_key: String) -> Self {
            let agent = ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build();
            Self {
                model: model_id.to_owned(),
                agent,
                api_key,
            }
        }

        /// Count tokens by calling the Anthropic `/v1/messages/count_tokens` API.
        ///
        /// Wraps `text` as a single user message. The returned count includes the
        /// request envelope overhead and is **not** comparable to bare-text offline
        /// counts.
        ///
        /// Retries up to [`MAX_RETRIES`] times on transient failure. A fixed
        /// per-request timeout of [`REQUEST_TIMEOUT`] is enforced.
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

            let mut last_err: Option<String> = None;

            // Bounded retry loop (constraint 3: no unbounded loops/retries).
            for attempt in 0..=MAX_RETRIES {
                let result = self
                    .agent
                    .post(COUNT_TOKENS_ENDPOINT)
                    .set("x-api-key", &self.api_key)
                    .set("anthropic-version", ANTHROPIC_VERSION)
                    .set("content-type", "application/json")
                    .send_string(&body.to_string());

                match result {
                    Ok(response) => {
                        let json: serde_json::Value = response.into_json().map_err(|e| {
                            TokenError::ApiResponse(format!("JSON parse error: {e}"))
                        })?;

                        let input_tokens = json
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .ok_or_else(|| {
                                TokenError::ApiResponse(format!(
                                    "missing 'input_tokens' field in response: {json}"
                                ))
                            })?;

                        return Ok(input_tokens as usize);
                    }
                    Err(e) => {
                        last_err = Some(format!("attempt {attempt}: {e}"));
                        // Continue to next retry
                    }
                }
            }

            Err(TokenError::NetworkRequest(
                last_err.unwrap_or_else(|| "unknown network error".to_owned()),
            ))
        }
    }
}

#[cfg(feature = "net-anthropic")]
pub use inner::AnthropicNetworkCounter;
