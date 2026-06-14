//! Encoding variants and model-ID→encoding lookup table.
//!
//! This module is the **single source of truth** for the mapping from model IDs
//! to token encodings. No other code in the workspace may maintain a parallel
//! model→encoding table (AC7 requires exactly one mapping).
//!
//! # Model ID resolution
//!
//! [`encoding_for_model`] uses a two-tier strategy:
//!
//! 1. **Exact match** — the curated table below for known model IDs.
//! 2. **Family-prefix fallback** — for unknown IDs:
//!    - `gpt-*`, `o1*`, `o3*`, `o4*`, `chatgpt-*` → [`Encoding::O200k`]
//!      (newer-than-table OpenAI models default to o200k).
//!    - `claude-*` → [`Encoding::AnthropicOffline`].
//!    - Anything else → [`Encoding::Heuristic`] (safe conservative ceiling).
//!
//! This design means an unknown model ID **never errors or panics** — it always
//! resolves to a sensible conservative encoding (PRISM #552 lesson: never error
//! on an unknown model ID).

/// The token encoding / counting strategy to use for a given model.
///
/// Each variant corresponds to a distinct counting implementation:
/// - Tiktoken-backed variants (`Cl100k`, `O200k`) use embedded BPE vocabularies.
/// - `AnthropicOffline` uses a deterministic offline approximation (cl100k × 1.25).
/// - `Heuristic` uses byte length as a provably-safe ceiling (`token_count ≤ byte_count`
///   for any BPE over UTF-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// OpenAI `cl100k_base` — GPT-3.5-turbo, GPT-4, GPT-4-turbo.
    Cl100k,
    /// OpenAI `o200k_base` — GPT-4o, GPT-4o-mini, o1, o3, GPT-4.1, and newer
    /// unknown OpenAI-family models (family-prefix fallback).
    O200k,
    /// Anthropic offline approximation — Claude Sonnet/Opus/Haiku and unknown
    /// `claude-*` models. Deterministic, zero network I/O. See
    /// [`crate::anthropic_offline`] for the formula and basis.
    AnthropicOffline,
    /// Conservative byte-length heuristic — unknown provider. Provably safe:
    /// `token_count ≤ byte_count` for any BPE over UTF-8.
    Heuristic,
}

/// Resolve a model ID to its token encoding.
///
/// This is the **single source of truth** for model→encoding mapping in the
/// workspace. All consumers (rskim-llm #302, rskim-contract #301, etc.) must
/// import and call this function rather than maintaining their own tables.
///
/// # Resolution strategy
///
/// 1. Exact match against the curated table for known model IDs.
/// 2. Family-prefix fallback for unknown IDs (see module docs).
///
/// This function never errors or panics — every string resolves to an encoding.
///
/// # Examples
///
/// ```
/// use rskim_tokens::{Encoding, encoding_for_model};
///
/// assert_eq!(encoding_for_model("gpt-4"), Encoding::Cl100k);
/// assert_eq!(encoding_for_model("gpt-4o"), Encoding::O200k);
/// assert_eq!(encoding_for_model("claude-sonnet-4-5"), Encoding::AnthropicOffline);
/// assert_eq!(encoding_for_model("some-unknown-llm"), Encoding::Heuristic);
/// ```
#[must_use]
pub fn encoding_for_model(model_id: &str) -> Encoding {
    // --- Tier 1: Exact match (curated table) ---
    match model_id {
        // cl100k_base encodings (GPT-3.5 / GPT-4 family)
        "gpt-3.5-turbo"
        | "gpt-3.5-turbo-0613"
        | "gpt-3.5-turbo-16k"
        | "gpt-3.5-turbo-16k-0613"
        | "gpt-4"
        | "gpt-4-0314"
        | "gpt-4-32k"
        | "gpt-4-32k-0314"
        | "gpt-4-turbo"
        | "gpt-4-turbo-2024-04-09"
        | "gpt-4-turbo-preview" => Encoding::Cl100k,

        // o200k_base encodings (GPT-4o / o-series / GPT-4.1)
        "gpt-4o"
        | "gpt-4o-2024-05-13"
        | "gpt-4o-2024-08-06"
        | "gpt-4o-mini"
        | "gpt-4o-mini-2024-07-18"
        | "o1"
        | "o1-mini"
        | "o1-preview"
        | "o3"
        | "o3-mini"
        | "gpt-4.1"
        | "gpt-4.1-mini"
        | "gpt-4.1-nano" => Encoding::O200k,

        // Anthropic offline approximation (Claude family — exact known IDs)
        "claude-sonnet-4-5"
        | "claude-opus-4-5"
        | "claude-haiku-4-5"
        | "claude-3-5-sonnet-20241022"
        | "claude-3-5-haiku-20241022"
        | "claude-3-opus-20240229"
        | "claude-3-sonnet-20240229"
        | "claude-3-haiku-20240307" => Encoding::AnthropicOffline,

        // --- Tier 2: Family-prefix fallback ---
        _ => family_prefix_fallback(model_id),
    }
}

/// Two-tier family-prefix fallback for unknown model IDs.
///
/// Never panics; always returns a valid encoding.
fn family_prefix_fallback(model_id: &str) -> Encoding {
    // OpenAI-family prefixes: unknown IDs resolve to o200k (newer-than-table assumption).
    // Covered prefixes: gpt-*, o1*, o3*, o4*, chatgpt-* — matching the spec (OQ5).
    // Note: o2 is intentionally absent — OpenAI ships no o2 model line and the
    // spec does not enumerate it. Dropping it keeps code and doc in sync.
    if model_id.starts_with("gpt-")
        || model_id.starts_with("o1")
        || model_id.starts_with("o3")
        || model_id.starts_with("o4")
        || model_id.starts_with("chatgpt-")
    {
        return Encoding::O200k;
    }

    // Anthropic-family prefix
    if model_id.starts_with("claude-") {
        return Encoding::AnthropicOffline;
    }

    // Unknown provider → safe conservative ceiling
    Encoding::Heuristic
}
