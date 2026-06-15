//! Core model types for Anthropic and OpenAI request bodies.
//!
//! # Raw-span retention mechanism
//!
//! To guarantee byte-identical round-trips (Invariant 5), every JSON region that this
//! crate does NOT interpret is retained as a [`serde_json::value::RawValue`] — a
//! borrowed slice of the original JSON text, preserving:
//!
//! - Non-canonical number tokens (`1.0`, `1e3`, `-0.5e2`) — a `serde_json::Value`
//!   round-trip reformats these; `RawValue` does not.
//! - `\uXXXX` escapes vs literal Unicode characters — `Value` normalises escapes;
//!   `RawValue` preserves the original bytes.
//! - Key insertion order — the `preserve_order` workspace feature keeps keys in
//!   their original order inside `serde_json::Map`; `RawValue` does the same for
//!   opaque blobs.
//! - Duplicate keys — `RawValue` at the blob level retains them; the typed model
//!   fields are unique-keyed by construction (Anthropic/OpenAI specs do not allow
//!   duplicate keys), so any duplicate in an opaque blob is carried through verbatim.
//!
//! # serde_json feature rationale
//!
//! The workspace enables `preserve_order` + `raw_value` ONLY — **not**
//! `arbitrary_precision`. This crate's `RawValue` mechanism preserves number tokens
//! as raw source bytes without `arbitrary_precision`, and `arbitrary_precision` would
//! change `Value` number semantics across 700+ existing call sites. Resolved Decision 1
//! (DECISIONS-RESOLVED.md 2026-06-13) and #301 both align to this choice.
//!
//! # Duplicate-key policy
//!
//! The typed model fields are parsed from unique-keyed JSON objects (Anthropic and
//! OpenAI specs do not permit duplicate top-level or message keys). If a duplicate key
//! appears in a position this crate parses as a typed field, `serde_json` with
//! `preserve_order` retains the **last** value (standard JSON behaviour). If a duplicate
//! key appears inside an opaque `RawValue` blob (e.g., inside an unknown field's value),
//! it is carried through verbatim as raw bytes — the crate does not inspect it.
//!
//! # Byte-stability domain
//!
//! Serialization is **per-skim-version stable**: the same parsed model value serialized
//! by the same version of this crate always produces identical bytes, both within one
//! process and across separate process invocations on the same OS. Stability is NOT
//! guaranteed across skim versions (a future refactor may choose a different
//! representation). Stability IS guaranteed across Linux/macOS/Windows within one
//! version — no hash-map iteration order leaks, no number reformatting, no
//! escape normalization.
//!
//! # JSON depth bound
//!
//! [`crate::MAX_DEPTH`] (64) is the maximum nesting depth. Bodies exceeding this depth
//! return [`crate::LlmError::DepthExceeded`]. This bound prevents stack overflow from
//! adversarial deeply-nested input. The bound was chosen to be well above any real
//! Anthropic/OpenAI body (which typically nest 4–6 levels deep) while staying far from
//! stack limits on all supported platforms.

pub mod anthropic;
pub mod openai;

use serde_json::value::RawValue;

/// An opaque JSON blob retained as raw source bytes.
///
/// Used for fields and values this crate does not interpret, ensuring byte-identical
/// round-trips regardless of the field's JSON structure.
pub type RawBlob = Box<RawValue>;
