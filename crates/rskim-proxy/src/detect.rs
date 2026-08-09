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

/// Return the bounded sniff window for `body`: at most [`SHAPE_SNIFF_LIMIT`] bytes.
fn sniff_window(body: &[u8]) -> &[u8] {
    &body[..body.len().min(SHAPE_SNIFF_LIMIT)]
}

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
    use serde::Deserialize;
    use serde::de::IgnoredAny;

    let sniff = sniff_window(body);

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
    let has_openai_role = body.messages.as_ref().is_some_and(|msgs| {
        msgs.iter().any(|msg| {
            msg.role
                .as_deref()
                .is_some_and(|r| matches!(r, "system" | "developer"))
        })
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
// Model extraction (AD-PXY-22)
// ============================================================================

/// Upper bound on top-level keys scanned while looking for `"model"`.
///
/// ## Honest rationale (ADR-003 / PF-005)
///
/// **Not a measured threshold — it is derived from the sniff window.** The scan
/// already cannot see past [`SHAPE_SNIFF_LIMIT`] bytes, and the shortest
/// possible top-level entry (`"a":0,`) costs 6 bytes, so no valid document can
/// present more than `SHAPE_SNIFF_LIMIT / 6` keys inside the window. The counter
/// is the explicit loop bound the reliability rule requires; it can never be the
/// binding constraint for well-formed input.
const MAX_SNIFF_KEYS: usize = SHAPE_SNIFF_LIMIT / 6;

/// Sentinel message for the early-exit `Err` raised once `"model"` is captured.
///
/// Not a failure: [`detect_model`] discards the error and reads the captured
/// value. See the `ModelSeed` doc comment for why `Err` (not `Ok`) is the
/// early-exit signal.
const STOP_AFTER_MODEL: &str = "model key found — scan complete";

/// Largest prefix of `sniff` that is valid UTF-8.
///
/// [`sniff_window`] cuts at a byte offset, which can land in the middle of a
/// multi-byte character. Truncating to `valid_up_to()` keeps the scan working on
/// such bodies instead of discarding the whole window.
fn valid_utf8_prefix(sniff: &[u8]) -> &str {
    match std::str::from_utf8(sniff) {
        Ok(s) => s,
        Err(e) => {
            // SAFETY-equivalent: `valid_up_to()` is by definition a valid boundary.
            std::str::from_utf8(&sniff[..e.valid_up_to()]).unwrap_or("")
        }
    }
}

/// Extract the model string from a request body, verbatim.
///
/// ## AD-PXY-22 — verbatim model extraction
///
/// Reuses the bounded shallow-JSON sniff budget (`SHAPE_SNIFF_LIMIT`) to read
/// only the top-level `"model"` key. The string is stored exactly as supplied —
/// no casing, alias, or version normalization. Grouping in analytics is
/// exact-string.
///
/// ## Why an early-stopping visitor, not `serde_json::from_str::<Struct>`
///
/// A derived struct forces serde to consume the **whole** document to look for
/// further fields, so a body larger than `SHAPE_SNIFF_LIMIT` — which is every
/// real L3 transcript — parses as truncated JSON and yields `None`. That would
/// leave `token_savings.model` NULL for essentially all production traffic and
/// silently empty the per-model breakdown (AC5/AC10/AC11).
///
/// The seed below stops at the `"model"` key and never inspects the rest of the
/// document, so trailing/truncated bytes after that key are tolerated. The
/// honest bound is therefore "detected iff `model` appears within the first
/// [`SHAPE_SNIFF_LIMIT`] bytes" rather than "iff the whole body fits in the
/// sniff window".
///
/// Returns `None` when:
/// - The sniff window holds no valid UTF-8 prefix
/// - JSON parsing fails before reaching `"model"` (malformed, or truncated
///   mid-way through an earlier value)
/// - `"model"` is absent, `null`, or not a string
/// - `"model"` sits beyond the sniff window
///
/// The function MUST NOT delay or reject the request — it is infallible and
/// always fails to `None`.
pub(crate) fn detect_model(body: &[u8]) -> Option<String> {
    use serde::Deserialize;
    use serde::de::{IgnoredAny, MapAccess, Visitor};

    // Bound the sniff to SHAPE_SNIFF_LIMIT — same budget as provider detection.
    let text = valid_utf8_prefix(sniff_window(body));
    if text.is_empty() {
        return None;
    }

    /// Top-level key discriminator — matched without allocating a `String`.
    enum Key {
        Model,
        Other,
    }

    impl<'de> Deserialize<'de> for Key {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct KeyVisitor;
            impl Visitor<'_> for KeyVisitor {
                type Value = Key;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object key")
                }
                fn visit_str<E>(self, v: &str) -> Result<Key, E> {
                    Ok(if v == "model" { Key::Model } else { Key::Other })
                }
            }
            d.deserialize_str(KeyVisitor)
        }
    }

    /// Seed that writes the model into the caller's slot and then aborts.
    ///
    /// The abort is why this is a `DeserializeSeed` writing through `&mut`
    /// rather than a plain `Deserialize` returning the value: `serde_json`'s
    /// `deserialize_map` calls `end_map()` after `visit_map` returns `Ok`, which
    /// requires the map to have been drained to its closing `}`. Returning early
    /// with `Ok` therefore fails on **every** document, truncated or not. Signalling
    /// completion with `Err` skips that check; the value is already in the slot,
    /// and the caller discards the error.
    struct ModelSeed<'a>(&'a mut Option<String>);

    impl<'de> serde::de::DeserializeSeed<'de> for ModelSeed<'_> {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_map(self)
        }
    }

    impl<'de> Visitor<'de> for ModelSeed<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a JSON object with an optional top-level \"model\" key")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            // Explicit loop bound (reliability rule); see MAX_SNIFF_KEYS.
            for _ in 0..MAX_SNIFF_KEYS {
                let Some(key) = map.next_key::<Key>()? else {
                    // Map drained without a `model` key — a clean, complete parse.
                    return Ok(());
                };
                match key {
                    // AD-PXY-22: verbatim. A non-string value (number, object,
                    // `null`) leaves the slot `None` rather than fabricating a
                    // stand-in; either way the scan is finished.
                    Key::Model => {
                        *self.0 = map.next_value::<String>().ok();
                        return Err(serde::de::Error::custom(STOP_AFTER_MODEL));
                    }
                    Key::Other => {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
            }
            Ok(())
        }
    }

    let mut model = None;
    let mut de = serde_json::Deserializer::from_str(text);
    // The result is deliberately discarded: `Err` is either a real parse failure
    // (slot stays `None`) or the STOP_AFTER_MODEL early-exit (slot already set).
    let _ = serde::de::DeserializeSeed::deserialize(ModelSeed(&mut model), &mut de);
    model
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

    // -------------------------------------------------------------------------
    // detect_model (AD-PXY-22)
    // -------------------------------------------------------------------------

    // AD-PXY-22: model is stored verbatim (no normalization).
    #[test]
    fn test_detect_model_verbatim_anthropic() {
        let body = br#"{"model": "claude-3-5-sonnet-20241022", "messages": []}"#;
        assert_eq!(
            detect_model(body),
            Some("claude-3-5-sonnet-20241022".to_owned()),
            "model string must be stored verbatim with no normalization (AD-PXY-22)"
        );
    }

    // AD-PXY-22: verbatim storage — uppercase is kept as-is.
    #[test]
    fn test_detect_model_verbatim_no_normalization() {
        let body = br#"{"model": "GPT-4O-MINI", "messages": []}"#;
        assert_eq!(
            detect_model(body),
            Some("GPT-4O-MINI".to_owned()),
            "uppercase model name must NOT be lowercased (AD-PXY-22)"
        );
    }

    // AD-PXY-22: model absent → None.
    #[test]
    fn test_detect_model_absent() {
        let body = br#"{"messages": [], "system": "be helpful"}"#;
        assert_eq!(detect_model(body), None, "no model field → None");
    }

    // AD-PXY-22: non-UTF-8 → None.
    #[test]
    fn test_detect_model_non_utf8() {
        let body: &[u8] = b"\xff\xfe{\"model\": \"gpt-4\"}";
        assert_eq!(detect_model(body), None, "non-UTF-8 → None (fail-open)");
    }

    // AD-PXY-22: malformed JSON → None.
    #[test]
    fn test_detect_model_malformed_json() {
        assert_eq!(detect_model(b"{broken"), None, "malformed JSON → None");
    }

    // AD-PXY-22: empty body → None.
    #[test]
    fn test_detect_model_empty_body() {
        assert_eq!(detect_model(b""), None, "empty body → None");
    }

    // AD-PXY-22 / regression: a realistic L3 request body is far larger than
    // SHAPE_SNIFF_LIMIT (8 KiB).  The `model` key sits at the top of the object,
    // well inside the sniff window, so it MUST still be extracted — the sniff
    // bound truncates the JSON, and a whole-document parse of a truncated
    // document fails.  Deleting the early-stop map visitor makes this fail
    // (model would be None for essentially all real traffic).
    #[test]
    fn test_detect_model_body_larger_than_sniff_limit() {
        let filler = "x".repeat(SHAPE_SNIFF_LIMIT * 4);
        let body = format!(
            r#"{{"model": "claude-3-5-sonnet-20241022", "messages": [{{"role": "user", "content": "{filler}"}}]}}"#
        );
        assert!(body.len() > SHAPE_SNIFF_LIMIT * 4);
        assert_eq!(
            detect_model(body.as_bytes()),
            Some("claude-3-5-sonnet-20241022".to_owned()),
            "model must be extracted from a body larger than SHAPE_SNIFF_LIMIT"
        );
    }

    // AD-PXY-22: `model` need not be the first key — anything inside the sniff
    // window is reachable even when the body is far larger.
    #[test]
    fn test_detect_model_not_first_key_in_large_body() {
        let filler = "y".repeat(SHAPE_SNIFF_LIMIT * 4);
        let body = format!(
            r#"{{"max_tokens": 1024, "stream": true, "model": "gpt-4o", "messages": [{{"role": "user", "content": "{filler}"}}]}}"#
        );
        assert_eq!(
            detect_model(body.as_bytes()),
            Some("gpt-4o".to_owned()),
            "model after other top-level keys must still be extracted"
        );
    }

    // The sniff window cuts at a byte offset that may split a multi-byte
    // character; the valid-UTF-8 prefix must still be scanned.
    #[test]
    fn test_detect_model_multibyte_boundary_in_large_body() {
        // "€" is 3 bytes, so repeating it guarantees the 8 KiB cut lands
        // mid-character for at least one of the two lengths below.
        for pad in [SHAPE_SNIFF_LIMIT, SHAPE_SNIFF_LIMIT + 1] {
            let filler = "€".repeat(pad);
            let body = format!(r#"{{"model": "claude-opus-4", "system": "{filler}"}}"#);
            assert_eq!(
                detect_model(body.as_bytes()),
                Some("claude-opus-4".to_owned()),
                "multi-byte truncation boundary must not discard the sniff window"
            );
        }
    }

    // AD-PXY-22: when `model` sits beyond the sniff window it is genuinely
    // unavailable — the bound is honest, not silently widened.
    #[test]
    fn test_detect_model_beyond_sniff_window_is_none() {
        let filler = "x".repeat(SHAPE_SNIFF_LIMIT * 2);
        let body = format!(r#"{{"system": "{filler}", "model": "gpt-4o"}}"#);
        assert_eq!(
            detect_model(body.as_bytes()),
            None,
            "a model key past SHAPE_SNIFF_LIMIT is out of budget → None"
        );
    }

    // AD-PXY-22: a non-object top-level JSON value yields None, not a panic.
    #[test]
    fn test_detect_model_non_object_json() {
        assert_eq!(detect_model(b"[1,2,3]"), None, "JSON array → None");
        assert_eq!(
            detect_model(b"\"just a string\""),
            None,
            "JSON string → None"
        );
    }

    // AD-PXY-22: an explicit JSON null model yields None.
    #[test]
    fn test_detect_model_null_value() {
        assert_eq!(
            detect_model(br#"{"model": null}"#),
            None,
            "null model → None"
        );
    }

    // AD-PXY-22: a non-string model value must not abort extraction with a
    // fabricated value — it yields None.
    #[test]
    fn test_detect_model_non_string_value() {
        assert_eq!(
            detect_model(br#"{"model": 42}"#),
            None,
            "numeric model → None"
        );
    }

    // AD-PXY-22: OpenAI model string verbatim.
    #[test]
    fn test_detect_model_openai_verbatim() {
        let body = br#"{"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}"#;
        assert_eq!(
            detect_model(body),
            Some("gpt-4o".to_owned()),
            "OpenAI model string stored verbatim"
        );
    }
}
