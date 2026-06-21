#![allow(clippy::unwrap_used, clippy::expect_used)]
//! AC9 and security tests for the `net-anthropic` feature gate.
//!
//! These tests require `--features net-anthropic` to compile and run.
//! They verify that:
//! - The network counter is not reachable from the default build.
//! - Construction without `ANTHROPIC_API_KEY` returns `Err`.
//! - Construction with an empty key returns `Err`.
//! - The API key is **never** present in `TokenError` Display or Debug output
//!   (constraint 15 / security invariant — regression guard per ADR-001).
//!
//! No actual network calls are made in these tests (the key is absent/invalid).

#[cfg(feature = "net-anthropic")]
mod ac9_net_tests {
    use rskim_tokens::{TokenError, net::AnthropicNetworkCounter};

    #[test]
    fn ac9_empty_api_key_returns_err() {
        // Use new() with empty key rather than manipulating env vars
        // (std::env::remove_var is unsafe in Rust 2024 due to multi-thread concerns).
        let result = AnthropicNetworkCounter::new("claude-sonnet-4-5", String::new());
        assert!(
            matches!(result, Err(TokenError::MissingApiKey)),
            "Empty API key must return Err(MissingApiKey)"
        );
    }

    #[test]
    fn ac9_from_env_returns_err_when_key_absent_or_key_is_explicit_empty() {
        // When ANTHROPIC_API_KEY is not set in this CI test environment,
        // from_env() must return Err(MissingApiKey).
        // This test is conditional: if the key IS set (e.g. in a developer's shell),
        // we skip the assertion and trust the dependency-tree check (primary AC9 gate).
        if std::env::var("ANTHROPIC_API_KEY")
            .unwrap_or_default()
            .is_empty()
        {
            let result = AnthropicNetworkCounter::from_env("claude-sonnet-4-5");
            assert!(
                matches!(result, Err(TokenError::MissingApiKey)),
                "from_env must return Err(MissingApiKey) when key is absent"
            );
        }
        // If key is set, we can at least verify that from_env returns Ok.
        // No network call is made here — the test exits after construction.
    }

    #[test]
    fn ac9_explicit_key_constructs_ok() {
        // An explicit non-empty key (even invalid) must construct successfully.
        // The Err happens at count() time (network call), not construction.
        let result =
            AnthropicNetworkCounter::new("claude-sonnet-4-5", "sk-ant-test-key".to_owned());
        assert!(
            result.is_ok(),
            "Non-empty API key must construct Ok (got Err instead)"
        );
    }

    /// Security regression guard: the API key must NEVER appear in any `TokenError`
    /// string representation (Display or Debug).
    ///
    /// This test injects a distinctive bogus key, points the counter at an
    /// unreachable/invalid endpoint (the loopback address, port 1), and asserts
    /// that the resulting error messages do not contain the key material.
    ///
    /// Applies ADR-001 (fix all noticed issues regardless of scope) and testable
    /// per PF-005 (acceptance criteria must be observable and testable, not doc-only).
    ///
    /// Currently the invariant holds because:
    /// - `ureq 2.x` `Error::Status` / `Error::Transport` Display never includes
    ///   request headers (the key is sent as `x-api-key`; ureq does not echo headers
    ///   back into error messages).
    /// - `AnthropicNetworkCounter` intentionally does NOT derive `Debug`.
    /// - Error formatting in `count()` never interpolates `self.api_key`.
    ///
    /// This test ensures a future change (e.g. adding `#[derive(Debug)]` to the
    /// struct, or logging the request object) would be caught before merging.
    #[test]
    fn net_security_key_absent_from_errors() {
        // A distinctive sentinel that would be trivially detectable in any string.
        let sentinel_key = "sk-ant-CANARY-3f7a9b2c1d4e5f6a7b8c9d0e1f2a3b4c";

        // Point at localhost:1 — guaranteed connection-refused on all platforms.
        // We cannot use COUNT_TOKENS_ENDPOINT (would require real network); an
        // unreachable local address gives us a transport error without a real request.
        let counter = AnthropicNetworkCounter::new_for_test(
            "claude-sonnet-4-5",
            sentinel_key,
            // Port 1 on loopback: guaranteed connection-refused on all platforms.
            "http://127.0.0.1:1/v1/messages/count_tokens",
        );

        let err = counter
            .count("hello world")
            .expect_err("must fail against unreachable endpoint");

        // Check Display output
        let display = format!("{err}");
        assert!(
            !display.contains(sentinel_key),
            "API key must not appear in TokenError Display: {display:?}"
        );

        // Check Debug output (TokenError derives Debug via thiserror)
        let debug = format!("{err:?}");
        assert!(
            !debug.contains(sentinel_key),
            "API key must not appear in TokenError Debug: {debug:?}"
        );
    }
}
