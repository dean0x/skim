//! Header-shape auth classification → [`AuthMode`].
//!
//! ## AD-PXY-08 — Auth-mode classification rules
//!
//! Auth mode is classified by HTTP **header shape only** — never by reading,
//! storing, or logging header values. The proxy produces this signal; #304
//! consumes it to select the per-request policy (D1 / D3).
//!
//! - `Authorization: Bearer <token>` present → [`AuthMode::Subscription`]
//! - `x-api-key: <key>` present → [`AuthMode::ApiKey`]
//! - Neither header → [`AuthMode::Ambiguous`]
//! - Both headers → [`AuthMode::Ambiguous`] (conservative; #304 maps
//!   `Ambiguous → ApiKey (Policy::Default)` per D1)
//!
//! ## Security invariant (AC13 / invariant 7)
//!
//! Auth header VALUES are NEVER inspected — classification uses only the
//! presence/absence of header NAMES. If both headers are present the result is
//! `Ambiguous` rather than attempting to interpret which takes precedence.
//!
//! The signal is read-only in [`crate::seam::TransformContext`]; it flows through
//! the seam to #304 which selects the per-call policy without storing state.

/// Header-shape auth classification.
///
/// `#[non_exhaustive]` so future auth-header patterns can be added without
/// breaking existing match arms in #304 or downstream crates (AC24 / AD-PXY-08).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthMode {
    /// `x-api-key` header present. Typical for Anthropic direct API keys.
    ///
    /// #304 maps this to `Policy::Default` (lossless + lossy compression allowed).
    ApiKey,

    /// `Authorization: Bearer …` header present. Typical for subscription / OAuth flows.
    ///
    /// #304 maps this to `Policy::LosslessOnly` (conservative: never compress lossy).
    Subscription,

    /// Neither `x-api-key` nor `Authorization: Bearer` header present, OR both are
    /// present simultaneously.
    ///
    /// Conservative default: #304 maps `Ambiguous → ApiKey (Policy::Default)` (D1).
    /// The ambiguity is recorded in the decision log but never causes a request failure.
    Ambiguous,
}

/// Header names we classify on (lowercase, as normalised by HTTP/1.1 parsers).
const HEADER_API_KEY: &str = "x-api-key";
const HEADER_AUTHORIZATION: &str = "authorization";
const BEARER_PREFIX: &str = "bearer ";

/// Classify the auth mode from a set of HTTP header name-value pairs.
///
/// Accepts an iterator of `(name, value)` pairs. Both are expected to be
/// lowercase-normalised (HTTP/1.1 headers are case-insensitive; callers SHOULD
/// normalise to lowercase before classifying).
///
/// VALUE bytes are inspected ONLY to check for the `bearer ` prefix — we never
/// read, store, or log the actual token content. Name-only classification is
/// insufficient because `Authorization: Basic …` must not classify as Subscription.
///
/// AD-PXY-08: shape-only, no token introspection, no network, no value logging.
pub fn classify_auth<'a>(headers: impl Iterator<Item = (&'a str, &'a str)>) -> AuthMode {
    let mut has_api_key = false;
    let mut has_bearer = false;

    for (name, value) in headers {
        if name == HEADER_API_KEY {
            has_api_key = true;
        } else if name == HEADER_AUTHORIZATION {
            // Only `Authorization: Bearer …` classifies as Subscription.
            // `Authorization: Basic …` or `Authorization: Token …` → Ambiguous
            // because we cannot safely assume they map to a subscription flow.
            if value.to_ascii_lowercase().starts_with(BEARER_PREFIX) {
                has_bearer = true;
            }
        }
    }

    match (has_api_key, has_bearer) {
        (true, false) => AuthMode::ApiKey,
        (false, true) => AuthMode::Subscription,
        // Both present OR neither present → Ambiguous (AD-PXY-08).
        _ => AuthMode::Ambiguous,
    }
}

// ============================================================================
// Tests (AC24 — shape-only, no value leakage)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Happy-path classification
    // -------------------------------------------------------------------------

    #[test]
    fn test_api_key_header_classifies_as_api_key() {
        let headers = [("x-api-key", "sk-ant-api03-REDACTED")];
        assert_eq!(
            classify_auth(headers.iter().map(|(k, v)| (*k, *v))),
            AuthMode::ApiKey
        );
    }

    #[test]
    fn test_bearer_token_classifies_as_subscription() {
        let headers = [("authorization", "Bearer eyJhbGciOiJSUzI1NiJ9.REDACTED")];
        assert_eq!(
            classify_auth(headers.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Subscription
        );
    }

    // Bearer prefix must be case-insensitive.
    #[test]
    fn test_bearer_case_insensitive() {
        let headers_upper = [("authorization", "BEARER sk-token")];
        assert_eq!(
            classify_auth(headers_upper.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Subscription,
            "BEARER prefix must classify as Subscription"
        );

        let headers_mixed = [("authorization", "Bearer sk-token")];
        assert_eq!(
            classify_auth(headers_mixed.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Subscription,
            "Bearer prefix must classify as Subscription"
        );
    }

    // -------------------------------------------------------------------------
    // Ambiguous cases
    // -------------------------------------------------------------------------

    #[test]
    fn test_no_auth_header_is_ambiguous() {
        let headers: [(&str, &str); 0] = [];
        assert_eq!(
            classify_auth(headers.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Ambiguous
        );
    }

    #[test]
    fn test_both_headers_present_is_ambiguous() {
        let headers = [
            ("x-api-key", "sk-ant-api03-REDACTED"),
            ("authorization", "Bearer eyJhbGc.REDACTED"),
        ];
        assert_eq!(
            classify_auth(headers.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Ambiguous,
            "both x-api-key AND authorization must produce Ambiguous"
        );
    }

    // Non-Bearer Authorization schemes must not classify as Subscription.
    #[test]
    fn test_basic_auth_is_ambiguous_not_subscription() {
        let headers = [("authorization", "Basic dXNlcjpwYXNz")];
        assert_eq!(
            classify_auth(headers.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Ambiguous,
            "Authorization: Basic must not classify as Subscription"
        );
    }

    #[test]
    fn test_token_auth_scheme_is_ambiguous() {
        let headers = [("authorization", "Token sk-api-token")];
        assert_eq!(
            classify_auth(headers.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Ambiguous,
            "Authorization: Token must not classify as Subscription"
        );
    }

    // -------------------------------------------------------------------------
    // Security: value content does not affect classification shape
    // -------------------------------------------------------------------------

    // NEGATIVE discriminating test (PF-007): deleting the has_api_key check would
    // cause this to return Ambiguous. The test proves the check is load-bearing.
    #[test]
    fn test_discriminating_api_key_present_returns_api_key() {
        let with_key = [("x-api-key", "anything")];
        let without_key: [(&str, &str); 0] = [];
        assert_eq!(
            classify_auth(with_key.iter().map(|(k, v)| (*k, *v))),
            AuthMode::ApiKey,
            "x-api-key present must classify as ApiKey"
        );
        assert_eq!(
            classify_auth(without_key.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Ambiguous,
            "x-api-key absent must NOT classify as ApiKey"
        );
    }

    // Unrelated headers must not affect classification.
    #[test]
    fn test_unrelated_headers_do_not_affect_classification() {
        let headers = [
            ("content-type", "application/json"),
            ("x-request-id", "req-abc-123"),
            ("accept", "*/*"),
        ];
        assert_eq!(
            classify_auth(headers.iter().map(|(k, v)| (*k, *v))),
            AuthMode::Ambiguous,
            "unrelated headers must not affect auth classification"
        );
    }
}
