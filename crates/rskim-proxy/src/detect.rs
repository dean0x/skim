//! Self-contained provider detection: path-suffix → bounded shallow-JSON shape → Unknown.
//!
//! ## AD-PXY-02 — Detection algorithm
//!
//! Detection is a **self-contained three-stage pipeline** that MUST NOT call
//! `rskim_llm::parse` or any other function that could fail or delay the
//! forwarding path (fail-open forbids coupling forwarding to parse success).
//!
//! 1. **Path suffix match** — `POST …/v1/messages` → Anthropic;
//!    `POST …/v1/chat/completions` → OpenAI. Suffix matching (not exact path)
//!    allows Azure-style custom base paths to classify correctly (AC2).
//!
//! 2. **Bounded shallow-JSON shape fallback** — only when path matches neither.
//!    Uses a `#[derive(Deserialize)]` struct with `IgnoredAny` for non-discriminator
//!    values (mirrors the #302 ShallowBody technique). No full `serde_json::Value`
//!    tree is constructed. Discriminators (AD-PXY-02 §3.4):
//!    - Top-level `system` field AND/OR `messages` array AND/OR `model` starting
//!      with `"claude"` → Anthropic.
//!    - `messages` array with a `role` of `"system"` or `"developer"` AND/OR
//!      `model` NOT starting with `"claude"` → OpenAI.
//!    - `choices` is an OpenAI RESPONSE field, not a request discriminator — excluded.
//!
//! 3. **Tie-break** — both-shaped, neither-shaped, or body truncated/oversize →
//!    **Unknown**. Detection MUST NOT reject, delay, or modify the request.
//!
//! ## Correctness boundary
//!
//! `ProxyProvider` is a LOCAL enum distinct from `rskim_llm::Provider`. The two
//! diverge intentionally: #302's parser always resolves to Anthropic or OpenAI
//! (no Unknown bucket, no path stage). #303's `ProxyProvider::Unknown` is the
//! conservative tie-break that routes to the default upstream (or 502) without
//! guessing. Do NOT conflate the two types.

/// Provider classification produced by the self-contained detection pipeline.
///
/// `#[non_exhaustive]` so future providers can be added without breaking
/// existing match arms in downstream crates (AC24 / AD-PXY-02).
///
/// This enum is LOCAL to the proxy and distinct from `rskim_llm::Provider`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProxyProvider {
    /// Anthropic `/v1/messages` API.
    Anthropic,
    /// OpenAI `/v1/chat/completions` API.
    OpenAI,
    /// Provider could not be determined from path or body shape.
    ///
    /// Tie-break for both-shaped, neither-shaped, truncated, or oversize bodies.
    /// Routes to the default upstream (or 502 if none configured — D8 / AC3).
    /// The transform seam is bypassed entirely for Unknown providers (AD-PXY-02).
    Unknown,
}

// ============================================================================
// Path-suffix detection
// ============================================================================

/// Classify a request path by suffix match.
///
/// Returns `Some(provider)` when the path unambiguously identifies a provider.
/// Returns `None` when the path matches neither known suffix (fall through to
/// shape-based detection).
///
/// Suffix matching (not exact path) allows Azure-style custom base paths:
/// e.g., `POST /azure/v1/messages` classifies as Anthropic.
///
/// AD-PXY-02: path is checked FIRST, before the JSON body is inspected.
fn detect_by_path(path: &str) -> Option<ProxyProvider> {
    // Strip query strings and anchors for a cleaner suffix match.
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);

    if path.ends_with("/v1/messages") {
        Some(ProxyProvider::Anthropic)
    } else if path.ends_with("/v1/chat/completions") {
        Some(ProxyProvider::OpenAI)
    } else {
        None
    }
}

// ============================================================================
// Bounded shallow-JSON shape detection
// ============================================================================

/// Maximum bytes to inspect from the body for shape-based detection.
///
/// Shape detection performs a bounded shallow JSON sniff — it reads only the
/// top-level keys of the JSON object, never the full value tree. Oversize or
/// deeply-nested bodies fall back to Unknown (fail-open, AD-PXY-02 / AC2).
///
/// 8 KiB is sufficient to see all top-level discriminator keys for both
/// Anthropic and OpenAI payloads (model, messages, system are always near the
/// start of any conforming request body). Bodies shorter than 8 KiB are fully
/// inspected. Used by `server.rs` to slice the body before calling
/// [`detect_provider`].
pub(crate) const SHAPE_SNIFF_LIMIT: usize = 8 * 1024;

/// Classify a request body by shallow JSON shape analysis.
///
/// Reads only the discriminator keys of the JSON object using a
/// `#[derive(Deserialize)]` struct with `serde::de::IgnoredAny` for all
/// non-discriminator values. This mirrors the #302 ShallowBody technique
/// (AD-PXY-02, §3.4): no full `Value` tree is constructed, only the keys we
/// need are materialised. Returns:
/// - `Some(Anthropic)` when the shape is exclusively Anthropic-shaped.
/// - `Some(OpenAI)` when the shape is exclusively OpenAI-shaped.
/// - `None` for both-shaped, neither-shaped, parse failure, or truncated body.
///
/// MUST NOT construct a full `Value` tree. MUST NOT call `rskim_llm::parse`.
/// AD-PXY-02: shape detection is the fallback, not the primary path.
fn detect_by_shape(body: &[u8]) -> Option<ProxyProvider> {
    use serde::de::IgnoredAny;
    use serde::Deserialize;

    // Only inspect up to SHAPE_SNIFF_LIMIT bytes.
    let sniff = if body.len() > SHAPE_SNIFF_LIMIT {
        &body[..SHAPE_SNIFF_LIMIT]
    } else {
        body
    };

    let Ok(text) = std::str::from_utf8(sniff) else {
        // Not valid UTF-8 → cannot be a valid JSON request body → Unknown.
        return None;
    };

    // Shallow-parse: only materialise the discriminator keys. All other top-level
    // fields are consumed as IgnoredAny (no allocation). Nested message role values
    // use a minimal role-only struct; all other message fields are IgnoredAny.
    // This mirrors the #302 ShallowBody technique (AD-PXY-02 §3.4).
    //
    // Discriminator table (AD-PXY-02 §3.4):
    //   Anthropic: top-level `system` field present; OR `model` starting with "claude".
    //   OpenAI:    `messages[].role` contains "system" or "developer"; OR `model`
    //              NOT starting with "claude" (when model is set).
    //
    // Note: `choices` is an OpenAI RESPONSE field — never present in a request body.
    //       It is not a valid request discriminator and is excluded from this table.

    #[derive(Deserialize)]
    struct ShallowMessage {
        #[serde(default)]
        role: Option<String>,
        #[serde(flatten)]
        _rest: std::collections::HashMap<String, IgnoredAny>,
    }

    #[derive(Deserialize)]
    struct ShallowBody {
        #[serde(default)]
        system: Option<IgnoredAny>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        messages: Option<Vec<ShallowMessage>>,
    }

    let Ok(body) = serde_json::from_str::<ShallowBody>(text) else {
        return None;
    };

    // Anthropic discriminators.
    let has_system_field = body.system.is_some();
    let model_str = body.model.as_deref().unwrap_or("");
    let model_is_claude = model_str.starts_with("claude");
    let model_is_set = !model_str.is_empty();

    // Check messages array for OpenAI-specific role values.
    let has_openai_role = body
        .messages
        .as_ref()
        .is_some_and(|msgs| {
            msgs.iter()
                .any(|msg| msg.role.as_deref().is_some_and(|r| matches!(r, "system" | "developer")))
        });
    let has_messages = body.messages.is_some();

    // Score Anthropic signals.
    let anthropic_signals = (has_system_field as u8)
        + (model_is_claude as u8)
        + (has_messages && !has_openai_role) as u8;

    // Score OpenAI signals (request-body signals only — no response-only fields).
    let openai_signals = (has_openai_role as u8) + ((model_is_set && !model_is_claude) as u8);

    match (anthropic_signals, openai_signals) {
        (a, 0) if a > 0 => Some(ProxyProvider::Anthropic),
        (0, o) if o > 0 => Some(ProxyProvider::OpenAI),
        // Both-shaped or neither-shaped → Unknown (tie-break, AD-PXY-02).
        _ => None,
    }
}

// ============================================================================
// Public detection API
// ============================================================================

/// Classify the provider for an HTTP request.
///
/// This is the full three-stage detection pipeline (AD-PXY-02):
/// 1. Path suffix match.
/// 2. Bounded JSON shape fallback (only when path matches neither).
/// 3. Tie-break → Unknown.
///
/// Detection MUST NOT reject, delay, or modify the request. It is always
/// infallible: all error cases resolve to `ProxyProvider::Unknown`.
///
/// # Arguments
///
/// - `path` — the HTTP request path (e.g., `/v1/messages`).
/// - `body` — the buffered request body bytes. May be empty or non-UTF-8;
///   detection handles both gracefully. Bodies larger than [`SHAPE_SNIFF_LIMIT`]
///   are partially inspected; the full body is not required.
///
/// # Examples
///
/// ```rust
/// use rskim_proxy::detect::{detect_provider, ProxyProvider};
///
/// assert_eq!(detect_provider("/v1/messages", b"{}"), ProxyProvider::Anthropic);
/// assert_eq!(detect_provider("/v1/chat/completions", b"{}"), ProxyProvider::OpenAI);
/// assert_eq!(detect_provider("/v1/unknown", b"not json"), ProxyProvider::Unknown);
/// ```
pub fn detect_provider(path: &str, body: &[u8]) -> ProxyProvider {
    // Stage 1: path suffix match.
    if let Some(provider) = detect_by_path(path) {
        return provider;
    }
    // Stage 2: bounded shallow-JSON shape fallback.
    if let Some(provider) = detect_by_shape(body) {
        return provider;
    }
    // Stage 3: tie-break → Unknown.
    ProxyProvider::Unknown
}

// ============================================================================
// Tests (AC2)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Path-suffix detection (Stage 1) — AC2
    // -------------------------------------------------------------------------

    #[test]
    fn test_path_exact_anthropic() {
        assert_eq!(
            detect_provider("/v1/messages", b""),
            ProxyProvider::Anthropic
        );
    }

    #[test]
    fn test_path_exact_openai() {
        assert_eq!(
            detect_provider("/v1/chat/completions", b""),
            ProxyProvider::OpenAI
        );
    }

    // Azure-style custom base paths (AC2: suffix match, not exact).
    #[test]
    fn test_path_azure_style_anthropic() {
        assert_eq!(
            detect_provider("/azure/openai/deployments/my-model/v1/messages", b""),
            ProxyProvider::Anthropic
        );
    }

    #[test]
    fn test_path_azure_style_openai() {
        assert_eq!(
            detect_provider("/openai/deployments/gpt-4o/v1/chat/completions", b""),
            ProxyProvider::OpenAI
        );
    }

    // Query string must not affect path suffix detection.
    #[test]
    fn test_path_with_query_string_anthropic() {
        assert_eq!(
            detect_provider("/v1/messages?debug=1", b""),
            ProxyProvider::Anthropic
        );
    }

    // Unrecognised path falls through to shape detection.
    #[test]
    fn test_path_unknown_falls_through_to_shape() {
        // Empty body + unknown path → Unknown.
        assert_eq!(
            detect_provider("/v1/embeddings", b"{}"),
            ProxyProvider::Unknown
        );
    }

    // -------------------------------------------------------------------------
    // Shape-based detection (Stage 2) — AC2
    // -------------------------------------------------------------------------

    #[test]
    fn test_shape_anthropic_system_field() {
        let body = br#"{"system": "You are a helpful assistant.", "messages": [], "model": "claude-3-5-sonnet-20241022"}"#;
        assert_eq!(detect_provider("/v1/other", body), ProxyProvider::Anthropic);
    }

    #[test]
    fn test_shape_anthropic_claude_model() {
        let body =
            br#"{"model": "claude-opus-4", "messages": [{"role": "user", "content": "hello"}]}"#;
        assert_eq!(detect_provider("/v1/other", body), ProxyProvider::Anthropic);
    }

    #[test]
    fn test_shape_openai_developer_role() {
        let body = br#"{"model": "gpt-4o", "messages": [{"role": "developer", "content": "You are helpful."}, {"role": "user", "content": "hi"}]}"#;
        assert_eq!(detect_provider("/v1/other", body), ProxyProvider::OpenAI);
    }

    #[test]
    fn test_shape_openai_system_role_non_claude_model() {
        let body = br#"{"model": "gpt-4o-mini", "messages": [{"role": "system", "content": "Be concise."}, {"role": "user", "content": "hello"}]}"#;
        assert_eq!(detect_provider("/v1/other", body), ProxyProvider::OpenAI);
    }

    // -------------------------------------------------------------------------
    // Tie-break → Unknown (Stage 3) — AC2, AC3
    // -------------------------------------------------------------------------

    #[test]
    fn test_unknown_for_neither_shaped() {
        // No discriminators present.
        assert_eq!(
            detect_provider("/v1/other", br#"{"foo": "bar"}"#),
            ProxyProvider::Unknown
        );
    }

    #[test]
    fn test_unknown_for_non_json() {
        assert_eq!(
            detect_provider("/v1/other", b"not json at all"),
            ProxyProvider::Unknown
        );
    }

    #[test]
    fn test_unknown_for_empty_body_unknown_path() {
        assert_eq!(detect_provider("/v1/other", b""), ProxyProvider::Unknown);
    }

    #[test]
    fn test_unknown_for_malformed_json() {
        assert_eq!(
            detect_provider("/v1/other", b"{broken json"),
            ProxyProvider::Unknown
        );
    }

    // Path-detection supersedes shape — even if body is OpenAI-shaped, path wins.
    #[test]
    fn test_path_wins_over_shape_anthropic_path_openai_body() {
        let openai_body =
            br#"{"model": "gpt-4o", "messages": [{"role": "system", "content": "test"}]}"#;
        // Path says Anthropic → Anthropic wins regardless of body shape.
        assert_eq!(
            detect_provider("/v1/messages", openai_body),
            ProxyProvider::Anthropic
        );
    }

    // PF-007: detection is infallible — never panics even on adversarial input.
    #[test]
    fn test_detection_is_infallible_on_adversarial_input() {
        let adversarial_inputs: &[&[u8]] = &[
            b"",
            b"\x00\x01\x02\xff",
            b"{\"nested\": {\"deeply\": {\"nested\": true}}}",
            b"null",
            b"[]",
            b"42",
            b"\"string\"",
        ];
        for input in adversarial_inputs {
            // Must not panic — result is Unknown for all these.
            let _ = detect_provider("/v1/other", input);
        }
    }

    // NEGATIVE discriminating test: path suffix detection distinguishes paths.
    // If detect_by_path did not check suffixes, /foo/v1/messages and /v1/bad
    // would both return Unknown. Deleting the suffix check would fail this test.
    #[test]
    fn test_discriminating_path_suffix_not_prefix() {
        // Matches suffix → Anthropic.
        assert_eq!(
            detect_provider("/custom/base/v1/messages", b""),
            ProxyProvider::Anthropic,
            "suffix match must classify Azure-style paths"
        );
        // Does NOT match suffix (prefix only) → falls through to shape → Unknown.
        assert_eq!(
            detect_provider("/v1/messages/and/more", b""),
            ProxyProvider::Unknown,
            "non-suffix match must NOT classify as Anthropic"
        );
    }
}
