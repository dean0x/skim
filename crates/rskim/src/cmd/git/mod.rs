//! Git output compression subcommand (#50, #103)
//!
//! Executes git commands and compresses output for LLM context windows.
//! Supports `status`, `diff`, and `log` subcommands with flag-aware
//! passthrough: when the user already specifies a compact format flag,
//! output is passed through unmodified.
//!
//! That flag-awareness is one shared gate, not a per-subcommand habit — see
//! [`MACHINE_CONTRACT_FLAGS`], which is evaluated in [`run`] ahead of dispatch
//! and therefore ahead of every ADR-001 net-savings verdict (ADR-022).
//!
//! The `diff` subcommand uses an AST-aware pipeline (#103): it parses
//! unified diff output, overlays changed line ranges on tree-sitter ASTs,
//! and renders changed nodes with full function boundaries and standard
//! `+`/`-` markers.

// Private: only accessed via run() dispatch in this module
mod commit;
mod diff;
mod fetch;
mod log;
mod push;
pub(super) mod shared;
mod show;
mod status;

use std::process::ExitCode;

use crate::cmd::execution as exec;
use crate::cmd::{OutputFormat, user_has_flag};
use crate::output::canonical::GitResult;
use crate::output::fidelity::Completeness;
use crate::runner::CommandRunner;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `git` subcommand.
///
/// Dispatches to `status`, `diff`, `log`, `show`, etc., or prints help.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // Handle --help / -h at the `skim git` level: only when the first
    // non-global-flag token is the help flag (e.g., `skim git --help`),
    // not when it appears deeper inside a subcommand (`skim git show --help`).
    if args.is_empty()
        || args
            .first()
            .is_some_and(|a| matches!(a.as_str(), "--help" | "-h"))
    {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);

    let (global_flags, rest) = split_global_flags(&filtered_args);

    let Some(subcmd) = rest.first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let subcmd_args = &rest[1..];
    let rec = crate::analytics::RecordingContext {
        enabled: analytics.enabled,
        command_type: crate::analytics::CommandType::Git,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };

    match subcmd.as_str() {
        // ADR-022 — the machine-contract passthrough gate.  It sits ahead of
        // every handler, and therefore ahead of every ADR-001 net-savings
        // verdict any of them applies.
        //
        // Scoped to the subcommands that have a compressing handler: the
        // `other` arm below already forwards unknown subcommands raw, and it
        // does so through `run_raw_passthrough`, which STREAMS.  Routing them
        // here instead would trade streaming for buffering and give up the
        // PF-021 early-close guarantees for no gain.  Listing the names in this
        // arm rather than in a separate const keeps the gate's domain and the
        // dispatch table the same list, so the two cannot drift.
        //
        // `--json` DISARMS it — see [`caller_requested_json`].  The gate is
        // keyed on what the caller typed, and `--json` is also something the
        // caller typed; what it names is skim's own machine contract rather
        // than git's, so honouring it is not the false negative the asymmetry
        // warns about.
        "status" | "diff" | "fetch" | "log" | "show" | "commit" | "push"
            if has_machine_contract_flag(subcmd_args) && !caller_requested_json(subcmd_args) =>
        {
            run_passthrough(&global_flags, subcmd.as_str(), subcmd_args, show_stats, rec)
        }
        "status" => status::run_status(&global_flags, subcmd_args, show_stats, rec),
        "diff" => diff::run_diff(&global_flags, subcmd_args, show_stats, rec),
        "fetch" => fetch::run_fetch(&global_flags, subcmd_args, show_stats, rec),
        "log" => log::run_log(&global_flags, subcmd_args, show_stats, rec),
        "show" => show::run_show(&global_flags, subcmd_args, show_stats, rec),
        "commit" => commit::run_commit(&global_flags, subcmd_args, show_stats, rec),
        "push" => push::run_push(&global_flags, subcmd_args, show_stats, rec),
        other => {
            // D2: unknown git subcommands (branch, checkout, rev-parse, stash, …) are
            // forwarded to the real git binary unchanged. skim only compresses the
            // subcommands it understands; everything else passes through byte-faithfully.
            // The banner is debug-gated per ADR-011 (no-loss path — the reader sees
            // exactly what git would produce).
            let safe = crate::cmd::sanitize_for_display(other);
            crate::debug_log!("skim git: unknown subcommand '{safe}' — passing through to git");
            // Reconstruct full args: subcommand + remaining subcmd_args, then
            // re-prepend any global git flags so they reach the real binary.
            let mut all_args: Vec<String> = global_flags.to_vec();
            all_args.push(other.to_string());
            all_args.extend_from_slice(subcmd_args);
            super::run_raw_passthrough("git", &all_args, &[])
        }
    }
}

// ============================================================================
// Machine-contract passthrough gate (ADR-022)
// ============================================================================

/// Flags whose presence makes the invocation's output a **machine contract**:
/// git's own bytes are served raw, unconditionally, ahead of and independent of
/// the ADR-001 net-savings verdict.
///
/// # Why a flag list and not a measurement
///
/// "This output format is a machine contract" is a question about the caller's
/// *intent*, and the ADR-001 byte comparison structurally cannot answer it.
/// Before this gate existed, every git invocation that came through
/// byte-identical did so by coincidence of two blunt mechanisms: the
/// net-savings size guard, which protects a format only while compression
/// happens to lose and therefore reverses when the repository state changes,
/// and "non-zero exit ⇒ forward raw", which protects only failing invocations.
/// Measured at `c2b4378`: `skim git log --stat -n 3` served **374 B** against
/// **32 733 B** of raw git with **zero** bytes on stderr, and all five of
/// `--stat` / `--shortstat` / `--numstat` / `--name-only` / `--name-status`
/// produced the *same* 374 B, i.e. the flag was swallowed without a trace.
///
/// # The error modes are asymmetric, so over-inclusion is the safe direction
///
/// A **false positive** costs nothing that matters.  The reader is served the
/// raw git bytes, which is byte-faithful by construction, and it is also
/// *guard-neutral*: [`run_passthrough`] records `parse_tier == "passthrough"`,
/// which skips the ADR-001 guard entirely rather than being graded by it.  The
/// only cost is the compression skim would otherwise have applied.
///
/// A **false negative** is the whole defect class.  A NUL-delimited,
/// tab-framed, or exit-code-bearing stream gets reshaped into prose — silently,
/// at exit 0, with no ADR-011 class-1 marker — and whatever parses it
/// downstream reads either garbage or, worse, a plausible wrong answer.
///
/// So when a flag's status is arguable, it belongs in this list.  Two
/// deliberate consequences of that asymmetry:
///
/// - The gate is **not** separator-aware — it does not route through
///   `args_before_separator` — so a pathspec literally named `--stat` after a
///   bare `--` also serves raw.  Separator-awareness could only turn a match
///   into a miss, i.e. only manufacture false negatives.
/// - Short-cluster matching (see [`CONTRACT_SHORT_OPTS`]) scans every character
///   of a single-dash token, so `git log -Szebra` — the pickaxe, whose value
///   merely happens to contain a `z` — also serves raw.
///
/// # Adding a flag
///
/// One entry here, and it applies to every git subcommand skim compresses.
/// That is the point of hoisting: the two per-command spellings this replaced
/// (`git diff`'s stat family and `git log`'s `--format`/`--pretty`) meant the
/// same flags were a contract for one subcommand and swallowed by its sibling.
/// `cmd/git/show.rs` keeps its own `PASSTHROUGH_FLAGS` list, which this gate
/// now reaches first.  This set is a strict superset of it, so `git show` is
/// unaffected — `--raw` was the last entry show carried alone, and it is
/// hoisted here rather than given a third spelling there.
const MACHINE_CONTRACT_FLAGS: &[&str] = &[
    // Record formats git documents as stable for scripts.  `user_has_flag`'s
    // `=` rule makes `--porcelain` cover `--porcelain=v1` and `=v2` too.
    "--porcelain",
    // Long form of `-z`; `cmd/git/status.rs` already classes it with `-z`.
    "--null",
    // Fixed-column and tab-framed summaries.  A renderer that reflows these
    // columns changes the field boundaries a caller splits on.
    "--stat",
    "--shortstat",
    "--numstat",
    "--name-only",
    "--name-status",
    // git's diff *record* format, `:100644 100644 <pre> <post> M\tpath` —
    // colon-prefixed, tab-delimited, fixed field order, a format that exists to
    // be split on.  `cmd/git/show.rs`'s `PASSTHROUGH_FLAGS` has always carried
    // it, so `--raw` was a contract on one subcommand and swallowed by its
    // siblings: measured at `c2b4378`, `git log --raw -n 3` served the same
    // 324 B as `--stat` against 1 695 B of raw git, while `git show --raw HEAD`
    // was already byte-exact at 564 B.
    "--raw",
    "--check",
    // Exit-code and silence contracts: the answer is the status, not the text,
    // so any byte skim adds is output the caller explicitly asked not to get.
    // `-q` is git's documented short form of `--quiet` on push/fetch/commit.
    "--quiet",
    "-q",
    "--exit-code",
    // Topology rendering: the `*`, `|` and `\` rails ARE the payload.  Measured
    // at `c2b4378`, dropping this flag was worse than lossy — `git log --graph`
    // over a 3-commit range served `log no commits` (15 B), because `--graph`
    // prefixes each commit with `* `, which the `%h`-shaped `is_commit_line`
    // filter rejects.  An absence read as evidence (the PF-021 shape).
    "--graph",
    // Caller-supplied format strings: skim cannot know what they encode, so it
    // cannot know what reshaping them destroys.  Hoisted from `log.rs`.
    "--format",
    "--pretty",
];

/// Short option characters that make the output a machine contract when they
/// appear **anywhere inside** a single-dash cluster.
///
/// Cluster-aware on purpose.  `git status -sz` is a real invocation (measured:
/// raw 13 B) whose `-z` an exact-token match would miss, and
/// `cmd/git/status.rs`'s `CONFLICTING_SHORT_OPTS` scan is already cluster-aware
/// — so an exact-token gate here would be *weaker* than what already ships,
/// i.e. a regression introduced by the fix rather than a pre-existing gap.
///
/// `z` is the only member.  The set's other single-dash flag, `-q`, is matched
/// as an exact token in [`MACHINE_CONTRACT_FLAGS`] instead, because git users
/// bundle `-z` (`git status -sz`) and do not bundle `-q`; listing `q` here as
/// well would only widen the false-positive surface with no case to serve.
///
/// `-s` (`--short`) is deliberately **absent** — short format is a human-facing
/// rendering that `cmd/git/status.rs` translates faithfully, and adding it here
/// would turn `skim git status -sb` into raw passthrough.
const CONTRACT_SHORT_OPTS: &[char] = &['z'];

/// Whether `args` carry a flag that makes this invocation's output a machine
/// contract — see [`MACHINE_CONTRACT_FLAGS`] for the set and the asymmetry that
/// governs it.
fn has_machine_contract_flag(args: &[String]) -> bool {
    if user_has_flag(args, MACHINE_CONTRACT_FLAGS) {
        return true;
    }
    // Single-dash clusters: `-z`, `-sz`, `-zs`, `-bz`, …  The `starts_with('-')`
    // guard makes byte index 1 a char boundary, so the slice cannot panic.
    args.iter().any(|a| {
        a.starts_with('-')
            && !a.starts_with("--")
            && a[1..].chars().any(|c| CONTRACT_SHORT_OPTS.contains(&c))
    })
}

/// Whether the caller typed skim's own `--json` view flag, which **disarms**
/// the machine-contract gate.
///
/// # Why a contract flag plus `--json` is not a gate miss
///
/// ADR-022 keys the gate on "flags and syntax the caller typed", and its
/// over-inclusion asymmetry rests on what a false negative costs: a contract
/// stream "gets reshaped into prose — silently, at exit 0, with no ADR-011
/// class-1 marker".  Neither half of that holds here.  `--json` is itself a
/// flag the caller typed, and what it asks for is *skim's* machine contract —
/// the JSON envelope, emitted through `exec::emit_json_envelope` with its
/// mandatory [`Completeness`] declaration and class-1 marker.  So the reader
/// is not silently served prose; they are served the machine-readable format
/// they requested, under disclosure.
///
/// The gate as first landed fired ahead of every handler, while `--json` is
/// extracted *inside* handlers by `extract_output_format`, and
/// [`run_passthrough`] forwarded the caller's argv to git unfiltered.  So a
/// skim-only flag reached git and git rejected the whole invocation.  Measured
/// on this branch before this guard existed:
///
/// ```text
/// $ skim git status --porcelain --json
/// error: unknown option `json'
/// exit=1                                      (0 B on stdout)
/// ```
///
/// against `c2b4378`, which served a 2 500 B envelope at exit 0.  Every
/// (contract flag × compressed subcommand) pair was affected, which is what
/// made `README.md`'s "All subcommands support `--json`" false.
///
/// # Acceptance rule
///
/// Mirrors [`crate::cmd::extract_json_flag`] exactly — a bare `--json` token,
/// and only **before** a POSIX `--` separator — because that is the function
/// the handler on the other side of this guard uses to extract the flag.  The
/// two must agree: a guard that disarmed on `-- --json` would route a
/// *pathspec* into a handler that forwards it to git unchanged, answering a
/// question git answers differently.  Pinned against that function by
/// `json_request_predicate_matches_extract_json_flag`, and non-allocating so
/// the guard costs nothing on the gated path.
fn caller_requested_json(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a.as_str() == "--json")
}

/// Remove skim's own view flags from an argv bound for the real `git` binary.
///
/// Returns `Some((forwarded, dropped))` when at least one token was removed,
/// and `None` — allocation-free — when the argv is already clean.  `None` is
/// the common case, and it keeps every currently-gated invocation
/// byte-for-byte unchanged.
///
/// # The set, and why it is narrower than `dispatch::strip_skim_flags`
///
/// Exactly the two flags `CLAUDE.md` documents as "additionally stripped for
/// `git` only": a bare `--json` and `--mode` / `--mode=<val>`.  Those are the
/// two that git handlers extract for themselves (`extract_output_format`,
/// `extract_diff_mode`), which is what makes them provably skim-only on this
/// tool.
///
/// The general helper also strips `--max-lines`, `--tokens` and
/// `--last-lines`.  Those are deliberately left alone.  No git handler
/// implements them — measured on `c2b4378` and on this branch,
/// `skim git log -n 2 --max-lines 10` is `fatal: ambiguous argument '10'`
/// with or without a contract flag — so stripping them here would convert a
/// hard error into a silently **unbounded** serve, manufacturing the exact
/// ADR-016 defect that "a bound the tool can exceed is not a bound" names.
/// That gap is real, pre-existing and wider than this gate; widening this set
/// is not how to close it.
///
/// # What actually reaches this function with something to drop
///
/// `--mode` is the case that needs it, and it gets the **opposite** answer to
/// `--json`: the gate keeps firing and the token is dropped.  `--mode` selects
/// a view of *source code*, and a `--stat` / `--numstat` / `--porcelain`
/// payload is not source code — there is no view to select.  Disarming the
/// gate for it instead would hand a stat payload to the AST pipeline, which is
/// F6 rebuilt: `git diff --no-color --stat` carries no `diff --git` header for
/// `parse_diff` to find.
///
/// `--json` reaches here only from the per-command gates this module does not
/// own — `show.rs`'s `PASSTHROUGH_FLAGS` and `ShowMode::MultiRef`, and
/// `fetch.rs`'s `--dry-run`/`--quiet` — where it was already a hard error
/// before the shared gate existed (measured at `c2b4378`:
/// `skim git show --raw --json HEAD` → `fatal: unrecognized argument: --json`,
/// exit 1).  Stripping upgrades those to a lossless raw serve.  The complete
/// fix is to disarm *those* gates on `--json` the way this one now is, which
/// is outside this change's file scope.
///
/// # POSIX `--`
///
/// Nothing is dropped at or after a bare `--`, matching `extract_json_flag`,
/// `extract_diff_mode` and `dispatch::strip_skim_flags`.  After the separator
/// these tokens are pathspecs, and `git status --porcelain -- --json` is a
/// real invocation whose answer (git matches no such path) both binaries
/// already give correctly.
fn strip_git_view_flags(args: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    // Fast path: skip the walk when no candidate token is present before `--`.
    let has_candidate = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a.as_str() == "--json" || a.as_str() == "--mode" || a.starts_with("--mode="));
    if !has_candidate {
        return None;
    }

    let mut forwarded: Vec<String> = Vec::with_capacity(args.len());
    let mut dropped: Vec<String> = Vec::new();
    let mut i = 0;
    let mut past_separator = false;

    while i < args.len() {
        let arg = &args[i];

        if past_separator {
            forwarded.push(arg.clone());
            i += 1;
            continue;
        }

        match arg.as_str() {
            "--" => {
                past_separator = true;
                forwarded.push(arg.clone());
                i += 1;
            }
            "--json" => {
                dropped.push(arg.clone());
                i += 1;
            }
            // `--mode <val>`: the value token belongs to the flag, so it goes
            // too.  Leaving it behind hands git an orphan positional that it
            // reads as a revision — `fatal: ambiguous argument 'full'`, the
            // same class of failure one token further along.
            "--mode" => {
                dropped.push(arg.clone());
                i += 1;
                if i < args.len() && !args[i].starts_with('-') {
                    dropped.push(args[i].clone());
                    i += 1;
                }
            }
            other if other.starts_with("--mode=") => {
                dropped.push(arg.clone());
                i += 1;
            }
            _ => {
                forwarded.push(arg.clone());
                i += 1;
            }
        }
    }

    debug_assert!(
        !dropped.is_empty(),
        "has_candidate was true, so the walk must have dropped at least one token"
    );
    Some((forwarded, dropped))
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    println!("skim git <status|diff|fetch|log|show|commit|push> [args...]");
    println!();
    println!("  Compress git command output for LLM context windows.");
    println!();
    println!("Subcommands:");
    println!("  status    Show compressed working tree status");
    println!("  diff      AST-aware diff with full function boundaries");
    println!("  fetch     Show compressed fetch summary (new branches, tags, pruned)");
    println!("  log       Show compressed commit log");
    println!("  show      Show compressed commit or file content at a ref");
    println!("  commit    Show compressed commit result (hash, subject, file stats)");
    println!("  push      Show compressed push result (refs pushed, up-to-date, rejected)");
    println!();
    println!("Global git flags (before subcommand):");
    println!("  -C <path>    Run as if git was started in <path>");
    println!("  --git-dir    Set the path to the repository");
    println!("  --work-tree  Set the path to the working tree");
    println!();
    println!("Flags (all subcommands):");
    println!("  --json           Machine-readable JSON output");
    println!("  --show-stats     Show token savings statistics");
    println!();
    println!("Examples:");
    println!("  skim git status");
    println!("  skim git status --json");
    println!("  skim git diff --cached");
    println!("  skim git diff --mode structure");
    println!("  skim git diff main..feature --json");
    println!("  skim git fetch");
    println!("  skim git fetch --prune");
    println!("  skim git log -n 5");
    println!("  skim git show HEAD");
    println!("  skim git show HEAD:src/main.rs");
    println!("  skim git diff --help                   Diff-specific options");
    println!("  skim git show --help                   Show-specific options");
}

// ============================================================================
// Global flag splitting
// ============================================================================

/// Split leading git global flags (e.g., `-C <path>`, `--git-dir=...`)
/// from the subcommand and its arguments.
///
/// Git global flags appear before the subcommand:
///   `git -C /path --no-pager status --short`
///         ^^^^^^^^^^^^^^^^^^ global  ^^^^^^ subcommand args
///
/// Returns `(global_flags, rest)` where `rest[0]` is the subcommand name.
fn split_global_flags(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut global_flags = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // Flags that consume a following value
        if matches!(arg.as_str(), "-C" | "--git-dir" | "--work-tree" | "-c") {
            global_flags.push(arg.clone());
            if i + 1 < args.len() {
                global_flags.push(args[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        // Flags with embedded value (--git-dir=..., --work-tree=...)
        if arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("-c=")
        {
            global_flags.push(arg.clone());
            i += 1;
            continue;
        }

        // Boolean global flags
        if matches!(
            arg.as_str(),
            "--no-pager" | "--bare" | "--no-replace-objects" | "--no-optional-locks"
        ) {
            global_flags.push(arg.clone());
            i += 1;
            continue;
        }

        // Not a global flag — this is the subcommand (or subcommand arg)
        break;
    }

    let rest = args[i..].to_vec();
    (global_flags, rest)
}

// ============================================================================
// Helpers
// ============================================================================

/// Build the analytics label string for a git subcommand invocation.
///
/// Returns `"skim git {subcmd} {args}"` when either `--show-stats` or analytics
/// recording is active, and an empty `String` otherwise.  This avoids an
/// unconditional `format!` allocation on the hot path when both flags are off.
///
/// All six parsed-command handlers (`show` ×2, `diff`, `status`, `log`, `fetch`)
/// share this exact guard logic; centralising it here eliminates the repeated
/// five-line block at each call site.
pub(super) fn build_analytics_label(
    subcmd: &str,
    args: &[String],
    show_stats: bool,
    analytics_enabled: bool,
) -> String {
    if show_stats || analytics_enabled {
        // Scrub credential-bearing URLs from args before persisting in the
        // analytics DB.  A user invoking `skim git push https://TOKEN@host/repo`
        // would otherwise have the token written to `~/.cache/skim/analytics.db`
        // via the `original_cmd` column.  Scrubbing here protects all current
        // and future git handlers that go through this function.
        let scrubbed: Vec<String> = args
            .iter()
            .map(|a| shared::scrub_credential_url(a).into_owned())
            .collect();
        crate::cmd::format_analytics_label("git", subcmd, &scrubbed.join(" "))
    } else {
        String::new()
    }
}

/// Record token stats and fire-and-forget analytics for any git handler.
///
/// Centralises the analytics + stats tail that previously appeared inline in
/// `run_passthrough`, `run_parsed_command`, and the deleted `record_show_result`.
///
/// Two production variants:
///   - [`finalize_git_output_owned`] — callers that own both strings (raw ≠ output).
///   - [`finalize_git_output_passthrough`] — callers where raw == output.
///
/// A borrowed variant exists in `#[cfg(test)]` only.
///
/// # Parameters (shared by all variants)
/// - `raw`       — Original git output before any compression.
/// - `output`    — Compressed output (may equal `raw` for passthrough).
/// - `label`     — Command label stored in the analytics DB.
/// - `show_stats`— Whether to print token-savings stats to stderr.
/// - `rec`       — Recording context (enabled, command_type, parse_tier, session_id).
/// - `duration`  — Wall-clock duration of the underlying git command.
///
/// Takes ownership of `raw` and `output`, moving them directly into the
/// analytics call when analytics are enabled — zero extra allocations on
/// the analytics path and zero allocations when analytics are off.
///
/// Use this variant in handlers that already own their output strings
/// (i.e. the string would be dropped immediately after the call anyway).
pub(super) fn finalize_git_output_owned(
    raw: String,
    output: String,
    label: String,
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
    duration: std::time::Duration,
) {
    if show_stats {
        let (orig, comp) = crate::tokens::count_token_pair(&raw, &output);
        crate::process::report_token_stats(orig, comp, "");
    }
    crate::analytics::try_record_command(rec, raw, output, label, duration);
}

/// Passthrough variant of [`finalize_git_output_owned`].
///
/// Use this when `raw` and `output` are **the same string** (passthrough
/// semantics: no compression occurred).  Takes ownership of `raw` so that
/// when analytics are enabled the buffer is **cloned once** (for
/// `raw_text`) and **moved once** (for `compressed_text`) — exactly 1 heap
/// allocation on the analytics-enabled path, 0 on the disabled path.
/// This is the PF-018 resolution: one clone + one move, not two clones.
///
/// Call sites: `run_passthrough`, `run_parsed_command` non-zero exit,
/// `run_diff` non-zero exit / empty diff / empty-after-parse, and the
/// equivalent failure paths in `show.rs`.
pub(super) fn finalize_git_output_passthrough(
    raw: String,
    label: String,
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
    duration: std::time::Duration,
) {
    if show_stats {
        // ALLOC NOTE: count_token_pair borrows; no allocation here.
        let (orig, comp) = crate::tokens::count_token_pair(&raw, &raw);
        crate::process::report_token_stats(orig, comp, "");
    }
    if rec.enabled {
        // 1 allocation: raw.clone() produces raw_text; raw is moved as
        // compressed_text.  Zero allocations when analytics are disabled.
        crate::analytics::try_record_command(rec, raw.clone(), raw, label, duration);
    }
}

/// Convert an optional exit code to an ExitCode.
fn map_exit_code(code: Option<i32>) -> ExitCode {
    match code {
        Some(0) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Run a git command with passthrough (no parsing).
pub(super) fn run_passthrough(
    global_flags: &[String],
    subcmd: &str,
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push(subcmd.to_string());
    // skim's own view flags are not git flags, and this function is the shared
    // sink every raw-serve path funnels through — the ADR-022 gate above plus
    // the five per-command gates in `show.rs`, `fetch.rs`, `commit.rs` and
    // `push.rs`.  Filtering here rather than at each gate means a call site
    // cannot reintroduce the defect by forgetting to filter, and a future
    // narrowing of `MACHINE_CONTRACT_FLAGS` cannot bring it back.
    match strip_git_view_flags(args) {
        Some((forwarded, dropped)) => {
            // ADR-011 class-2, so `SKIM_DEBUG`-gated: the reader receives
            // git's own bytes for a payload that has no skim view to select,
            // which is a lossless raw fallback and not a loss-bearing one.
            // `cli_git_contract_flags.rs::assert_byte_identical` pins that
            // classification for every gated invocation by comparing BOTH
            // descriptors against the raw control, so promoting this to an
            // unconditional marker is a contract change, not a tweak.
            let safe = crate::cmd::sanitize_for_display(&dropped.join(" "));
            crate::debug_log!(
                "skim git {subcmd}: '{safe}' is a skim view flag, not a git flag — \
                 serving git's own output for this machine-contract format"
            );
            full_args.extend(forwarded);
        }
        None => full_args.extend_from_slice(args),
    }

    let runner = CommandRunner::new();
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
    let output = runner.run("git", &arg_refs)?;

    if exec::write_to_stdout(&output.stdout)? == exec::StdoutStatus::PipeClosed {
        return Ok(exec::pipe_closed_exit());
    }
    if !output.stderr.is_empty()
        && exec::write_to_stderr(&output.stderr)? == exec::StdoutStatus::PipeClosed
    {
        return Ok(exec::pipe_closed_exit());
    }

    let exit_code = output.exit_code;
    // Passthrough: raw == compressed. Move stdout into the passthrough variant
    // so the analytics path clones once and moves once — 1 allocation total
    // instead of 2 (PF-018 resolution).  Label is built lazily via
    // build_analytics_label so the format! is skipped when both show_stats
    // and analytics are disabled (PF-021).
    finalize_git_output_passthrough(
        output.stdout,
        build_analytics_label(subcmd, args, show_stats, rec.enabled),
        show_stats,
        rec.with_tier("passthrough"),
        output.duration,
    );

    Ok(map_exit_code(exit_code))
}

/// Options for [`run_parsed_command`] that bundle infrequently-varied flags.
///
/// Grouping these reduces the argument count to stay within Clippy's
/// `too_many_arguments` limit while keeping all parameters documented together.
///
/// # No `Default` — intentional (ADR-011 / D1)
///
/// `completeness` has no sensible default: a `--json` handler that does not
/// state whether its envelope carries everything git produced is exactly the
/// silent-loss defect the disclosure split closes.  Deriving `Default` here
/// would reintroduce a way to skip that decision, so it is deliberately absent
/// — every construction site must spell the value out.
pub(super) struct ParsedCommandOptions {
    /// When `true`, the parser receives `stderr + stdout` combined.  Git fetch
    /// writes its output to stderr; set to `true` for fetch, `false` otherwise.
    pub combine_stderr: bool,
    /// When `Some`, the net-savings guard compares the compressed result against
    /// this string instead of the internal command's stdout.  Satisfies C-7: the
    /// baseline must reflect the **user's literal command** output, not skim's
    /// internally substituted command.  Pass `None` for standard behaviour.
    pub raw_override: Option<String>,
    /// Whether the `--json` envelope this run emits contains everything the raw
    /// git command produced.  Consumed only on the JSON path; the text path is
    /// governed by the net-savings guard instead.
    pub completeness: Completeness,
}

impl ParsedCommandOptions {
    /// Combined-stderr options: parser receives `stderr + stdout`, no baseline override.
    ///
    /// Used by commit, fetch, and push — all write their primary output to stderr.
    /// Each states its own `completeness`; the constructor does not choose one.
    pub fn combined(completeness: Completeness) -> Self {
        Self {
            combine_stderr: true,
            raw_override: None,
            completeness,
        }
    }
}

/// Run a git command and parse its output with the given parser function.
///
/// Callers are responsible for baking global flags into `subcmd_args` before
/// calling this function.
///
/// `label` is the analytics label string built by the caller from the user's
/// **original** (pre-rewrite) args via [`build_analytics_label`].
///
/// See [`ParsedCommandOptions`] for `combine_stderr`, `raw_override`, and
/// `completeness` docs.
///
/// # AD-GIT-14 (2026-04-11) — analytics recording on non-zero exit
///
/// Previously, a non-zero exit code caused an early return with no analytics
/// recording, so every failed git invocation was silently absent from the DB.
/// The fix calls `finalize_git_output_passthrough` on the failure path using
/// the empty stdout buffer, keeping analytics consistent with the passing path.
/// `raw == compressed` on failure, so the single-clone passthrough variant is
/// used (PF-018).  The same pattern applies to `run_diff` non-zero exits.
pub(super) fn run_parsed_command<F>(
    subcmd_args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
    output_format: OutputFormat,
    label: String,
    opts: ParsedCommandOptions,
    parser: F,
) -> anyhow::Result<ExitCode>
where
    F: FnOnce(&str) -> GitResult,
{
    let ParsedCommandOptions {
        combine_stderr,
        raw_override,
        completeness,
    } = opts;
    let runner = CommandRunner::new();
    let arg_refs: Vec<&str> = subcmd_args.iter().map(String::as_str).collect();
    let output = runner.run("git", &arg_refs)?;

    if output.exit_code != Some(0) {
        // On failure, scrub credential URLs line-by-line before forwarding to
        // stderr/stdout (PF-024).  Git push (and fetch/clone) embeds auth tokens
        // in remote URLs on auth failures, e.g.:
        //   fatal: unable to access 'https://ghp_xxx@github.com/org/repo.git'
        // Without scrubbing these appear verbatim in the terminal.  This guard
        // applies to all git subcommands going through run_parsed_command, not
        // just push — generic protection is safer than a subcmd-specific gate.
        let scrubbed_stderr = shared::scrub_lines(&output.stderr);
        if !scrubbed_stderr.is_empty()
            && exec::write_line_to_stderr(&scrubbed_stderr)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        // Scrub stdout once; reuse for both terminal output and analytics (PF-024).
        let scrubbed_stdout = shared::scrub_lines(&output.stdout);
        if !scrubbed_stdout.is_empty()
            && exec::write_line_to_stdout(&scrubbed_stdout)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        let exit_code = output.exit_code;
        // Record analytics even on non-zero exit so the DB reflects failed
        // invocations (PF-018).
        finalize_git_output_passthrough(
            scrubbed_stdout,
            label,
            show_stats,
            rec.with_tier("passthrough"),
            output.duration,
        );
        return Ok(map_exit_code(exit_code));
    }

    // Git fetch writes to stderr; other subcommands write to stdout.
    let raw: String = if combine_stderr {
        format!("{}\n{}", output.stderr, output.stdout)
    } else {
        output.stdout
    };

    let result = parser(&raw);
    // Capture parse_tier before result is consumed by rendering.
    let parse_tier = result.parse_tier;

    // Serialize first without printing so the net-savings guard can decide.
    //
    // Exemptions:
    // - JSON output: must never be rewritten to non-JSON.
    // - Already-passthrough tier: `raw` IS the body; guard is a no-op.
    //
    // "raw" baseline = raw_override when supplied (C-7: guard against the user's
    // literal command output), otherwise post-ANSI-strip `raw` (stdout or combined
    // stderr+stdout).
    let guard_raw: &str = raw_override.as_deref().unwrap_or(&raw);
    let (result_str, effective_tier) = match output_format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize result: {e}"))?;
            // ADR-011 / D1 — the declaration comes from the caller
            // (`ParsedCommandOptions::completeness`), because only the caller
            // knows what its parser modelled.  `elided` is `None`: these
            // parsers summarise into `operation`/`summary`/`details` with no
            // 1:1 unit to count against the raw output.
            if exec::emit_json_envelope(
                &json,
                completeness,
                "git",
                None,
                exec::LineTermination::Newline,
            )? == exec::StdoutStatus::PipeClosed
            {
                return Ok(exec::pipe_closed_exit());
            }
            (json, parse_tier)
        }
        OutputFormat::Text => {
            let s = result.to_string();
            let tier_str: Option<&'static str> = if parse_tier.is_some_and(|t| t == "passthrough") {
                // Already passthrough — skip guard, print as-is.
                if exec::write_line_to_stdout(&s)? == exec::StdoutStatus::PipeClosed {
                    return Ok(exec::pipe_closed_exit());
                }
                parse_tier
            } else {
                // Apply net-savings guard.
                match exec::savings_decision(guard_raw, &s) {
                    exec::SavingsDecision::Keep => {
                        if exec::write_line_to_stdout(&s)? == exec::StdoutStatus::PipeClosed {
                            return Ok(exec::pipe_closed_exit());
                        }
                        parse_tier
                    }
                    exec::SavingsDecision::Passthrough => {
                        // Emit the user's raw output if available (C-7), otherwise
                        // emit the internal command raw; record under "passthrough" tier.
                        let emit_raw = raw_override.as_deref().unwrap_or(&raw);
                        // Byte-exact: `emit_raw` is git's own stdout, and a
                        // newline git never wrote is a divergence from raw that
                        // no ADR-011 marker discloses.  Unreachable today —
                        // every non-newline-terminated git format (`-z`,
                        // `--null`, `--pretty=format:`, `--format=`) is in
                        // `MACHINE_CONTRACT_FLAGS`, so the ADR-022 gate serves
                        // it raw ahead of dispatch and it never reaches this
                        // verdict.  Kept and routed here for the same reason
                        // `status.rs` keeps its now-unreachable flag-stripping
                        // arms: a future narrowing of the gate would return the
                        // leak.  See `exec::emit_raw_passthrough_exact` for the
                        // measurements on both arms.
                        let (tier, status) = exec::emit_raw_passthrough_exact(emit_raw)?;
                        if status == exec::StdoutStatus::PipeClosed {
                            return Ok(exec::pipe_closed_exit());
                        }
                        Some(tier)
                    }
                }
            };
            (s, tier_str)
        }
    };

    // Scrub credentials before analytics recording (PF-024).  The parser used
    // the un-scrubbed `raw` to extract ref data; only the analytics copy needs
    // scrubbing.
    let analytics_raw = shared::scrub_lines(&raw);

    // `analytics_raw` and `result_str` are owned here; move them directly.
    // `label` is supplied by the caller from the user's original (pre-rewrite) args
    // so the analytics DB records the invocation as the user typed it.
    // `parse_tier` propagates the parser's tier annotation to the analytics DB (AD-GIT-12).
    // When savings_decision flips to Passthrough, effective_tier overrides parse_tier.
    finalize_git_output_owned(
        analytics_raw,
        result_str,
        label,
        show_stats,
        rec.with_tier_opt(effective_tier),
        output.duration,
    );

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::user_has_flag;

    // ========================================================================
    // split_global_flags tests
    // ========================================================================

    #[test]
    fn test_split_no_global_flags() {
        let args: Vec<String> = vec!["status".into(), "--short".into()];
        let (global, rest) = split_global_flags(&args);
        assert!(global.is_empty());
        assert_eq!(rest, vec!["status", "--short"]);
    }

    #[test]
    fn test_split_with_c_flag() {
        let args: Vec<String> = vec!["-C".into(), "/tmp".into(), "status".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["-C", "/tmp"]);
        assert_eq!(rest, vec!["status"]);
    }

    #[test]
    fn test_split_with_git_dir_equals() {
        let args: Vec<String> = vec!["--git-dir=/repo/.git".into(), "log".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["--git-dir=/repo/.git"]);
        assert_eq!(rest, vec!["log"]);
    }

    #[test]
    fn test_split_with_no_pager() {
        let args: Vec<String> = vec!["--no-pager".into(), "diff".into(), "--cached".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["--no-pager"]);
        assert_eq!(rest, vec!["diff", "--cached"]);
    }

    #[test]
    fn test_split_multiple_global_flags() {
        let args: Vec<String> = vec![
            "-C".into(),
            "/tmp".into(),
            "--no-pager".into(),
            "status".into(),
        ];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["-C", "/tmp", "--no-pager"]);
        assert_eq!(rest, vec!["status"]);
    }

    // ========================================================================
    // --no-optional-locks global flag
    // ========================================================================

    #[test]
    fn test_split_with_no_optional_locks() {
        let args: Vec<String> = vec!["--no-optional-locks".into(), "status".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["--no-optional-locks"]);
        assert_eq!(rest, vec!["status"]);
    }

    // ========================================================================
    // Passthrough flag detection tests
    // ========================================================================

    #[test]
    fn test_status_passthrough_with_porcelain() {
        assert!(user_has_flag(
            &["--porcelain".to_string()],
            &["--porcelain", "--short", "-s"]
        ));
    }

    #[test]
    fn test_status_passthrough_with_short() {
        assert!(user_has_flag(
            &["-s".to_string()],
            &["--porcelain", "--short", "-s"]
        ));
    }

    #[test]
    fn test_diff_passthrough_with_name_only() {
        assert!(user_has_flag(
            &["--name-only".to_string()],
            &["--stat", "--name-only", "--name-status"]
        ));
    }

    #[test]
    fn test_diff_no_passthrough_without_flag() {
        assert!(!user_has_flag(
            &["--cached".to_string()],
            &["--stat", "--name-only", "--name-status"]
        ));
    }

    #[test]
    fn test_log_passthrough_with_oneline() {
        assert!(user_has_flag(
            &["--oneline".to_string()],
            &["--format", "--pretty", "--oneline"]
        ));
    }

    #[test]
    fn test_log_passthrough_with_format() {
        assert!(user_has_flag(
            &["--format".to_string()],
            &["--format", "--pretty", "--oneline"]
        ));
    }

    // ========================================================================
    // user_has_flag / map_exit_code helpers
    // ========================================================================

    #[test]
    fn test_user_has_flag_empty_args() {
        assert!(!user_has_flag(&[], &["--flag"]));
    }

    #[test]
    fn test_map_exit_code_success() {
        let code = map_exit_code(Some(0));
        // ExitCode doesn't impl PartialEq, so compare via Debug
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }

    #[test]
    fn test_map_exit_code_failure() {
        let code = map_exit_code(Some(1));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn test_map_exit_code_none() {
        let code = map_exit_code(None);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    // ========================================================================
    // Prefix-match passthrough (--format=%H, --porcelain=v2)
    // ========================================================================

    #[test]
    fn test_log_passthrough_with_format_equals() {
        assert!(user_has_flag(
            &["--format=%H".to_string()],
            &["--format", "--pretty", "--oneline"]
        ));
    }

    #[test]
    fn test_status_passthrough_with_porcelain_v2() {
        assert!(user_has_flag(
            &["--porcelain=v2".to_string()],
            &["--porcelain", "--short", "-s"]
        ));
    }

    // ========================================================================
    // --check passthrough for diff
    // ========================================================================

    #[test]
    fn test_diff_passthrough_with_check() {
        assert!(user_has_flag(
            &["--check".to_string()],
            &["--stat", "--name-only", "--name-status", "--check"]
        ));
    }

    // ========================================================================
    // --shortstat and --numstat passthrough for diff
    // ========================================================================

    #[test]
    fn test_diff_passthrough_with_shortstat() {
        assert!(user_has_flag(
            &["--shortstat".to_string()],
            &[
                "--stat",
                "--shortstat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--check"
            ]
        ));
    }

    #[test]
    fn test_diff_passthrough_with_numstat() {
        assert!(user_has_flag(
            &["--numstat".to_string()],
            &[
                "--stat",
                "--shortstat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--check"
            ]
        ));
    }

    // ========================================================================
    // Non-zero exit analytics documentation
    // ========================================================================

    /// Borrowed variant of `finalize_git_output_owned` — test-only.
    ///
    /// Takes `&str` references and clones them only when analytics are enabled.
    /// No production call site uses this; prefer `finalize_git_output_owned` or
    /// `finalize_git_output_passthrough` in handlers.
    fn finalize_git_output(
        raw: &str,
        output: &str,
        label: String,
        show_stats: bool,
        rec: crate::analytics::RecordingContext<'_>,
        duration: std::time::Duration,
    ) {
        if show_stats {
            let (orig, comp) = crate::tokens::count_token_pair(raw, output);
            crate::process::report_token_stats(orig, comp, "");
        }
        crate::analytics::try_record_command(
            rec,
            raw.to_string(),
            output.to_string(),
            label,
            duration,
        );
    }

    /// Documents that `run_parsed_command` records analytics on non-zero exit.
    ///
    /// Previously, a non-zero exit returned early without recording, causing
    /// failed invocations (e.g., `git log` on a bare repo) to be invisible in
    /// the analytics DB. The fix calls `finalize_git_output` on the error path
    /// with raw==compressed (passthrough semantics) so the DB is consistent.
    ///
    /// This test validates `finalize_git_output` itself is callable with
    /// empty strings (the non-zero path uses empty stdout on most failures).
    #[test]
    fn test_finalize_git_output_accepts_empty_strings() {
        // Analytics disabled via injected false — no env var mutation needed.
        finalize_git_output(
            "",
            "",
            "skim git log".to_string(),
            false,
            crate::analytics::RecordingContext {
                enabled: false,
                command_type: crate::analytics::CommandType::Git,
                parse_tier: None,
                session_id: None,
            },
            std::time::Duration::ZERO,
        );
    }

    // ========================================================================
    // build_analytics_label credential scrubbing (PF-024 companion)
    // ========================================================================

    /// Verifies that build_analytics_label scrubs credential-bearing URLs from
    /// args before composing the label that is persisted in analytics.db.
    ///
    /// Without scrubbing, `skim git push https://TOKEN@host/repo` would write
    /// the token into `~/.cache/skim/analytics.db` via the `original_cmd` column.
    #[test]
    fn test_build_analytics_label_scrubs_credentials() {
        let args = vec!["https://ghp_SuperSecretToken@github.com/org/repo".to_string()];
        let label = build_analytics_label("push", &args, true, true);
        assert!(
            !label.contains("ghp_SuperSecretToken"),
            "analytics label must not contain the credential token; got: {label}"
        );
        assert!(
            label.contains("github.com/org/repo"),
            "analytics label must preserve the host/path; got: {label}"
        );
    }

    /// No trailing space when args is empty.
    ///
    /// `build_analytics_label("status", &[], true, true)` must return
    /// `"skim git status"`, not `"skim git status "`.
    #[test]
    fn test_build_analytics_label_no_trailing_space_when_no_args() {
        let label = build_analytics_label("status", &[], true, true);
        assert_eq!(label, "skim git status");
    }

    // ========================================================================
    // Failure-path credential scrubbing (PF-024 / Task 6c)
    // ========================================================================

    /// Verifies that credentials embedded in git stderr output are scrubbed
    /// before being written to the terminal AND before being recorded in
    /// analytics.db on the non-zero exit path.
    ///
    /// We cannot drive `run_parsed_command` end-to-end in a unit test (it
    /// shells out to a real `git` binary).  Instead we test the scrubbing
    /// helper directly on the kind of line that git emits on auth failures:
    ///
    ///   fatal: unable to access 'https://ghp_xxx@github.com/org/repo.git':
    ///     The requested URL returned error: 403
    ///
    /// The scrubbing logic in `run_parsed_command` calls `shared::scrub_credential_url`
    /// line-by-line on both stderr and stdout before forwarding to the terminal
    /// and before passing to `finalize_git_output_passthrough`.
    #[test]
    fn test_error_path_stderr_scrubbing() {
        use crate::cmd::git::shared::scrub_credential_url;

        let stderr_line =
            "fatal: unable to access 'https://ghp_abc123@github.com/org/repo.git': 403";
        let scrubbed = scrub_credential_url(stderr_line);
        assert!(
            !scrubbed.contains("ghp_abc123"),
            "credential token must be stripped from error output; got: {scrubbed}"
        );
        assert!(
            scrubbed.contains("github.com/org/repo.git"),
            "host/path must be preserved in error output; got: {scrubbed}"
        );
        assert!(
            scrubbed.contains("403"),
            "error details must be preserved; got: {scrubbed}"
        );
    }

    /// ssh:// credentials on the error path are scrubbed (AD-GP-1 companion).
    #[test]
    fn test_error_path_stderr_scrubs_ssh_url() {
        use crate::cmd::git::shared::scrub_credential_url;

        let stderr_line =
            "fatal: Could not read from remote repository ssh://deploy@github.com/org/repo.git";
        let scrubbed = scrub_credential_url(stderr_line);
        assert!(
            !scrubbed.contains("deploy@"),
            "ssh credential must be stripped; got: {scrubbed}"
        );
        assert!(
            scrubbed.contains("github.com/org/repo.git"),
            "host/path must be preserved; got: {scrubbed}"
        );
    }

    // ========================================================================
    // Machine-contract passthrough gate (ADR-022)
    // ========================================================================

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| (*t).to_string()).collect()
    }

    /// Every flag in the closed set is recognised on its own.
    ///
    /// Enumerated from the const rather than hand-listed, so a future entry
    /// cannot be added without the predicate being exercised on it.
    #[test]
    fn every_contract_flag_is_recognised_alone() {
        for &flag in MACHINE_CONTRACT_FLAGS {
            assert!(
                has_machine_contract_flag(&argv(&[flag])),
                "'{flag}' is in MACHINE_CONTRACT_FLAGS but the predicate misses it"
            );
        }
    }

    /// `--porcelain=v1` / `=v2` are the same contract as bare `--porcelain`.
    ///
    /// This is what `user_has_flag`'s `=` rule buys, and it is the whole of
    /// F9: `git status --porcelain=v2 --branch` is a stability contract, so
    /// every `#`-prefixed header line — `# branch.oid` included, which has no
    /// prefix match anywhere in `status.rs` — must reach the reader verbatim.
    #[test]
    fn porcelain_matches_its_versioned_forms() {
        for form in ["--porcelain", "--porcelain=v1", "--porcelain=v2"] {
            assert!(
                has_machine_contract_flag(&argv(&[form, "--branch"])),
                "'{form}' must be recognised as a machine contract"
            );
        }
        // `--no-porcelain` is the opposite request and must NOT match.
        assert!(!has_machine_contract_flag(&argv(&["--no-porcelain"])));
    }

    /// `-z` is matched inside a bundled short cluster, not only as a lone token.
    ///
    /// `git status -sz` is a real invocation.  `status.rs`'s own
    /// `CONFLICTING_SHORT_OPTS` scan is already cluster-aware, so an
    /// exact-token gate here would be *weaker* than the shipped behaviour — a
    /// regression introduced by this fix rather than a pre-existing gap.
    #[test]
    fn contract_shorts_are_matched_inside_a_cluster() {
        for cluster in ["-z", "-sz", "-zs", "-uz", "-bz"] {
            assert!(
                has_machine_contract_flag(&argv(&[cluster])),
                "cluster '{cluster}' contains a contract short and must match"
            );
        }
    }

    /// `-s` / `--short` / `--long` are NOT contracts and must not be gated.
    ///
    /// Short format is a human-facing rendering that `status.rs` translates
    /// faithfully; gating it would turn `skim git status -sb` — which has its
    /// own pinned test — into raw passthrough.  `--oneline` is excluded for the
    /// same reason: `log.rs` answers it by injecting an equivalent `--format`.
    #[test]
    fn non_contract_flags_are_not_gated() {
        for args in [
            vec!["-s"],
            vec!["-sb"],
            vec!["-b"],
            vec!["--short"],
            vec!["--long"],
            vec!["--oneline"],
            vec!["--oneline", "-n", "3"],
            vec!["--cached"],
            vec!["--mode", "full"],
            vec!["-U100000"],
            vec!["HEAD~1..HEAD"],
            vec![],
        ] {
            assert!(
                !has_machine_contract_flag(&argv(&args)),
                "{args:?} carries no machine-contract flag but the gate fired"
            );
        }
    }

    /// The gate is deliberately NOT separator-aware (ADR-022).
    ///
    /// A pathspec literally named `--stat` after a bare `--` serves raw.  That
    /// is a false positive, and a false positive costs only the compression
    /// skim would have applied — the reader still gets git's own bytes, and
    /// `run_passthrough` records `parse_tier == "passthrough"`, which skips the
    /// ADR-001 guard rather than being graded by it.  Routing through
    /// `args_before_separator` could only turn a match into a miss, i.e. only
    /// manufacture the failure mode that IS the defect class.
    ///
    /// Pinning it stops a later "correctness" edit from narrowing the gate.
    #[test]
    fn gate_is_not_separator_aware_by_design() {
        assert!(has_machine_contract_flag(&argv(&["--", "--stat"])));
        assert!(has_machine_contract_flag(&argv(&["--", "-z"])));
    }

    /// `--raw` is recognised **in position**, next to the args it really ships
    /// with, on each subcommand where it is a git flag.
    ///
    /// [`every_contract_flag_is_recognised_alone`] already covers every entry
    /// as a lone token, so this deliberately covers what that cannot: the flag
    /// surrounded by neighbours, in the three argv shapes the defect was
    /// measured through.  Before this entry existed `git log --raw -n 3` served
    /// 324 B against 1 695 B of raw git — byte-identical to what `--stat`
    /// served, the signature of the flag being swallowed rather than honoured —
    /// while `git show --raw HEAD` was already byte-exact, because
    /// `show.rs`'s `PASSTHROUGH_FLAGS` carried `--raw` and no sibling's list
    /// did.  That split is what this entry closes.
    #[test]
    fn raw_record_format_is_a_contract_on_every_subcommand() {
        for args in [
            vec!["--raw", "-n", "3"],  // git log --raw -n 3
            vec!["--raw", "--cached"], // git diff --raw --cached
            vec!["--raw", "HEAD"],     // git show --raw HEAD
            vec!["--cached", "--raw"], // trailing position
            vec!["-n", "3", "--raw", "--no-renames"],
        ] {
            assert!(
                has_machine_contract_flag(&argv(&args)),
                "{args:?} carries `--raw`, a tab-delimited record format, but the gate missed it"
            );
        }
    }

    /// `--raw` matches as a whole token, not as a prefix of a longer flag.
    ///
    /// `user_has_flag` matches an entry exactly or followed by `=`, so a longer
    /// flag that merely *starts* with `raw` cannot be captured by this entry.
    /// Neither token below is real git — both are rejected with
    /// "unrecognized argument" (measured) — so this pins the matcher's
    /// discipline rather than a live invocation: it is the barrier against the
    /// `=` rule being loosened into a bare `starts_with`, which would silently
    /// widen every entry in the set, not just this one.
    #[test]
    fn raw_does_not_match_a_longer_flag_by_prefix() {
        assert!(!has_machine_contract_flag(&argv(&["--rawest"])));
        assert!(!has_machine_contract_flag(&argv(&["--no-raw"])));
    }

    /// A lone `-` is not a cluster and must not panic the byte slice.
    #[test]
    fn bare_dash_tokens_are_inert() {
        assert!(!has_machine_contract_flag(&argv(&["-"])));
        assert!(!has_machine_contract_flag(&argv(&["--"])));
        assert!(!has_machine_contract_flag(&argv(&[""])));
    }

    // ========================================================================
    // `--json` disarms the gate (ADR-022 regression)
    // ========================================================================

    /// Argv shapes where the two predicates must agree, spanning both answers.
    ///
    /// Shared by [`json_request_predicate_matches_extract_json_flag`] and the
    /// two gate-behaviour tests, so a shape cannot be covered by one and
    /// missed by the others.
    const JSON_SHAPES: &[&[&str]] = &[
        &["--json"],
        &["--porcelain", "--json"],
        &["--json", "--porcelain"],
        &["--stat", "-n", "3", "--json"],
        &["--json", "--", "src/a.rs"],
        // Not skim's flag: after the separator it is a pathspec.
        &["--", "--json"],
        &["--porcelain", "--", "--json"],
        // Not skim's flag: `--json=v` is a tool-owned form, never stripped.
        &["--json=title"],
        &["--porcelain", "--json=title"],
        // No `--json` at all.
        &["--porcelain"],
        &["--stat", "-n", "3"],
        &[],
    ];

    /// [`caller_requested_json`] must accept exactly what
    /// [`crate::cmd::extract_json_flag`] accepts.
    ///
    /// This is the drift barrier that lets the guard be a non-allocating scan
    /// instead of a call into that function.  The two agreeing is what makes
    /// the handoff sound: the guard stands the gate down precisely when the
    /// handler on the other side will extract the flag.  Were they to
    /// disagree, `-- --json` would be routed into a handler that forwards a
    /// pathspec to git unchanged — a different answer than git's own.
    #[test]
    fn json_request_predicate_matches_extract_json_flag() {
        for shape in JSON_SHAPES {
            let args = argv(shape);
            let (_filtered, extracted) = crate::cmd::extract_json_flag(&args);
            assert_eq!(
                caller_requested_json(&args),
                extracted,
                "{shape:?}: the gate guard and `extract_json_flag` must agree \
                 on whether the caller typed skim's `--json`"
            );
        }
    }

    /// The gate stands down when a contract flag is paired with `--json`.
    ///
    /// Before this guard, `skim git status --porcelain --json` exited 1 with
    /// `error: unknown option 'json'` and 0 B on stdout, against a 2 500 B
    /// envelope at exit 0 on `c2b4378`.
    #[test]
    fn gate_stands_down_when_the_caller_asked_for_json() {
        for shape in [
            vec!["--porcelain", "--json"],
            vec!["--porcelain=v2", "--json"],
            vec!["-z", "--json"],
            vec!["-sz", "--json"],
            vec!["--stat", "-n", "3", "--json"],
            vec!["--json", "--graph"],
            vec!["--quiet", "--json"],
        ] {
            let args = argv(&shape);
            assert!(
                has_machine_contract_flag(&args),
                "{shape:?}: precondition — the contract flag must still match, \
                 or this test proves nothing about the `--json` guard"
            );
            assert!(
                caller_requested_json(&args),
                "{shape:?}: `--json` must disarm the gate"
            );
        }
    }

    /// `--json` that is NOT skim's flag leaves the gate armed.
    ///
    /// Two shapes, one reason each: after a bare `--` the token is a pathspec,
    /// and `--json=<value>` is a tool-owned form skim never claims.  Both must
    /// still be served raw, because the caller asked for no skim view at all.
    #[test]
    fn gate_stays_armed_for_json_that_is_not_skims_flag() {
        for shape in [
            vec!["--porcelain", "--", "--json"],
            vec!["--stat", "--", "--json"],
            vec!["--porcelain", "--json=title"],
        ] {
            let args = argv(&shape);
            assert!(has_machine_contract_flag(&args), "{shape:?}: precondition");
            assert!(
                !caller_requested_json(&args),
                "{shape:?}: this `--json` is not skim's view flag, so the gate \
                 must stay armed and serve git's own bytes"
            );
        }
    }

    // ========================================================================
    // strip_git_view_flags — skim view flags must never reach git
    // ========================================================================

    /// A clean argv returns `None`, so no allocation and no behaviour change.
    ///
    /// This is what keeps every invocation G already pinned byte-for-byte
    /// identical: the filter is inert unless there is something to filter.
    #[test]
    fn strip_git_view_flags_is_none_on_a_clean_argv() {
        for shape in [
            vec!["--porcelain"],
            vec!["--stat", "-n", "3"],
            vec!["--quiet", "--", "src/a.rs"],
            vec!["-z"],
            vec!["--", "--json"],
            vec!["--", "--mode=full"],
            vec!["--json=title"],
            vec![],
        ] {
            assert!(
                strip_git_view_flags(&argv(&shape)).is_none(),
                "{shape:?} carries no skim view flag before `--`, so the filter \
                 must be inert"
            );
        }
    }

    /// Both `--mode` spellings are dropped, and the space form takes its value.
    ///
    /// Leaving the value behind is the same defect one token later: git reads
    /// the orphan `full` as a revision and fails with
    /// `fatal: ambiguous argument 'full'`.
    #[test]
    fn strip_git_view_flags_drops_mode_with_its_value() {
        for (shape, expect_forwarded, expect_dropped) in [
            (
                vec!["--stat", "--mode=full"],
                vec!["--stat"],
                vec!["--mode=full"],
            ),
            (
                vec!["--stat", "--mode", "pseudo"],
                vec!["--stat"],
                vec!["--mode", "pseudo"],
            ),
            (
                vec!["--mode", "full", "--numstat", "HEAD~1..HEAD"],
                vec!["--numstat", "HEAD~1..HEAD"],
                vec!["--mode", "full"],
            ),
            // A `--mode` whose value is missing must not consume a later flag.
            (vec!["--stat", "--mode"], vec!["--stat"], vec!["--mode"]),
            (vec!["--mode", "--stat"], vec!["--stat"], vec!["--mode"]),
            (
                vec!["--stat", "--json", "--mode=full"],
                vec!["--stat"],
                vec!["--json", "--mode=full"],
            ),
        ] {
            let (forwarded, dropped) = strip_git_view_flags(&argv(&shape))
                .unwrap_or_else(|| panic!("{shape:?} must drop at least one token"));
            assert_eq!(
                forwarded,
                argv(&expect_forwarded),
                "forwarded for {shape:?}"
            );
            assert_eq!(dropped, argv(&expect_dropped), "dropped for {shape:?}");
        }
    }

    /// Everything at and after a bare `--` is forwarded verbatim.
    #[test]
    fn strip_git_view_flags_keeps_everything_after_the_separator() {
        let args = argv(&["--stat", "--mode=full", "--", "--json", "--mode=pseudo"]);
        let (forwarded, dropped) = strip_git_view_flags(&args).expect("must drop `--mode=full`");
        assert_eq!(
            forwarded,
            argv(&["--stat", "--", "--json", "--mode=pseudo"])
        );
        assert_eq!(dropped, argv(&["--mode=full"]));
    }

    /// The bound flags are NOT stripped — stripping them would breach ADR-016.
    ///
    /// No git handler implements `--max-lines` / `--tokens` / `--last-lines`
    /// (measured: `skim git log -n 2 --max-lines 10` is
    /// `fatal: ambiguous argument '10'` on `c2b4378` and on this branch, with
    /// or without a contract flag).  Dropping them would turn that hard error
    /// into a silently **unbounded** serve, which is precisely the "a bound the
    /// tool can exceed is not a bound" defect ADR-016 exists to prevent.
    ///
    /// Pinned from this side so a later "complete the set against
    /// `dispatch::strip_skim_flags`" edit has to argue with the reason rather
    /// than discover it.
    #[test]
    fn strip_git_view_flags_does_not_strip_the_bound_flags() {
        for shape in [
            vec!["--stat", "--max-lines", "10"],
            vec!["--stat", "--max-lines=10"],
            vec!["--stat", "--tokens", "500"],
            vec!["--stat", "--last-lines", "10"],
            vec!["--stat", "--line-numbers"],
        ] {
            assert!(
                strip_git_view_flags(&argv(&shape)).is_none(),
                "{shape:?}: these are not this filter's business — dropping them \
                 would serve unbounded output to a caller who asked for a bound"
            );
        }
    }
}
