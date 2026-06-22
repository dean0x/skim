//! Decision record type and non-blocking bounded-channel sink.
//!
//! # Decision record schema
//!
//! Every transform outcome — modification or passthrough — produces exactly one
//! [`DecisionRecord`]. Records are structured JSON:
//!
//! ```json
//! {
//!   "request_id": "req-abc-123",
//!   "component": "metadata-reorder",
//!   "decision": "modified",
//!   "bytes_in": 4096,
//!   "bytes_out": 4082
//! }
//! ```
//!
//! The `request_id` is always caller-assigned (invariant 5: no entropy in the
//! transform path — no UUID generation here).
//!
//! # Sink contract
//!
//! [`DecisionSink`] is a trait with a single non-blocking method. The concrete
//! [`ChannelDecisionSink`] wraps a bounded `crossbeam_channel::Sender`.
//!
//! When the channel is at capacity, `try_send` returns `Err(SinkFull)`. The
//! caller (in [`crate::guardrail`]) MUST then emit byte-faithful passthrough
//! rather than an unlogged modification — invariant 8 (logged-never-silent).
//!
//! The method is explicitly NOT `async` — this crate is entirely sync.
//!
//! # Sensitive field redaction
//!
//! Auth material must NEVER appear unredacted in decision records. The scrub
//! lists below are declared in this crate (not imported from the binary crate)
//! so they are available to all consumers without pulling in `rskim`.

use std::sync::Arc;
use thiserror::Error;

// ============================================================================
// Sensitive field scrub lists (declared here, not imported from rskim binary)
// ============================================================================

/// Exact key names that must be redacted in decision record values.
///
/// Matches the scrub list in `crates/rskim/src/cmd/file/env.rs::SENSITIVE_EXACT`.
/// Kept in sync manually; the canonical source for binary-facing redaction is the
/// env.rs list.
pub const SENSITIVE_EXACT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "DATABASE_URL",
    "NPM_TOKEN",
    "STRIPE_SECRET_KEY",
    "SENTRY_DSN",
    "SENDGRID_API_KEY",
];

/// Key suffixes that indicate sensitive values (case-insensitive).
///
/// Matches `crates/rskim/src/cmd/file/env.rs::SENSITIVE_SUFFIXES`.
pub const SENSITIVE_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_API_KEY",
    "_SECRET_KEY",
    "_PRIVATE_KEY",
    "_ENCRYPTION_KEY",
    "_SIGNING_KEY",
    "_ACCESS_KEY",
    "_HMAC_KEY",
    "_CREDENTIAL",
    "_AUTH",
];

/// Provider API key value prefixes that indicate a raw secret value.
///
/// These match the `SENSITIVE_VALUE_PREFIXES` defined in
/// `crates/rskim-contract/src/harness/mod.rs` for the test-time scan.
/// Extending the production redaction path to cover this axis closes the
/// defense-in-depth gap (AC12 / invariant 7): a `request_id` shaped like
/// a raw key VALUE (e.g., `sk-ant-api03-...`, `ghp_...`, `AKIA...`) is now
/// also redacted, mirroring the harness's value-axis scanner.
///
/// This is the "x-api-key echo proxy anti-pattern" guard: if a reverse-proxy
/// inadvertently echoes the bearer token back as the correlation id, this
/// constant ensures it is redacted before reaching a decision record.
pub const SENSITIVE_VALUE_PREFIXES: &[&str] = &[
    "sk-ant-",
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "AKIA",
];

/// Returns `true` if `key` matches a sensitive key NAME (identifier-axis) and
/// its value should be redacted before appearing in any log record.
///
/// Uses case-insensitive ASCII comparison to avoid heap allocation on every
/// call (avoids `to_uppercase()` — request_ids are ASCII correlators).
///
/// # Examples
///
/// ```rust
/// use rskim_contract::log::is_sensitive_key;
/// assert!(is_sensitive_key("ANTHROPIC_API_KEY"));
/// assert!(is_sensitive_key("MY_TOKEN"));
/// assert!(!is_sensitive_key("MODEL_NAME"));
/// ```
pub fn is_sensitive_key(key: &str) -> bool {
    // Exact match (case-insensitive, no allocation).
    SENSITIVE_EXACT
        .iter()
        .any(|&e| e.eq_ignore_ascii_case(key))
        // Suffix match: compare on bytes to avoid char-boundary panics when
        // `key` contains multibyte UTF-8 (e.g. `req-€x`).  All suffix
        // constants are pure ASCII, so `s.len()` is a byte length and
        // `eq_ignore_ascii_case` on byte slices is semantically identical to
        // the string version — but never panics on a non-char-boundary index.
        || SENSITIVE_SUFFIXES
            .iter()
            .any(|&s| {
                let kb = key.as_bytes();
                let sb = s.as_bytes();
                kb.len() >= sb.len()
                    && kb[kb.len() - sb.len()..].eq_ignore_ascii_case(sb)
            })
}

/// Returns `true` if `value` starts with a known provider API key VALUE prefix
/// (e.g., `sk-ant-`, `ghp_`, `AKIA`).
///
/// This is the value-axis companion to [`is_sensitive_key`] (which covers the
/// key-name axis). Together they close the AC12 defense-in-depth gap: a
/// `request_id` that IS a raw API key value is now also redacted.
///
/// Minimum length guard (8 bytes) prevents false positives on short strings.
pub fn is_sensitive_value(value: &str) -> bool {
    if value.len() < 8 {
        return false;
    }
    SENSITIVE_VALUE_PREFIXES
        .iter()
        .any(|&p| value.starts_with(p))
}

/// Sanitize a `request_id` for safe embedding in a decision record.
///
/// Returns the input unchanged if it matches neither a sensitive key pattern nor
/// a known API key value prefix. Returns `"<redacted>"` if either axis matches —
/// this fires in all build profiles (release AND debug), not just in assertions.
///
/// Covers two axes (AC12 / invariant 7 defense-in-depth):
/// 1. **Key-name axis**: [`is_sensitive_key`] — redacts identifier-shaped strings
///    like `ANTHROPIC_API_KEY` or `MY_TOKEN`.
/// 2. **Value axis**: [`is_sensitive_value`] — redacts raw API key values like
///    `sk-ant-api03-...`, `ghp_...`, `AKIA...` (the x-api-key echo proxy
///    anti-pattern).
///
/// # Examples
///
/// ```rust
/// use rskim_contract::log::sanitize_request_id;
/// assert_eq!(sanitize_request_id("req-abc-123"), "req-abc-123");
/// assert_eq!(sanitize_request_id("ANTHROPIC_API_KEY"), "<redacted>");
/// assert_eq!(sanitize_request_id("my_token"), "<redacted>");
/// assert_eq!(sanitize_request_id("sk-ant-api03-abc123"), "<redacted>");
/// assert_eq!(sanitize_request_id("ghp_abc123456"), "<redacted>");
/// ```
pub fn sanitize_request_id(request_id: &str) -> &str {
    if is_sensitive_key(request_id) || is_sensitive_value(request_id) {
        "<redacted>"
    } else {
        request_id
    }
}

// ============================================================================
// DecisionRecord
// ============================================================================

/// The decision variant: whether bytes were modified or passed through.
///
/// This is the **wire vocabulary** read by #305 (persistence). Only two variants
/// exist at the wire layer; the refining reason is carried separately in
/// [`OutcomeReason`] (added in #342, unblocking #304's full 5→3 reason mapping).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Decision {
    /// Output bytes differ from input bytes.
    Modified,
    /// Output bytes equal input bytes (fail-open or no-op).
    Passthrough,
}

/// Refining outcome reason for a block-level compression decision.
///
/// Added in **#342** as a schema coordination step between #301 (this crate,
/// schema owner) and #305 (persistence) per ADR-004. This field extends the
/// two-variant [`Decision`] wire vocabulary with a five-value refining reason
/// so that #304's `BlockRouter` can record the full 5→3 outcome mapping without
/// carrying a local wrapper that risks drifting from the #305 schema.
///
/// # Mapping to [`Decision`] (binding 5→3 rule, #304 §3)
///
/// | `OutcomeReason` | Wire [`Decision`] | #304 semantic |
/// |---|---|---|
/// | `Full` | `Modified` | Compressed, clean parse; no tree-sitter ERROR nodes added |
/// | `Degraded` | `Modified` | Compressed, but parse had syntax errors (degraded tier) |
/// | `Passthrough` | `Passthrough` | Skipped/forwarded: pre-filter, tie, misclassification |
/// | `FailedOpen` | `Passthrough` | Compressor returned `Err`; block stays original |
/// | `PolicyPassthrough` | `Passthrough` | Lossless-only policy active (`LosslessOnly` auth) |
///
/// # Persistence dependency (#305)
///
/// #305 is downstream and will add a `reason` column to the decision record
/// table once this schema lands. Do not add `OutcomeReason` variants without
/// coordinating with #305. The enum is `#[non_exhaustive]` to allow additive
/// extension without breaking downstream matchers.
///
/// # Usage
///
/// Set `reason` via the sanitizing constructors — never via struct-literal
/// construction (which is impossible since `request_id` is private).
///
/// ```rust
/// use rskim_contract::log::{DecisionRecord, OutcomeReason};
///
/// // Passthrough constructor always uses OutcomeReason::Passthrough.
/// let r = DecisionRecord::passthrough("req-1", "identity", 100);
/// assert_eq!(r.reason, OutcomeReason::Passthrough);
///
/// // Modified constructor defaults to OutcomeReason::Full (clean compression).
/// let r2 = DecisionRecord::modified("req-2", "block-router", 200, 150);
/// assert_eq!(r2.reason, OutcomeReason::Full);
///
/// // Use modified_with_reason for degraded/failed-open/policy-passthrough.
/// let r3 = DecisionRecord::modified_with_reason(
///     "req-3", "block-router", 200, 180, OutcomeReason::Degraded,
/// );
/// assert_eq!(r3.reason, OutcomeReason::Degraded);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutcomeReason {
    /// Compressed with a clean parse — no tree-sitter ERROR nodes were added.
    /// Maps to wire [`Decision::Modified`].
    Full,
    /// Compressed but the parse had syntax errors (degraded tier).
    /// Maps to wire [`Decision::Modified`].
    Degraded,
    /// Block skipped or forwarded: pre-filter hit, tie, or misclassification.
    /// Maps to wire [`Decision::Passthrough`].
    ///
    /// This is also the **schema-evolution default** for records written by a
    /// pre-#342 binary that lacks the `reason` key.  `#[serde(default)]` on
    /// [`DecisionRecord::reason`] causes serde to call `OutcomeReason::default()`
    /// when the key is absent, yielding `Passthrough` — the most conservative
    /// interpretation of an unknown-origin record.
    Passthrough,
    /// Compressor returned `Err`; block forwarded byte-identical (fail-open).
    /// Maps to wire [`Decision::Passthrough`].
    FailedOpen,
    /// Lossless-only policy active (e.g., subscription/OAuth `LosslessOnly` auth).
    /// Every block is forwarded byte-identical; no compressor runs.
    /// Maps to wire [`Decision::Passthrough`].
    PolicyPassthrough,
}

impl Default for OutcomeReason {
    /// Returns [`OutcomeReason::Passthrough`].
    ///
    /// Used by `#[serde(default)]` on [`DecisionRecord::reason`] to handle
    /// records written by a pre-#342 binary that lacks the `reason` key in
    /// their JSON.  `Passthrough` is chosen because it is the most conservative
    /// interpretation: a record with an unknown reason is treated as a
    /// passthrough rather than silently attributed to a modification tier.
    ///
    /// This pairs with the [`DecisionRecord::passthrough`] default and matches
    /// the wire [`Decision::Passthrough`] default, keeping the two vocabularies
    /// consistent for legacy records.
    fn default() -> Self {
        OutcomeReason::Passthrough
    }
}

/// A structured decision record produced by every L3 transform.
///
/// The record is JSON-serialisable. It is produced for every call to
/// [`crate::contract::Contract::transform`], whether the transform modifies
/// the input or passes it through unchanged.
///
/// # Invariant 8 (logged-never-silent)
///
/// An unlogged modification must not be emitted. If [`DecisionSink::try_send`]
/// returns `Err(SinkFull)`, the transform MUST fall back to passthrough.
/// See [`crate::guardrail::guarded_transform`].
///
/// # AC12 redaction boundary
///
/// `request_id` is private so that all construction goes through
/// [`DecisionRecord::passthrough`] or [`DecisionRecord::modified`], which call
/// [`sanitize_request_id`] unconditionally. Struct-literal construction that
/// bypasses sanitization cannot be written against the public API — enforcing
/// the 'parse at boundaries, trust internally' principle.
///
/// Downstream consumers (#305, #306, #307) that build records directly MUST use
/// the constructor methods; they cannot bypass redaction via a struct literal.
///
/// # Schema evolution (#342)
///
/// The `reason` and optional `tokens_in`/`tokens_out` fields were added in
/// #342 as a shared schema coordination step between this crate (#301, schema
/// owner) and #305 (persistence). They are serialized as part of the record
/// JSON and are consumed by #304's `BlockRouter` decision-logging path.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecisionRecord {
    /// Caller-assigned request identifier. Never generated by the transform path.
    /// Private so all construction goes through the sanitizing constructors.
    request_id: String,
    /// Stable component name (e.g., `"metadata-reorder"`, `"identity"`).
    pub component: &'static str,
    /// Whether this record represents a modification or passthrough.
    pub decision: Decision,
    /// Refining outcome reason (five-value vocabulary). Added in #342 to
    /// unblock #304's full 5→3 reason mapping. Defaults to
    /// [`OutcomeReason::Passthrough`] on passthrough and
    /// [`OutcomeReason::Full`] on modification; use
    /// [`DecisionRecord::modified_with_reason`] or
    /// [`DecisionRecord::passthrough_with_reason`] for other values.
    ///
    /// # Schema evolution / backward compatibility
    ///
    /// `#[serde(default)]` makes this field optional on the **deserialization**
    /// side: records written by a pre-#342 binary that lack the `reason` key
    /// deserialize successfully, with the field defaulting to
    /// [`OutcomeReason::Passthrough`] (see [`OutcomeReason::default`]).
    ///
    /// This mirrors the pattern used by the sibling optional fields
    /// `tokens_in`/`tokens_out`: those are `Option<usize>` with
    /// `skip_serializing_if = "Option::is_none"` for the *serialize* direction,
    /// while serde's built-in `Option` defaulting (absent key → `None`)
    /// handles the *deserialize* direction independently — no `#[serde(default)]`
    /// is required for them because `Option` already defaults to `None`.
    /// The `reason` field uses the same split: `skip_serializing_if` governs
    /// serialization; `#[serde(default)]` governs deserialization.
    /// Together they satisfy the 'parse at boundaries, trust internally'
    /// principle: a schema-additive field must not hard-fail on valid
    /// pre-field records at the persistence boundary (#305, per ADR-001 /
    /// ADR-004).
    #[serde(default)]
    pub reason: OutcomeReason,
    /// Input byte count.
    pub bytes_in: usize,
    /// Output byte count.
    pub bytes_out: usize,
    /// Input token count (accounting only — never used in the byte gate).
    ///
    /// `None` when the caller has no token counter or token counting is disabled.
    /// Populated by #304's `BlockRouter` via its injected `token_counter` closure.
    /// Must NOT influence the never-inflate accept/reject decision (AC10).
    ///
    /// Typed `usize` to match `rskim_tokens::Counter::count(&str) -> usize` and
    /// the `token_counter: Arc<dyn Fn(&str) -> usize + Send + Sync>` closure
    /// documented in the #304 plan (304-plan.md:108,118), keeping this field
    /// consistent with the producing API and the sibling `bytes_in`/`bytes_out`
    /// fields (also `usize`). Using `u64` here would require a lossy-looking
    /// `as u64` cast at every call site (ADR-001: fix type drift before it
    /// propagates to #304/#305 consumers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_in: Option<usize>,
    /// Output token count (accounting only — never used in the byte gate).
    ///
    /// `None` when the caller has no token counter or token counting is disabled.
    /// Populated by #304's `BlockRouter` after substitution.
    /// Must NOT influence the never-inflate accept/reject decision (AC10).
    ///
    /// Typed `usize` for the same reason as [`DecisionRecord::tokens_in`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_out: Option<usize>,
}

impl DecisionRecord {
    /// Construct a passthrough record.
    ///
    /// `bytes_in == bytes_out` for passthrough; the invariant is recorded
    /// in both fields for consistency.
    ///
    /// Sets `reason = OutcomeReason::Passthrough` and `tokens_in`/`tokens_out`
    /// to `None`. Use [`DecisionRecord::passthrough_with_reason`] when a more
    /// specific reason is needed (e.g., `FailedOpen`, `PolicyPassthrough`).
    ///
    /// # AC12 request_id sanitization (enforced in all build profiles)
    ///
    /// `request_id` is serialized verbatim into the record JSON. AC12 mandates
    /// that auth material MUST NEVER appear unredacted in any log record. Callers
    /// MUST use an opaque, non-secret correlation identifier (e.g., a UUID or a
    /// hashed request identifier), never a bearer token, API key, or value derived
    /// from a request header that could carry auth material.
    ///
    /// As a defense-in-depth **production** safeguard (not merely a debug
    /// assertion), this constructor redacts any `request_id` that matches a
    /// known sensitive key pattern — replacing it with the literal string
    /// `"<redacted>"`. This covers the realistic proxy anti-pattern where a
    /// caller derives the request_id from a request header containing an auth
    /// key (e.g., an x-api-key echo). The redaction fires in release builds
    /// unlike a `debug_assert!`.
    pub fn passthrough(request_id: &str, component: &'static str, bytes_in: usize) -> Self {
        Self {
            request_id: sanitize_request_id(request_id).to_owned(),
            component,
            decision: Decision::Passthrough,
            reason: OutcomeReason::Passthrough,
            bytes_in,
            bytes_out: bytes_in,
            tokens_in: None,
            tokens_out: None,
        }
    }

    /// Construct a passthrough record with a specific [`OutcomeReason`].
    ///
    /// Use this when a more specific reason than `Passthrough` is needed —
    /// for example, `FailedOpen` (compressor error) or `PolicyPassthrough`
    /// (lossless-only auth policy). The wire [`Decision`] is always
    /// `Passthrough`; only the refining reason differs.
    ///
    /// # AC12 request_id sanitization
    ///
    /// See [`DecisionRecord::passthrough`] for the full AC12 sanitization
    /// contract. The same production-safe redaction is applied here.
    ///
    /// # Panics
    ///
    /// `reason` MUST be a passthrough-family variant (`Passthrough`, `FailedOpen`,
    /// or `PolicyPassthrough`). Passing a modification-family variant (`Full`,
    /// `Degraded`) here is a caller bug. This is a **release-active** assertion
    /// (`assert!`, not `debug_assert!`) because these are public module-boundary
    /// constructors — a mismatched pair produces a silently-inconsistent record
    /// that breaks the binding 5→3 wire-vocabulary mapping (the PF-006
    /// silent-wrong-path class). Panicking on a caller bug is defensible at a
    /// boundary constructor; hot-path code elsewhere uses `debug_assert!`.
    pub fn passthrough_with_reason(
        request_id: &str,
        component: &'static str,
        bytes_in: usize,
        reason: OutcomeReason,
    ) -> Self {
        assert!(
            matches!(
                reason,
                OutcomeReason::Passthrough
                    | OutcomeReason::FailedOpen
                    | OutcomeReason::PolicyPassthrough
            ),
            "passthrough_with_reason called with a modification-family reason ({reason:?}); \
             use modified_with_reason instead"
        );
        Self {
            request_id: sanitize_request_id(request_id).to_owned(),
            component,
            decision: Decision::Passthrough,
            reason,
            bytes_in,
            bytes_out: bytes_in,
            tokens_in: None,
            tokens_out: None,
        }
    }

    /// Construct a modification record.
    ///
    /// Sets `reason = OutcomeReason::Full` (clean compression — no syntax
    /// errors introduced). Use [`DecisionRecord::modified_with_reason`] when
    /// a more specific reason is needed (e.g., `Degraded` for compressed-but-
    /// parse-error output).
    ///
    /// # AC12 request_id sanitization
    ///
    /// See [`DecisionRecord::passthrough`] for the full AC12 sanitization contract.
    /// The same production-safe redaction is applied here.
    pub fn modified(
        request_id: &str,
        component: &'static str,
        bytes_in: usize,
        bytes_out: usize,
    ) -> Self {
        Self {
            request_id: sanitize_request_id(request_id).to_owned(),
            component,
            decision: Decision::Modified,
            reason: OutcomeReason::Full,
            bytes_in,
            bytes_out,
            tokens_in: None,
            tokens_out: None,
        }
    }

    /// Construct a modification record with a specific [`OutcomeReason`].
    ///
    /// Use this when the default `Full` reason does not apply — for example,
    /// `Degraded` when compression succeeded but the parse had syntax errors.
    ///
    /// # AC12 request_id sanitization
    ///
    /// See [`DecisionRecord::passthrough`] for the full AC12 sanitization
    /// contract. The same production-safe redaction is applied here.
    ///
    /// # Panics
    ///
    /// `reason` MUST be a modification-family variant (`Full` or `Degraded`).
    /// Passing a passthrough-family variant here is a caller bug. This is a
    /// **release-active** assertion (`assert!`, not `debug_assert!`) — see
    /// [`DecisionRecord::passthrough_with_reason`] for the full rationale.
    pub fn modified_with_reason(
        request_id: &str,
        component: &'static str,
        bytes_in: usize,
        bytes_out: usize,
        reason: OutcomeReason,
    ) -> Self {
        assert!(
            matches!(reason, OutcomeReason::Full | OutcomeReason::Degraded),
            "modified_with_reason called with a passthrough-family reason ({reason:?}); \
             use passthrough_with_reason instead"
        );
        Self {
            request_id: sanitize_request_id(request_id).to_owned(),
            component,
            decision: Decision::Modified,
            reason,
            bytes_in,
            bytes_out,
            tokens_in: None,
            tokens_out: None,
        }
    }

    /// Attach token counts to an existing record, returning the updated record.
    ///
    /// Token counts are **accounting only** — they MUST NOT influence the
    /// never-inflate accept/reject decision (AC10 / #342). Call this after
    /// the byte gate has already accepted or rejected the candidate, and only
    /// when a token counter is available.
    ///
    /// Both arguments are `usize` to match `rskim_tokens::Counter::count(&str) -> usize`
    /// and the `token_counter: Arc<dyn Fn(&str) -> usize + Send + Sync>` closure
    /// documented in the #304 plan, avoiding a `as u64` cast at every call site.
    ///
    /// Returns `Self` (consumes and rebuilds) so callers can chain:
    /// ```rust
    /// use rskim_contract::log::DecisionRecord;
    /// let r = DecisionRecord::modified("req-1", "block-router", 1000, 600)
    ///     .with_tokens(120, 72);
    /// assert_eq!(r.tokens_in, Some(120));
    /// assert_eq!(r.tokens_out, Some(72));
    /// ```
    #[must_use]
    pub fn with_tokens(self, tokens_in: usize, tokens_out: usize) -> Self {
        Self {
            tokens_in: Some(tokens_in),
            tokens_out: Some(tokens_out),
            ..self
        }
    }

    /// Returns the sanitized request identifier embedded in this record.
    ///
    /// The value was sanitized via [`sanitize_request_id`] at construction time;
    /// sensitive key names and raw API key values are replaced with `"<redacted>"`.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns `true` if this is a passthrough record.
    pub fn is_passthrough(&self) -> bool {
        matches!(self.decision, Decision::Passthrough)
    }

    /// Construct a record with an UNSANITIZED `request_id`.
    ///
    /// # Safety
    ///
    /// This bypasses [`sanitize_request_id`] and allows any string — including
    /// sensitive key names — to appear as the `request_id`. It exists ONLY for
    /// the AC18 self-test broken impl ([`crate::harness::self_test::SacrosanctLeakingContract`])
    /// that must demonstrate the harness catches an unredacted sensitive id.
    ///
    /// **Production code MUST use [`DecisionRecord::passthrough`] or
    /// [`DecisionRecord::modified`] instead.**
    #[cfg(any(test, feature = "harness"))]
    pub fn with_unsanitized_request_id(
        request_id: impl Into<String>,
        component: &'static str,
        decision: Decision,
        bytes_in: usize,
        bytes_out: usize,
    ) -> Self {
        let reason = match decision {
            Decision::Modified => OutcomeReason::Full,
            Decision::Passthrough => OutcomeReason::Passthrough,
        };
        Self {
            request_id: request_id.into(),
            component,
            decision,
            reason,
            bytes_in,
            bytes_out,
            tokens_in: None,
            tokens_out: None,
        }
    }

    /// Serialise this record to a compact JSON string.
    ///
    /// Returns `None` if serialisation fails (should never happen in practice
    /// since all fields are primitive types).
    pub fn to_json(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }
}

// ============================================================================
// DecisionSink trait and SinkFull error
// ============================================================================

/// Error returned when the bounded decision channel is at capacity.
///
/// When `try_send` returns `SinkFull`, the caller MUST emit byte-faithful
/// passthrough (invariant 8 / AC14). A modification whose record was not
/// accepted MUST fail conformance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("decision sink is full — caller must fall back to passthrough")]
pub struct SinkFull;

/// Non-blocking bounded sink for decision records.
///
/// # Contract
///
/// - `try_send` MUST be non-blocking by type (no `async`, no `await`).
/// - On `Err(SinkFull)`, the caller MUST NOT emit a modification.
/// - Implementations MUST be `Send + Sync` (can be cloned and shared across threads).
///
/// # Dependency injection
///
/// The sink is a constructor parameter, not a global. Tests use [`MockSink`];
/// production code uses [`ChannelDecisionSink`]. The #305 sink-persistence
/// consumer provides the persistent database-backed implementation.
pub trait DecisionSink: Send + Sync {
    /// Attempt to send `record` to the sink without blocking.
    ///
    /// Returns `Ok(())` if the record was accepted, `Err(SinkFull)` if the
    /// channel is at capacity. The caller MUST fall back to passthrough on
    /// `Err(SinkFull)`.
    fn try_send(&self, record: DecisionRecord) -> std::result::Result<(), SinkFull>;
}

// ============================================================================
// ChannelDecisionSink — crossbeam-channel backed concrete implementation
// ============================================================================

/// Bounded crossbeam-channel backed [`DecisionSink`].
///
/// Wraps a `crossbeam_channel::Sender<DecisionRecord>` with bounded capacity.
/// `try_send` maps `TrySendError::Full` to `SinkFull` and `TrySendError::Disconnected`
/// to `SinkFull` (fail-open: treat a disconnected receiver as "full" so callers
/// fall back to passthrough, never block).
///
/// # Construction
///
/// ```rust
/// use rskim_contract::log::{ChannelDecisionSink, DecisionRecord};
///
/// let (sink, receiver) = ChannelDecisionSink::new(128);
/// // Hand `sink` to a Contract impl; consume from `receiver` on a drain thread.
/// ```
///
/// # Thread safety
///
/// `ChannelDecisionSink` is `Clone`, `Send`, and `Sync`. Multiple components can
/// share the same sink by cloning it.
#[derive(Clone)]
pub struct ChannelDecisionSink {
    sender: crossbeam_channel::Sender<DecisionRecord>,
}

impl ChannelDecisionSink {
    /// Create a new sink with the given channel capacity.
    ///
    /// Returns `(sink, receiver)`. Pass `sink` to transform components;
    /// drain `receiver` on a background thread to avoid blocking the transform
    /// path when the channel approaches capacity.
    ///
    /// # Panics
    ///
    /// Never panics. `crossbeam_channel::bounded` accepts any capacity ≥ 0.
    pub fn new(capacity: usize) -> (Self, crossbeam_channel::Receiver<DecisionRecord>) {
        let (sender, receiver) = crossbeam_channel::bounded(capacity);
        (Self { sender }, receiver)
    }
}

impl DecisionSink for ChannelDecisionSink {
    fn try_send(&self, record: DecisionRecord) -> std::result::Result<(), SinkFull> {
        // Both Full and Disconnected map to SinkFull: callers fall back to passthrough.
        self.sender.try_send(record).map_err(|_| SinkFull)
    }
}

// ============================================================================
// MockSink — in-memory test double
// ============================================================================

/// In-memory decision sink for tests.
///
/// Captures all records sent to it so tests can assert the exact record
/// content without a real channel. Thread-safe via `std::sync::Mutex`.
///
/// ```rust
/// use rskim_contract::log::{MockSink, DecisionSink, DecisionRecord};
/// use std::sync::Arc;
///
/// let sink = Arc::new(MockSink::new());
/// let record = DecisionRecord::passthrough("req-1", "test", 42);
/// sink.try_send(record).unwrap();
/// let records = sink.drain();
/// assert_eq!(records.len(), 1);
/// assert_eq!(records[0].request_id(), "req-1");
/// ```
pub struct MockSink {
    records: std::sync::Mutex<Vec<DecisionRecord>>,
    /// When `true`, `try_send` always returns `Err(SinkFull)`.
    full: std::sync::atomic::AtomicBool,
}

impl MockSink {
    /// Create a new empty mock sink.
    pub fn new() -> Self {
        Self {
            records: std::sync::Mutex::new(Vec::new()),
            full: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Configure the sink to simulate a full channel for the next calls.
    pub fn set_full(&self, full: bool) {
        self.full.store(full, std::sync::atomic::Ordering::Relaxed);
    }

    /// Drain and return all captured records, leaving the sink empty.
    ///
    /// Never panics: the internal mutex is poison-tolerant — if a thread panicked
    /// while holding the lock, `lock()` recovers the guard via `into_inner()`
    /// rather than panicking.
    pub fn drain(&self) -> Vec<DecisionRecord> {
        std::mem::take(&mut *self.lock())
    }

    /// Return the number of records currently held.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Returns `true` if no records have been captured.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<DecisionRecord>> {
        self.records.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for MockSink {
    fn default() -> Self {
        Self::new()
    }
}

impl DecisionSink for MockSink {
    fn try_send(&self, record: DecisionRecord) -> std::result::Result<(), SinkFull> {
        if self.full.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(SinkFull);
        }
        self.lock().push(record);
        Ok(())
    }
}

// Allow Arc<MockSink> to be used as a DecisionSink.
impl<T: DecisionSink> DecisionSink for Arc<T> {
    fn try_send(&self, record: DecisionRecord) -> std::result::Result<(), SinkFull> {
        (**self).try_send(record)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn is_sensitive_key_exact_match() {
        assert!(is_sensitive_key("ANTHROPIC_API_KEY"));
        assert!(is_sensitive_key("OPENAI_API_KEY"));
        assert!(is_sensitive_key("GITHUB_TOKEN"));
    }

    #[test]
    fn is_sensitive_key_suffix_match() {
        assert!(is_sensitive_key("MY_SERVICE_TOKEN"));
        assert!(is_sensitive_key("DB_PASSWORD"));
        assert!(is_sensitive_key("SERVICE_API_KEY"));
    }

    #[test]
    fn is_sensitive_key_non_sensitive() {
        assert!(!is_sensitive_key("MODEL_NAME"));
        assert!(!is_sensitive_key("REQUEST_ID"));
        assert!(!is_sensitive_key("COMPONENT"));
        assert!(!is_sensitive_key("SORT_KEY")); // _KEY alone is not sensitive
    }

    #[test]
    fn is_sensitive_key_case_insensitive() {
        assert!(is_sensitive_key("anthropic_api_key"));
        assert!(is_sensitive_key("my_service_token"));
    }

    /// Regression test for the char-boundary panic in `is_sensitive_key`.
    ///
    /// Before the fix, `key[key.len() - s.len()..]` was used for suffix matching.
    /// When `key` ends in a multibyte UTF-8 code point and `s.len()` (a byte
    /// count) lands inside that code point, Rust string slicing panics with
    /// 'byte index N is not a char boundary'.  The byte-slice fix avoids this
    /// entirely because `as_bytes()` operates on raw bytes.
    ///
    /// This test is the discriminating guard: reverting to string slicing causes
    /// a panic here (not a false-positive failure).
    #[test]
    fn is_sensitive_key_multibyte_utf8_no_panic() {
        // "req-€x" ends in 'x' preceded by the 3-byte UTF-8 sequence for '€'.
        // `key.len()` = 7; a suffix like "_KEY" (4 bytes) produces byte index 3,
        // which lands inside the '€' sequence — a non-char-boundary panic before the fix.
        assert!(!is_sensitive_key("req-€x"));
        // Longer multibyte suffix — '€' is 3 bytes; offset 4 from end is boundary inside '€'.
        assert!(!is_sensitive_key("req-€"));
        // A key that genuinely matches a suffix but also contains multibyte chars must still work.
        assert!(is_sensitive_key("préfix_TOKEN"));
    }

    #[test]
    fn decision_record_passthrough_fields() {
        let r = DecisionRecord::passthrough("req-1", "identity", 100);
        assert_eq!(r.request_id(), "req-1");
        assert_eq!(r.component, "identity");
        assert!(r.is_passthrough());
        assert_eq!(r.bytes_in, 100);
        assert_eq!(r.bytes_out, 100);
        // #342 additive guarantee: reason defaults to Passthrough without changing call sites.
        assert_eq!(r.reason, OutcomeReason::Passthrough);
        assert_eq!(r.tokens_in, None);
        assert_eq!(r.tokens_out, None);
    }

    #[test]
    fn decision_record_modified_fields() {
        let r = DecisionRecord::modified("req-2", "compressor", 200, 150);
        assert_eq!(r.bytes_in, 200);
        assert_eq!(r.bytes_out, 150);
        assert!(!r.is_passthrough());
        // #342 additive guarantee: reason defaults to Full without changing call sites.
        assert_eq!(r.reason, OutcomeReason::Full);
    }

    #[test]
    fn decision_record_to_json_round_trips() {
        let r = DecisionRecord::passthrough("req-3", "test", 42);
        let json = r.to_json().expect("serialisation must succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("must produce valid JSON");
        assert_eq!(parsed["request_id"], "req-3");
        assert_eq!(parsed["component"], "test");
        assert_eq!(parsed["decision"], "passthrough");
        assert_eq!(parsed["reason"], "passthrough");
        assert_eq!(parsed["bytes_in"], 42);
        assert_eq!(parsed["bytes_out"], 42);
        // tokens_in/tokens_out are absent when None (skip_serializing_if).
        assert!(
            parsed.get("tokens_in").is_none(),
            "tokens_in must be absent when None"
        );
        assert!(
            parsed.get("tokens_out").is_none(),
            "tokens_out must be absent when None"
        );
    }

    // ========================================================================
    // OutcomeReason and new constructors (#342)
    // ========================================================================

    /// Compile-time guard: all 5 OutcomeReason variants must exist.
    /// Deleting a variant causes this array construction to fail to compile.
    #[test]
    fn outcome_reason_variants_are_distinct() {
        let _reasons = [
            OutcomeReason::Full,
            OutcomeReason::Degraded,
            OutcomeReason::Passthrough,
            OutcomeReason::FailedOpen,
            OutcomeReason::PolicyPassthrough,
        ];
    }

    /// passthrough_with_reason sets a specific passthrough-family reason.
    #[test]
    fn passthrough_with_reason_failed_open() {
        let r = DecisionRecord::passthrough_with_reason(
            "req-fo",
            "block-router",
            500,
            OutcomeReason::FailedOpen,
        );
        assert!(r.is_passthrough());
        // discriminating: must be FailedOpen, not generic Passthrough
        assert_eq!(r.reason, OutcomeReason::FailedOpen);
        assert_ne!(r.reason, OutcomeReason::Passthrough);
        assert_eq!(r.bytes_in, 500);
        assert_eq!(r.bytes_out, 500);
    }

    /// passthrough_with_reason works for PolicyPassthrough.
    #[test]
    fn passthrough_with_reason_policy_passthrough() {
        let r = DecisionRecord::passthrough_with_reason(
            "req-pp",
            "block-router",
            300,
            OutcomeReason::PolicyPassthrough,
        );
        assert!(r.is_passthrough());
        assert_eq!(r.reason, OutcomeReason::PolicyPassthrough);
        // wire Decision is still Passthrough (2-variant wire vocab preserved).
        assert_eq!(r.decision, Decision::Passthrough);
    }

    /// modified_with_reason sets Degraded for a parse-error compression.
    #[test]
    fn modified_with_reason_degraded() {
        let r = DecisionRecord::modified_with_reason(
            "req-deg",
            "block-router",
            1000,
            700,
            OutcomeReason::Degraded,
        );
        assert!(!r.is_passthrough());
        // discriminating: must be Degraded, not Full
        assert_eq!(r.reason, OutcomeReason::Degraded);
        assert_ne!(r.reason, OutcomeReason::Full);
        assert_eq!(r.decision, Decision::Modified);
        assert_eq!(r.bytes_in, 1000);
        assert_eq!(r.bytes_out, 700);
    }

    /// with_tokens attaches token counts without affecting the byte gate fields.
    #[test]
    fn with_tokens_is_accounting_only() {
        let r = DecisionRecord::modified("req-tok", "block-router", 1000, 600).with_tokens(120, 72);
        // Token counts are present.
        assert_eq!(r.tokens_in, Some(120));
        assert_eq!(r.tokens_out, Some(72));
        // Byte fields are unchanged by token attachment.
        assert_eq!(r.bytes_in, 1000);
        assert_eq!(r.bytes_out, 600);
        // reason is unchanged.
        assert_eq!(r.reason, OutcomeReason::Full);
    }

    /// Tokens are serialized into JSON when present.
    /// Absence when None is already covered by decision_record_to_json_round_trips.
    #[test]
    fn token_fields_json_round_trip() {
        let r = DecisionRecord::modified("req-tj", "block-router", 800, 400).with_tokens(100, 50);
        let json = r.to_json().expect("serialisation must succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("must produce valid JSON");
        assert_eq!(parsed["tokens_in"], 100, "tokens_in must appear in JSON");
        assert_eq!(parsed["tokens_out"], 50, "tokens_out must appear in JSON");
    }

    /// OutcomeReason serializes with snake_case names (per serde rename_all).
    #[test]
    fn outcome_reason_serde_snake_case() {
        let cases = [
            (OutcomeReason::Full, "full"),
            (OutcomeReason::Degraded, "degraded"),
            (OutcomeReason::Passthrough, "passthrough"),
            (OutcomeReason::FailedOpen, "failed_open"),
            (OutcomeReason::PolicyPassthrough, "policy_passthrough"),
        ];
        for (reason, expected_str) in cases {
            let serialized = serde_json::to_string(&reason).expect("serialization must succeed");
            assert_eq!(
                serialized,
                format!("\"{expected_str}\""),
                "OutcomeReason::{reason:?} must serialize as \"{expected_str}\""
            );
        }
    }

    // ========================================================================
    // Typed round-trip deserialization tests (PF-007 load-bearing direction)
    // ========================================================================

    /// Legacy-JSON backward-compatibility (schema-evolution default, #342).
    ///
    /// A record written by a pre-#342 binary omits the `reason` key entirely.
    /// `#[serde(default)]` on `DecisionRecord::reason` must cause it to
    /// deserialize as `OutcomeReason::Passthrough` rather than failing.
    /// Also verifies that absent `tokens_in`/`tokens_out` produce `None`.
    ///
    /// This test is the discriminating guard for the `#[serde(default)]` fix:
    /// deleting that attribute causes this test to fail with
    /// `missing field "reason"`.
    ///
    /// Note: `DecisionRecord::component` is typed `&'static str`, so the
    /// derived `Deserialize` impl only satisfies `Deserialize<'static>`.
    /// We use a `&'static str` JSON literal (via `include_str!`-compatible
    /// `static`) to satisfy the lifetime constraint.
    #[test]
    fn legacy_record_deserializes_without_reason_or_tokens() {
        // 'static string literal satisfies the Deserialize<'static> bound
        // imposed by `component: &'static str` in DecisionRecord.
        static LEGACY_JSON: &str = r#"{"request_id":"req-1","component":"identity","decision":"passthrough","bytes_in":100,"bytes_out":100}"#;
        let record: DecisionRecord =
            serde_json::from_str(LEGACY_JSON).expect("pre-#342 record must deserialize");
        assert_eq!(
            record.reason,
            OutcomeReason::Passthrough,
            "absent reason key must default to Passthrough"
        );
        assert_eq!(record.tokens_in, None, "absent tokens_in must be None");
        assert_eq!(record.tokens_out, None, "absent tokens_out must be None");
        assert_eq!(record.request_id(), "req-1");
    }

    /// Typed round-trip: passthrough record serializes then deserializes back to
    /// the exact same typed fields. This is the load-bearing direction for #305
    /// persistence (which reads records back), per PF-007.
    ///
    /// Uses a `Box::leak`-promoted `String` to obtain a `&'static str` for
    /// `serde_json::from_str`, which requires `'static` due to the
    /// `component: &'static str` field in `DecisionRecord`.
    #[test]
    fn passthrough_record_typed_round_trip() {
        let r = DecisionRecord::passthrough("req-rt-pt", "identity", 200);
        let json: &'static str =
            Box::leak(r.to_json().expect("serialisation must succeed").into_boxed_str());
        let back: DecisionRecord =
            serde_json::from_str(json).expect("typed deserialization must succeed");
        assert_eq!(back.reason, OutcomeReason::Passthrough);
        assert_eq!(back.tokens_in, None);
        assert_eq!(back.tokens_out, None);
        assert_eq!(back.bytes_in, 200);
        assert_eq!(back.bytes_out, 200);
    }

    /// Typed round-trip: `modified_with_reason(Degraded)` record.
    /// Validates that the `Degraded` reason survives serde round-trip.
    #[test]
    fn modified_with_reason_degraded_typed_round_trip() {
        let r = DecisionRecord::modified_with_reason(
            "req-rt-deg",
            "block-router",
            1000,
            700,
            OutcomeReason::Degraded,
        );
        let json: &'static str =
            Box::leak(r.to_json().expect("serialisation must succeed").into_boxed_str());
        let back: DecisionRecord =
            serde_json::from_str(json).expect("typed deserialization must succeed");
        assert_eq!(back.reason, OutcomeReason::Degraded);
        assert_eq!(back.tokens_in, None);
        assert_eq!(back.tokens_out, None);
        assert_eq!(back.bytes_in, 1000);
        assert_eq!(back.bytes_out, 700);
    }

    /// Typed round-trip: `with_tokens(Some path)` record.
    /// Validates that `tokens_in`/`tokens_out` survive serde round-trip typed as
    /// `Option<usize>` (discriminating: a ser/de type mismatch would surface
    /// as an unexpected value in the typed field after round-trip).
    #[test]
    fn with_tokens_typed_round_trip() {
        let r = DecisionRecord::modified("req-rt-tok", "block-router", 800, 400)
            .with_tokens(120, 72);
        let json: &'static str =
            Box::leak(r.to_json().expect("serialisation must succeed").into_boxed_str());
        let back: DecisionRecord =
            serde_json::from_str(json).expect("typed deserialization must succeed");
        assert_eq!(back.reason, OutcomeReason::Full);
        assert_eq!(back.tokens_in, Some(120usize));
        assert_eq!(back.tokens_out, Some(72usize));
        assert_eq!(back.bytes_in, 800);
        assert_eq!(back.bytes_out, 400);
    }

    /// OutcomeReason typed deserialization: all 5 variants must round-trip.
    /// A snake_case mismatch (e.g. `"failed_open"` → missing variant) would
    /// fail this test.  `OutcomeReason` is a pure enum with no `&'static str`
    /// fields, so `from_str` works with any lifetime.
    #[test]
    fn outcome_reason_typed_deserialize_all_variants() {
        let cases = [
            ("\"full\"", OutcomeReason::Full),
            ("\"degraded\"", OutcomeReason::Degraded),
            ("\"passthrough\"", OutcomeReason::Passthrough),
            ("\"failed_open\"", OutcomeReason::FailedOpen),
            ("\"policy_passthrough\"", OutcomeReason::PolicyPassthrough),
        ];
        for (json_str, expected) in cases {
            let got: OutcomeReason = serde_json::from_str(json_str)
                .expect("OutcomeReason variant must deserialize from snake_case JSON string");
            assert_eq!(got, expected, "OutcomeReason from {json_str}");
        }
    }

    // ========================================================================
    // Family-consistency invariant tests (release-active assert! guards)
    // ========================================================================

    /// Family guard (discriminating): passing a modification-family reason to
    /// `passthrough_with_reason` MUST trip the `assert!`. This fires in BOTH
    /// debug and release builds (unlike the former `debug_assert!`). Deleting
    /// the guard would let a caller record `Decision::Passthrough` with reason
    /// `Full`, silently violating the 5→3 mapping (the PF-006 silent-wrong-path
    /// class).
    #[test]
    #[should_panic(expected = "passthrough_with_reason called with a modification-family reason")]
    fn passthrough_with_reason_rejects_modification_family() {
        let _ = DecisionRecord::passthrough_with_reason(
            "req-bad",
            "block-router",
            100,
            OutcomeReason::Full,
        );
    }

    /// Family guard (discriminating): passing a passthrough-family reason to
    /// `modified_with_reason` MUST trip the `assert!` (fires in debug AND
    /// release). Deleting the guard would let a caller record
    /// `Decision::Modified` with reason `FailedOpen`, silently violating the
    /// 5→3 mapping (the PF-006 silent-wrong-path class).
    #[test]
    #[should_panic(expected = "modified_with_reason called with a passthrough-family reason")]
    fn modified_with_reason_rejects_passthrough_family() {
        let _ = DecisionRecord::modified_with_reason(
            "req-bad",
            "block-router",
            100,
            80,
            OutcomeReason::FailedOpen,
        );
    }

    #[test]
    fn channel_sink_accepts_records() {
        let (sink, rx) = ChannelDecisionSink::new(16);
        let r = DecisionRecord::passthrough("r1", "c", 10);
        sink.try_send(r).expect("channel has capacity");
        let received = rx.try_recv().expect("record must be present");
        assert_eq!(received.request_id(), "r1");
    }

    #[test]
    fn channel_sink_full_returns_sink_full() {
        let (sink, _rx) = ChannelDecisionSink::new(1);
        let r1 = DecisionRecord::passthrough("r1", "c", 1);
        sink.try_send(r1).expect("first send must succeed");
        let r2 = DecisionRecord::passthrough("r2", "c", 2);
        let err = sink.try_send(r2).expect_err("channel must be full");
        assert_eq!(err, SinkFull);
    }

    #[test]
    fn channel_sink_disconnected_returns_sink_full() {
        // Drop the receiver so the sender is disconnected.
        let (sender, _receiver) = crossbeam_channel::bounded::<DecisionRecord>(1);
        drop(_receiver);
        let sink = ChannelDecisionSink { sender };
        let r = DecisionRecord::passthrough("r1", "c", 10);
        let err = sink
            .try_send(r)
            .expect_err("disconnected must look like Full");
        assert_eq!(err, SinkFull);
    }

    #[test]
    fn channel_sink_is_clone_send_sync() {
        fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
        assert_send_sync_clone::<ChannelDecisionSink>();
    }

    #[test]
    fn mock_sink_captures_records() {
        let sink = MockSink::new();
        sink.try_send(DecisionRecord::passthrough("r1", "c", 1))
            .unwrap();
        sink.try_send(DecisionRecord::modified("r2", "c", 10, 8))
            .unwrap();
        let records = sink.drain();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].request_id(), "r1");
        assert_eq!(records[1].request_id(), "r2");
        assert!(sink.is_empty());
    }

    #[test]
    fn mock_sink_full_simulation() {
        let sink = MockSink::new();
        sink.set_full(true);
        let err = sink
            .try_send(DecisionRecord::passthrough("r1", "c", 1))
            .expect_err("must return SinkFull when set_full=true");
        assert_eq!(err, SinkFull);
        sink.set_full(false);
        sink.try_send(DecisionRecord::passthrough("r2", "c", 1))
            .expect("must accept after set_full=false");
        assert_eq!(sink.len(), 1);
    }

    #[test]
    fn arc_mock_sink_impl() {
        let sink = Arc::new(MockSink::new());
        sink.try_send(DecisionRecord::passthrough("r1", "c", 5))
            .unwrap();
        assert_eq!(sink.len(), 1);
    }

    // ========================================================================
    // sanitize_request_id tests (AC12 production enforcement)
    // ========================================================================

    #[test]
    fn sanitize_request_id_safe_values_unchanged() {
        assert_eq!(sanitize_request_id("req-abc-123"), "req-abc-123");
        assert_eq!(sanitize_request_id("uuid-1234-5678"), "uuid-1234-5678");
        assert_eq!(sanitize_request_id(""), "");
    }

    #[test]
    fn sanitize_request_id_sensitive_exact_match_redacted() {
        assert_eq!(sanitize_request_id("ANTHROPIC_API_KEY"), "<redacted>");
        assert_eq!(sanitize_request_id("OPENAI_API_KEY"), "<redacted>");
        assert_eq!(sanitize_request_id("GITHUB_TOKEN"), "<redacted>");
    }

    #[test]
    fn sanitize_request_id_sensitive_suffix_redacted() {
        // A value that looks like an env var key name with a sensitive suffix.
        assert_eq!(sanitize_request_id("MY_TOKEN"), "<redacted>");
        assert_eq!(sanitize_request_id("SERVICE_API_KEY"), "<redacted>");
        assert_eq!(sanitize_request_id("DB_PASSWORD"), "<redacted>");
    }

    #[test]
    fn sanitize_request_id_value_prefix_redacted() {
        // AC12 value-axis: raw API key values must also be redacted (defense-in-depth).
        // This covers the x-api-key echo proxy anti-pattern.
        assert_eq!(
            sanitize_request_id("sk-ant-api03-abc1234567890abcdef"),
            "<redacted>",
            "Anthropic key value prefix must be redacted"
        );
        assert_eq!(
            sanitize_request_id("ghp_abc123456789abcdef"),
            "<redacted>",
            "GitHub token value prefix must be redacted"
        );
        assert_eq!(
            sanitize_request_id("AKIAIOSFODNN7EXAMPLE"),
            "<redacted>",
            "AWS access key value prefix must be redacted"
        );
    }

    #[test]
    fn is_sensitive_value_short_strings_not_redacted() {
        // Short strings (< 8 bytes) must not false-positive.
        assert!(!is_sensitive_value("sk-"));
        assert!(!is_sensitive_value("ghp_"));
        assert!(!is_sensitive_value("short"));
    }

    #[test]
    fn decision_record_passthrough_sanitizes_sensitive_request_id() {
        // Simulate a caller accidentally passing a sensitive key name as request_id.
        // The constructor must sanitize it to "<redacted>" in all build profiles.
        let r = DecisionRecord::passthrough("ANTHROPIC_API_KEY", "identity", 100);
        assert_eq!(
            r.request_id(),
            "<redacted>",
            "sensitive key name must be redacted to '<redacted>' in production"
        );
        // Verify it round-trips to JSON without the sensitive key name.
        let json = r.to_json().expect("serialisation must succeed");
        assert!(
            !json.contains("ANTHROPIC_API_KEY"),
            "sensitive key must not appear in record JSON"
        );
        assert!(
            json.contains("<redacted>"),
            "redaction marker must appear in record JSON"
        );
    }

    #[test]
    fn decision_record_passthrough_sanitizes_api_key_value() {
        // AC12 value-axis: raw API key value as request_id must be redacted.
        let r = DecisionRecord::passthrough(
            "sk-ant-api03-FAKEKEYFORTESTING1234567890",
            "identity",
            100,
        );
        assert_eq!(
            r.request_id(),
            "<redacted>",
            "raw API key value must be redacted to '<redacted>'"
        );
        let json = r.to_json().expect("serialisation must succeed");
        assert!(
            !json.contains("sk-ant-api03"),
            "raw API key value must not appear in record JSON"
        );
    }

    #[test]
    fn decision_record_modified_sanitizes_sensitive_request_id() {
        let r = DecisionRecord::modified("MY_TOKEN", "transform", 200, 150);
        assert_eq!(r.request_id(), "<redacted>");
        let json = r.to_json().expect("serialisation must succeed");
        assert!(!json.contains("MY_TOKEN"));
        assert!(json.contains("<redacted>"));
    }

    #[test]
    fn decision_record_normal_request_id_preserved() {
        // Non-sensitive request IDs pass through unchanged.
        let r = DecisionRecord::passthrough("req-00001", "identity", 42);
        assert_eq!(r.request_id(), "req-00001");
    }
}
