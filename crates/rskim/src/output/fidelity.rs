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
    /// The format the reader asked for.  It is what a JSON caller derives
    /// `passthrough_reproduces_argv` FROM (via
    /// `cmd::dispatch::passthrough_strips_json`); it is not itself a reason the
    /// hint can be false, because a text caller can be just as unable to reach
    /// the hatch — see that field.
    pub(crate) output_format: OutputFormat,
    /// `true` when `SKIM_PASSTHROUGH=1 skim <tool> <argv>` really does hand the
    /// reader the full output.  Two independent ways for it to be `false`, and a
    /// caller must consider both:
    ///
    /// 1. **The argv does not survive the strip.**  A skim-only flag that
    ///    `cmd::dispatch::strip_skim_flags` leaves in place reaches the real tool
    ///    and is rejected — bare `--json` for every tool but `git`.  Callers on
    ///    the JSON path derive this from `cmd::dispatch::passthrough_strips_json`.
    /// 2. **The gate never fires.**  `cmd/dispatch.rs`'s convergence gate
    ///    declines whenever `handler_reads_stdin` is true, which off a TTY is
    ///    every multi-level dispatcher (`cargo`, `dotnet`, `go`, `swift`), and
    ///    `cmd/build/mod.rs::run_parsed_command` has no passthrough branch of its
    ///    own — it always spawns and always summarises.  So
    ///    `SKIM_PASSTHROUGH=1 skim cargo check` returns the compressed summary in
    ///    exactly the agent harnesses and CI jobs that read the marker (PF-039,
    ///    Active).  Text output, unreachable hatch: that combination is why this
    ///    field, not `output_format`, is what [`remedy_for`] branches on.
    pub(crate) passthrough_reproduces_argv: bool,
}

/// Resolve the narrowest escape-hatch remedy that is **actually true** for the
/// current invocation.
///
/// # The narrow arm — `(_, false)`
///
/// Keyed on reachability alone, deliberately **not** on the format.  It was
/// `(Json, false)` until consistency-01, which made `false` a no-op for every
/// text caller: the build family's class-1 marker
/// ([`crate::output::diagnostics_summary_marker`]) is text, cannot reach the
/// hatch at all (PF-039), and still printed `SKIM_PASSTHROUGH=1` — an ADR-011
/// class-1 marker advertising a remedy the invocation printing it cannot use.
/// The JSON reason for unreachability has not gone anywhere: `strip_skim_flags`
/// only removes bare `--json` for `git`, so `SKIM_PASSTHROUGH=1 skim psql --json`
/// still forwards `--json` to the real `psql`, which rejects it.  Both reasons
/// now converge on the one remedy that is literally true — run the tool itself.
///
/// # The default arm
///
/// A reachable hatch returns the legacy `"SKIM_PASSTHROUGH=1 for full output"`
/// literal, which keeps the pinned marker assertions across the suite green.
/// Widening the narrow arm moved no existing caller into it: the two
/// `crate::output` file-read call sites pass `true`, and
/// `cmd::execution::emit_json_envelope` passes `Json` with a bool it derives per
/// tool, so its `(Json, false)` and `(Json, true)` results are byte-identical to
/// before.
pub(crate) fn remedy_for(ctx: &RemedyCtx<'_>) -> Cow<'static, str> {
    match (ctx.output_format, ctx.passthrough_reproduces_argv) {
        // ADR-011 class-1: the remedy must be literally reachable from the
        // invocation that prints it.  The hatch does not work here — either the
        // argv would not survive the strip, or the gate never fires for this
        // handler — so name the only true remedy.
        (_, false) => Cow::Owned(format!("run '{}' directly for the full output", ctx.tool)),
        _ => Cow::Borrowed("SKIM_PASSTHROUGH=1 for full output"),
    }
}

/// The ADR-011 class-1 elision marker for skim's buffered-pipe memory cap.
///
/// # What it discloses
///
/// [`crate::runner::CommandRunner::run_with_env`] accumulates a child's whole
/// stdout in memory, so [`crate::runner::MAX_OUTPUT_BYTES`] is a memory-safety
/// bound and stays. What changed is its *consequence*: the cap used to throw the
/// entire accumulated buffer away and return an error, so a 70 MiB `yarn build`
/// log reached the reader as `Error: output exceeded 67108864 byte limit`,
/// exit 1, and **zero bytes** — strictly less than the raw tool produced, with
/// nothing disclosed. That is the inverse of #317's compress-never-truncate
/// MUST, whose one carve-out is an unavoidable safety bound that says so with
/// exact counts. The reader now keeps every byte that fit; this line names the
/// bound and the byte count it stopped at.
///
/// # Why the narrow remedy, on every call
///
/// `SKIM_PASSTHROUGH=1` is a measured no-op for the build family off a TTY
/// (PF-039), and the buffered runner cannot tell a `cargo` from a `git`: it
/// holds a program name and nothing else. ADR-011 forbids a class-1 marker from
/// advertising a hatch the invocation printing it cannot use, so this asks
/// [`remedy_for`] for the arm that is true for *every* program — run the tool
/// itself. `OutputFormat::Text` is passed because the `(_, false)` arm ignores
/// the format; it is not a claim about the caller's output format.
///
/// # Emission contract
///
/// Callers write the returned line with a bare `eprintln!`, never
/// `crate::debug_log!`. It fires only when the reader was shown less than raw,
/// which makes it ADR-011 class 1 — unconditional, and never silenceable by the
/// absence of `SKIM_DEBUG`.
pub(crate) fn output_cap_marker(program: &str, kept_bytes: usize, cap_bytes: usize) -> String {
    let remedy = remedy_for(&RemedyCtx {
        tool: program,
        output_format: OutputFormat::Text,
        passthrough_reproduces_argv: false,
    });
    super::elision_marker_unbounded_with_remedy(
        &format!(
            "the first {kept_bytes} bytes of '{program}' stdout \
             (skim's {cap_bytes}-byte memory cap)"
        ),
        "output",
        &remedy,
    )
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
/// context window as stdout, so a 60-byte stdout saving bought with a 162-byte
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
/// The line that actually reaches stderr carries its own terminator
/// ([`crate::output::EmittedNotice::line`], which the emitters write with
/// `eprint!`), so the wire cost is one byte and one cl100k token higher — the
/// same single terminator the trimmed stdout comparison already normalises away
/// on both sides.
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
    match crate::tokens::count_token_pair(raw_t, comp_t) {
        (Some(raw_tok), Some(comp_tok)) => {
            // Lazy: the notice is tokenised here and nowhere else, so the
            // byte-decided exits above never pay for it.
            let notice_tok = match notice {
                None => Some(0),
                Some(text) => crate::tokens::count_tokens(text).ok(),
            };
            verdict_from_token_costs(raw_tok, comp_tok, notice_tok)
        }
        // Tokeniser unavailable: the notice-charged byte comparison decides.
        _ => FidelityDecision::Keep,
    }
}

/// The token-space verdict, once both stdout bodies have been counted.
///
/// Split out of [`decide_with_notice`] so every combination of counts is
/// reachable from a test — in particular `notice_tok == None`, which no input
/// can produce today (see below) and which was therefore the one path to `Keep`
/// after the token gate had been consulted that nothing pinned.
///
/// # `None` → `Keep` is deliberate, not a fall-through
///
/// [`decide_with_notice`]'s byte early-exit has already fired by the time this
/// runs, and it charged `notice.len()` to the compressed side — so bytes said
/// "strictly smaller **even after** paying for its own disclosure". With no
/// token cost for the notice in hand, that byte verdict is the only measurement
/// there is, and it is the same rule the `(None, None)` body arm applies one
/// line below the call. The alternative — `Passthrough`, matching the token-tie
/// arm — would let a *measurement failure* on the disclosure overturn a verdict
/// the disclosure was already charged against, which is not the rule any other
/// exit of this function follows.
///
/// # Currently unreachable, deliberately kept
///
/// `crate::tokens::count_tokens` is `Ok(get_counter().count(text))` — infallible
/// for every input — so `notice_tok` is never `None` in production, and
/// `count_token_pair`'s `(None, None)` arm is unreachable for the same reason.
/// Both arms stay because that signature is frozen as `Result` (`tokens.rs`
/// AC15) and `Counter` has a heuristic fallback path: if counting ever becomes
/// fallible, the rule must already be written down and pinned rather than
/// inferred from whichever arm happens to be taken.
fn verdict_from_token_costs(
    raw_tok: usize,
    comp_tok: usize,
    notice_tok: Option<usize>,
) -> FidelityDecision {
    match notice_tok {
        Some(cost) if comp_tok.saturating_add(cost) < raw_tok => FidelityDecision::Keep,
        // Token tie, token-expansion, or a saving the disclosure swallows, even
        // though bytes were shorter → Passthrough.
        Some(_) => FidelityDecision::Passthrough,
        None => FidelityDecision::Keep,
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

    /// `remedy_for`'s default branch — a REACHABLE hatch, whatever the format —
    /// returns the exact legacy literal so pinned assertions across the suite
    /// stay green.
    ///
    /// Held at `passthrough_reproduces_argv: true`.  Before consistency-01 this
    /// case was spelled `(Text, false)` and still took the default arm, which is
    /// precisely the no-op the widening removed.
    #[test]
    fn remedy_for_default_is_legacy_literal() {
        let ctx = RemedyCtx {
            tool: "npm",
            output_format: OutputFormat::Text,
            passthrough_reproduces_argv: true,
        };
        assert_eq!(
            remedy_for(&ctx),
            "SKIM_PASSTHROUGH=1 for full output",
            "default remedy must match legacy literal to preserve pinned test assertions"
        );
    }

    /// consistency-01: a TEXT caller that cannot reach the hatch takes the narrow
    /// arm.  RED before the widening — `(Text, false)` returned the legacy hint,
    /// which is the build family's case (PF-039: `SKIM_PASSTHROUGH=1 skim cargo
    /// check` serves the compressed summary off a TTY) and made the class-1
    /// marker advertise a remedy the invocation printing it cannot use.
    #[test]
    fn remedy_for_text_unreachable_hatch_narrows_to_direct_run() {
        let ctx = RemedyCtx {
            tool: "cargo",
            output_format: OutputFormat::Text,
            passthrough_reproduces_argv: false,
        };
        let remedy = remedy_for(&ctx);
        assert_eq!(
            remedy, "run 'cargo' directly for the full output",
            "a text caller with no reachable hatch must name the tool"
        );
        assert!(
            !remedy.contains("SKIM_PASSTHROUGH=1"),
            "the narrow arm must NOT print a remedy that cannot work; got: {remedy:?}"
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

    // -----------------------------------------------------------------------
    // reliability-02: the 64 MiB pipe-cap disclosure
    // -----------------------------------------------------------------------

    /// The whole class-1 contract in one string: the exact kept count, the exact
    /// bound, the program, and a remedy that program can actually be given.
    #[test]
    fn output_cap_marker_carries_exact_counts_and_a_reachable_remedy() {
        let marker = output_cap_marker("yarn", 67_100_672, 67_108_864);

        assert_eq!(
            marker,
            "[skim] output elided beyond the first 67100672 bytes of 'yarn' stdout \
             (skim's 67108864-byte memory cap) — run 'yarn' directly for the full output"
        );
    }

    /// The hatch is a measured no-op for the build family off a TTY (PF-039),
    /// and the buffered runner holds a program name and nothing else — so this
    /// marker must never advertise it.
    #[test]
    fn output_cap_marker_never_advertises_the_passthrough_hatch() {
        let marker = output_cap_marker("cargo", 1, 2);
        assert!(
            !marker.contains("SKIM_PASSTHROUGH"),
            "a cap marker cannot promise a hatch it cannot verify; got: {marker:?}"
        );
        assert!(
            marker.contains("run 'cargo' directly"),
            "the remedy must name the tool; got: {marker:?}"
        );
    }

    /// A marker that rounds its counts sends the reader back for bytes they
    /// already have, or hides bytes they do not — ADR-011 requires the exact
    /// numbers, so a distinctive non-round pair must survive verbatim.
    #[test]
    fn output_cap_marker_does_not_round_its_counts() {
        let marker = output_cap_marker("git", 12_345, 67_890);
        assert!(marker.contains("12345"), "kept count verbatim: {marker:?}");
        assert!(marker.contains("67890"), "cap verbatim: {marker:?}");
    }

    // -----------------------------------------------------------------------
    // regression-08: the token-space verdict, including the one arm no input
    // can drive through `decide_with_notice`
    // -----------------------------------------------------------------------

    /// `(bodies counted, notice NOT counted)` → `Keep`, deliberately.
    ///
    /// This is the only path that reaches `Keep` after the token gate was
    /// consulted and could not answer, and nothing pinned it. It is trusted
    /// because the byte early-exit that ran before it charged `notice.len()` to
    /// the compressed side and still found it strictly shorter — so the byte
    /// verdict IS a notice-charged verdict, and it is the only measurement left.
    /// `Passthrough` (the token-tie answer one arm up) would let a *measurement
    /// failure on the disclosure* overturn a verdict the disclosure had already
    /// been charged against.
    ///
    /// Pinned against `verdict_from_token_costs` rather than through
    /// `decide_with_notice`, because no input can produce this state today:
    /// `tokens::count_tokens` is `Ok(counter.count(text))` for every input.
    #[test]
    fn unmeasurable_notice_keeps_the_notice_charged_byte_verdict() {
        assert_eq!(
            verdict_from_token_costs(100, 40, None),
            FidelityDecision::Keep,
            "an unmeasurable notice must not overturn the notice-charged byte verdict"
        );
    }

    /// A notice whose token cost swallows the token saving → `Passthrough`, the
    /// same conservative rule the tie gets.
    #[test]
    fn notice_that_swallows_the_token_saving_passes_through() {
        assert_eq!(
            verdict_from_token_costs(100, 90, Some(10)),
            FidelityDecision::Passthrough,
            "90 + 10 == 100 is a tie, not a saving — strictly-smaller is the rule"
        );
        assert_eq!(
            verdict_from_token_costs(100, 90, Some(11)),
            FidelityDecision::Passthrough,
            "one token past the tie is a net expansion"
        );
    }

    /// A notice the token saving can pay for → `Keep`.
    #[test]
    fn notice_within_the_token_saving_keeps() {
        assert_eq!(
            verdict_from_token_costs(100, 90, Some(9)),
            FidelityDecision::Keep,
            "90 + 9 < 100: the saving survives its own disclosure"
        );
        assert_eq!(
            verdict_from_token_costs(100, 99, Some(0)),
            FidelityDecision::Keep,
            "a caller with no notice is charged Some(0), never None"
        );
    }

    /// The `saturating_add` is load-bearing: a pathological notice cost must not
    /// wrap around into a false `Keep` (and must not panic in debug builds).
    #[test]
    fn notice_cost_saturates_instead_of_wrapping() {
        assert_eq!(
            verdict_from_token_costs(100, usize::MAX, Some(1)),
            FidelityDecision::Passthrough,
            "usize::MAX + 1 must saturate, not wrap to 0 and read as a saving"
        );
    }
}
