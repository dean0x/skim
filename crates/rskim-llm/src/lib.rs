//! LLM message model: parse/serialize Anthropic & OpenAI request bodies with
//! content-block classification.
//!
//! # Overview
//!
//! `rskim-llm` provides a typed model for Anthropic `/v1/messages` and OpenAI
//! `/v1/chat/completions` request bodies, with:
//!
//! - **Byte-identical round-trips** — `serialize(parse(bytes)) == bytes` for all
//!   valid inputs (Invariant 5). Unknown fields, non-canonical number tokens, and
//!   `\uXXXX` escape sequences are all preserved verbatim.
//! - **Provider auto-detection** — structural heuristics identify Anthropic vs OpenAI
//!   from the request body alone.
//! - **Content-block classification** — six classes: `code`, `json`, `log`, `text`,
//!   `mixed`, `unknown`. Deterministic: identical input always produces identical output.
//! - **Chunked ingestion** — accepts streaming byte chunks, parses at end-of-input.
//! - **Text-for-text mutation** — replace one leaf text payload, leaving all other
//!   bytes identical.
//! - **No I/O, no async, no ambient state** — pure transform library.
//!
//! # Raw-span retention mechanism
//!
//! The byte-identity guarantee is implemented via `serde_json` `RawValue` +
//! `preserve_order`. Every JSON region this crate does not interpret is retained as a
//! [`serde_json::value::RawValue`] — a raw byte slice of the original JSON text.
//! The `preserve_order` workspace feature keeps typed `Map` keys in their original
//! order. Together, these preserve numbers, escapes, key order, and duplicate keys
//! byte-faithfully without the `arbitrary_precision` feature (which would change
//! `Value` number semantics across the workspace). See [`model`] for full rationale.
//!
//! # serde_json feature rationale
//!
//! The workspace enables `preserve_order` + `raw_value` ONLY — **not**
//! `arbitrary_precision`. This crate's `RawValue` mechanism preserves number tokens
//! as raw source bytes without `arbitrary_precision`. Resolved Decision 1
//! (DECISIONS-RESOLVED.md, 2026-06-13) documents this choice. See also the
//! workspace `Cargo.toml` comment for the coordinated rationale.
//!
//! # Duplicate-key policy
//!
//! Typed model fields are unique-keyed (Anthropic/OpenAI specs do not allow duplicate
//! top-level or message keys). Duplicate keys in opaque `RawValue` blobs are
//! carried through verbatim. If a duplicate key appears in a position parsed as a
//! typed field, `serde_json` with `preserve_order` retains the **last** value.
//! See [`model`] for details.
//!
//! # Byte-stability domain
//!
//! Serialization is **per-skim-version stable**: identical bytes within one version,
//! across runs and OSes. Not guaranteed across versions.
//!
//! # JSON depth bound
//!
//! [`MAX_DEPTH`] = 64. Bodies exceeding this depth return [`LlmError::DepthExceeded`].
//! This bound is well above any real Anthropic/OpenAI body (4–6 levels typical) while
//! preventing stack overflow from adversarial input. Checked via PF-004-safe saturating
//! arithmetic before the comparison.
//!
//! # Memory constant k
//!
//! Peak allocation during parse is bounded by approximately `k = 3.5 × body_size` for
//! typical tool-result-heavy bodies. This accounts for: the input buffer (1×), the
//! parsed `serde_json::Value` intermediate (≤1.5×), and the typed model (≤1×). The
//! `RawValue` mechanism avoids re-encoding number tokens so there is no size inflation.
//! The `k = 3.5×` figure is an analytical estimate; wiring it up as an enforced
//! counting-allocator regression gate (AC14) is a tracked follow-up (Wave-1 perf gate,
//! #309) — there is no isolated counting-allocator test binary in this crate yet.
//!
//! # No I/O, no ambient state
//!
//! This crate does not link any async runtime, HTTP, filesystem, or RNG dependency,
//! and does not read environment variables, the clock, or random sources. AC12.
//!
//! # Wave-1 related tickets
//!
//! - #300 `rskim-tokens` — token counting library
//! - #301 `rskim-contract` — LLM contract validation; consumes this crate's byte pipeline
//! - #304/#307 — consume this crate's classifier for routing decisions
//! - #306 — `cache_control` injection lives in a separate layer above this crate
//!   (Resolved Decision 7: no-envelope-mutation invariant is absolute here)
//! - #309 — Wave-1 tracking; crate-naming convention (`rskim-*`, no `l3-` infix)
//! - #323 — cross-OS CI matrix (workspace-wide Windows+macOS jobs)
//! - #326 — follow-up: deterministic unfenced-code inference beyond fence-tag v1
//! - #327 — follow-up: shared log-rule library extraction
//! - #328 — follow-up: conformance-harness registration with #301's harness

#![deny(missing_docs)]

pub mod classify;
pub mod error;
pub mod ingest;
pub mod model;
pub mod mutate;
pub mod parse;
pub mod provider;
pub mod serialize;
pub(crate) mod splice;

pub use classify::{Class, Classification, classify};
pub use error::LlmError;
pub use ingest::ChunkIngestionBuilder;
pub use model::anthropic::AnthropicBody;
pub use model::openai::OpenAiBody;
pub use mutate::{BlockDescriptor, list_blocks, mutate_block};
pub use parse::{ParsedBody, parse, parse_with_provider};
pub use provider::Provider;
pub use serialize::{serialize, serialize_to_string};

/// Crate-level `Result` alias.
pub type Result<T> = std::result::Result<T, LlmError>;

/// Maximum JSON nesting depth accepted by this crate.
///
/// Bodies exceeding this depth return [`LlmError::DepthExceeded`]. The bound is
/// checked before structural parsing using PF-004-safe saturating arithmetic (u32),
/// preventing overflow on adversarial input.
///
/// Justification: real Anthropic/OpenAI bodies nest 4–6 levels deep. This bound
/// (64) provides a generous safety margin while staying far from stack limits on
/// all supported platforms (Linux/macOS/Windows, default stack ≥ 1 MB).
pub const MAX_DEPTH: u32 = 64;

/// Classify all mutable text blocks in a parsed body.
///
/// Returns a list of `(block_id, classification)` pairs for every mutable text
/// leaf. Exempt blocks are not included (they always return `unknown` if
/// [`classify`] is called on them directly).
///
/// # Examples
///
/// ```
/// use rskim_llm::{parse, classify_body};
///
/// let json = r#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"```rust\nfn main() {}\n```"}],"max_tokens":100}"#;
/// let body = parse(json.as_bytes())?;
/// let results = classify_body(&body);
/// assert_eq!(results.len(), 1);
/// assert_eq!(results[0].1.class, rskim_llm::Class::Code);
/// # Ok::<(), rskim_llm::LlmError>(())
/// ```
pub fn classify_body(body: &ParsedBody) -> Vec<(String, Classification)> {
    match body {
        ParsedBody::Anthropic(b) => {
            use model::anthropic::anthropic_leaf_texts;
            anthropic_leaf_texts(b)
                .into_iter()
                .map(|(id, text)| (id, classify(text)))
                .collect()
        }
        ParsedBody::OpenAi(b) => {
            use model::openai::OpenAiContent;
            let mut results = Vec::new();
            for (mi, msg) in b.messages.iter().enumerate() {
                match &msg.content {
                    Some(OpenAiContent::Text(text)) => {
                        results.push((format!("m{mi}"), classify(text)));
                    }
                    Some(OpenAiContent::Parts(parts)) => {
                        for (pi, part) in parts.iter().enumerate() {
                            if part.part_type == "text"
                                && let Some(text) = &part.text
                            {
                                results.push((format!("m{mi}p{pi}"), classify(text)));
                            }
                        }
                    }
                    None => {}
                }
            }
            results
        }
    }
}
