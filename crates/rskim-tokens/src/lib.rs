//! Multi-provider token counting library.
//!
//! `rskim-tokens` provides a deterministic, panic-free API for counting tokens
//! across four encoding strategies:
//!
//! | Encoding | Tokenizer | Notes |
//! |---|---|---|
//! | [`Encoding::Cl100k`] | OpenAI `cl100k_base` (BPE) | GPT-3.5-turbo, GPT-4 |
//! | [`Encoding::O200k`] | OpenAI `o200k_base` (BPE) | GPT-4o, o1, o3, GPT-4.1 |
//! | [`Encoding::AnthropicOffline`] | cl100k × 1.25 (offline approx) | Claude family |
//! | [`Encoding::Heuristic`] | `byte_len` (proven upper bound) | Unknown provider |
//!
//! # Quick start
//!
//! ```
//! use rskim_tokens::{Encoding, Counter, encoding_for_model};
//!
//! // Construct a counter (fallible — embedded vocab decode, practically infallible)
//! let counter = Counter::new(Encoding::Cl100k)?;
//!
//! // Count tokens (infallible)
//! let n = counter.count("Hello, world!");
//! assert!(n > 0);
//!
//! // Closure adapter for rskim_core::truncate_to_token_budget
//! let f = counter.as_closure();
//! assert_eq!(f("Hello, world!"), n);
//!
//! // Model-ID lookup
//! let enc = encoding_for_model("gpt-4o");
//! assert_eq!(enc, Encoding::O200k);
//! # Ok::<(), rskim_tokens::TokenError>(())
//! ```
//!
//! # Network counter (opt-in feature)
//!
//! An Anthropic API-backed counter is available behind the **non-default**
//! `net-anthropic` Cargo feature:
//!
//! ```toml
//! [dependencies]
//! rskim-tokens = { path = "...", features = ["net-anthropic"] }
//! ```
//!
//! The default build pulls **no HTTP/TLS crates** (proven by CI dependency-tree
//! assertion, AC9).
//!
//! # Design principles
//!
//! - Construction is **fallible** (`Result<Counter, TokenError>`) but practically
//!   always `Ok` — tiktoken embeds its vocab at compile time.
//! - Counting is **infallible** (`fn count(&self, &str) -> usize`) — no `Err`,
//!   no panics in production code (`clippy::unwrap_used = deny`).
//! - One `Counter` owns exactly one `Encoding` — mixing encodings requires
//!   constructing two instances, awkward by design (constraint 8).
//! - All counters are `Send + Sync` — safe for multi-threaded use.
//!
//! # Related tickets
//!
//! - #301 `rskim-contract` — consumes `encoding_for_model` for LLM-contract validation.
//! - #302 `rskim-llm` — consumes counter closures for token-budget enforcement.
//! - #304/#305 — consume counter closures for truncation workflows.
//! - #309 — Wave-1 tracking issue; convention: no `l3-` infix on crate names.
//! - #324 — follow-up: publish `rskim-tokens` to crates.io for external consumers.

#![deny(missing_docs)]

pub mod anthropic_offline;
pub mod counter;
pub mod encoding;
pub mod error;
pub mod heuristic;

#[cfg(feature = "net-anthropic")]
pub mod net;

pub use counter::Counter;
pub use encoding::{Encoding, encoding_for_model};
pub use error::TokenError;

/// Convenience constructor: build a [`Counter`] for a model ID.
///
/// Equivalent to `Counter::new(encoding_for_model(model_id))`.
///
/// # Errors
///
/// Returns [`TokenError::TiktokenInit`] if tiktoken fails to initialise
/// (practically unreachable — embedded vocab).
///
/// # Examples
///
/// ```
/// use rskim_tokens::counter_for_model;
///
/// let counter = counter_for_model("gpt-4o")?;
/// let n = counter.count("Hello!");
/// assert!(n > 0);
/// # Ok::<(), rskim_tokens::TokenError>(())
/// ```
pub fn counter_for_model(model_id: &str) -> Result<Counter, TokenError> {
    Counter::new(encoding_for_model(model_id))
}
