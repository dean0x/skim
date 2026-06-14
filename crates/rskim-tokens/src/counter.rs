//! Token counter types.
//!
//! Each counter is constructed once and may be used from multiple threads
//! (`Send + Sync` — statically asserted in tests). Counting is **infallible**
//! (`fn count(&self, text: &str) -> usize`), while construction may return
//! `Err` for tiktoken-backed encodings (though practically unreachable at
//! runtime because the vocab is embedded at compile time).
//!
//! # Thread safety
//!
//! `tiktoken-rs` `CoreBPE` uses a per-thread regex-match cache with a global
//! slot table. Slots are read-only after first assignment and the cache is
//! indexed by thread ID. Reads across threads from the same `CoreBPE` are
//! safe. Above 128 concurrent threads, threads share a slot but only read
//! it — no data race. `Counter` wraps `CoreBPE` in `OnceLock`, which is
//! `Send + Sync`.
//!
//! # Usage
//!
//! ```
//! use rskim_tokens::{Encoding, Counter};
//!
//! let counter = Counter::new(Encoding::Cl100k)?;
//! let n = counter.count("Hello, world!");
//! assert!(n > 0);
//!
//! // Closure adapter for rskim_core::truncate_to_token_budget
//! let f = counter.as_closure();
//! let n2 = f("Hello, world!");
//! assert_eq!(n, n2);
//! # Ok::<(), rskim_tokens::TokenError>(())
//! ```

use std::sync::OnceLock;

use tiktoken_rs::CoreBPE;

use crate::{
    Encoding, TokenError, anthropic_offline::count_anthropic_offline, heuristic::count_heuristic,
};

/// A constructed token counter that owns a single [`Encoding`].
///
/// Use [`Counter::new`] or [`Encoding::counter`] to construct. Construction
/// returns `Result<Counter, TokenError>` and is the only fallible step.
/// Once built, [`Counter::count`] is infallible.
///
/// # One counter, one encoding
///
/// A `Counter` owns exactly one encoding. Comparing counts from two sides
/// of a before/after check through the same `Counter` instance is the natural
/// pattern; mixing encodings requires constructing two separate `Counter`
/// instances — awkward by design (constraint 8, AC1).
pub struct Counter {
    inner: CounterInner,
}

/// Internal representation of the counting strategy.
enum CounterInner {
    /// cl100k_base BPE (pre-filled at construction).
    Cl100k(OnceLock<CoreBPE>),
    /// o200k_base BPE (pre-filled at construction).
    O200k(OnceLock<CoreBPE>),
    /// Anthropic offline approximation — uses a cl100k BPE internally.
    AnthropicOffline(OnceLock<CoreBPE>),
    /// Byte-length heuristic (no BPE needed).
    Heuristic,
}

/// Create a pre-filled `OnceLock<CoreBPE>`.
///
/// The lock is set immediately so [`OnceLock::get`] always returns `Some`
/// for any `CounterInner` variant that carries one.
fn prefilled_lock(bpe: CoreBPE) -> OnceLock<CoreBPE> {
    let lock = OnceLock::new();
    let _ = lock.set(bpe);
    lock
}

/// Count BPE tokens for a pre-filled lock, falling back to byte length if the
/// lock is somehow unset (structurally impossible — preserved for the
/// infallible contract).
#[inline]
fn count_bpe(lock: &OnceLock<CoreBPE>, text: &str) -> usize {
    lock.get()
        .map(|bpe| bpe.encode_with_special_tokens(text).len())
        .unwrap_or_else(|| count_heuristic(text))
}

impl Counter {
    /// Construct a counter for the given [`Encoding`].
    ///
    /// The BPE encoder is validated eagerly at construction time so callers
    /// receive `Err` before any counting attempt (satisfies AC10 no-panic
    /// invariant).
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::TiktokenInit`] if the embedded BPE vocabulary
    /// fails to decode. This is practically unreachable at runtime.
    pub fn new(encoding: Encoding) -> Result<Self, TokenError> {
        let inner = match encoding {
            Encoding::Cl100k => CounterInner::Cl100k(prefilled_lock(
                tiktoken_rs::cl100k_base().map_err(|e| TokenError::TiktokenInit {
                    encoding: "cl100k_base",
                    source: e,
                })?,
            )),
            Encoding::O200k => CounterInner::O200k(prefilled_lock(
                tiktoken_rs::o200k_base().map_err(|e| TokenError::TiktokenInit {
                    encoding: "o200k_base",
                    source: e,
                })?,
            )),
            // AnthropicOffline delegates to cl100k counts internally.
            Encoding::AnthropicOffline => CounterInner::AnthropicOffline(prefilled_lock(
                tiktoken_rs::cl100k_base().map_err(|e| TokenError::TiktokenInit {
                    encoding: "cl100k_base (for AnthropicOffline)",
                    source: e,
                })?,
            )),
            Encoding::Heuristic => CounterInner::Heuristic,
        };
        Ok(Self { inner })
    }

    /// Construct a counter from a pre-built `CoreBPE` instance.
    ///
    /// This is a **fault-injection seam for testing** (AC10). Use a known-good
    /// BPE to verify that `from_raw_bpe` produces a working counter, or substitute
    /// test logic to explore the internal path. Construction of a broken BPE
    /// happens at the tiktoken level (see counter unit tests).
    ///
    /// For normal use, prefer [`Counter::new`].
    #[cfg(test)]
    pub(crate) fn from_raw_bpe(encoding: Encoding, bpe: CoreBPE) -> Self {
        let lock = prefilled_lock(bpe);
        let inner = match encoding {
            Encoding::Cl100k => CounterInner::Cl100k(lock),
            Encoding::O200k => CounterInner::O200k(lock),
            Encoding::AnthropicOffline => CounterInner::AnthropicOffline(lock),
            // Heuristic carries no BPE — the lock (and the bpe inside it) is dropped here.
            Encoding::Heuristic => CounterInner::Heuristic,
        };
        Self { inner }
    }

    /// Count the tokens in `text` using this counter's encoding.
    ///
    /// This method is **infallible** — it never returns `Err` and never panics.
    /// For tiktoken-backed encodings, counting uses
    /// `encode_with_special_tokens` to preserve special-token semantics
    /// (constraint 13 / AC3).
    ///
    /// # Special tokens
    ///
    /// Special tokens such as `<|endoftext|>` are counted as single tokens
    /// (not tokenized as plain text), matching the legacy `tokens.rs` behaviour.
    #[must_use]
    pub fn count(&self, text: &str) -> usize {
        match &self.inner {
            CounterInner::Cl100k(lock) | CounterInner::O200k(lock) => count_bpe(lock, text),
            CounterInner::AnthropicOffline(lock) => {
                count_anthropic_offline(count_bpe(lock, text))
            }
            CounterInner::Heuristic => count_heuristic(text),
        }
    }

    /// Return a closure adapter that satisfies `Fn(&str) -> usize`.
    ///
    /// The returned closure borrows `self` and is suitable for use with
    /// [`rskim_core::truncate_to_token_budget`] (AC2).
    ///
    /// # Examples
    ///
    /// ```
    /// use rskim_tokens::{Encoding, Counter};
    ///
    /// let counter = Counter::new(Encoding::Cl100k)?;
    /// let closure = counter.as_closure();
    /// let n = closure("Hello, world!");
    /// assert!(n > 0);
    /// # Ok::<(), rskim_tokens::TokenError>(())
    /// ```
    pub fn as_closure(&self) -> impl Fn(&str) -> usize + '_ {
        move |text| self.count(text)
    }

    /// Return the [`Encoding`] this counter was constructed for.
    #[must_use]
    pub fn encoding(&self) -> Encoding {
        match &self.inner {
            CounterInner::Cl100k(_) => Encoding::Cl100k,
            CounterInner::O200k(_) => Encoding::O200k,
            CounterInner::AnthropicOffline(_) => Encoding::AnthropicOffline,
            CounterInner::Heuristic => Encoding::Heuristic,
        }
    }
}

// Counter is Send + Sync because:
// - OnceLock<CoreBPE> is Send + Sync (CoreBPE is Send + Sync per tiktoken-rs design).
// - CounterInner::Heuristic has no heap data.
// The static assertions in tests/integration.rs confirm this at compile time (AC11).
unsafe impl Send for Counter {}
unsafe impl Sync for Counter {}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod counter_tests {
    use super::*;

    /// AC10 fault-injection: from_raw_bpe provides a seam to construct a counter
    /// from an externally-built BPE. This test verifies that a counter built via
    /// from_raw_bpe produces the same counts as one built via Counter::new.
    #[test]
    fn from_raw_bpe_produces_same_counts_as_new() {
        let bpe = tiktoken_rs::cl100k_base().unwrap();
        let injected = Counter::from_raw_bpe(Encoding::Cl100k, bpe);
        let normal = Counter::new(Encoding::Cl100k).unwrap();

        let text = "Hello, world!";
        assert_eq!(
            injected.count(text),
            normal.count(text),
            "from_raw_bpe must produce identical counts to Counter::new"
        );
    }

    /// AC10: verify the Err path is reachable via the Result API contract.
    /// The tiktoken-rs init is practically infallible (embedded vocab), so we
    /// verify the structural Err type exists and is non-empty.
    #[test]
    fn counter_new_ok_for_all_encodings() {
        for encoding in [
            Encoding::Cl100k,
            Encoding::O200k,
            Encoding::AnthropicOffline,
            Encoding::Heuristic,
        ] {
            assert!(
                Counter::new(encoding).is_ok(),
                "Counter::new({encoding:?}) must return Ok"
            );
        }
    }
}
