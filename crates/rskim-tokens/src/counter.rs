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
//! it — no data race. `Counter` holds an `Arc<CoreBPE>` handed out by the
//! process-wide BPE cache (see the private `cached_bpe`); `Arc<CoreBPE>` is `Send +
//! Sync` because `CoreBPE` is, so many `Counter`s on many threads share one
//! immutable table.
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

use std::sync::{Arc, OnceLock};

use tiktoken_rs::CoreBPE;

use crate::{
    Encoding, Result, TokenError, anthropic_offline::count_anthropic_offline,
    heuristic::count_heuristic,
};

/// A constructed token counter that owns a single [`Encoding`].
///
/// Use [`Counter::new`] to construct. Construction
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
    /// BPE-backed counter (cl100k_base or o200k_base). The `Encoding` tag
    /// distinguishes which vocabulary is loaded so `Counter::encoding()` can
    /// report the correct variant without storing a separate field.
    Bpe(Encoding, Arc<CoreBPE>),
    /// Anthropic offline approximation — uses a cl100k BPE internally.
    AnthropicOffline(Arc<CoreBPE>),
    /// Byte-length heuristic (no BPE needed).
    Heuristic,
}

/// Memoised BPE-construction outcome: either the shared table or the stringified
/// build error.
///
/// The error is a `String` rather than an `anyhow::Error` because `anyhow::Error`
/// is not `Clone` and the memoised failure must be re-wrapped into a *fresh*
/// [`TokenError::TiktokenInit`] on every subsequent call (see [`cached_bpe`]).
type CachedBpe = std::result::Result<Arc<CoreBPE>, String>;

/// Process-wide `cl100k_base` table (shared by `Cl100k` and `AnthropicOffline`).
static CL100K: OnceLock<CachedBpe> = OnceLock::new();

/// Process-wide `o200k_base` table.
static O200K: OnceLock<CachedBpe> = OnceLock::new();

/// Build (once) or hand out (thereafter) a process-wide shared BPE table.
///
/// # Why this cache exists
///
/// `tiktoken_rs::cl100k_base()` / `o200k_base()` are **constructors, not
/// accessors**: each call base64-decodes ~100,257 vocabulary lines, inserts
/// every one into a hash map, and compiles a `fancy_regex` pattern. Measured
/// cost: **~230 ms per call**. Before this cache, `Counter::new` called the
/// constructor on every construction, and the proxy analytics consumer
/// (`rskim::cmd::proxy_analytics::spawn_consumer`) constructs one `Counter`
/// **per event** — it must, because each event carries its own provider+model
/// and the encoding is resolved per event via `encoding_for_provider_model`.
/// Hoisting a single `Counter` out of that drain loop is *not* a valid fix: a
/// proxy multiplexes providers, so one hoisted `Counter` would count OpenAI
/// bodies with the Anthropic tokeniser. The rebuild cost therefore has to be
/// removed here, at the table.
///
/// Measured consumer-shutdown latency for a 20-event drain before the cache:
/// 4573 ms at idle (p50) and 18972 ms at 30× CPU oversubscription. Token
/// counting was 99.9 % of it; the SQLite writes were ≤ 12 ms.
///
/// # Keyed by TABLE, not by `Encoding`
///
/// There are four [`Encoding`] variants but only **two** BPE tables:
/// `Cl100k` and `AnthropicOffline` both use `cl100k_base` (the latter
/// post-multiplies the count by 1.25), `O200k` uses `o200k_base`, and
/// `Heuristic` uses none. Keying the cache by `Encoding` would build a second,
/// byte-identical copy of `cl100k_base` for `AnthropicOffline`.
///
/// # Why `Arc<CoreBPE>` and not `tiktoken_rs::cl100k_base_singleton()`
///
/// The upstream singleton returns `&'static CoreBPE` (no `Mutex`, so no
/// serialisation concern) but it `.unwrap()`s inside `lazy_static!`. A vocab
/// decode failure would therefore become a permanent panic, converting
/// `Counter::new`'s documented `Result` contract into an abort (AC10 no-panic
/// construction) and destroying the fault-injection seam the AC10 tests need.
/// `OnceLock<Result<Arc<CoreBPE>, String>>` memoises the *outcome* — success or
/// failure — and re-wraps a memoised failure into a fresh `TokenError` on every
/// call, preserving both properties.
///
/// # Why sharing one table across threads is sound
///
/// `CoreBPE` is `Send + Sync`. tiktoken proves this itself: its own
/// `lazy_static! { static ref CL100K_BASE: CoreBPE }` only compiles if
/// `CoreBPE: Sync`. Its `_get_tl_regex` merely **reads** `regex_tls[hash % 128]`
/// — the slot table is populated at construction and never mutated afterwards,
/// so concurrent readers cannot race. The analytics file-op recorder already
/// relies on this (`rows.into_par_iter()` under rayon).
fn cached_bpe(
    slot: &'static OnceLock<CachedBpe>,
    encoding: &'static str,
    build: fn() -> std::result::Result<CoreBPE, anyhow::Error>,
) -> Result<Arc<CoreBPE>> {
    match slot.get_or_init(|| build().map(Arc::new).map_err(|e| e.to_string())) {
        Ok(bpe) => Ok(Arc::clone(bpe)),
        Err(msg) => Err(TokenError::TiktokenInit {
            encoding,
            source: anyhow::anyhow!(msg.clone()),
        }),
    }
}

/// Shared `cl100k_base` table (`Cl100k` and `AnthropicOffline`).
fn cl100k() -> Result<Arc<CoreBPE>> {
    cached_bpe(&CL100K, "cl100k_base", tiktoken_rs::cl100k_base)
}

/// Shared `o200k_base` table (`O200k`).
fn o200k() -> Result<Arc<CoreBPE>> {
    cached_bpe(&O200K, "o200k_base", tiktoken_rs::o200k_base)
}

/// Count BPE tokens for a shared table.
#[inline]
fn count_bpe(bpe: &CoreBPE, text: &str) -> usize {
    bpe.encode_with_special_tokens(text).len()
}

impl Counter {
    /// Construct a counter for the given [`Encoding`].
    ///
    /// The BPE table is resolved eagerly at construction time so callers
    /// receive `Err` before any counting attempt (satisfies AC10 no-panic
    /// invariant). The table itself is built at most **once per process** and
    /// shared via `Arc` — see the private `cached_bpe` for why.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::TiktokenInit`] if the embedded BPE vocabulary
    /// fails to decode. This is practically unreachable at runtime.
    pub fn new(encoding: Encoding) -> Result<Self> {
        let inner = match encoding {
            Encoding::Cl100k => CounterInner::Bpe(Encoding::Cl100k, cl100k()?),
            Encoding::O200k => CounterInner::Bpe(Encoding::O200k, o200k()?),
            // AnthropicOffline delegates to cl100k counts internally — it shares
            // the very same cached table (no second copy).
            Encoding::AnthropicOffline => CounterInner::AnthropicOffline(cl100k()?),
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
        let bpe = Arc::new(bpe);
        let inner = match encoding {
            Encoding::Cl100k | Encoding::O200k => CounterInner::Bpe(encoding, bpe),
            Encoding::AnthropicOffline => CounterInner::AnthropicOffline(bpe),
            // Heuristic carries no BPE — the Arc (and the bpe inside it) is dropped here.
            Encoding::Heuristic => CounterInner::Heuristic,
        };
        Self { inner }
    }

    /// Borrow the shared BPE table backing this counter, if it has one.
    ///
    /// Test-only accessor supporting the `Arc::ptr_eq` table-sharing guards in
    /// `counter_tests`. Production code never needs the raw table.
    #[cfg(test)]
    pub(crate) fn bpe_for_test(&self) -> Option<&Arc<CoreBPE>> {
        match &self.inner {
            CounterInner::Bpe(_, bpe) | CounterInner::AnthropicOffline(bpe) => Some(bpe),
            CounterInner::Heuristic => None,
        }
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
            CounterInner::Bpe(_, bpe) => count_bpe(bpe, text),
            CounterInner::AnthropicOffline(bpe) => count_anthropic_offline(count_bpe(bpe, text)),
            CounterInner::Heuristic => count_heuristic(text),
        }
    }

    /// Return a closure adapter that satisfies `Fn(&str) -> usize`.
    ///
    /// The returned closure borrows `self` and is suitable for use with
    /// `rskim_core::truncate_to_token_budget` (AC2).
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

    /// Construct an infallible byte-length heuristic counter.
    ///
    /// This constructor never fails and requires no embedded vocabulary.
    /// Use it as a guaranteed fallback when tiktoken initialisation is
    /// unavailable (e.g. the `Cl100k`/`O200k` arms of `Counter::new` return
    /// `Err` on a corrupted build). Satisfies the no-panic requirement of
    /// AC10: callers can use this instead of `unreachable!()` or `unwrap()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use rskim_tokens::{Counter, Encoding};
    ///
    /// let counter = Counter::heuristic();
    /// assert_eq!(counter.encoding(), Encoding::Heuristic);
    /// // Byte-length heuristic: "hello" is 5 bytes
    /// assert_eq!(counter.count("hello"), 5);
    /// ```
    #[must_use]
    pub fn heuristic() -> Self {
        Self {
            inner: CounterInner::Heuristic,
        }
    }

    /// Return the [`Encoding`] this counter was constructed for.
    #[must_use]
    pub fn encoding(&self) -> Encoding {
        match &self.inner {
            CounterInner::Bpe(enc, _) => *enc,
            CounterInner::AnthropicOffline(_) => Encoding::AnthropicOffline,
            CounterInner::Heuristic => Encoding::Heuristic,
        }
    }
}

// Counter is Send + Sync *automatically* (auto-derived by the compiler):
// - Arc<CoreBPE> is Send + Sync because CoreBPE is Send + Sync.
// - CounterInner::Heuristic carries no data.
//
// We deliberately do NOT hand-write `unsafe impl Send/Sync` here: doing so would
// suppress the compiler's auto-trait check and could silently mask a genuine data
// race if a future field were not thread-safe. The static assertions in
// tests/integration.rs (`assert_impl_all!(Counter: Send, Sync)`) verify the
// auto-derived bounds hold at compile time (AC11).

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

    /// AC10: the `TokenError::TiktokenInit` Err arm is **exercised**, not merely
    /// asserted to exist. The embedded vocab never fails in practice, so we
    /// pre-seed a test-local cache slot with a memoised `Err` and call the real
    /// production helper [`cached_bpe`] against it. That covers the whole error
    /// translation *including* the memoised-`Err` re-wrap, which is the arm that
    /// makes a cached failure keep returning `Result` instead of panicking.
    #[test]
    #[allow(unreachable_patterns)] // see the feature-gating note on the match's catch-all arm
    fn cached_bpe_err_path_is_exercised() {
        static POISONED: OnceLock<CachedBpe> = OnceLock::new();
        POISONED
            .set(Err("simulated embedded-vocab decode failure".to_string()))
            .unwrap_or_else(|_| panic!("test-local slot must be settable exactly once"));

        // The `build` fn is a real, working constructor: if the memoised Err were
        // ignored and the builder re-run, this would return Ok and fail the match.
        let injected = cached_bpe(&POISONED, "cl100k_base", tiktoken_rs::cl100k_base);
        match injected {
            Err(TokenError::TiktokenInit { encoding, source }) => {
                assert_eq!(encoding, "cl100k_base");
                assert!(
                    source.to_string().contains("simulated"),
                    "source error must be propagated, got: {source}"
                );
            }
            // CoreBPE has no Debug impl, so describe the unexpected arm by hand.
            // The `Err(other)` arm is unreachable under default features (only
            // `TiktokenInit` exists) but required for exhaustiveness under
            // `--all-features`, where `TokenError`'s net-anthropic-gated variants
            // (`MissingApiKey`/`NetworkRequest`/`ApiResponse`) appear.
            Err(other) => panic!("expected TiktokenInit Err, got a different error: {other}"),
            Ok(_) => panic!("expected Err, got Ok(Arc<CoreBPE>)"),
        }

        // The memoised failure must be re-wrappable an unbounded number of times —
        // `anyhow::Error` is not Clone, which is why the cache stores a String.
        assert!(
            cached_bpe(&POISONED, "cl100k_base", tiktoken_rs::cl100k_base).is_err(),
            "a memoised Err must keep producing Err on every subsequent call"
        );
    }

    /// **Strict guard against reintroducing the per-event BPE rebuild.**
    ///
    /// Every `Counter::new(Cl100k)` must hand back the *same* `Arc<CoreBPE>`, and
    /// `AnthropicOffline` must share that identical table rather than building a
    /// second byte-identical copy (the cache is keyed by TABLE, not by
    /// `Encoding`).
    ///
    /// This is a **pointer-identity** assertion, not a wall-clock one, so it can
    /// never flake under CPU oversubscription — unlike a "construction must take
    /// < N ms" test. Reverting `Counter::new` to call
    /// `tiktoken_rs::cl100k_base()` per construction makes the pointers differ
    /// and fails here immediately.
    #[test]
    fn counter_new_shares_one_cl100k_table_across_constructions() {
        let a = Counter::new(Encoding::Cl100k).unwrap();
        let b = Counter::new(Encoding::Cl100k).unwrap();
        let anthropic = Counter::new(Encoding::AnthropicOffline).unwrap();

        let a_bpe = a.bpe_for_test().expect("Cl100k counter must carry a table");
        let b_bpe = b.bpe_for_test().expect("Cl100k counter must carry a table");
        let anthropic_bpe = anthropic
            .bpe_for_test()
            .expect("AnthropicOffline counter must carry a table");

        assert!(
            Arc::ptr_eq(a_bpe, b_bpe),
            "two Cl100k Counters must share ONE cached cl100k_base table; \
             distinct pointers mean Counter::new rebuilds the ~100k-entry BPE \
             per construction (~230ms each)"
        );
        assert!(
            Arc::ptr_eq(a_bpe, anthropic_bpe),
            "AnthropicOffline must reuse the SAME cl100k_base table as Cl100k; \
             a distinct pointer means the cache is keyed by Encoding instead of \
             by table, doubling the resident BPE memory"
        );
    }

    /// Companion guard for the second cached table — see
    /// [`counter_new_shares_one_cl100k_table_across_constructions`].
    #[test]
    fn counter_new_shares_one_o200k_table_across_constructions() {
        let a = Counter::new(Encoding::O200k).unwrap();
        let b = Counter::new(Encoding::O200k).unwrap();

        let a_bpe = a.bpe_for_test().expect("O200k counter must carry a table");
        let b_bpe = b.bpe_for_test().expect("O200k counter must carry a table");

        assert!(
            Arc::ptr_eq(a_bpe, b_bpe),
            "two O200k Counters must share ONE cached o200k_base table; \
             distinct pointers mean Counter::new rebuilds the BPE per construction"
        );
    }

    /// The two cached tables must be distinct — sharing one slot between
    /// `cl100k_base` and `o200k_base` would silently count OpenAI bodies with the
    /// wrong vocabulary.
    #[test]
    fn cl100k_and_o200k_are_separate_tables() {
        let cl = Counter::new(Encoding::Cl100k).unwrap();
        let o2 = Counter::new(Encoding::O200k).unwrap();
        assert!(
            !Arc::ptr_eq(
                cl.bpe_for_test().expect("cl100k table"),
                o2.bpe_for_test().expect("o200k table")
            ),
            "cl100k_base and o200k_base must be separate cached tables"
        );
    }

    /// The heuristic counter carries no table at all.
    #[test]
    fn heuristic_counter_carries_no_bpe_table() {
        assert!(
            Counter::heuristic().bpe_for_test().is_none(),
            "Heuristic counters must not hold a BPE table"
        );
    }
}
