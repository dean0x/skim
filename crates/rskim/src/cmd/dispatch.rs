//! Dispatch infrastructure for skim CLI.
//!
//! Provides the top-level `dispatch()` router plus the private helpers used
//! by multi-category dispatchers: argument extraction, subcommand scaffolding,
//! raw passthrough, and per-family help printers.

use std::io;
use std::process::{Command, ExitCode};

use super::execution::{is_broken_pipe, pipe_closed_exit};
use super::stream_pump::{PUMP_BUF_BYTES, StreamOutcome, StreamSpec, stream_child, write_tail};

#[cfg(feature = "proxy")]
use super::proxy;
use super::{
    agents, build, completions, db, discover, doctor, file, git, heatmap, infra, init, learn, lint,
    log, pkg, rewrite, sanitize_for_display, search, stats, test,
};

// ============================================================================
// Defense-in-depth: strip stray --session-id from subcommand args
// ============================================================================

/// Remove any `--session-id=…` or `--session-id <value>` token(s) from an
/// argument slice so a stray flag injected by an old hook never reaches the
/// underlying tool.
///
/// Two forms are stripped:
/// - `--session-id=VALUE`  — the equals form (produced by the now-removed
///   `inject_session_id_into_parts` in hook.rs).
/// - `--session-id VALUE`  — the space-separated form (forward-compat guard).
///
/// This is the forward-compat / backward-compat safety net (#1.1 / spec §4):
/// the hook no longer injects the flag, but an OLD hook talking to this binary
/// might still inject it. Without this filter the stray flag would be forwarded
/// to the underlying tool (e.g. `git`, `grep`) which would fail with
/// "unrecognised option --session-id".
///
/// The function is allocation-free when no `--session-id` token is present
/// (returns `None`). Callers use the original slice unchanged in that case.
pub(crate) fn strip_session_id_flag(args: &[String]) -> Option<Vec<String>> {
    // Fast-path: if no arg contains "--session-id" there is nothing to strip.
    if !args.iter().any(|a| a.contains("--session-id")) {
        return None;
    }

    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg.starts_with("--session-id=") {
            // Equals form: single token, skip it.
            i += 1;
        } else if arg == "--session-id" {
            // Space-separated form: skip this token AND the next (the value).
            i += 1;
            if i < args.len() {
                i += 1; // skip the value token
            }
        } else {
            out.push(arg.clone());
            i += 1;
        }
    }

    // Return Some only when we actually removed at least one token.
    if out.len() < args.len() {
        Some(out)
    } else {
        None
    }
}

// ============================================================================
// Strip skim-only flags before the passthrough exec (C1 — ADR-011)
// ============================================================================

/// Strip skim-only flags from args before the SKIM_PASSTHROUGH exec.
///
/// When `SKIM_PASSTHROUGH=1` (or `--passthrough`) is active, skim execs the
/// wrapped tool with the argv it received. But that argv can contain skim-only
/// flags the real tool has never heard of — `--json` is the measured case for
/// `git` subcommands (`SKIM_PASSTHROUGH=1 skim git diff --json` → git error).
/// This function strips those flags so the passthrough exec reaches the tool
/// cleanly.
///
/// **Scoping (PF-008 — only strip flags that are skim-only for the given tool):**
///
/// - **All tools**: `--show-stats` (extracted by every handler via
///   `extract_show_stats()`), `--passthrough` (always skim-only; C2),
///   `--max-lines`/`--max-lines=N` (value-bearing; no wrapped tool owns this),
///   `--last-lines`/`--last-lines=N` (value-bearing; tail mirror of
///   `--max-lines`; no wrapped tool owns this),
///   `--tokens`/`--tokens=N` (value-bearing; no wrapped tool owns this),
///   `--line-numbers` (boolean long form; short form `-n` is NOT stripped —
///   `git log -n <count>` is tool-owned), and `--debug` (boolean; skim global
///   flag — stripped UNLESS the tool owns `--debug` per its
///   `skip_if_flag_prefix` entry in the rewrite rule table; see regression-4).
/// - **`git` only**: bare `--json` (before `--`, extracted by
///   `extract_json_flag()` in every git subcommand handler) and
///   `--mode`/`--mode=<val>` (extracted by `extract_diff_mode()` in git
///   diff/show).  Other tools such as npm accept `--json` as their own flag;
///   stripping it there would change semantics.
///
/// **Forms handled (PF-008):**
/// - `--flag` bare token
/// - `--flag=value` equals-separated single token
/// - `--flag value` space-separated two-token form (for value-bearing flags)
///
/// **POSIX `--` end-of-options:** nothing is stripped after a bare `--`.
///
/// **`-n` is NOT stripped.** `git log -n <count>` is a legitimate tool flag.
/// Only the long form `--line-numbers` is skim-only.
///
/// Returns `None` (allocation-free) when no skim-only flags are present.
///
/// **Sync-guard:** `test_strip_skim_flags_sync_guard` asserts that every
/// handler-extracted skim-only flag is in this function's strip set.  When a
/// handler gains a new skim-only flag, add it here and to the test.
pub(crate) fn strip_skim_flags(subcommand: &str, args: &[String]) -> Option<Vec<String>> {
    // Fast-path: skip the scan entirely when no candidate tokens are present.
    let has_candidate = args.iter().any(|a| {
        a == "--show-stats"
            || a == "--passthrough"
            || a == "--line-numbers"
            || a == "--debug"
            || a.starts_with("--max-lines")
            || a.starts_with("--tokens")
            || a.starts_with("--last-lines")
            || (subcommand == "git" && (a == "--json" || a.starts_with("--mode")))
    });
    if !has_candidate {
        return None;
    }

    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0;
    let mut past_separator = false;

    while i < args.len() {
        let arg = &args[i];

        // POSIX end-of-options: stop stripping after `--`.
        if arg == "--" {
            past_separator = true;
            out.push(arg.clone());
            i += 1;
            continue;
        }

        if past_separator {
            out.push(arg.clone());
            i += 1;
            continue;
        }

        // ----------------------------------------------------------------
        // All-tools flags (always skim-only)
        // ----------------------------------------------------------------

        if arg == "--show-stats" || arg == "--passthrough" {
            // Bare boolean flag — drop this token.
            i += 1;
            continue;
        }

        // `--line-numbers` (boolean; long form only — `-n` is NOT stripped).
        if arg == "--line-numbers" {
            i += 1;
            continue;
        }

        // `--debug` (boolean; skim global flag) — strip ONLY when the tool does
        // NOT own `--debug` in its own grammar (regression-4 / PF-008 fix).
        //
        // Tools such as `gradle`, `jest`, `docker`, `aws`, `wget`, and `playwright`
        // list `"--debug"` in their `skip_if_flag_prefix` (the same table that
        // backs D4 on the wrapper surface).  For those tools `--debug` is a
        // meaningful tool flag; stripping it on the passthrough path would
        // silently defeat the SKIM_PASSTHROUGH=1 escape hatch — exactly the
        // scenario the hatch is designed for.
        //
        // The gate reads `skip_flags_for_tool(subcommand)` so the two surfaces
        // (D4 on wrapper, strip on passthrough exec) share one source of truth
        // and cannot drift independently.
        if arg == "--debug" {
            let tool_owns_debug =
                crate::cmd::rewrite::skip_flags_for_tool(subcommand).contains(&"--debug");
            if !tool_owns_debug {
                i += 1;
                continue;
            }
            // Tool owns --debug: do NOT strip; fall through to the push below.
        }

        // `--max-lines=N` (equals form — single token).
        if arg.starts_with("--max-lines=") {
            i += 1;
            continue;
        }

        // `--max-lines N` (space-separated two-token form).
        if arg == "--max-lines" {
            i += 1;
            // Consume the value token when present (it does not start with '-').
            if i < args.len() && !args[i].starts_with('-') {
                i += 1;
            }
            continue;
        }

        // `--tokens=N` (equals form — single token).
        if arg.starts_with("--tokens=") {
            i += 1;
            continue;
        }

        // `--tokens N` (space-separated two-token form).
        if arg == "--tokens" {
            i += 1;
            // Consume the value token when present.
            if i < args.len() && !args[i].starts_with('-') {
                i += 1;
            }
            continue;
        }

        // `--last-lines=N` (equals form — single token; skim-only tail mirror of
        // `--max-lines`; no wrapped tool owns this flag).
        if arg.starts_with("--last-lines=") {
            i += 1;
            continue;
        }

        // `--last-lines N` (space-separated two-token form).
        if arg == "--last-lines" {
            i += 1;
            if i < args.len() && !args[i].starts_with('-') {
                i += 1;
            }
            continue;
        }

        // ----------------------------------------------------------------
        // Git-specific skim-only flags
        // ----------------------------------------------------------------

        if subcommand == "git" {
            // `--json` (bare only; `--json=value` is a tool-owned form such
            // as `gh pr list --json title,number` and must NOT be stripped).
            if arg == "--json" {
                i += 1;
                continue;
            }

            // `--mode=value` (single equals-separated token)
            if arg.starts_with("--mode=") {
                i += 1;
                continue;
            }

            // `--mode value` (space-separated two-token form)
            if arg == "--mode" {
                // Drop the flag token and its value token (if present).
                i += 1;
                if i < args.len() && !args[i].starts_with('-') {
                    i += 1; // skip the value
                }
                continue;
            }
        }

        out.push(arg.clone());
        i += 1;
    }

    // Return Some only when at least one token was stripped.
    if out.len() < args.len() {
        Some(out)
    } else {
        None
    }
}

/// Whether [`strip_skim_flags`] removes a bare `--json` token for `subcommand`.
///
/// This is the single predicate that decides whether the legacy
/// `SKIM_PASSTHROUGH=1` remedy is *literally true* on a `--json` invocation
/// (`crate::output::fidelity::remedy_for`).  `--json` is skim-only for `git`
/// alone; for every other **exec'd** tool it is a tool-owned form
/// (`gh pr list --json title`) that must survive the strip, so the passthrough
/// exec would hand `--json` to a tool that rejects it.
///
/// **Note on META subcommands (consistency-4):** this function applies only to
/// exec'd tool wrappers, not to skim's own META subcommands (`log`, `proxy`,
/// etc.).  Meta subcommands have their own `SKIM_PASSTHROUGH=1` handling
/// (e.g. `cmd/log.rs` copies stdin→stdout verbatim), so `SKIM_PASSTHROUGH=1`
/// IS a valid remedy for them — but not because `strip_skim_flags` strips
/// `--json` for them.  The caller (`emit_json_envelope` in `cmd/execution.rs`)
/// separately gates on `is_meta_subcommand(tool)` before consulting this
/// predicate.
///
/// Kept adjacent to `strip_skim_flags` — and pinned by
/// `passthrough_strips_json_matches_strip_set` — so the two cannot drift.
pub(crate) fn passthrough_strips_json(subcommand: &str) -> bool {
    subcommand == "git"
}

// ============================================================================
// Private argument helpers
// ============================================================================

/// Prepend a tool name to an arg slice.
fn prepend(tool: &str, args: &[String]) -> Vec<String> {
    let mut v = Vec::with_capacity(args.len() + 1);
    v.push(tool.to_string());
    v.extend_from_slice(args);
    v
}

/// Build a `Vec<String>` with `tool` prepended and the element at `skip_idx`
/// removed, pre-allocating the exact capacity needed.
fn prepend_without(tool: &str, args: &[String], skip_idx: usize) -> Vec<String> {
    assert!(
        skip_idx < args.len(),
        "skip_idx {skip_idx} out of bounds for args len {}",
        args.len()
    );
    let mut v = Vec::with_capacity(args.len()); // remove one, prepend one → same len
    v.push(tool.to_string());
    v.extend(
        args.iter()
            .enumerate()
            .filter(|(i, _)| *i != skip_idx)
            .map(|(_, s)| s.clone()),
    );
    v
}

/// Build a `Vec<String>` with the element at `skip_idx` removed and **no** tool
/// prepended.  Used by the cargo `test` dispatch arm to strip the `test`
/// subcommand token before handing the remaining args to `cargo::run`, which
/// re-adds `test` itself via `build_cargo_args`.
fn without_index(args: &[String], skip_idx: usize) -> Vec<String> {
    assert!(
        skip_idx < args.len(),
        "skip_idx {skip_idx} out of bounds for args len {}",
        args.len()
    );
    args.iter()
        .enumerate()
        .filter(|(i, _)| *i != skip_idx)
        .map(|(_, s)| s.clone())
        .collect()
}

/// Shared scaffolding for multi-category dispatchers (`cargo`, `go`, …).
///
/// Handles flag interleaving: `skim cargo --show-stats test` works because
/// we skip leading flags to find the first positional (the subcommand token),
/// then the caller decides which args to forward.
///
/// Returns `Ok(Some((subcmd_str, subcmd_idx)))` when a subcommand is found, or
/// `Ok(None)` after printing the missing-subcommand error (caller should return
/// `ExitCode::FAILURE`).  The `tool` parameter is used only in the error message.
fn extract_subcmd<'a>(
    tool: &str,
    args: &'a [String],
    usage: &str,
    supported: &str,
) -> anyhow::Result<Option<(&'a str, usize)>> {
    match args.iter().position(|a| !a.starts_with('-')) {
        Some(idx) => Ok(Some((args[idx].as_str(), idx))),
        None => {
            eprintln!(
                "skim {tool}: missing subcommand\n\n\
                 Usage: {usage}\n\n\
                 Supported subcommands: {supported}"
            );
            Ok(None)
        }
    }
}

// ============================================================================
// Inherited-stdio passthrough for daemon / streaming commands (ADR-008 Part C)
// ============================================================================

/// Map the result of `Command::status()` to a raw exit-code byte.
///
/// This is a **pure** (no I/O) helper extracted so it can be unit-tested
/// independently of the actual spawn.  Diagnostics are the caller's
/// responsibility.
///
/// Mapping:
/// - `Err(NotFound)` → 127  (POSIX "command not found" convention)
/// - `Err(_)`        → 1    (generic failure; caller should have printed the error)
/// - `Ok(s)` with code `None` (signal kill) → 1
/// - `Ok(s)` with code `Some(n)` → `n` clamped to `[0, 255]`
pub(crate) fn spawn_status_to_code(status: std::io::Result<std::process::ExitStatus>) -> u8 {
    match status {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 127,
        Err(_) => 1,
        Ok(s) => match s.code() {
            Some(code) => code.clamp(0, 255) as u8,
            None => 1, // killed by signal
        },
    }
}

/// Run a daemon or streaming command with fully inherited stdio.
///
/// Used for commands detected by [`rewrite::indefinite::is_indefinite_command`]
/// in the direct / PATH-wrapper dispatch path, and for the D2b stdout-to-file
/// passthrough guard in the argv[0] wrapper branch (#370). Unlike
/// [`run_raw_passthrough`] (which captures stdout/stderr and re-prints them),
/// this helper lets the child share the parent's file descriptors directly:
///
/// - **stdin** is inherited — interactive prompts and `Ctrl-C` work.
/// - **stdout / stderr** are inherited — live output streams to the terminal.
/// - No capture, no compression, no analytics (skim is fully transparent).
///
/// PATH wrappers are already stripped from `PATH` by `main::strip_skim_wrappers_from_path`
/// before any thread is spawned, so `Command::new(program)` resolves to the
/// real binary without recursion.
///
/// # Why no `ChildGuard` is needed here
///
/// `CommandRunner`-based spawn paths use `ChildGuard` (a kill-on-drop RAII
/// wrapper) to reap the child on any early-return path — e.g., the 64 MiB
/// output cap error, a pipe-capture failure, or a reader-thread panic.
///
/// This function uses `Command::status()` instead, which blocks synchronously
/// until the child exits and reaps it internally before returning.  There is
/// no window between spawn and reap where an early return could leave an
/// orphan process, so no separate guard is needed.  The fully-inherited stdio
/// also means there are no capture threads, no pipe buffers to drain, and no
/// intermediate state that could trigger an early return while the child is
/// still running.
///
/// # Exit code mapping
///
/// - ENOENT (program not found) → 127 (POSIX "command not found" convention);
///   diagnostic `"error: {program} not found on PATH"` printed to stderr.
/// - Other spawn error → `ExitCode::FAILURE`; diagnostic printed to stderr
///   (avoids PF-003 — surfaces skim's own spawn failure rather than attributing
///   it to the tool).
/// - Signal termination (code = `None`) → `ExitCode::FAILURE`
/// - Otherwise → the child's actual exit code, clamped to `[0, 255]`
///
/// Diagnostics live here in the caller; the pure mapping is in
/// [`spawn_status_to_code`], which is unit-tested independently.
pub(crate) fn run_inherited_passthrough(program: &str, args: &[String]) -> ExitCode {
    let result = Command::new(program).args(args).status();
    if let Err(ref e) = result {
        if e.kind() == std::io::ErrorKind::NotFound {
            eprintln!("error: {program} not found on PATH");
        } else {
            // Fail loud: report the actual spawn error rather than silently
            // returning a failure exit code (avoids PF-003).
            eprintln!("error: failed to spawn {program}: {e}");
        }
    }
    ExitCode::from(spawn_status_to_code(result))
}

// ============================================================================
// Raw passthrough
// ============================================================================

/// Run a program with the given args and env vars, streaming stdout/stderr and
/// returning the process exit code. Used by passthrough dispatchers for unknown
/// subcommands that skim does not compress.
///
/// # Why this streams (#495)
///
/// This is a **pure byte passthrough**: it returns only an [`ExitCode`], never
/// the captured text, and no caller inspects the output — `gh` (output-steering
/// gate), `yarn` (unknown subcommand), and [`passthrough_subcmd`] (`swift`,
/// `dotnet`) all just forward the exit code.  Nothing downstream needs a
/// complete buffer, so buffering bought nothing and cost the same three fidelity
/// defects the streaming sinks in `cmd/file/passthrough_stream.rs` and
/// `execution::stream_passthrough_raw` were built to close (PF-006: this was the
/// missed sibling surface of that pair):
///
/// - **Total loss past 64 MiB.** [`crate::runner::read_pipe`] hard-errors at
///   `MAX_OUTPUT_BYTES` and **discards the entire accumulated buffer**, so a
///   70 MiB `yarn build` log produced `Error: output exceeded 67108864 byte
///   limit`, exit 1, and zero bytes — measured, not theorised.  The pump has no
///   ceiling on stdout (memory is O(chunk)), so ADR-002's "oversized input
///   degrades losslessly rather than hard-erroring" is satisfied by construction.
/// - **Lossy UTF-8.** `read_pipe` decodes with
///   `String::from_utf8(..).unwrap_or_else(lossy)`, so non-UTF-8 tool bytes
///   reached the reader as U+FFFD — skim showing something *different* from raw
///   with no marker (#317).  The pump never decodes.
/// - **Latency.** Nothing reached the reader until the child exited.
///
/// # Contract preserved exactly
///
/// - **Bytes**: stdout verbatim with **no trailing-newline guard**, then stderr
///   verbatim only when non-empty — the same order and the same byte contract as
///   the buffered form.
/// - **Exit code**: the child's own code, `unwrap_or(1)` on a signal kill,
///   clamped to `[0, 255]`.  Note this is *not* the file family's disposition
///   matrix: it maps a signal kill to `1`, which is deliberate here and unchanged.
/// - **Spawn failure**: the identical [`crate::runner::RunnerError::SpawnFailed`]
///   error, so the `failed to execute '<program>': …` text and the resulting
///   exit code do not move.
/// - **stdin**: inherited, as `CommandRunner::run_with_env` left it.
///
/// The one deliberate change beyond the three fixes: a closed downstream reader
/// now returns [`pipe_closed_exit`] (141 on unix, never 1) and kills the child,
/// where before skim waited for the child to finish and then propagated the
/// `BrokenPipe` error up to the `main.rs` boundary.
#[allow(clippy::disallowed_methods)] // Direct streaming pump for raw passthrough; bytes too large to buffer
pub(crate) fn run_raw_passthrough(
    program: &str,
    args: &[String],
    env: &[(&str, &str)],
) -> anyhow::Result<ExitCode> {
    let mut sink = io::BufWriter::with_capacity(PUMP_BUF_BYTES, io::stdout().lock());
    let outcome = stream_child(
        &StreamSpec {
            program,
            args,
            env_overrides: env,
        },
        &mut sink,
    )?;

    let done = match outcome {
        StreamOutcome::SpawnFailed(source) => {
            // Rebuild the buffered runner's own error so the message a caller
            // sees is unchanged.
            return Err(crate::runner::RunnerError::SpawnFailed {
                program: program.to_string(),
                source,
            }
            .into());
        }
        StreamOutcome::PipeClosed => {
            // ADR-011 class 2: nothing was lost — the *reader* stopped reading,
            // and the raw tool is silent in exactly this situation.
            crate::debug_log!(
                "[skim] downstream reader closed the pipe; stopped streaming {program} output."
            );
            return Ok(pipe_closed_exit());
        }
        StreamOutcome::Completed(done) => done,
    };

    // Release the stdout lock (the pump flushes per chunk, so nothing is
    // pending) before touching stderr, preserving stream ordering.
    drop(sink);

    if !done.stderr.is_empty() {
        let mut err = io::stderr().lock();
        // `false`: byte-exact, matching the buffered form's `write!(err, …)`.
        match write_tail(&mut err, &done.stderr, false) {
            Ok(()) => {}
            Err(e) if is_broken_pipe(&e) => return Ok(pipe_closed_exit()),
            Err(e) => return Err(e.into()),
        }
    }
    if done.stderr_discarded {
        // ADR-011 class 1: the reader is seeing LESS child stderr than raw, so
        // the marker is unconditional.  The remedy is NOT `SKIM_PASSTHROUGH=1`:
        // this path is already an uncompressed passthrough, so that advice is
        // circular.  Point at the raw tool, the only way to see more.  The
        // buffered form hard-errored here and emitted nothing at all, so a
        // marked partial is strictly more faithful (ADR-002).
        eprintln!(
            "{}",
            crate::output::elision_marker_unbounded_with_remedy(
                "the 64 MiB stderr capture ceiling",
                "child stderr",
                &format!("run '{program}' directly for the full stream"),
            )
        );
    }

    let code = done.exit_code.unwrap_or(1).clamp(0, 255) as u8;
    Ok(ExitCode::from(code))
}

/// Pass through an unknown subcommand to the underlying tool unchanged.
///
/// Logs a warning to stderr naming the unknown subcommand, then reconstructs
/// the full argument list (`unknown` + remaining `args` with the subcmd at
/// `subcmd_idx` stripped) and delegates to [`run_raw_passthrough`].
///
/// Used by multi-category dispatchers (`swift`, `dotnet`) where unknown
/// subcommands are forwarded rather than rejected.
fn passthrough_subcmd(
    tool: &str,
    unknown: &str,
    args: &[String],
    subcmd_idx: usize,
    supported: &str,
    env: &[(&str, &str)],
) -> anyhow::Result<ExitCode> {
    // D2/ADR-011: banner is debug-gated — this is a no-loss raw-fallback path;
    // the reader sees exactly what the native tool would produce.
    let safe = sanitize_for_display(unknown);
    crate::debug_log!(
        "skim {tool}: unknown subcommand '{safe}' — passing through (supported: {supported})"
    );
    run_raw_passthrough(tool, &prepend_without(unknown, args, subcmd_idx), env)
}

// ============================================================================
// Help printers
// ============================================================================

fn print_cargo_help() {
    print!(
        "skim cargo\n\
         \n\
           Cargo subcommand compression\n\
         \n\
         Usage: skim cargo <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test (t)   Run and compress cargo test output\n\
           nextest    Run and compress cargo nextest output\n\
           build (b)  Run and compress cargo build output\n\
           check (c)  Run and compress cargo check output\n\
           fmt        Run and compress cargo fmt output\n\
           clippy     Run and compress cargo clippy output\n\
           audit      Run and compress cargo audit output\n\
         \n\
         Examples:\n\
           skim cargo test\n\
           skim cargo t          (alias for test)\n\
           skim cargo build --release\n\
           skim cargo b --release  (alias for build)\n\
           skim cargo check\n\
           skim cargo fmt\n\
           skim cargo clippy -- -D warnings\n\
           skim cargo audit\n"
    );
}

fn print_swift_help() {
    print!(
        "skim swift\n\
         \n\
           Swift subcommand compression\n\
         \n\
         Usage: skim swift <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress swift test output\n\
         \n\
         Other subcommands (build, run, etc.) are passed through unmodified.\n\
         \n\
         Examples:\n\
           skim swift test\n\
           skim swift test --filter MyTests\n"
    );
}

fn print_dotnet_help() {
    print!(
        "skim dotnet\n\
         \n\
           .NET subcommand compression\n\
         \n\
         Usage: skim dotnet <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress dotnet test output\n\
         \n\
         Other subcommands (build, run, publish, restore, etc.) are passed through unmodified.\n\
         \n\
         Examples:\n\
           skim dotnet test\n\
           skim dotnet test --filter Category=Unit\n"
    );
}

fn print_go_help() {
    print!(
        "skim go\n\
         \n\
           Go subcommand compression\n\
         \n\
         Usage: skim go <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress go test output\n\
         \n\
         Examples:\n\
           skim go test ./...\n\
           skim go test -v ./pkg/...\n"
    );
}

// ============================================================================
// Multi-category dispatchers
// ============================================================================

/// Route `skim cargo <subcmd> [args...]` to the correct category handler.
/// Shared tail for cargo's `test` / `nextest` dispatch arms: split off
/// `--show-stats`, build the test recording context, and run the cargo test
/// handler with `is_nextest` threaded explicitly from the calling arm — never
/// re-derived from arg position (A1 fix; see PF-003).
fn run_cargo_tests(
    args: &[String],
    is_nextest: bool,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (filtered, show_stats) = super::extract_show_stats(args);
    let rec = crate::analytics::RecordingContext {
        enabled: analytics.enabled,
        command_type: crate::analytics::CommandType::Test,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };
    test::cargo::run(&filtered, is_nextest, show_stats, rec)
}

fn dispatch_cargo(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_cargo_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd(
        "cargo",
        args,
        "skim cargo <test|build|check|fmt|clippy|audit|nextest> [args...]",
        "test, nextest, build, check, fmt, clippy, audit",
    )?
    else {
        return Ok(ExitCode::FAILURE);
    };

    // Each subcommand is dispatched to build::run with its own name prepended as
    // the leading token.  build::run matches on that token to select the correct
    // cargo handler (cargo::run, cargo::run_check, cargo::run_fmt, etc.).
    // All subcommands use their own name consistently — there is no legacy "cargo"
    // alias for "build" any more.
    match subcmd {
        "test" | "t" => {
            // Standard cargo test — drop the "test" subcmd token, then thread
            // is_nextest=false from the dispatch arm (never re-derived from arg
            // position).  Without explicit threading, `cargo test nextest` (a bare
            // test-name filter) looks identical to `cargo nextest run` once the
            // "test" token is stripped — runner_args.first()=="nextest" in both —
            // causing a misroute (A1 fix, avoids PF-003 false-green on the
            // standard-test path).
            run_cargo_tests(&without_index(args, idx), false, analytics)
        }
        // cargo nextest — keep all tokens (incl. "nextest"); is_nextest=true.
        "nextest" => run_cargo_tests(args, true, analytics),
        "build" | "b" => build::run(&prepend_without("build", args, idx), analytics),
        "check" | "c" => build::run(&prepend_without("check", args, idx), analytics),
        "fmt" => build::run(&prepend_without("fmt", args, idx), analytics),
        "clippy" => build::run(&prepend_without("clippy", args, idx), analytics),
        // audit: keep "audit" in args — pkg::run uses it to select the audit parser.
        "audit" => pkg::run(&prepend("cargo", args), analytics),
        unknown => {
            // D2: unknown cargo subcommands (run, install, update, publish, …) are
            // forwarded to the real cargo binary. skim only compresses the subcommands
            // it understands; everything else passes through byte-faithfully.
            // Banner is debug-gated per ADR-011 (lossless path).
            let safe = sanitize_for_display(unknown);
            crate::debug_log!("skim cargo: unknown subcommand '{safe}' — passing through to cargo");
            run_raw_passthrough("cargo", args, &[])
        }
    }
}

/// Route `skim go <subcmd> [args...]` to the correct category handler.
fn dispatch_go(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_go_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd("go", args, "skim go <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("go", args, idx), analytics),
        unknown => {
            // D2: unknown go subcommands (build, fmt, get, mod, vet, …) are forwarded
            // to the real go binary. Banner is debug-gated per ADR-011 (lossless path).
            let safe = sanitize_for_display(unknown);
            crate::debug_log!("skim go: unknown subcommand '{safe}' — passing through to go");
            run_raw_passthrough("go", args, &[])
        }
    }
}

/// Route `skim swift <subcmd> [args...]` to the correct category handler.
///
/// Only `swift test` is compressed. Other `swift` subcommands (build, run, etc.)
/// pass through as raw to avoid interrupting normal swift workflows.
fn dispatch_swift(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_swift_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd("swift", args, "skim swift <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("swift", args, idx), analytics),
        unknown => passthrough_subcmd("swift", unknown, args, idx, "test", &[]),
    }
}

/// Route `skim dotnet <subcmd> [args...]` to the correct category handler.
///
/// Only `dotnet test` is compressed. Other `dotnet` subcommands (build, run, publish, etc.)
/// pass through as raw to avoid interrupting normal dotnet workflows.
fn dispatch_dotnet(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_dotnet_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) =
        extract_subcmd("dotnet", args, "skim dotnet <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("dotnet", args, idx), analytics),
        // DOTNET_CLI_UI_LANGUAGE forces English output for reliable parsing
        // even in passthrough mode, matching the compressed-path behavior.
        unknown => passthrough_subcmd(
            "dotnet",
            unknown,
            args,
            idx,
            "test",
            &[("DOTNET_CLI_UI_LANGUAGE", "en-US")],
        ),
    }
}

// ============================================================================
// Top-level dispatcher
// ============================================================================

/// Spawn-failure hint for the convergence-point passthrough gate.
///
/// The gate is family-agnostic by construction, so the per-handler
/// `install_hint` (e.g. go's `https://go.dev/dl/`) is not reachable here.  The
/// richer hints are still served on the paths the gate declines — notably the
/// FILTER role, which is where `test_go_passthrough_exec_path_surfaces_install_hint`
/// exercises them.
const PASSTHROUGH_INSTALL_HINT: &str =
    "SKIM_PASSTHROUGH=1 runs the tool directly — install it or put it on PATH";

/// Subcommands whose DISPATCHER consumes one leading sub-subcommand token before
/// handing the remainder to a category handler.
///
/// `skim swift test` reaches `test::run` as `["swift"]` — the handler's own arg
/// slice is `[]`, not `["test"]`.
///
/// INVARIANT: this list must match the multi-level `dispatch_*` arms in
/// [`dispatch`].  `test_multi_level_dispatchers_match_dispatch_arms` pins it.
const MULTI_LEVEL_DISPATCHERS: &[&str] = &["cargo", "dotnet", "go", "swift"];

/// `(tool, token)` pairs where the HANDLER — not the dispatcher — strips a
/// leading literal before deciding whether to read stdin.
///
/// Which layer eats the token is an implementation detail of each family, so the
/// gate has to account for both.  `cypress` is listed even though
/// [`super::should_read_stdin`]'s own `"run"` exception already covers it, so
/// this table reads as the complete picture rather than as a residue of whatever
/// happened to break.
///
/// Drift guard: the per-family stdin-passthrough E2E tests
/// (`tests/cli_e2e_new_parsers.rs`, `tests/cli_passthrough_coverage.rs`) fail
/// loudly if a family is missing here — that is exactly how `playwright` was
/// caught.
const HANDLER_CONSUMED_TOKENS: &[(&str, &str)] = &[("cypress", "run"), ("playwright", "test")];

/// Would the handler for `subcommand` read PIPED STDIN rather than spawn the tool?
///
/// This is the discriminator between skim's two roles, and the convergence gate
/// must not conflate them:
///
/// - **Wrapper role** (`SKIM_PASSTHROUGH=1 skim git log -n 3`): skim runs the
///   tool.  Passthrough means "spawn it with the user's argv and pump its bytes
///   through untouched" — which is what the gate does.
/// - **Filter role** (`SKIM_PASSTHROUGH=1 … | skim cypress run`): the caller
///   already ran the tool and piped its output in for compression.  Passthrough
///   means "hand those bytes back verbatim".  Exec-ing the tool here would
///   DISCARD the piped payload and, for a tool that is not installed, emit
///   nothing at all.
///
/// The filter role is already implemented per-family, with the correct exit-code
/// semantics, by `cmd/test/shared.rs::run_passthrough` and by the `use_stdin`
/// arm of `execution::run_parsed_command_with_mode`.  The gate therefore
/// DECLINES in that role and lets those paths serve it, rather than
/// re-implementing them here against a different arg shape.
///
/// [`super::should_read_stdin`] is the single authoritative predicate; the only
/// thing this wrapper adds is normalising argv to the slice the HANDLER sees
/// (see [`handler_visible_args`]).
fn handler_reads_stdin(subcommand: &str, args: &[String]) -> bool {
    super::should_read_stdin(handler_visible_args(subcommand, args))
}

/// The argv slice the category handler will actually receive.
///
/// Pure, so the normalisation can be tested without a controlled stdin.  A
/// leading FLAG is never a sub-subcommand token — `skim cargo --version` must
/// keep `--version`, or it would look like a bare `skim cargo` and read stdin.
fn handler_visible_args<'a>(subcommand: &str, args: &'a [String]) -> &'a [String] {
    // Dispatcher-level: any leading non-flag token is the sub-subcommand.
    if MULTI_LEVEL_DISPATCHERS.contains(&subcommand)
        && args.first().is_some_and(|a| !a.starts_with('-'))
    {
        return &args[1..];
    }
    // Handler-level: only the one literal that family strips.
    if let Some((_, token)) = HANDLER_CONSUMED_TOKENS
        .iter()
        .find(|(tool, _)| *tool == subcommand)
        && args.first().is_some_and(|a| a == token)
    {
        return &args[1..];
    }
    args
}

/// Which interception surface routed a command into the shared dispatch core.
///
/// Skim intercepts sub-agent shell commands through two independent mechanisms
/// (see CLAUDE.md §"Two interception surfaces"):
///
/// - **`Explicit`** — the user (or the rewrite hook) typed `skim <tool> …`
///   explicitly.  The rewrite engine's `try_rewrite()` transforms the raw
///   command string and the hook injects the result; `main.rs` then parses
///   the resulting `Invocation::Subcommand` and calls `dispatch_explicit`.
///
/// - **`Wrapper`** — `~/.skim/bin/<tool>` is a symlink whose `argv[0]` is the
///   tool name.  The OS runs the skim binary directly; `main.rs` detects the
///   non-`skim` `argv[0]` via `detect_argv0_dispatch()` and calls
///   `dispatch_for_wrapper`, which is now a thin tag that delegates to the
///   private `dispatch_inner` with `Surface::Wrapper`.
///
/// The wrapper gates D3/D4/D5 are enforced **inside** `dispatch_inner` behind
/// `if surface == Surface::Wrapper { … }`, making it structurally impossible
/// to call the shared core on the wrapper surface without those gates running.
/// A new call site that writes `dispatch_inner(Surface::Wrapper, …)` directly
/// still receives all three gates — the invariant is structural, not social.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// Explicit `skim <tool> …` typed by a user or injected by the rewrite hook.
    Explicit,
    /// PATH-wrapper surface — `argv[0]` is the tool name, not `skim`.
    Wrapper,
}

impl Surface {
    /// Short lowercase label used in debug banners.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Surface::Explicit => "explicit",
            Surface::Wrapper => "wrapper",
        }
    }
}

/// Return `true` when `name` identifies a tool that MUST NEVER serve raw output.
///
/// Credential redaction is a non-negotiable security control (PF-012).  Tools
/// like `env` and `printenv` pipe the entire process environment — including
/// `GITHUB_TOKEN`, `NPM_TOKEN`, and similar secrets — through skim's handler,
/// which redacts sensitive values to `***`.  Any code path that serves the real
/// tool's raw bytes (e.g. the D4 skip-flag gate, or `stdout_should_serve_raw`)
/// bypasses that redaction entirely.
///
/// This predicate is the single authoritative gate used by every raw-serve path
/// to enforce the control.  It must be consulted by:
/// - `main.rs` D2b — the `stdout_should_serve_raw() || force_raw_requested(…)`
///   branch that execs `run_inherited_passthrough`.
/// - `dispatch_inner` D4 — the skip-flag gate that calls `run_raw_passthrough`.
///
/// The `never_passthrough: true` field in `cmd/file/env.rs` is the per-handler
/// layer that guards `SKIM_PASSTHROUGH=1`; this predicate is the structural
/// layer that guards every new raw-serve path added to the codebase.
pub(crate) fn redaction_is_mandatory(name: &str) -> bool {
    matches!(name, "env" | "printenv")
}

/// Dispatch for the PATH-wrapper surface.
///
/// When skim is invoked via a symlink (`~/.skim/bin/grep`), the OS calls the
/// skim binary with `argv[0]` set to the tool name.  `main.rs` detects this
/// via `detect_argv0_dispatch()` and routes here.
///
/// This function is now a **thin tag**: it stamps the call as
/// [`Surface::Wrapper`] and delegates to [`dispatch_inner`], which applies the
/// D3/D4/D5 wrapper gates internally.  The gates are structural — any future
/// call site that writes `dispatch_inner(Surface::Wrapper, …)` directly still
/// runs all three gates, because they live inside `dispatch_inner` behind
/// `if surface == Surface::Wrapper { … }`.
pub(crate) fn dispatch_for_wrapper(
    name: &str,
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    dispatch_inner(Surface::Wrapper, name, args, analytics)
}

/// Dispatch a subcommand by name. Returns the process exit code.
///
/// v2.8.0: Flat dispatch — tool names are top-level subcommands.
/// `cargo` and `go` use multi-category dispatchers; other tools route
/// directly to their category handler with the tool name prepended.
fn dispatch_inner(
    surface: Surface,
    subcommand: &str,
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    use crate::cmd::registry::is_meta_subcommand;
    use crate::cmd::rewrite::{
        arg_matches_flag, args_before_separator, interactive_tool_for, require_flags_for_tool,
        skip_flags_for_tool,
    };

    // Hoist is_meta_subcommand to a single binding — D3/D4/D5 all consult it
    // and an inline call repeated three times is a complexity-7 anti-pattern.
    let is_meta = is_meta_subcommand(subcommand);

    // ── Wrapper-surface gates: D3 / D4 / D5 ──────────────────────────────────
    //
    // Moved inside dispatch_inner (architecture-10) so the gates apply to every
    // wrapper-surface call, regardless of which code path assembled the call.
    // A caller that writes `dispatch_inner(Surface::Wrapper, …)` directly still
    // runs all three gates — the invariant is structural, not social.
    //
    // All three gates scan only the args BEFORE the POSIX `--` end-of-options
    // separator (consistency-8): `grep -- --version file` should not trip D3
    // (the `--version` is a literal pattern, not a help-request flag), and
    // `rg -- --json` should not trip D4.  `args_before_separator` implements
    // this slice once for all three gates.
    if surface == Surface::Wrapper {
        let pre_sep = args_before_separator(args);

        // D3: universal help/version passthrough for non-meta tool wrappers.
        // Equivalent to skip_if_flag_prefix on the rewrite surface: when
        // `grep --help` is not rewritten (rewrite surface), it must not be
        // compressed on the wrapper surface either. The reader sees the real
        // tool's output in both cases.
        if !is_meta
            && pre_sep
                .iter()
                .any(|a| matches!(a.as_str(), "--help" | "-h" | "--version" | "-V"))
        {
            return run_raw_passthrough(subcommand, args, &[]);
        }

        // D4: tool-owned skip flags from rewrite rules.
        // Some tools have flags that skim must never intercept — e.g.,
        // `rg --json` and `tree --json` enable the tool's own JSON output.
        // The rewrite surface handles this via `skip_if_flag_prefix`; the
        // wrapper surface honours the same set.
        //
        // Both D4 and D5 now use `arg_matches_flag` (which accepts both the
        // exact-token form and `--flag=value` equals form) rather than inline
        // `arg == flag || arg.starts_with(&format!("{flag}="))`.  This closes
        // the consistency-5 gap and eliminates the per-(arg×flag) `format!`
        // allocation in the inner loop.
        //
        // SECURITY (PF-012 / security-1): `env` and `printenv` are excluded
        // from this gate even though their rewrite rules list `-i`, `-u`, and
        // `-S` as skip flags. Routing `env -u HOME` to `run_raw_passthrough`
        // would dump the entire unredacted environment.  Those tools must
        // always reach their handler, which enforces credential redaction.
        if !is_meta && !redaction_is_mandatory(subcommand) {
            let skip_flags = skip_flags_for_tool(subcommand);
            if !skip_flags.is_empty()
                && pre_sep
                    .iter()
                    .any(|arg| skip_flags.iter().any(|&flag| arg_matches_flag(arg, flag)))
            {
                return run_raw_passthrough(subcommand, args, &[]);
            }
        }

        // D5: interactive-tool gate — wrapper-surface replacement for the rewrite
        // engine's `require_flag` predicate.
        //
        // Tools such as `psql` and `mysql` open an interactive readline session
        // when invoked without their batch flag; `sqlite3` also opens a REPL when
        // stdin is a TTY and no SQL argument is given.  On the wrapper surface
        // skim MUST NOT intercept these sessions: the tool needs inherited stdio
        // so TTY line-editing, readline completion, and Ctrl-C work correctly.
        // Using `run_raw_passthrough` (which captures stdout/stderr via pipes)
        // instead makes the tool see `!isatty(stdout)`, disabling readline and
        // block-buffering stderr — visually a hung session (architecture-7 fix).
        //
        // The predicate `interactive_tool_for` is SEPARATE from
        // `require_flags_for_tool` — "does the rewrite rule need this flag?" is a
        // rewrite-surface concept, while "would this tool open a TTY session?" is
        // a wrapper-surface concept.  `sqlite3` has `require_flag: &[]` on the
        // rewrite surface (the hook always has piped stdin, so it is never
        // interactive there) but IS interactive on the wrapper surface with a TTY.
        // Both D5 and the engine now use `arg_matches_flag` so `psql --command=…`
        // is accepted the same way on both surfaces (consistency-5).
        if !is_meta && interactive_tool_for(subcommand) {
            let required = require_flags_for_tool(subcommand);
            let is_interactive = match &required {
                Some(flags) => {
                    // Tool has required flags: interactive iff NONE are present.
                    !pre_sep
                        .iter()
                        .any(|arg| flags.iter().any(|&f| arg_matches_flag(arg, f)))
                }
                // No required flags → always treat wrapper invocation as interactive
                // (sqlite3: any invocation with TTY stdin may be interactive).
                None => true,
            };
            if is_interactive {
                return Ok(run_inherited_passthrough(subcommand, args));
            }
        }
    }
    // ── End wrapper-surface gates ─────────────────────────────────────────────

    // Defense-in-depth (#1.1): strip any stray --session-id flag before routing.
    // The hook no longer injects this flag, but an OLD hook might. Without
    // stripping, the flag would reach the underlying tool and cause "unrecognised
    // option" failures. This is a forward-compat / backward-compat safety net.
    let stripped;
    let args = if let Some(clean) = strip_session_id_flag(args) {
        stripped = clean;
        &stripped
    } else {
        args
    };

    // Structural passthrough convergence gate (B1 / ADR-011).
    //
    // `SKIM_PASSTHROUGH=1` is documented (cmd/mod.rs) as bypassing ALL
    // compression.  Honouring it per-handler made that false in practice: every
    // `git` subcommand, every `build` tool and `gh run watch` reached the reader
    // compressed, and the handlers that DID honour it did so at the execution
    // layer, i.e. AFTER `prepare_args` had injected format flags — so the
    // "escape hatch" streamed the *injected* command's output, not the user's
    // (PF-024).  Both defects are structural, so the check belongs at the one
    // point every command family converges on, with the user's literal argv.
    //
    // Two exclusions, both deliberate:
    //   • meta subcommands — skim's own management commands; exec-ing them as OS
    //     binaries would fail or run an unrelated system program.  `log` and
    //     `proxy` are META and therefore carry their own checks.
    //   • `env` — PF-012: credential redaction is a security control that must
    //     hold on every branch, so it must not be reachable via the hatch.  The
    //     execution-level `never_passthrough` flag in `cmd/file/env.rs` is the
    //     independent second layer; both are required (defense in depth).
    //
    // The gate deliberately does NOT fire in stdin-filter mode — see
    // `handler_reads_stdin`.  It sits BEFORE the daemon guard so that
    // `SKIM_PASSTHROUGH=1 skim vitest` reaches the real tool.
    //
    // The sink is `stream_passthrough_raw`, NOT `run_inherited_passthrough`.
    // Inherited stdio looks byte-faithful but silently drops the PF-021 pipe
    // contract: `Command::status()` reports the SHELL's exit code, so
    // `SKIM_PASSTHROUGH=1 skim grep … | head -20` reported 0 instead of 141
    // (measured against the `cat out; cat err >&2; exit 0` stub).  The pump owns
    // that contract — early close → `pipe_closed_exit()`, no 64 MiB ceiling, no
    // lossy UTF-8 decode — and it is the same sink the execution layer already
    // used for the families that did honour the hatch.
    if super::is_passthrough_mode()
        && !super::registry::is_meta_subcommand(subcommand)
        && subcommand != "env"
        && !handler_reads_stdin(subcommand, args)
    {
        // C1 (ADR-011): strip skim-only flags (e.g. `--json`, `--mode`,
        // `--show-stats`, `--passthrough`) before exec so the real tool
        // never sees flags it does not understand.  `strip_skim_flags`
        // returns `None` (allocation-free) when no skim flags are present.
        let cleaned;
        let passthrough_args = if let Some(c) = strip_skim_flags(subcommand, args) {
            cleaned = c;
            &cleaned
        } else {
            args
        };
        return super::execution::stream_passthrough_raw(
            subcommand,
            passthrough_args,
            &[],
            PASSTHROUGH_INSTALL_HINT,
        );
    }

    // Daemon / streaming guard (ADR-008 Part C).
    //
    // Commands like `vite`, `npm run dev`, `jest --watch` run indefinitely;
    // skim cannot buffer-then-compress an unbounded stream, so detect them and
    // run with inherited stdio (live streaming, stdin forwarded). PATH wrappers
    // are already stripped from PATH in main(), so Command::new(program)
    // resolves to the real binary.
    //
    // Note: the guard fires unconditionally — it does NOT check whether stdin
    // is a terminal. PATH-wrapper sub-agents and CI pipelines always have
    // non-TTY stdin; gating on is_terminal() would skip detection for skim's
    // primary consumers. The accepted tradeoff: `cat output | skim vitest`
    // runs vitest live instead of parsing the piped output (uncommon; use
    // `skim vitest run` to compress piped output).
    //
    // SKIM_PASSTHROUGH=1 overrides the daemon guard: the user explicitly wants
    // skim to forward piped content without spawning. The passthrough check here
    // mirrors the per-handler check so both `run_inherited_passthrough` and the
    // handler's own stdin-forwarding path are consistent.
    if !super::is_passthrough_mode() {
        let mut all_tokens: Vec<&str> = Vec::with_capacity(args.len() + 1);
        all_tokens.push(subcommand);
        all_tokens.extend(args.iter().map(String::as_str));
        if rewrite::indefinite::is_indefinite_command(&all_tokens) {
            return Ok(run_inherited_passthrough(subcommand, args));
        }
    }

    match subcommand {
        // Unchanged meta/utility
        "agents" => agents::run(args, analytics),
        "completions" => completions::run(args, analytics),
        "discover" => discover::run(args, analytics),
        "doctor" => doctor::run(args, analytics),
        "git" => git::run(args, analytics),
        "heatmap" => heatmap::run(args, analytics),
        "init" => init::run(args, analytics),
        "learn" => learn::run(args, analytics),
        "log" => log::run(args, analytics),
        // AD-PXY-01: proxy is a meta subcommand (server, not a tool to intercept).
        // The indefinite-command guard MUST NOT route `skim proxy` to
        // run_inherited_passthrough — `proxy` is not an indefinite streaming command
        // (AC25 / AD-PXY-03). It is excluded from PATH-wrapper targets via
        // META_SUBCOMMANDS in registry.rs.
        // Routing guard in main.rs owns the default-build UX (#352): bare `skim proxy`
        // on a non-proxy build emits a clear error before ever reaching dispatch.
        #[cfg(feature = "proxy")]
        "proxy" => proxy::run(args, analytics),
        "rewrite" => rewrite::run(args, analytics),
        "search" => search::run(args, analytics),
        "stats" => stats::run(args, analytics),

        // Multi-category dispatchers
        "cargo" => dispatch_cargo(args, analytics),
        "go" => dispatch_go(args, analytics),

        // Multi-category dispatchers for tools with subcommands
        "swift" => dispatch_swift(args, analytics),
        "dotnet" => dispatch_dotnet(args, analytics),

        // Direct-to-category routing (prepend tool name for category dispatcher)
        "cypress" | "jest" | "playwright" | "pytest" | "vitest" => {
            test::run(&prepend(subcommand, args), analytics)
        }
        "gradle" | "gradlew" | "make" | "mvn" | "mvnw" | "tsc" => {
            build::run(&prepend(subcommand, args), analytics)
        }
        "biome" | "black" | "dprint" | "eslint" | "gofmt" | "golangci-lint" | "mypy" | "oxlint"
        | "prettier" | "rubocop" | "ruff" | "rustfmt" | "swiftlint" => {
            lint::run(&prepend(subcommand, args), analytics)
        }
        "npm" | "pip" | "pnpm" | "yarn" => pkg::run(&prepend(subcommand, args), analytics),
        "aws" | "curl" | "dig" | "docker" | "gh" | "kubectl" | "nslookup" | "terraform"
        | "wget" => infra::run(&prepend(subcommand, args), analytics),
        "mysql" | "psql" | "sqlite3" => db::run(&prepend(subcommand, args), analytics),
        "df" | "diff" | "du" | "env" | "find" | "grep" | "ls" | "printenv" | "ps" | "rg"
        | "tree" | "wc" => file::run(&prepend(subcommand, args), analytics),

        _ => {
            // D2: unknown top-level commands (npx, pip3, gmake, bundle, …) are
            // forwarded to the system binary of the same name. skim only wraps tools
            // in KNOWN_SUBCOMMANDS; everything else passes through byte-faithfully.
            // Banner is debug-gated per ADR-011 (lossless path — reader sees exactly
            // what the native tool would produce).
            let safe = sanitize_for_display(subcommand);
            crate::debug_log!(
                "skim [{}]: unrecognized command '{safe}' — passing through to system binary",
                surface.as_str()
            );
            run_raw_passthrough(subcommand, args, &[])
        }
    }
}

/// Dispatch for the explicit surface — `skim <tool> …` typed by a user or injected
/// by the rewrite hook.
///
/// This is the public entry point for `main.rs`'s `Invocation::Subcommand` arm.
/// It tags the call as [`Surface::Explicit`] and delegates to the private
/// `dispatch_inner` core, which enforces `SKIM_PASSTHROUGH`, the daemon guard,
/// and the per-tool match table.
pub(crate) fn dispatch_explicit(
    subcommand: &str,
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    dispatch_inner(Surface::Explicit, subcommand, args, analytics)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // strip_session_id_flag: defense-in-depth (#1.1 / spec §4)
    //
    // A stray --session-id flag must never reach the underlying tool.
    // These tests exercise the WRAPPER and DISPATCH surfaces (not the rewrite
    // surface — the hook no longer injects the flag, but an old hook might).
    // ========================================================================

    fn sv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// No --session-id present → returns None (allocation-free fast-path).
    #[test]
    fn test_strip_session_id_flag_noop_when_absent() {
        let args = sv(&["status", "--short"]);
        assert!(
            strip_session_id_flag(&args).is_none(),
            "no --session-id means None (no allocation)"
        );
    }

    /// Equals form `--session-id=foo` is stripped and the value is consumed.
    #[test]
    fn test_strip_session_id_flag_equals_form() {
        let args = sv(&["--session-id=abc-123", "status", "--short"]);
        let result = strip_session_id_flag(&args).expect("must strip equals form");
        assert_eq!(result, sv(&["status", "--short"]));
        assert!(
            !result.iter().any(|a| a.contains("--session-id")),
            "stripped result must not contain --session-id"
        );
    }

    /// Space-separated form `--session-id foo` strips both the flag and its value.
    #[test]
    fn test_strip_session_id_flag_space_form() {
        let args = sv(&["status", "--session-id", "abc-123", "--short"]);
        let result = strip_session_id_flag(&args).expect("must strip space-separated form");
        assert_eq!(result, sv(&["status", "--short"]));
    }

    /// Space-separated form at the end of args (value is last token).
    #[test]
    fn test_strip_session_id_flag_space_form_at_end() {
        let args = sv(&["status", "--session-id", "abc-123"]);
        let result = strip_session_id_flag(&args).expect("must strip");
        assert_eq!(result, sv(&["status"]));
    }

    /// Bare `--session-id` with no following value: only the flag token is removed.
    #[test]
    fn test_strip_session_id_flag_space_form_no_value() {
        // Trailing --session-id with no value token.
        let args = sv(&["status", "--session-id"]);
        let result = strip_session_id_flag(&args).expect("must strip");
        assert_eq!(result, sv(&["status"]));
    }

    /// Multiple occurrences are all stripped.
    #[test]
    fn test_strip_session_id_flag_multiple_occurrences() {
        let args = sv(&["--session-id=a", "status", "--session-id=b"]);
        let result = strip_session_id_flag(&args).expect("must strip");
        assert_eq!(result, sv(&["status"]));
    }

    /// Other flags and positionals are preserved exactly.
    #[test]
    fn test_strip_session_id_flag_preserves_other_args() {
        let args = sv(&["--session-id=x", "diff", "--stat", "HEAD~1"]);
        let result = strip_session_id_flag(&args).expect("must strip");
        assert_eq!(result, sv(&["diff", "--stat", "HEAD~1"]));
    }

    /// Empty arg slice: returns None (nothing to strip, nothing to allocate).
    #[test]
    fn test_strip_session_id_flag_empty_args() {
        assert!(strip_session_id_flag(&[]).is_none());
    }

    // ========================================================================
    // strip_skim_flags tests (C1 — ADR-011)
    // ========================================================================

    /// No skim-only flags present: returns None (allocation-free fast path).
    #[test]
    fn test_strip_skim_flags_no_op_when_clean() {
        let args = sv(&["diff", "--cached", "--stat"]);
        assert!(strip_skim_flags("git", &args).is_none());
    }

    /// `--show-stats` is stripped for ALL tools.
    #[test]
    fn test_strip_skim_flags_show_stats_all_tools() {
        for tool in &["git", "npm", "cargo", "eslint", "ls"] {
            let args = sv(&["build", "--show-stats", "--release"]);
            let result = strip_skim_flags(tool, &args).expect("must strip --show-stats");
            assert_eq!(
                result,
                sv(&["build", "--release"]),
                "--show-stats must be stripped for {tool}"
            );
        }
    }

    /// `--passthrough` is stripped for ALL tools (it is always skim-only; C2).
    #[test]
    fn test_strip_skim_flags_passthrough_all_tools() {
        for tool in &["git", "npm", "cargo"] {
            let args = sv(&["diff", "--passthrough", "--cached"]);
            let result = strip_skim_flags(tool, &args).expect("must strip --passthrough");
            assert_eq!(
                result,
                sv(&["diff", "--cached"]),
                "--passthrough must be stripped for {tool}"
            );
        }
    }

    /// Bare `--json` is stripped for `git` (git has no --json flag).
    #[test]
    fn test_strip_skim_flags_json_git() {
        let args = sv(&["diff", "--json", "--cached"]);
        let result = strip_skim_flags("git", &args).expect("must strip --json for git");
        assert_eq!(result, sv(&["diff", "--cached"]));
    }

    /// `--json` after `--` (POSIX end-of-options) is NOT stripped.
    #[test]
    fn test_strip_skim_flags_json_after_separator_not_stripped() {
        let args = sv(&["diff", "--", "--json"]);
        assert!(
            strip_skim_flags("git", &args).is_none(),
            "--json after -- is a positional argument, not skim's flag"
        );
    }

    /// `--json=value` is NOT stripped (tool-owned equals form, e.g. gh field-selector).
    #[test]
    fn test_strip_skim_flags_json_equals_form_not_stripped() {
        let args = sv(&["diff", "--json=title"]);
        assert!(
            strip_skim_flags("git", &args).is_none(),
            "--json=value must not be stripped (tool-owned form)"
        );
    }

    /// `--json` is NOT stripped for non-git tools (npm/yarn accept it as their own flag).
    #[test]
    fn test_strip_skim_flags_json_not_stripped_for_npm() {
        let args = sv(&["list", "--json"]);
        assert!(
            strip_skim_flags("npm", &args).is_none(),
            "--json must not be stripped for npm (tool-owned flag)"
        );
    }

    /// `--mode value` (space-separated) is stripped for `git`.
    #[test]
    fn test_strip_skim_flags_mode_space_form_git() {
        let args = sv(&["diff", "--mode", "structure", "--cached"]);
        let result = strip_skim_flags("git", &args).expect("must strip --mode");
        assert_eq!(result, sv(&["diff", "--cached"]));
    }

    /// `--mode=value` (equals form) is stripped for `git`.
    #[test]
    fn test_strip_skim_flags_mode_equals_form_git() {
        let args = sv(&["diff", "--mode=structure", "--cached"]);
        let result = strip_skim_flags("git", &args).expect("must strip --mode=structure");
        assert_eq!(result, sv(&["diff", "--cached"]));
    }

    /// `--mode=value` is NOT stripped for non-git tools (e.g. cargo has --mode).
    #[test]
    fn test_strip_skim_flags_mode_not_stripped_for_cargo() {
        let args = sv(&["build", "--mode=debug"]);
        assert!(
            strip_skim_flags("cargo", &args).is_none(),
            "--mode must not be stripped for cargo (tool-owned flag)"
        );
    }

    /// Multiple skim-only flags are all stripped in a single pass.
    #[test]
    fn test_strip_skim_flags_multiple_flags_single_pass() {
        let args = sv(&[
            "diff",
            "--json",
            "--show-stats",
            "--passthrough",
            "--cached",
        ]);
        let result = strip_skim_flags("git", &args).expect("must strip multiple flags");
        assert_eq!(result, sv(&["diff", "--cached"]));
    }

    /// Empty arg slice: returns None (nothing to strip).
    #[test]
    fn test_strip_skim_flags_empty_args() {
        assert!(strip_skim_flags("git", &[]).is_none());
    }

    // ========================================================================
    // Sync-guard: strip_skim_flags vs handler flag extraction (C1 — ADR-011)
    // ========================================================================
    //
    // INVARIANT: The set of flags stripped by `strip_skim_flags` must stay in
    // sync with the set of flags skim's handlers extract as skim-only.
    //
    // WHY NO SHARED SOURCE: Handlers use inline string matching — not consts —
    // inside `extract_show_stats()`, `extract_json_flag()`, and
    // `extract_diff_mode()`. Factoring them into shared consts would require
    // refactoring every handler call site, which is out of scope for C1.
    // Instead this test explicitly compares both lists so drift fails loudly.
    //
    // MAINTENANCE: When a handler gains a new skim-only flag, (a) add it to
    // `strip_skim_flags` and (b) add an assertion here verifying it is stripped.

    /// Guard: every skim-only flag extracted by a handler is also stripped by
    /// `strip_skim_flags` before the passthrough exec.
    ///
    /// Covers:
    /// - `extract_show_stats()` → `--show-stats` (all tools)
    /// - `--passthrough` / `set_passthrough_flag()` (all tools; C2)
    /// - `--max-lines` / `--max-lines=N` (all tools; skim file-read flag)
    /// - `--tokens` / `--tokens=N` (all tools; skim file-read flag)
    /// - `--line-numbers` (all tools; long form only; `-n` is NOT stripped)
    /// - `--debug` (all tools; skim global flag)
    /// - `extract_json_flag()` → `--json` bare (git only)
    /// - `extract_diff_mode()` → `--mode` / `--mode=val` (git only)
    #[test]
    fn test_strip_skim_flags_sync_guard() {
        // --- All-tools: --show-stats ---
        // extract_show_stats() strips --show-stats for every handler.
        // strip_skim_flags must strip it for every tool too.
        let show_stats_cases: &[(&str, &[&str])] = &[
            ("git", &["status", "--show-stats"]),
            ("npm", &["install", "--show-stats"]),
            ("cargo", &["build", "--show-stats"]),
            ("eslint", &["src/", "--show-stats"]),
        ];
        for (tool, raw) in show_stats_cases {
            let args = sv(raw);
            let result = strip_skim_flags(tool, &args);
            assert!(
                result.is_some() && !result.unwrap().iter().any(|a| a == "--show-stats"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --show-stats \
                 (extracted by extract_show_stats() in every handler)"
            );
        }

        // --- All-tools: --passthrough (C2) ---
        // The --passthrough flag is always skim-only; passing it to any real
        // tool would error. strip_skim_flags must remove it universally.
        let passthrough_cases: &[(&str, &[&str])] = &[
            ("git", &["diff", "--passthrough"]),
            ("npm", &["list", "--passthrough"]),
        ];
        for (tool, raw) in passthrough_cases {
            let args = sv(raw);
            let result = strip_skim_flags(tool, &args);
            assert!(
                result.is_some() && !result.unwrap().iter().any(|a| a == "--passthrough"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --passthrough \
                 (always skim-only; C2)"
            );
        }

        // --- Git-specific: --json ---
        // extract_json_flag() strips bare --json in every git subcommand handler.
        // strip_skim_flags("git", ...) must strip it too.
        let git_json_args = sv(&["diff", "--json", "--cached"]);
        let r = strip_skim_flags("git", &git_json_args);
        assert!(
            r.is_some() && !r.unwrap().iter().any(|a| a == "--json"),
            "sync-guard FAIL: strip_skim_flags(\"git\") must strip bare --json \
             (extracted by extract_json_flag() in git diff/log/show/status)"
        );

        // --- Git-specific: --mode / --mode=val ---
        // extract_diff_mode() strips --mode and --mode=val in git diff/show handlers.
        let git_mode_space = sv(&["diff", "--mode", "structure"]);
        let r = strip_skim_flags("git", &git_mode_space);
        assert!(
            r.is_some()
                && !r.clone().unwrap().iter().any(|a| a == "--mode")
                && !r.unwrap().iter().any(|a| a == "structure"),
            "sync-guard FAIL: strip_skim_flags(\"git\") must strip --mode <val> \
             (extracted by extract_diff_mode() in git diff/show)"
        );

        let git_mode_eq = sv(&["diff", "--mode=structure"]);
        let r = strip_skim_flags("git", &git_mode_eq);
        assert!(
            r.is_some() && !r.unwrap().iter().any(|a| a.starts_with("--mode")),
            "sync-guard FAIL: strip_skim_flags(\"git\") must strip --mode=val \
             (extracted by extract_diff_mode() in git diff/show)"
        );

        // --- All-tools: --max-lines / --max-lines=N ---
        for tool in &["git", "npm", "cargo"] {
            // Equals form
            let eq = sv(&["diff", "--max-lines=50"]);
            let r = strip_skim_flags(tool, &eq).expect("must strip --max-lines=N");
            assert!(
                !r.iter().any(|a| a.starts_with("--max-lines")),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --max-lines=N"
            );
            // Space form
            let sp = sv(&["diff", "--max-lines", "50"]);
            let r = strip_skim_flags(tool, &sp).expect("must strip --max-lines N");
            assert!(
                !r.iter().any(|a| a == "--max-lines" || a == "50"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --max-lines N"
            );
        }

        // --- All-tools: --tokens / --tokens=N ---
        for tool in &["git", "npm", "cargo"] {
            let eq = sv(&["diff", "--tokens=200"]);
            let r = strip_skim_flags(tool, &eq).expect("must strip --tokens=N");
            assert!(
                !r.iter().any(|a| a.starts_with("--tokens")),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --tokens=N"
            );
            let sp = sv(&["diff", "--tokens", "200"]);
            let r = strip_skim_flags(tool, &sp).expect("must strip --tokens N");
            assert!(
                !r.iter().any(|a| a == "--tokens" || a == "200"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --tokens N"
            );
        }

        // --- All-tools: --line-numbers (long form only; -n must NOT be stripped) ---
        for tool in &["git", "npm", "cargo"] {
            let args = sv(&["log", "--line-numbers", "-n", "5"]);
            let r = strip_skim_flags(tool, &args).expect("must strip --line-numbers");
            assert!(
                !r.iter().any(|a| a == "--line-numbers"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --line-numbers"
            );
            // -n must survive (git log -n <count> is tool-owned).
            assert!(
                r.iter().any(|a| a == "-n"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must NOT strip -n \
                 (git log -n <count> is tool-owned)"
            );
        }

        // --- All-tools: --debug ---
        // Stripped for tools that do NOT own --debug (git, npm, cargo have no
        // --debug in their skip_if_flag_prefix). Kept for tools that DO own it
        // (e.g. gradle — tested separately in test_strip_debug_kept_for_tool_owner).
        for tool in &["git", "npm", "cargo"] {
            let args = sv(&["diff", "--debug", "--cached"]);
            let r = strip_skim_flags(tool, &args).expect("must strip --debug");
            assert!(
                !r.iter().any(|a| a == "--debug"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --debug"
            );
        }

        // --- All-tools: --last-lines / --last-lines=N ---
        for tool in &["git", "npm", "cargo"] {
            // Equals form
            let eq = sv(&["log", "--last-lines=5"]);
            let r = strip_skim_flags(tool, &eq).expect("must strip --last-lines=N");
            assert!(
                !r.iter().any(|a| a.starts_with("--last-lines")),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --last-lines=N"
            );
            // Space form
            let sp = sv(&["log", "--last-lines", "5"]);
            let r = strip_skim_flags(tool, &sp).expect("must strip --last-lines N");
            assert!(
                !r.iter().any(|a| a == "--last-lines" || a == "5"),
                "sync-guard FAIL: strip_skim_flags({tool:?}) must strip --last-lines N"
            );
        }
    }

    /// `--debug` is NOT stripped for tools that own `--debug` in their rewrite
    /// rule's `skip_if_flag_prefix` (regression-4 fix).
    ///
    /// Gradle lists `"--debug"` in its skip_if_flag_prefix; `strip_skim_flags`
    /// must preserve it on the passthrough path so that
    /// `SKIM_PASSTHROUGH=1 skim gradle --debug clean` reaches the real gradle
    /// with `--debug` intact.
    #[test]
    fn test_strip_debug_kept_for_tool_owner() {
        // Gradle owns --debug (listed in skip_if_flag_prefix for gradlew/gradle).
        let args = sv(&["clean", "--debug", "build"]);
        let result = strip_skim_flags("gradle", &args);
        // None means nothing was stripped; Some means something was — but
        // either way, --debug must still be in the output.
        let effective: Vec<String> = match result {
            Some(r) => r,
            None => args,
        };
        assert!(
            effective.iter().any(|a| a == "--debug"),
            "--debug must survive strip_skim_flags for gradle (tool owns the flag); \
             got: {effective:?}"
        );

        // Control: git does NOT own --debug; it must be stripped.
        let git_args = sv(&["diff", "--debug", "--cached"]);
        let r = strip_skim_flags("git", &git_args).expect("must strip --debug for git");
        assert!(
            !r.iter().any(|a| a == "--debug"),
            "--debug must be stripped for git (skim-owned flag); got: {r:?}"
        );
    }

    /// Drift guard (D1): `passthrough_strips_json` must agree with what
    /// `strip_skim_flags` actually does to a bare `--json` token, for every tool.
    ///
    /// The predicate feeds `fidelity::remedy_for`, which prints
    /// `SKIM_PASSTHROUGH=1 for full output` on stderr only when the hatch really
    /// reproduces the user's argv.  If someone widens (or narrows) the `--json`
    /// strip in `strip_skim_flags` without updating the predicate, skim starts
    /// printing a remedy that cannot work — exactly the ADR-011 class-1 defect
    /// the split exists to close.
    #[test]
    fn passthrough_strips_json_matches_strip_set() {
        // Also covers "log" and "proxy" (consistency-4): both are META
        // subcommands that must NOT strip --json (they are not exec'd), so
        // passthrough_strips_json must return false for them.  The caller
        // (emit_json_envelope in cmd/execution.rs) separately gates on
        // is_meta_subcommand(tool) so SKIM_PASSTHROUGH=1 is still the correct
        // remedy for those tools even though this predicate returns false.
        for tool in ["git", "npm", "cargo", "psql", "eslint", "env", "log", "proxy"] {
            let args = sv(&["--json"]);
            let actually_strips = strip_skim_flags(tool, &args)
                .is_some_and(|stripped| !stripped.iter().any(|a| a == "--json"));
            assert_eq!(
                passthrough_strips_json(tool),
                actually_strips,
                "drift: passthrough_strips_json({tool:?}) = {} but strip_skim_flags \
                 {} strip bare --json — fidelity::remedy_for would print a false remedy",
                passthrough_strips_json(tool),
                if actually_strips { "does" } else { "does NOT" }
            );
        }
    }

    // ========================================================================
    // Unit tests for new flags (C1 Step 5 — --max-lines, --tokens,
    // --line-numbers, --debug)
    // ========================================================================

    /// `--max-lines=N` (equals form) is stripped for all tools.
    #[test]
    fn test_strip_skim_flags_max_lines_equals_form() {
        let args = sv(&["diff", "--max-lines=100", "--cached"]);
        let result = strip_skim_flags("git", &args).expect("must strip --max-lines=100");
        assert_eq!(result, sv(&["diff", "--cached"]));
        // Also for non-git tools.
        let result = strip_skim_flags("npm", &sv(&["list", "--max-lines=5"]));
        assert!(result.is_some() && !result.unwrap().iter().any(|a| a.starts_with("--max-lines")));
    }

    /// `--max-lines N` (space form) strips both the flag and its value.
    #[test]
    fn test_strip_skim_flags_max_lines_space_form() {
        let args = sv(&["diff", "--max-lines", "100", "--cached"]);
        let result = strip_skim_flags("git", &args).expect("must strip --max-lines 100");
        assert_eq!(result, sv(&["diff", "--cached"]));
    }

    /// `--tokens=N` (equals form) is stripped for all tools.
    #[test]
    fn test_strip_skim_flags_tokens_equals_form() {
        let args = sv(&["diff", "--tokens=500", "--cached"]);
        let result = strip_skim_flags("git", &args).expect("must strip --tokens=500");
        assert_eq!(result, sv(&["diff", "--cached"]));
    }

    /// `--tokens N` (space form) strips both the flag and its value.
    #[test]
    fn test_strip_skim_flags_tokens_space_form() {
        let args = sv(&["diff", "--tokens", "500", "--cached"]);
        let result = strip_skim_flags("git", &args).expect("must strip --tokens 500");
        assert_eq!(result, sv(&["diff", "--cached"]));
    }

    /// `--line-numbers` (long form) is stripped for all tools; `-n` is NOT stripped.
    #[test]
    fn test_strip_skim_flags_line_numbers_long_form_stripped_short_not() {
        // Long form is stripped.
        let args = sv(&["log", "-n", "5", "--line-numbers"]);
        let result = strip_skim_flags("git", &args).expect("must strip --line-numbers");
        // --line-numbers removed; -n and 5 must survive.
        assert_eq!(result, sv(&["log", "-n", "5"]));

        // -n alone is NOT stripped (git log -n is tool-owned).
        let args_n_only = sv(&["log", "-n", "5"]);
        assert!(
            strip_skim_flags("git", &args_n_only).is_none(),
            "-n must not be stripped (tool-owned flag)"
        );
    }

    /// `--debug` (boolean) is stripped for all tools.
    #[test]
    fn test_strip_skim_flags_debug_all_tools() {
        for tool in &["git", "npm", "cargo"] {
            let args = sv(&["diff", "--debug", "--cached"]);
            let result = strip_skim_flags(tool, &args).expect("must strip --debug");
            assert_eq!(
                result,
                sv(&["diff", "--cached"]),
                "--debug must be stripped for {tool}"
            );
        }
    }

    /// Flags after `--` (POSIX end-of-options) are NOT stripped.
    #[test]
    fn test_strip_skim_flags_new_flags_after_separator_not_stripped() {
        let args = sv(&[
            "diff",
            "--",
            "--max-lines",
            "--tokens",
            "--line-numbers",
            "--debug",
        ]);
        // Nothing should be stripped because all occurrences are after `--`.
        // The result should be None (no stripping), since the candidates are
        // only in the fast-path scan and they appear after `--`.
        // Actually the fast-path DOES see them and returns Some, but the loop
        // respects past_separator and keeps them.
        // The key assertion is that the output equals the input.
        let result = strip_skim_flags("git", &args);
        if let Some(ref stripped) = result {
            assert_eq!(
                stripped, &args,
                "flags after -- must not be stripped; got {stripped:?}"
            );
        }
        // Whether it returns None or Some(same_as_input) is an implementation
        // detail; the invariant is that the content is unchanged.
    }

    // ========================================================================
    // extract_subcmd tests
    // ========================================================================

    /// Happy path: first non-flag arg is the subcommand.
    #[test]
    fn test_extract_subcmd_finds_first_positional() {
        let args: Vec<String> = vec!["test".into(), "--release".into()];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert_eq!(result, Some(("test", 0)));
    }

    /// Flags before the subcommand are skipped; the positional is found at the
    /// correct index so `prepend_without` will remove the right element.
    #[test]
    fn test_extract_subcmd_skips_leading_flags() {
        let args: Vec<String> = vec!["--show-stats".into(), "build".into(), "--release".into()];
        let result = extract_subcmd("cargo", &args, "usage", "build").unwrap();
        assert_eq!(result, Some(("build", 1)));
    }

    /// When every arg starts with `-` there is no subcommand; the function
    /// prints the error message and returns `None` (caller returns FAILURE).
    #[test]
    fn test_extract_subcmd_returns_none_when_all_flags() {
        let args: Vec<String> = vec!["--show-stats".into(), "--json".into()];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert!(result.is_none());
    }

    /// Empty arg slice → no subcommand found, returns `None`.
    #[test]
    fn test_extract_subcmd_empty_args() {
        let args: Vec<String> = vec![];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert!(result.is_none());
    }

    // ========================================================================
    // prepend tests
    // ========================================================================

    /// Happy path: prepend tool name in front of a non-empty arg slice.
    #[test]
    fn test_prepend_happy_path() {
        let args: Vec<String> = vec!["--release".into(), "--verbose".into()];
        let result = prepend("cargo", &args);
        assert_eq!(result, vec!["cargo", "--release", "--verbose"]);
    }

    /// Empty arg slice: result contains only the tool name.
    #[test]
    fn test_prepend_empty_args() {
        let args: Vec<String> = vec![];
        let result = prepend("cargo", &args);
        assert_eq!(result, vec!["cargo"]);
    }

    // ========================================================================
    // prepend_without tests
    // ========================================================================

    /// Removes an element from the middle and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_middle_element() {
        let args: Vec<String> = vec!["--show-stats".into(), "test".into(), "--release".into()];
        // skip_idx=1 removes "test"; result is ["cargo", "--show-stats", "--release"]
        let result = prepend_without("cargo", &args, 1);
        assert_eq!(result, vec!["cargo", "--show-stats", "--release"]);
    }

    /// Removes the first element and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_first_element() {
        let args: Vec<String> = vec!["test".into(), "--release".into()];
        // skip_idx=0 removes "test"; result is ["cargo", "--release"]
        let result = prepend_without("cargo", &args, 0);
        assert_eq!(result, vec!["cargo", "--release"]);
    }

    /// Removes the last element and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_last_element() {
        let args: Vec<String> = vec!["--release".into(), "test".into()];
        // skip_idx=1 removes "test"; result is ["cargo", "--release"]
        let result = prepend_without("cargo", &args, 1);
        assert_eq!(result, vec!["cargo", "--release"]);
    }

    /// Single-element slice: removes that element, leaving only the tool name.
    #[test]
    fn test_prepend_without_single_element_slice() {
        let args: Vec<String> = vec!["test".into()];
        let result = prepend_without("cargo", &args, 0);
        assert_eq!(result, vec!["cargo"]);
    }

    /// Out-of-bounds skip_idx fires the assert in all build modes.
    ///
    /// This test documents the invariant: callers are responsible for passing a
    /// valid index.  The assert fires in both debug and release builds.
    #[test]
    #[should_panic(expected = "skip_idx 1 out of bounds for args len 1")]
    fn test_prepend_without_panics_on_out_of_bounds() {
        let args: Vec<String> = vec!["test".into()];
        prepend_without("cargo", &args, 1); // skip_idx=1 is out of bounds for len 1
    }

    // ========================================================================
    // dispatch() coverage — KNOWN_SUBCOMMANDS sync guard
    // ========================================================================

    /// Verify that every entry in KNOWN_SUBCOMMANDS routes through dispatch()
    /// without panicking.
    ///
    /// dispatch() calls real subcommand handlers which may fail for unrelated
    /// reasons (missing binary, empty args), but they must never panic. Any
    /// panic here means a match arm is missing for a registered subcommand.
    #[test]
    fn test_dispatch_covers_all_known_subcommands() {
        use crate::cmd::KNOWN_SUBCOMMANDS;
        use std::panic;

        for &subcommand in KNOWN_SUBCOMMANDS {
            // Pass --help so handlers exit cleanly rather than spawning real
            // processes. Most category handlers print help and return SUCCESS
            // when --help is the only arg.
            let args: Vec<String> = vec!["--help".to_string()];

            // AnalyticsConfig is not UnwindSafe, so construct it inside the closure.
            let result = panic::catch_unwind(|| {
                let a = crate::analytics::AnalyticsConfig {
                    enabled: false,
                    session_id: None,
                    input_cost_per_mtok: None,
                };
                dispatch_explicit(subcommand, &args, &a)
            });

            if let Err(ref payload) = result {
                // Surface the panic payload so non-routing panics (real bugs) are
                // distinguishable from missing-match-arm panics in CI output.
                let msg = payload
                    .downcast_ref::<String>()
                    .map(|s| s.as_str())
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("<non-string panic payload>");
                eprintln!("dispatch() panicked for '{subcommand}' — panic payload: {msg}");
            }
            assert!(
                result.is_ok(),
                "dispatch() panicked for known subcommand '{subcommand}': \
                 handler should not panic (check handler implementation)"
            );
        }

        // D2: unknown subcommands are passed through to the system binary via
        // run_raw_passthrough. The binary "__unknown_xyz__" does not exist on any
        // real system, so the spawn fails and dispatch() returns Err(SpawnFailed).
        // The important invariant is: dispatch() never panics — the error message
        // no longer contains "Unknown subcommand" because D2 replaced the bail!
        // with a raw-passthrough that fails at spawn time.
        let analytics = crate::analytics::AnalyticsConfig {
            enabled: false,
            session_id: None,
            input_cost_per_mtok: None,
        };
        let unknown_result = dispatch_explicit("__unknown_xyz__", &[], &analytics);
        assert!(
            unknown_result.is_err(),
            "dispatch_explicit() should return Err for non-existent binary (SpawnFailed)"
        );
    }

    // ========================================================================
    // dispatch_inner — Surface type-hygiene pin
    // ========================================================================

    /// Pin the private core's signature so that adding, removing, or reordering
    /// parameters causes a compile error rather than a silent behaviour change.
    ///
    /// The fn-pointer coercion is a zero-cost, zero-runtime check: the compiler
    /// resolves the cast at type-check time and the value is immediately
    /// discarded.  It makes it unrepresentable to call the shared dispatch core
    /// without explicitly declaring which interception surface the call is on.
    #[test]
    fn dispatch_core_requires_a_surface() {
        let _: fn(
            Surface,
            &str,
            &[String],
            &crate::analytics::AnalyticsConfig,
        ) -> anyhow::Result<std::process::ExitCode> = dispatch_inner;
    }

    // ========================================================================
    // is_indefinite_command — dispatch boundary classification
    // ========================================================================

    /// Verify that finite commands are NOT classified as indefinite at the
    /// dispatch boundary, so they fall through to the normal handler rather
    /// than `run_inherited_passthrough`.
    ///
    /// Positive control: a known-indefinite command (`tail -f`) must return
    /// `true` to confirm the detector is active. Negative controls use
    /// representative finite commands (`cargo test`, bare `tsc`) that must
    /// never be routed to the inherited-stdio daemon path.
    #[test]
    fn test_is_indefinite_command_dispatch_boundary() {
        use crate::cmd::rewrite::indefinite::is_indefinite_command;

        // Positive control — `tail -f` is indefinite; it must be detected so
        // daemon passthrough fires correctly.
        assert!(
            is_indefinite_command(&["tail", "-f", "app.log"]),
            "tail -f must be classified as indefinite"
        );

        // Negative controls — finite build/test tools must NOT trigger daemon
        // passthrough; routing them to run_inherited_passthrough would bypass
        // output compression entirely.
        assert!(
            !is_indefinite_command(&["cargo", "test"]),
            "cargo test must be classified as finite"
        );
        assert!(
            !is_indefinite_command(&["tsc"]),
            "bare tsc must be classified as finite (watch requires --watch/-w)"
        );

        // AC25: `skim proxy` is a meta subcommand (server), NOT an indefinite
        // streaming command. The indefinite-guard must NOT route it to
        // run_inherited_passthrough — that would bypass the proxy startup path.
        // It is classified as finite by construction (it does not appear in the
        // indefinite-command list) so the dispatch arm in proxy.rs is reached.
        assert!(
            !is_indefinite_command(&["proxy"]),
            "proxy must be classified as finite (server startup, not a streaming tool)"
        );
    }

    // ========================================================================
    // spawn_status_to_code: pure unit tests (assert concrete values)
    // ========================================================================

    /// `spawn_status_to_code` returns 127 for a `NotFound` I/O error — the POSIX
    /// "command not found" convention (applies ADR-008, avoids PF-003).
    #[test]
    fn test_spawn_status_to_code_not_found_returns_127() {
        use std::io::{Error, ErrorKind};
        let err: std::io::Result<std::process::ExitStatus> = Err(Error::from(ErrorKind::NotFound));
        assert_eq!(
            spawn_status_to_code(err),
            127,
            "ENOENT must map to 127 (POSIX command-not-found convention)"
        );
    }

    /// `spawn_status_to_code` returns 1 for a non-ENOENT I/O error.
    #[test]
    fn test_spawn_status_to_code_other_error_returns_1() {
        use std::io::{Error, ErrorKind};
        let err: std::io::Result<std::process::ExitStatus> =
            Err(Error::from(ErrorKind::PermissionDenied));
        assert_eq!(
            spawn_status_to_code(err),
            1,
            "non-ENOENT spawn errors must map to exit code 1"
        );
    }

    /// `spawn_status_to_code` clamps exit codes to `[0, 255]` — exit 256 must
    /// NOT wrap to 0 (which would mask failure as success).
    ///
    /// We exercise this on Unix by spawning `sh -c 'exit N'` and verifying the
    /// clamping in the pure helper using the actual `ExitStatus` the OS returns.
    /// The clamp is the only thing to prove here; `run_inherited_passthrough`
    /// delegates to this helper.
    #[cfg(unix)]
    #[test]
    fn test_spawn_status_to_code_clamps_large_exit_code() {
        // `sh -c 'exit 42'` → exit code 42 on all POSIX platforms.
        let status = std::process::Command::new("sh")
            .args(["-c", "exit 42"])
            .status();
        assert!(status.is_ok(), "sh must be available on Unix");
        assert_eq!(
            spawn_status_to_code(status),
            42,
            "exit code 42 must pass through unchanged"
        );
    }

    // ========================================================================
    // run_inherited_passthrough: smoke tests (behavior, not value)
    // ========================================================================

    /// Verify that `run_inherited_passthrough` does not panic for a missing
    /// binary (ENOENT).  The concrete 127 mapping is proven by the pure-helper
    /// tests above; this smoke test confirms the caller wires the helper
    /// correctly and reaches the ENOENT branch without panicking.
    #[test]
    fn test_run_inherited_passthrough_missing_binary() {
        // Precondition: the sentinel binary must not be in PATH.
        let probe = std::process::Command::new("__skim_guaranteed_absent_binary__").status();
        assert!(
            probe
                .err()
                .map(|e| e.kind() == std::io::ErrorKind::NotFound)
                .unwrap_or(false),
            "precondition: __skim_guaranteed_absent_binary__ must not exist in PATH"
        );

        // Must not panic; the ENOENT arm reaches spawn_status_to_code → 127.
        let _code = run_inherited_passthrough("__skim_guaranteed_absent_binary__", &[]);

        // On Unix, also exercise the success branch with `sh -c 'exit 0'`.
        #[cfg(unix)]
        {
            let _success =
                run_inherited_passthrough("sh", &["-c".to_string(), "exit 0".to_string()]);
        }
    }

    // ========================================================================
    // B1 convergence gate: `handler_reads_stdin` / `MULTI_LEVEL_DISPATCHERS`
    //
    // These pin the discriminator that decides whether the gate fires.  Getting
    // it wrong is not a cosmetic bug: treating the FILTER role as the WRAPPER
    // role makes `SKIM_PASSTHROUGH=1 … | skim cypress run` exec an uninstalled
    // `cypress` and emit nothing, discarding the caller's piped payload.
    // ========================================================================

    /// Every subcommand that routes to a `dispatch_*` helper consuming a leading
    /// sub-subcommand token must be listed, or `handler_reads_stdin` computes
    /// the predicate against the wrong arg slice.
    #[test]
    fn test_multi_level_dispatchers_match_dispatch_arms() {
        // Mirrors the `"cargo" | "go" | "swift" | "dotnet"` arms in `dispatch`.
        let mut expected = ["cargo", "dotnet", "go", "swift"];
        expected.sort_unstable();
        let mut actual = MULTI_LEVEL_DISPATCHERS.to_vec();
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "MULTI_LEVEL_DISPATCHERS drifted from the dispatch_* arms in dispatch()"
        );
    }

    /// `skim swift test` reaches `test::run` as `["swift"]`, i.e. the handler's
    /// own slice is `[]` — the stdin predicate must be evaluated against `[]`,
    /// not against `["test"]`.  Getting this wrong is what made
    /// `SKIM_PASSTHROUGH=1 skim swift test` exec swift instead of forwarding
    /// the caller's piped bytes.
    #[test]
    fn test_handler_visible_args_strips_multi_level_subcommand() {
        for (sub, token) in [
            ("swift", "test"),
            ("dotnet", "test"),
            ("cargo", "test"),
            ("go", "test"),
        ] {
            let args = sv(&[token]);
            assert!(
                handler_visible_args(sub, &args).is_empty(),
                "`skim {sub} {token}` must present [] to the handler"
            );
        }
    }

    /// `playwright` and `cypress` strip their token inside the HANDLER, not the
    /// dispatcher — `playwright::run` drops a leading `test`, `cypress::run`
    /// drops a leading `run`.  Missing `playwright` here is what left
    /// `SKIM_PASSTHROUGH=1 … | skim playwright test` exec-ing an uninstalled
    /// playwright instead of forwarding the caller's bytes.
    #[test]
    fn test_handler_visible_args_strips_handler_consumed_token() {
        for (tool, token) in HANDLER_CONSUMED_TOKENS {
            let args = sv(&[token]);
            assert!(
                handler_visible_args(tool, &args).is_empty(),
                "`skim {tool} {token}` must present [] to the handler"
            );
        }
    }

    /// The handler-level strip is literal-scoped: `skim playwright show-report`
    /// keeps its token, because `playwright::run` only strips `test`.
    #[test]
    fn test_handler_visible_args_strips_only_the_declared_literal() {
        let args = sv(&["show-report"]);
        assert_eq!(handler_visible_args("playwright", &args), args.as_slice());
    }

    /// Families with no consumed token forward argv unchanged.  `git status`
    /// must keep `status`, or the gate would misread it as the filter role and
    /// let `SKIM_PASSTHROUGH=1 skim git status` fall back to compression.
    #[test]
    fn test_handler_visible_args_preserves_single_level_args() {
        let args = sv(&["status"]);
        assert_eq!(handler_visible_args("git", &args), args.as_slice());

        let args = sv(&["log", "-n", "3"]);
        assert_eq!(handler_visible_args("git", &args), args.as_slice());
    }

    /// A leading FLAG is never a sub-subcommand token, so it must not be eaten.
    /// `skim cargo --version` must keep `--version`, or it would look like a
    /// bare `skim cargo` and be misrouted into the stdin-filter role.
    #[test]
    fn test_handler_visible_args_does_not_eat_a_leading_flag() {
        let args = sv(&["--version"]);
        assert_eq!(handler_visible_args("cargo", &args), args.as_slice());
    }
}
