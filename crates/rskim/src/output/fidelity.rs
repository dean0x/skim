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

use std::borrow::Cow;

use crate::cmd::execution::OutputFormat;

// ============================================================================
// Completeness — disclosure-gate type (ADR-015 / D1)
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

/// Context for [`remedy_for`] — everything that decides whether the legacy
/// `SKIM_PASSTHROUGH=1` hint is *literally true* for this invocation.
pub(crate) struct RemedyCtx<'a> {
    /// The tool whose output is being served (`"git"`, `"psql"`, `"eslint"`, …).
    /// Always a closed-vocabulary handler name, never user-supplied text.
    pub(crate) tool: &'a str,
    /// The format the reader asked for.  Only [`OutputFormat::Json`] can make
    /// the legacy hint false, because `--json` is the one skim-only flag that
    /// is not stripped for every tool before the passthrough exec.
    pub(crate) output_format: OutputFormat,
    /// `true` when `SKIM_PASSTHROUGH=1 skim <tool> <argv>` re-executes the real
    /// tool with an argv it accepts — i.e. every skim-only flag in `argv` is
    /// removed by `cmd::dispatch::strip_skim_flags` before exec.  Callers on the
    /// JSON path derive this from `cmd::dispatch::passthrough_strips_json`.
    pub(crate) passthrough_reproduces_argv: bool,
}

/// Resolve the narrowest escape-hatch remedy that is **actually true** for the
/// current invocation.
///
/// # The narrow arm — `(Json, false)`
///
/// `strip_skim_flags` only removes bare `--json` for `git`; for every other tool
/// `--json` is a *tool-owned* form (`gh pr list --json title`) that must survive
/// the strip.  So `SKIM_PASSTHROUGH=1 skim psql --json` forwards `--json` to the
/// real `psql`, which rejects it — the legacy hint would be a false remedy.  On
/// that path the only true remedy is running the tool directly.
///
/// # The default arm
///
/// Everything else returns the legacy `"SKIM_PASSTHROUGH=1 for full output"`
/// literal, which keeps the pinned marker assertions across the suite green.
pub(crate) fn remedy_for(ctx: &RemedyCtx<'_>) -> Cow<'static, str> {
    match (ctx.output_format, ctx.passthrough_reproduces_argv) {
        // ADR-011 class-1: the remedy must be literally reachable from the
        // invocation that prints it.  `--json` survives the strip for this tool,
        // so the passthrough exec would fail — name the only true remedy.
        (OutputFormat::Json, false) => {
            Cow::Owned(format!("run '{}' directly for the full output", ctx.tool))
        }
        _ => Cow::Borrowed("SKIM_PASSTHROUGH=1 for full output"),
    }
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
///
/// # Charge-nothing shim
///
/// This entry point prices the stdout bodies and nothing else. Callers that
/// know the *stderr* disclosure the `Keep` branch will print must use
/// [`decide_with_notice`] so the guard can charge it (ADR-001 amendment
/// 2026-09-24). Keeping `decide` as a shim means a call site that has no
/// notice — or has not been audited for one yet — cannot accidentally charge
/// the wrong thing by omission.
pub(crate) fn decide(raw: &str, compressed: &str) -> FidelityDecision {
    decide_with_notice(raw, compressed, None)
}

/// [`decide`], with the stderr disclosure that the `Keep` branch would emit
/// charged against the compressed side.
///
/// # Why the guard must see the notice (ADR-001 amendment 2026-09-24)
///
/// The guard decides `Keep` vs `Passthrough` by comparing **stdout** sizes, and
/// has never seen the ADR-008 / ADR-011 class-1 marker the same invocation is
/// about to print to stderr. Agent harnesses capture stderr into the same
/// context window as stdout, so a 60-byte stdout saving bought with a 124-byte
/// disclosure is a net loss the guard currently scores as a win.
///
/// # `notice` is the DIFFERENTIAL cost, not the absolute cost
///
/// The caller passes `Some(marker)` only when emitting that marker is a
/// *consequence of choosing `Keep`*:
///
/// ```text
/// overhead = cost(notice if Keep) - cost(notice if Passthrough)
/// ```
///
/// On a path where the `Passthrough` branch prints a byte-identical marker of
/// its own, the difference is zero and the caller passes `None`. Charging the
/// absolute cost there would double-count — the defect tracked as #519 — and
/// would push the guard toward raw on exactly the paths where raw is *also*
/// disclosed. Making the caller supply a differential keeps #519's resolution a
/// property of the rule rather than a special case bolted onto it.
///
/// # Trim symmetry
///
/// `raw` and `compressed` are trimmed before comparison, so the cost charged
/// for the notice is likewise its trimmed length: the `String` built by
/// [`crate::output::lossy_view_marker`], which carries no trailing newline.
/// `process.rs` emits it with `eprintln!`, so the wire cost is one byte and one
/// cl100k token higher — the same single terminator the trimmed stdout
/// comparison already normalises away on both sides.
///
/// # Laziness
///
/// The notice is tokenised **only** on the token slow path, and only once the
/// tokeniser has proved available for the bodies. The byte early-exit and both
/// cap fallbacks return without ever tokenising it: when byte arithmetic alone
/// settles the verdict, the token cost of the notice is not computed.
///
/// A const lookup table of per-mode token costs is deliberately **not** used: a
/// stale entry would be a silently wrong guard verdict with no failing test and
/// no visible diff (PF-027 genus). The live count cannot go stale.
pub(crate) fn decide_with_notice(
    raw: &str,
    compressed: &str,
    notice: Option<&str>,
) -> FidelityDecision {
    /// 256 KiB — above this threshold skip tokenisation (performance cap).
    const TOKEN_SIZE_CAP: usize = 256 * 1024;
    /// 4 KiB — longest non-whitespace run above which skip tokenisation.
    const TOKEN_RUN_CAP: usize = 4 * 1024;
    // Compile-time invariant: size cap must be strictly greater than run cap.
    const { assert!(TOKEN_SIZE_CAP > TOKEN_RUN_CAP) };

    let raw_t = raw.trim();
    let comp_t = compressed.trim();

    // The disclosure is part of what `Keep` costs the reader, so it is charged
    // to the compressed side at EVERY exit — bytes here, tokens below. An exit
    // that priced only the body would make the verdict depend on which exit
    // happened to be taken rather than on what the reader actually receives.
    let notice_bytes = notice.map_or(0, str::len);

    // Byte early-exit: not strictly shorter → Passthrough (conservative rule).
    // Covers empty-raw case (0 < 0 fails → Passthrough) and ties (n == n fails).
    if comp_t.len().saturating_add(notice_bytes) >= raw_t.len() {
        return FidelityDecision::Passthrough;
    }

    // comp_t.len() + notice < raw_t.len() — bytes say compressed is strictly
    // smaller even after paying for its own disclosure.
    let over_size_cap = raw.len() > TOKEN_SIZE_CAP || compressed.len() > TOKEN_SIZE_CAP;

    let over_run_cap = !over_size_cap
        && (longest_nonwhitespace_run(raw) > TOKEN_RUN_CAP
            || longest_nonwhitespace_run(compressed) > TOKEN_RUN_CAP);

    if over_size_cap || over_run_cap {
        // Byte path: the notice-charged byte comparison above already decided.
        return FidelityDecision::Keep;
    }

    // Token slow path: confirm the byte saving is also a token saving.
    match crate::process::count_token_pair(raw_t, comp_t) {
        (Some(raw_tok), Some(comp_tok)) => {
            // Lazy: the notice is tokenised here and nowhere else, so the
            // byte-decided exits above never pay for it.
            let notice_tok = match notice {
                None => Some(0),
                Some(text) => crate::tokens::count_tokens(text).ok(),
            };
            match notice_tok {
                Some(cost) if comp_tok.saturating_add(cost) < raw_tok => FidelityDecision::Keep,
                Some(_) => {
                    // Token tie, token-expansion, or a saving the disclosure
                    // swallows, even though bytes were shorter → Passthrough.
                    FidelityDecision::Passthrough
                }
                // The bodies tokenised but the notice did not. Fall back to the
                // byte verdict, which was computed WITH the notice charged and
                // said Keep — the same rule the `(None, None)` arm below uses.
                None => FidelityDecision::Keep,
            }
        }
        // Tokeniser unavailable: the notice-charged byte comparison decides.
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
    // Notice charging (ADR-001 amendment 2026-09-24)
    // -----------------------------------------------------------------------

    /// `decide` must remain a CHARGE-NOTHING shim. The A4 regression tests in
    /// `guardrail.rs` and the ~30 `savings_decision` tests in `execution.rs`
    /// reach the gate through it, and they pin body-only arithmetic.
    ///
    /// The same inputs flip once the notice is supplied, which is what makes
    /// this a statement about `decide` rather than about the inputs.
    #[test]
    fn decide_is_charge_free_and_decide_with_notice_is_not() {
        // Compressed is far shorter than raw in BOTH bytes and tokens.
        let raw = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let compressed = "summary";
        assert_eq!(
            decide(raw, compressed),
            FidelityDecision::Keep,
            "decide() must price the bodies and nothing else"
        );
        assert_eq!(
            decide_with_notice(raw, compressed, None),
            decide(raw, compressed),
            "decide() must be exactly decide_with_notice(.., None)"
        );

        // A disclosure wider than the saving turns the same win into a loss.
        let notice = "x".repeat(raw.len());
        assert_eq!(
            decide_with_notice(raw, compressed, Some(&notice)),
            FidelityDecision::Passthrough,
            "a saving the disclosure swallows must not be kept"
        );
    }

    /// The BYTE early exit charges the notice — the exit that never tokenises.
    ///
    /// Raw is above `TOKEN_SIZE_CAP`, so if the byte exit did not charge, the
    /// cap fallback would return `Keep` without any token arithmetic ever
    /// running. The verdict therefore isolates the byte charge at exit 1.
    #[test]
    fn notice_charged_at_byte_early_exit_above_size_cap() {
        let raw = "x".repeat(512 * 1024);
        let compressed = "x".repeat(512 * 1024 - 50); // 50-byte body saving
        assert_eq!(
            decide(&raw, &compressed),
            FidelityDecision::Keep,
            "uncharged: a 50-byte saving above the size cap is kept"
        );
        let notice = "n".repeat(100); // disclosure costs twice the saving
        assert_eq!(
            decide_with_notice(&raw, &compressed, Some(&notice)),
            FidelityDecision::Passthrough,
            "the byte early exit must price the notice, not just the body"
        );
    }

    /// The CAP-FALLBACK exit prices the same thing as the byte exit.
    ///
    /// Both inputs exceed `TOKEN_RUN_CAP` (4 KiB of unbroken non-whitespace),
    /// so the token slow path is skipped and the verdict rests entirely on the
    /// notice-charged byte comparison. A notice inside the saving keeps; a
    /// notice wider than the saving falls through to raw.
    #[test]
    fn notice_charged_on_run_cap_fallback_exit() {
        let raw = "y".repeat(5000); // one 5000-byte run > TOKEN_RUN_CAP
        let compressed = "y".repeat(4000); // 1000-byte body saving
        let inside = "z".repeat(500);
        assert_eq!(
            decide_with_notice(&raw, &compressed, Some(&inside)),
            FidelityDecision::Keep,
            "a disclosure the saving covers is still a net win"
        );
        let wider = "z".repeat(1200);
        assert_eq!(
            decide_with_notice(&raw, &compressed, Some(&wider)),
            FidelityDecision::Passthrough,
            "the cap fallback must price the notice too, or the verdict \
             depends on which exit was taken"
        );
    }

    /// The TOKEN slow path charges the notice independently of the byte gate.
    ///
    /// Sizes are measured against cl100k, not assumed:
    ///
    /// | string                 | bytes | tokens |
    /// |------------------------|-------|--------|
    /// | `"a" * 3000` (raw)     |  3000 |    375 |
    /// | `"a" * 100` (compressed)|  100 |     13 |
    /// | 600 space-separated letters (notice) | 1199 | 600 |
    ///
    /// Bytes: 100 + 1199 = 1299 < 3000 — the byte gate PASSES with a 2.3x
    /// margin, so this verdict can only come from the token arithmetic.
    /// Tokens: 13 + 600 = 613 >= 375 — the disclosure costs 1.6x the whole raw
    /// token count. Both margins are wide enough to survive a tokeniser
    /// patch bump.
    #[test]
    fn notice_charged_on_token_slow_path() {
        let raw = "a".repeat(3000);
        let compressed = "a".repeat(100);
        assert_eq!(
            decide(&raw, &compressed),
            FidelityDecision::Keep,
            "uncharged: shorter in both bytes and tokens"
        );

        // Token-dense, byte-cheap: 600 single letters, ~1 token per 2 bytes.
        let notice: String = "abcdefghijklmnopqrstuvwxyz"
            .chars()
            .cycle()
            .take(600)
            .map(String::from)
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(notice.len(), 1199, "notice byte size moved");
        assert!(
            compressed.len() + notice.len() < raw.len(),
            "precondition: the byte gate must PASS so the token gate decides"
        );
        assert_eq!(
            decide_with_notice(&raw, &compressed, Some(&notice)),
            FidelityDecision::Passthrough,
            "a byte saving whose disclosure costs more TOKENS than it saves \
             must not be kept"
        );
    }

    /// An empty notice is indistinguishable from no notice at either exit.
    #[test]
    fn empty_notice_costs_nothing() {
        let raw = "alpha beta gamma delta epsilon zeta eta theta";
        let compressed = "summary";
        assert_eq!(
            decide_with_notice(raw, compressed, Some("")),
            decide(raw, compressed),
            "an empty disclosure must not move the verdict"
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
    // D1: Completeness / view_differs / remedy_for (ADR-015)
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
        let ctx = RemedyCtx {
            tool: "git",
            output_format: OutputFormat::Text,
            passthrough_reproduces_argv: true,
        };
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
        let ctx = RemedyCtx {
            tool: "npm",
            output_format: OutputFormat::Text,
            passthrough_reproduces_argv: false,
        };
        assert_eq!(
            remedy_for(&ctx),
            "SKIM_PASSTHROUGH=1 for full output",
            "default remedy must match legacy literal to preserve pinned test assertions"
        );
    }

    /// `git --json` keeps the legacy hint: `strip_skim_flags` removes bare
    /// `--json` for git, so `SKIM_PASSTHROUGH=1 skim git log --json` really does
    /// re-exec `git log` with an argv git accepts.
    #[test]
    fn remedy_for_git_json_keeps_legacy_hint() {
        let ctx = RemedyCtx {
            tool: "git",
            output_format: OutputFormat::Json,
            passthrough_reproduces_argv: true,
        };
        assert_eq!(
            remedy_for(&ctx),
            "SKIM_PASSTHROUGH=1 for full output",
            "git strips --json before the passthrough exec, so the legacy hint is true"
        );
    }

    /// `psql --json` takes the narrow arm: `--json` is NOT stripped for psql, so
    /// the passthrough exec would hand `--json` to the real psql and fail.  The
    /// only true remedy is running the tool directly.
    #[test]
    fn remedy_for_psql_json_narrows_to_direct_run() {
        let ctx = RemedyCtx {
            tool: "psql",
            output_format: OutputFormat::Json,
            passthrough_reproduces_argv: false,
        };
        let remedy = remedy_for(&ctx);
        assert_eq!(
            remedy, "run 'psql' directly for the full output",
            "the (Json, false) arm must name the tool, not the unreachable hatch"
        );
        assert!(
            !remedy.contains("SKIM_PASSTHROUGH=1"),
            "the narrow arm must NOT print a remedy that cannot work; got: {remedy:?}"
        );
    }
}
