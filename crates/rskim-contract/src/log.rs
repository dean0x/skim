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
        // Suffix match: sensitive suffixes are all uppercase; compare with
        // an ASCII case-insensitive ends_with to avoid to_uppercase().
        || SENSITIVE_SUFFIXES
            .iter()
            .any(|&s| key.len() >= s.len() && key[key.len() - s.len()..].eq_ignore_ascii_case(s))
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Decision {
    /// Output bytes differ from input bytes.
    Modified,
    /// Output bytes equal input bytes (fail-open or no-op).
    Passthrough,
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecisionRecord {
    /// Caller-assigned request identifier. Never generated by the transform path.
    /// Private so all construction goes through the sanitizing constructors.
    request_id: String,
    /// Stable component name (e.g., `"metadata-reorder"`, `"identity"`).
    pub component: &'static str,
    /// Whether this record represents a modification or passthrough.
    pub decision: Decision,
    /// Input byte count.
    pub bytes_in: usize,
    /// Output byte count.
    pub bytes_out: usize,
}

impl DecisionRecord {
    /// Construct a passthrough record.
    ///
    /// `bytes_in == bytes_out` for passthrough; the invariant is recorded
    /// in both fields for consistency.
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
            bytes_in,
            bytes_out: bytes_in,
        }
    }

    /// Construct a modification record.
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
            bytes_in,
            bytes_out,
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
        Self {
            request_id: request_id.into(),
            component,
            decision,
            bytes_in,
            bytes_out,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
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

    #[test]
    fn decision_record_passthrough_fields() {
        let r = DecisionRecord::passthrough("req-1", "identity", 100);
        assert_eq!(r.request_id(), "req-1");
        assert_eq!(r.component, "identity");
        assert!(r.is_passthrough());
        assert_eq!(r.bytes_in, 100);
        assert_eq!(r.bytes_out, 100);
    }

    #[test]
    fn decision_record_modified_fields() {
        let r = DecisionRecord::modified("req-2", "compressor", 200, 150);
        assert_eq!(r.bytes_in, 200);
        assert_eq!(r.bytes_out, 150);
        assert!(!r.is_passthrough());
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
        assert_eq!(parsed["bytes_in"], 42);
        assert_eq!(parsed["bytes_out"], 42);
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
