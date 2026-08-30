//! Unified fidelity gate (A2) — single `decide()` used by both:
//!
//! - **L2-A** (`output/guardrail.rs`): file-transform path (`process.rs`).
//! - **L2-B** (`cmd/execution.rs::savings_decision`): command-handler path.
//!
//! Prior to A2 the two sites had diverging semantics:
//!
//! | Property | L2-A (`guardrail.rs`) | L2-B (`savings_decision`) |
//! |---|---|---|
//! | 256-byte floor | yes | no |
//! | Byte tie | KEEP (≤ passed) | PASSTHROUGH (≥ fails) |
//! | Token tie | KEEP (≤ passed) | PASSTHROUGH (≥ fails) |
//!
//! After A2 both sites delegate here.  The unified rule:
//!
//! **Keep IFF compressed is strictly smaller than raw in BOTH bytes AND tokens.**
//! Tie (equal) → Passthrough.  This is the conservative rule that matches
//! the #317 / ADR-001 "never expand" invariant.
//!
//! # What "never larger in bytes" means
//!
//! The byte gate is the *fast early exit*: if compressed (trimmed) is not
//! strictly shorter than raw (trimmed) in bytes, skip tokenisation entirely
//! and return `Passthrough`.  This means skim's output is always ≤ raw in
//! bytes when `Keep` is returned — the "never-expand-in-bytes" guarantee.
//!
//! # 256-byte floor removal (A4)
//!
//! The floor was a `guardrail.rs`-only exemption that skipped the guard for
//! tiny payloads.  With the unified gate the floor is gone: every payload,
//! regardless of size, is subject to the same conservative rule.  Tiny
//! payloads where the compressed form is byte-larger than raw now fall through
//! to Passthrough rather than being silently exempt.
//!
//! # L3 guardrail (`rskim-contract`) is NOT affected
//!
//! `rskim-contract/src/guardrail.rs` is the Layer-3 proxy guard.  It has
//! deliberate differences (per-unit byte-only gate, no tokeniser, no floor)
//! and is tracked for migration in #325.  This module does not touch it.

// ============================================================================
// Completeness / ViewClass — disclosure-gate types (ADR-015 / D1)
// ============================================================================

/// Whether the served view contains all content that was in the raw output.
///
/// Text-mode callers derive this via [`view_differs`].
/// JSON-mode callers MUST supply it explicitly: a JSON envelope always differs
/// textually from raw even when it faithfully represents every byte, so byte
/// comparison cannot detect content loss on the JSON path.
///
/// # No `Default` — intentional
///
/// A newly-written `--json` handler that tries to construct output without
/// explicitly choosing a `Completeness` gets a compile error.  This is the
/// type-level enforcement that prevents handlers from silently defaulting to
/// `Complete` (ADR-015 / D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum Completeness {
    /// The served bytes are byte-identical to raw (true lossless passthrough).
    /// No disclosure is owed.
    Complete,
    /// The view is structurally re-encoded (e.g. a JSON envelope) but
    /// faithfully represents all content that was in raw.  No disclosure is
    /// owed, but the re-encoding means byte comparison alone cannot prove this —
    /// the caller must declare it explicitly.
    Reencoded,
    /// The view drops or elides content that was in raw.
    /// An ADR-011 class-1 disclosure marker MUST be emitted.
    Lossy,
}

/// What kind of view is being served to the reader.
///
/// [`decide`] applies only to [`ViewClass::TextSubstitution`]; a
/// [`ViewClass::JsonEnvelope`] never substitutes raw text — it always differs
/// textually from raw, so substitution-gate semantics are meaningless for it.
/// Only [`Completeness`] determines whether disclosure is owed on that path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewClass {
    /// Compressed text substituted for raw text.  [`decide`] determines
    /// whether the substitution is cost-effective.
    TextSubstitution,
    /// Structured JSON wrapping the content.  [`decide`] is inapplicable;
    /// only [`Completeness`] determines whether disclosure is owed.
    JsonEnvelope,
}

/// Returns `true` when the served view differs byte-for-byte from raw
/// (ignoring trailing whitespace, consistent with [`decide`]).
///
/// # JSON path — do NOT use to infer `Completeness`
///
/// A JSON envelope always differs textually from raw even when it contains
/// all content.  JSON callers must supply [`Completeness`] explicitly.
pub(crate) fn view_differs(raw: &str, served: &str) -> bool {
    raw.trim() != served.trim()
}

/// Context for [`remedy_for`].
///
/// Currently empty: every invocation returns the same default remedy because
/// skim-only flags (`--json`, `--mode`) are stripped before the passthrough
/// exec (Phase C), making `SKIM_PASSTHROUGH=1` universally reachable.
///
/// Add fields here when narrower, invocation-specific remedies become possible.
pub(crate) struct RemedyCtx {
    _priv: (),
}

impl RemedyCtx {
    /// Construct a new [`RemedyCtx`].
    pub(crate) fn new() -> Self {
        Self { _priv: () }
    }
}

/// Resolve the narrowest escape-hatch remedy that is **actually true** for the
/// current invocation.
///
/// The default branch — and currently the only branch — returns the legacy
/// `"SKIM_PASSTHROUGH=1 for full output"` literal.  Because skim-only flags
/// are stripped before passthrough exec (Phase C, ADR-015), this remedy is
/// reachable from every invocation path, including `--json` and `--mode`.
///
/// The `_ctx` parameter is reserved for future narrower remedies; the default
/// branch ensures the pinned test assertions that check for `"SKIM_PASSTHROUGH=1"`
/// in marker text remain green.
pub(crate) fn remedy_for(_ctx: &RemedyCtx) -> &'static str {
    // ADR-011 class-1: the remedy string must be literally reachable from the
    // invocation that prints it.  Phase C flag-stripping makes SKIM_PASSTHROUGH=1
    // universally true, so this default is no longer false for any path.
    "SKIM_PASSTHROUGH=1 for full output"
}

// ============================================================================
// FidelityDecision — substitution gate
// ============================================================================

/// Outcome of the unified fidelity gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum FidelityDecision {
    /// Compressed body is strictly smaller in bytes AND tokens — emit it.
    Keep,
    /// Compressed body is equal or larger — emit raw verbatim instead.
    Passthrough,
}

/// Byte length of the longest run of consecutive non-ASCII-whitespace bytes.
///
/// cl100k BPE splits on whitespace, so a long no-split run is the pathological
/// (~O(n²) per-word merge) dimension. This scans the input once (O(n)) with no
/// allocation and bounds the worst-case per-word merge cost.
pub(crate) fn longest_nonwhitespace_run(s: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for &b in s.as_bytes() {
        if b.is_ascii_whitespace() {
            current = 0;
        } else {
            current += 1;
            longest = longest.max(current);
        }
    }
    longest
}

/// Decide whether to keep the compressed form or fall back to raw.
///
/// The rule is conservative: keep compressed IFF it is **strictly smaller**
/// than raw in both bytes and tokens. Any tie returns `Passthrough`.
///
/// # Trimming
///
/// Both sides are trimmed before byte comparison to normalise trailing
/// whitespace (e.g. a `println!` trailing newline should not flip the
/// decision arbitrarily).
///
/// # Size cap (256 KiB)
///
/// Tokenisation costs ~0.3 s/MB in release; for inputs above 256 KiB the
/// function falls back to byte comparison. The strict byte gate has already
/// fired at this point (compressed is byte-shorter), so `Keep` is still correct.
///
/// # Run cap (4 KiB longest non-whitespace run)
///
/// cl100k's per-word merge is O(n²) in run length; runs > 4 KiB fall back to
/// the byte path (same as above cap).
///
/// # Tokeniser unavailable
///
/// When `count_token_pair` returns `(None, None)`, byte comparison alone
/// decides. Strictly byte-shorter → `Keep`; never panics, never expands.
pub(crate) fn decide(raw: &str, compressed: &str) -> FidelityDecision {
    /// 256 KiB — above this threshold skip tokenisation (performance cap).
    const TOKEN_SIZE_CAP: usize = 256 * 1024;
    /// 4 KiB — longest non-whitespace run above which skip tokenisation.
    const TOKEN_RUN_CAP: usize = 4 * 1024;
    // Compile-time invariant: size cap must be strictly greater than run cap.
    const { assert!(TOKEN_SIZE_CAP > TOKEN_RUN_CAP) };

    let raw_t = raw.trim();
    let comp_t = compressed.trim();

    // Byte early-exit: not strictly shorter → Passthrough (conservative rule).
    // Covers empty-raw case (0 < 0 fails → Passthrough) and ties (n == n fails).
    if comp_t.len() >= raw_t.len() {
        return FidelityDecision::Passthrough;
    }

    // comp_t.len() < raw_t.len() — bytes say compressed is strictly shorter.
    let over_size_cap = raw.len() > TOKEN_SIZE_CAP || compressed.len() > TOKEN_SIZE_CAP;

    let over_run_cap = !over_size_cap
        && (longest_nonwhitespace_run(raw) > TOKEN_RUN_CAP
            || longest_nonwhitespace_run(compressed) > TOKEN_RUN_CAP);

    if over_size_cap || over_run_cap {
        // Byte path: comp_t.len() < raw_t.len() was verified above → Keep.
        return FidelityDecision::Keep;
    }

    // Token slow path: confirm the byte saving is also a token saving.
    match crate::process::count_token_pair(raw_t, comp_t) {
        (Some(raw_tok), Some(comp_tok)) => {
            if comp_tok < raw_tok {
                // Strictly fewer tokens — keep compressed.
                FidelityDecision::Keep
            } else {
                // Token tie or token-expansion even though bytes were shorter → Passthrough.
                FidelityDecision::Passthrough
            }
        }
        // Tokeniser unavailable: byte comparison says comp_t.len() < raw_t.len() → Keep.
        _ => FidelityDecision::Keep,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Basic decisions
    // -----------------------------------------------------------------------

    #[test]
    fn decide_shorter_keep() {
        let raw = "a".repeat(100);
        let compressed = "a".repeat(50);
        assert_eq!(decide(&raw, &compressed), FidelityDecision::Keep);
    }

    #[test]
    fn decide_tie_passthrough() {
        let s = "hello world\n";
        assert_eq!(
            decide(s, s),
            FidelityDecision::Passthrough,
            "tie (identical) → Passthrough (conservative)"
        );
    }

    #[test]
    fn decide_larger_passthrough() {
        let raw = "short\n";
        let compressed = raw.repeat(3);
        assert_eq!(decide(raw, &compressed), FidelityDecision::Passthrough);
    }

    // -----------------------------------------------------------------------
    // A4: No 256-byte floor — tiny payloads are NOT exempt
    // -----------------------------------------------------------------------

    /// A4: A 1-byte raw with a 100-byte compressed form must NOT be exempt.
    /// Pre-A4 (guardrail.rs MIN_RAW_SIZE_FOR_GUARDRAIL): Tier 0 would skip and
    /// return Passed { output: compressed }.  Post-A4: Passthrough fires.
    #[test]
    fn a4_no_floor_tiny_raw_compressed_larger_passthrough() {
        let raw = "x";
        let compressed = "this is a much longer string that has many more bytes than raw";
        assert_eq!(
            decide(raw, compressed),
            FidelityDecision::Passthrough,
            "A4: tiny raw must NOT skip the guard — compressed larger → Passthrough"
        );
    }

    /// A4: A tiny raw with a clearly shorter compressed form → Keep.
    /// The floor was removed but the rule is otherwise the same — strictly shorter wins.
    ///
    /// Uses inputs where compressed is substantially shorter in BOTH bytes AND tokens
    /// so the test is not sensitive to tokenizer availability (byte-cap fallback → Keep;
    /// token slow-path → Keep; tokenizer unavailable → byte-comparison → Keep).
    #[test]
    fn a4_no_floor_tiny_raw_compressed_smaller_keep() {
        // raw: 8 distinct words → at least 8 tokens; tiny (< 256 bytes)
        let raw = "alpha beta gamma delta epsilon zeta eta theta";
        // compressed: single word, ~7 bytes, ~1 token — clearly shorter in both dimensions
        let compressed = "summary";
        assert_eq!(
            decide(raw, compressed),
            FidelityDecision::Keep,
            "A4: tiny raw, substantially shorter compressed → Keep (floor removal does not break this)"
        );
    }

    // -----------------------------------------------------------------------
    // Tie semantics — Passthrough on tie (not just on expansion)
    // -----------------------------------------------------------------------

    /// Byte tie (equal trimmed lengths) must produce Passthrough, not Keep.
    /// Pre-A2 guardrail.rs used `<=` (tied bytes → Passed/Keep).
    /// Post-A2: `>=` early-exit means tie → Passthrough.
    #[test]
    fn a2_byte_tie_passthrough() {
        let raw = "hello world"; // 11 bytes
        let compressed = "world hello"; // 11 bytes — same length, tie
        assert_eq!(
            decide(raw, compressed),
            FidelityDecision::Passthrough,
            "A2: byte tie must produce Passthrough (strictly-smaller rule)"
        );
    }

    // -----------------------------------------------------------------------
    // Performance / cap guards (carry-over from savings_decision tests)
    // -----------------------------------------------------------------------

    #[test]
    fn above_cap_shorter_keep() {
        let raw = "x".repeat(512 * 1024);
        let compressed = "x".repeat(1024);
        assert_eq!(decide(&raw, &compressed), FidelityDecision::Keep);
    }

    #[test]
    fn above_cap_longer_passthrough() {
        let raw = "x".repeat(512 * 1024);
        let compressed = "y".repeat(512 * 1024 + 1);
        assert_eq!(decide(&raw, &compressed), FidelityDecision::Passthrough);
    }

    // -----------------------------------------------------------------------
    // D1: Completeness / ViewClass / view_differs / remedy_for (ADR-015)
    // -----------------------------------------------------------------------

    /// Completeness has no Default impl; constructing one requires an explicit
    /// variant.  This test simply confirms the type is usable and that the three
    /// variants are distinct.
    #[test]
    fn completeness_variants_are_distinct() {
        assert_ne!(Completeness::Complete, Completeness::Lossy);
        assert_ne!(Completeness::Reencoded, Completeness::Lossy);
        assert_ne!(Completeness::Complete, Completeness::Reencoded);
    }

    /// The JSON path must be declared `Reencoded` when all content is present,
    /// `Lossy` when content is dropped.  The test also verifies `ViewClass`
    /// discriminates the two surfaces.
    #[test]
    fn view_class_separates_text_from_json() {
        assert_ne!(ViewClass::TextSubstitution, ViewClass::JsonEnvelope);
    }

    /// `view_differs` returns false when the strings are byte-identical (modulo
    /// trailing whitespace).
    #[test]
    fn view_differs_identical_returns_false() {
        let raw = "hello world\n";
        assert!(!view_differs(raw, raw));
        assert!(!view_differs("foo\n", "foo")); // trailing-ws normalisation
    }

    /// `view_differs` returns true when bytes diverge.
    #[test]
    fn view_differs_changed_returns_true() {
        assert!(view_differs("original content", "compressed summary"));
        assert!(view_differs("line1\nline2\n", "line1\n")); // content removed
    }

    /// `remedy_for` returns a string containing `SKIM_PASSTHROUGH=1` so that
    /// the ~N pinned test assertions across the test suite stay green.
    #[test]
    fn remedy_for_contains_passthrough_hint() {
        let ctx = RemedyCtx::new();
        let remedy = remedy_for(&ctx);
        assert!(
            remedy.contains("SKIM_PASSTHROUGH=1"),
            "remedy_for default must contain SKIM_PASSTHROUGH=1 (legacy literal); got: {remedy:?}"
        );
    }

    /// `remedy_for` default branch returns the exact legacy literal so pinned
    /// test assertions are not broken by the new dispatch function.
    #[test]
    fn remedy_for_default_is_legacy_literal() {
        let ctx = RemedyCtx::new();
        assert_eq!(
            remedy_for(&ctx),
            "SKIM_PASSTHROUGH=1 for full output",
            "default remedy must match legacy literal to preserve pinned test assertions"
        );
    }
}
