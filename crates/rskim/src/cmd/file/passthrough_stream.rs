//! Streamed raw-passthrough execution for the pure-passthrough file family (#495).
//!
//! The buffered sink in `cmd/execution.rs` reads a child's entire stdout into a
//! `String` and writes it once at the end.  For tools that compress, that is
//! required — the ADR-001 net-savings guard cannot compare a *complete* raw
//! string against a *complete* compressed one until both exist.  For the
//! pure-passthrough family there is no compressed view at all
//! (`parse_impl` always returns `ParseResult::RawPassthrough`), so buffering
//! bought nothing and cost the fidelity defects catalogued in
//! [`crate::cmd::stream_pump`] (PF-021).
//!
//! This module is the family's **policy** layer: notices, the trailing-newline
//! guard, the exit-disposition matrix, and analytics.  The mechanism — spawn,
//! concurrent stderr drain, byte pump, reap, and the ordering between them — is
//! [`crate::cmd::stream_pump::stream_child`], shared with the
//! `SKIM_PASSTHROUGH=1` escape hatch in `cmd/execution.rs` so the two sinks
//! cannot drift on the parts that are easy to get wrong.

use std::io::{self, BufWriter, Write};
use std::process::ExitCode;
use std::time::Instant;

use super::PassthroughSpec;
use crate::cmd::RunContext;
use crate::cmd::execution::{
    ExitDisposition, classify_exit, format_analytics_label, is_broken_pipe, pipe_closed_exit,
};
use crate::cmd::stream_pump::{
    PUMP_BUF_BYTES, StreamOutcome, StreamSpec, stream_child, write_tail,
};

/// Run a pure-passthrough file tool, streaming its stdout to skim's stdout.
///
/// Byte-, latency-, and exit-code-faithful to the raw tool.
///
/// # Exit contract
///
/// - Clean EOF → the child's own exit code (which is naturally `0` when the
///   producer finishes before the reader leaves).
/// - Downstream reader closed the pipe → [`pipe_closed_exit`] (`141` on unix,
///   `128 + SIGPIPE`).
///
/// **Never `1` on pipe closure.**  For `grep`/`rg`/`diff`, exit 1 is the wire
/// protocol for "no matches found", so exiting 1 because the reader went away
/// reports a false negative to anything inspecting `$?`.
///
/// # Analytics
///
/// A run cut short by a closed reader records **nothing** — a truncated run
/// would record misleading savings.  A completed run records byte counts where
/// the buffered path records BPE token counts.  Savings are exactly `0` on both
/// paths (raw *is* the body for this family), so the load-bearing number is
/// unchanged; only the raw/compressed magnitudes differ.  `--show-stats` is
/// deliberately routed to the buffered sink so the number skim *displays* stays
/// byte-for-byte what it was.
pub(super) fn run_passthrough_streamed(
    spec: &PassthroughSpec<'_>,
    args: &[String],
    ctx: &RunContext,
) -> anyhow::Result<ExitCode> {
    let start = Instant::now();

    // Parity with `ParseResult::emit_markers` on the buffered path: this family
    // always produces the RawPassthrough tier, so the notice is a constant.
    // Debug-gated (ADR-011 class 2 — nothing is lost).
    crate::debug_log!("[skim:notice] output passed through without parsing");

    let mut sink = BufWriter::with_capacity(PUMP_BUF_BYTES, io::stdout().lock());
    let outcome = stream_child(
        &StreamSpec {
            program: spec.program,
            args,
            env_overrides: &[],
        },
        &mut sink,
    )?;

    let done = match outcome {
        // Matches `execution::obtain_output`'s `is_spawn_error` branch.
        StreamOutcome::SpawnFailed(_) => {
            eprintln!("error: '{}' not found", spec.program);
            eprintln!("hint: {}", spec.install_hint);
            return Ok(ExitCode::FAILURE);
        }
        StreamOutcome::PipeClosed => {
            // ADR-011 class 2: nothing was lost — the *reader* stopped reading,
            // and raw `grep | head` is silent in exactly this situation, so an
            // unconditional notice would itself diverge from raw.
            crate::debug_log!(
                "[skim] downstream reader closed the pipe; stopped streaming {} output.",
                spec.program
            );
            // Deliberately skips analytics: a run truncated by the reader would
            // record misleading savings.
            return Ok(pipe_closed_exit());
        }
        StreamOutcome::Completed(done) => done,
    };

    // Same exit-disposition matrix as the buffered sink (#317).  For this family
    // both dispositions emit identical stdout bytes — raw either way — so the
    // disposition only steers the notice, the analytics tier, and the
    // trailing-newline guard.
    let disposition = classify_exit(done.exit_code, spec.expected_exit_codes);
    let unexpected = disposition == ExitDisposition::UnexpectedFailure;
    if unexpected {
        match done.exit_code {
            Some(code) => {
                // Lossless raw fallback — debug-gated banner (ADR-011 class 2).
                crate::debug_log!(
                    "[skim] {} exited {code}; raw output (not compressed).",
                    spec.program
                );
            }
            None => {
                // Loss-bearing: killed mid-write, so stdout may be partial.
                // Unconditional per ADR-011 class 1.
                eprintln!(
                    "[skim] {} killed by signal; output may be partial — SKIM_PASSTHROUGH=1 for raw output",
                    spec.program
                );
            }
        }
    }

    // The buffered sink routes an unexpected failure through `passthrough_raw`
    // (no trailing-newline guard) and everything else through
    // `emit_raw_passthrough` (guard on).  Reproduced exactly so the two sinks
    // are byte-identical for the same command — do not "simplify" this to a
    // constant without changing the buffered sink in the same commit.
    let ensure_newline = !unexpected;

    if ensure_newline && done.stdout_bytes > 0 && done.last_stdout_byte != Some(b'\n') {
        match sink.write_all(b"\n").and_then(|()| sink.flush()) {
            Ok(()) => {}
            Err(e) if is_broken_pipe(&e) => return Ok(pipe_closed_exit()),
            Err(e) => return Err(e.into()),
        }
    }
    drop(sink);

    if !done.stderr.is_empty() {
        // `forward_stderr: true` for the whole passthrough family: these parsers
        // consume only stdout, so child diagnostics must never be dropped.
        let mut err = io::stderr().lock();
        match write_tail(&mut err, &done.stderr, ensure_newline) {
            Ok(()) => {}
            Err(e) if is_broken_pipe(&e) => return Ok(pipe_closed_exit()),
            Err(e) => return Err(e.into()),
        }
    }
    if done.stderr_discarded {
        // ADR-011 class 1: the reader is seeing LESS child stderr than raw, so
        // the marker is unconditional and carries the SKIM_PASSTHROUGH=1 hint.
        // The buffered path hard-errors here instead (`read_pipe` fails past the
        // ceiling), so a marked partial is strictly more faithful.
        eprintln!(
            "{}",
            crate::output::elision_marker_unbounded(
                "the 64 MiB stderr capture ceiling",
                "child stderr"
            )
        );
    }

    let tier = if unexpected { "raw" } else { "passthrough" };
    crate::analytics::try_record_command_with_counts(
        crate::analytics::RecordingContext {
            enabled: ctx.analytics_enabled,
            command_type: crate::analytics::CommandType::FileOps,
            parse_tier: None,
            session_id: ctx.session_id.as_deref(),
        }
        .with_tier(tier),
        done.stdout_bytes,
        done.stdout_bytes,
        format_analytics_label("file", spec.program, &args.join(" ")),
        start.elapsed(),
    );

    Ok(ExitCode::from(
        done.exit_code.unwrap_or(1).clamp(0, 255) as u8
    ))
}
