//! Streamed raw-passthrough execution for the pure-passthrough file family (#495).
//!
//! The buffered sink in `cmd/execution.rs` reads a child's entire stdout into a
//! `String` and writes it once at the end.  For tools that compress, that is
//! required — the ADR-001 net-savings guard cannot compare a *complete* raw
//! string against a *complete* compressed one until both exist.  For the
//! pure-passthrough family there is no compressed view at all
//! (`parse_impl` always returns `ParseResult::RawPassthrough`), so buffering
//! bought nothing and cost three separate fidelity defects (PF-021):
//!
//! 1. **Silent data loss on slow producers.**  A reader that closed early
//!    received everything raw `grep` had already emitted and nothing from skim,
//!    because skim's single write took `EPIPE` and discarded the whole buffer.
//!    For grep, empty output IS the encoding of "no matches" — a #317
//!    compress-never-truncate violation that reads as a successful true
//!    negative.
//! 2. **Latency.**  Nothing reached the reader until the child exited.
//! 3. **Non-UTF-8 corruption.**  `runner::read_pipe` decodes with
//!    `String::from_utf8(..).unwrap_or_else(lossy)`, so non-UTF-8 bytes reached
//!    the reader as U+FFFD.
//!
//! This module is the streaming sink.  It is a **raw byte pump**, not a line
//! splitter: there is no parser to feed, so decoding would only reintroduce
//! defect 3.
//!
//! # DESIGN NOTE — stderr is drained concurrently (PF-023 / AD-STR-8)
//!
//! `CommandRunner::run_with_env` uses two reader threads, which makes the
//! pipe-full deadlock *structurally impossible*.  Streaming stdout on the
//! calling thread reintroduces it: if nobody drains stderr, the child blocks
//! once that pipe fills, stops writing stdout, and the stdout pump blocks
//! forever.  [`drain_capped`] therefore runs on its own thread, is started
//! **before the first stdout read**, and **never stops draining** — past its
//! ceiling it keeps reading and discarding rather than leaving the pipe full.
//!
//! # DESIGN NOTE — ChildGuard bounds the child's lifetime (ADR-008)
//!
//! The child is wrapped in [`ChildGuard`] **at spawn**.  skim imposes no
//! internal timeout, so kill-on-drop is the *only* thing that stops a child once
//! the reader has left.  Without it `skim grep -rn x / | head -20` would keep
//! scanning the filesystem after `head` exited, where raw `grep` dies at once.
//! On the early-close path the child is killed **before** the drain thread is
//! joined: the child may be blocked writing to a stdout pipe nobody is reading,
//! so its stderr would never reach EOF and the join would hang.

use std::io::{self, BufWriter, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use super::PassthroughSpec;
use crate::cmd::RunContext;
use crate::cmd::execution::{
    ExitDisposition, classify_exit, format_analytics_label, is_broken_pipe, pipe_closed_exit,
};
use crate::runner::{ChildGuard, MAX_OUTPUT_BYTES};

/// Chunk size for the stdout pump and the stderr drain (64 KiB).
///
/// Matched to a typical pipe-buffer size so a saturated producer is consumed in
/// whole-buffer reads, and to the `BufWriter` capacity below so a full chunk
/// bypasses the buffer entirely (`BufWriter` writes straight through when the
/// incoming slice is at least its capacity).
const PUMP_BUF_BYTES: usize = 64 * 1024;

/// Why the stdout pump stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
enum PumpStop {
    /// The child closed its stdout — the whole stream was delivered.
    Eof,
    /// The downstream reader closed the pipe; no further output can be
    /// delivered.  This is a normal end-of-consumption event, not a failure.
    PipeClosed,
}

/// What a completed [`pump`] delivered.
#[derive(Debug, Clone, Copy)]
#[must_use]
struct PumpReport {
    stop: PumpStop,
    /// Bytes successfully written downstream.
    bytes: usize,
    /// Last byte written, if any — drives the trailing-newline guard.
    last_byte: Option<u8>,
}

/// Copy every byte from `reader` to `writer`, flushing once per chunk.
///
/// # Why flush per chunk and not per line
///
/// Real `grep`/`find`/`ls` are block-buffered when stdout is a pipe, so
/// chunk-granular delivery *is* raw parity.  Flushing per line would add roughly
/// one syscall per output line (~90 k on a 6.9 MB `grep`) for no fidelity gain.
/// Flushing per chunk instead of never is what makes time-to-first-byte correct:
/// `read` returns as soon as *any* bytes are available, so a slow producer's
/// first line is written and flushed immediately rather than sitting in a 64 KiB
/// buffer until the producer catches up.
///
/// # Bounds
///
/// The loop is bounded by the child's stdout reaching EOF, which
/// [`ChildGuard`]'s kill-on-drop guarantees on every early-return path.  There
/// is no byte ceiling: memory is O(chunk) because each chunk is written out
/// before the next is read, so the 64 MiB `read_pipe` ceiling that the buffered
/// path needs does not apply here.
///
/// Read errors propagate, matching `runner::read_pipe`.  A `BrokenPipe` on the
/// *write* side is not an error — it is [`PumpStop::PipeClosed`].
fn pump(reader: &mut impl Read, writer: &mut impl Write) -> io::Result<PumpReport> {
    let mut chunk = vec![0u8; PUMP_BUF_BYTES];
    let mut bytes = 0usize;
    let mut last_byte = None;

    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            return Ok(PumpReport {
                stop: PumpStop::Eof,
                bytes,
                last_byte,
            });
        }
        match writer.write_all(&chunk[..n]).and_then(|()| writer.flush()) {
            Ok(()) => {
                bytes += n;
                last_byte = Some(chunk[n - 1]);
            }
            Err(e) if is_broken_pipe(&e) => {
                return Ok(PumpReport {
                    stop: PumpStop::PipeClosed,
                    bytes,
                    last_byte,
                });
            }
            Err(e) => return Err(e),
        }
    }
}

/// Read `reader` to EOF, keeping at most `limit` bytes.
///
/// Returns the retained bytes and whether anything was discarded.
///
/// **Never stops draining.**  Past `limit` the loop keeps reading and throwing
/// bytes away so the child's stderr pipe cannot fill — a collector that stopped
/// reading would deadlock the stdout pump (PF-023 / AD-STR-8).  Discarding is
/// loss-bearing and the caller must emit
/// [`crate::output::elision_marker_unbounded`] for it (ADR-011 class 1).
///
/// `limit` is parameterised for unit testing with sub-ceiling sizes, following
/// the `runner::read_pipe_degrade_impl(reader, limit)` precedent; the caller
/// passes [`MAX_OUTPUT_BYTES`].
///
/// A read error ends the drain with whatever was collected: this runs on a
/// detached collector thread with no channel to report through, and the stdout
/// stream — the data the reader actually asked for — is unaffected.
fn drain_capped<R: Read>(mut reader: R, limit: usize) -> (Vec<u8>, bool) {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; PUMP_BUF_BYTES];
    let mut discarded = false;

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return (kept, discarded),
            Ok(n) => {
                let room = limit.saturating_sub(kept.len());
                let take = room.min(n);
                if take > 0 {
                    kept.extend_from_slice(&chunk[..take]);
                }
                if take < n {
                    discarded = true;
                }
            }
            Err(_) => return (kept, discarded),
        }
    }
}

/// Write `bytes` to `out`, appending a newline when `ensure_trailing_newline`
/// is set and the payload is non-empty and does not already end with one.
///
/// Mirrors `execution::write_and_flush` so the streamed and buffered sinks
/// cannot drift on the trailing-newline guard.
fn write_tail(out: &mut impl Write, bytes: &[u8], ensure_trailing_newline: bool) -> io::Result<()> {
    out.write_all(bytes)?;
    if ensure_trailing_newline && !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        out.write_all(b"\n")?;
    }
    out.flush()
}

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

    // ChildGuard wraps the child AT SPAWN (ADR-008): on every early return
    // below, kill-on-drop is what stops a tool that would otherwise keep
    // working after the reader has gone.
    let mut child = match Command::new(spec.program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => ChildGuard(c),
        // Any spawn failure is reported as "not found" with the install hint,
        // matching `execution::obtain_output`'s `is_spawn_error` branch.
        Err(_) => {
            eprintln!("error: '{}' not found", spec.program);
            eprintln!("hint: {}", spec.install_hint);
            return Ok(ExitCode::FAILURE);
        }
    };

    // Parity with `ParseResult::emit_markers` on the buffered path: this family
    // always produces the RawPassthrough tier, so the notice is a constant.
    // Debug-gated (ADR-011 class 2 — nothing is lost).
    crate::debug_log!("[skim:notice] output passed through without parsing");

    // Started BEFORE the first stdout read — see the PF-023 design note above.
    let stderr_drain = child
        .0
        .stderr
        .take()
        .map(|err| std::thread::spawn(move || drain_capped(err, MAX_OUTPUT_BYTES)));

    let mut sink = BufWriter::with_capacity(PUMP_BUF_BYTES, io::stdout().lock());
    let report = match child.0.stdout.take() {
        Some(mut out) => pump(&mut out, &mut sink)?,
        None => PumpReport {
            stop: PumpStop::Eof,
            bytes: 0,
            last_byte: None,
        },
    };

    if report.stop == PumpStop::PipeClosed {
        // Kill BEFORE joining: the child may be blocked writing to a stdout pipe
        // nobody is reading, so its stderr would never reach EOF and the join
        // would hang.
        let _ = child.0.kill();
        if let Some(handle) = stderr_drain {
            let _ = handle.join();
        }
        // ADR-011 class 2: nothing was lost — the *reader* stopped reading, and
        // raw `grep | head` is silent in exactly this situation, so an
        // unconditional notice would itself diverge from raw.
        crate::debug_log!(
            "[skim] downstream reader closed the pipe; stopped streaming {} output.",
            spec.program
        );
        // Deliberately skips analytics: a run truncated by the reader would
        // record misleading savings.
        return Ok(pipe_closed_exit());
    }

    let status = child.0.wait()?;
    let (child_stderr, stderr_discarded) = stderr_drain
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    // Same exit-disposition matrix as the buffered sink (#317).  For this family
    // both dispositions emit identical stdout bytes — raw either way — so the
    // disposition only steers the notice, the analytics tier, and the
    // trailing-newline guard.
    let disposition = classify_exit(status.code(), spec.expected_exit_codes);
    let unexpected = disposition == ExitDisposition::UnexpectedFailure;
    if unexpected {
        match status.code() {
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

    if ensure_newline && report.bytes > 0 && report.last_byte != Some(b'\n') {
        match sink.write_all(b"\n").and_then(|()| sink.flush()) {
            Ok(()) => {}
            Err(e) if is_broken_pipe(&e) => return Ok(pipe_closed_exit()),
            Err(e) => return Err(e.into()),
        }
    }
    drop(sink);

    if !child_stderr.is_empty() {
        // `forward_stderr: true` for the whole passthrough family: these parsers
        // consume only stdout, so child diagnostics must never be dropped.
        let mut err = io::stderr().lock();
        match write_tail(&mut err, &child_stderr, ensure_newline) {
            Ok(()) => {}
            Err(e) if is_broken_pipe(&e) => return Ok(pipe_closed_exit()),
            Err(e) => return Err(e.into()),
        }
    }
    if stderr_discarded {
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
        report.bytes,
        report.bytes,
        format_analytics_label("file", spec.program, &args.join(" ")),
        start.elapsed(),
    );

    Ok(ExitCode::from(
        status.code().unwrap_or(1).clamp(0, 255) as u8
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that yields `remaining` bytes of `b'e'` and counts how many
    /// bytes it was actually asked for, so a test can prove the collector kept
    /// draining past its ceiling.
    struct CountingReader {
        remaining: usize,
        read_total: usize,
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.remaining);
            buf[..n].fill(b'e');
            self.remaining -= n;
            self.read_total += n;
            Ok(n)
        }
    }

    // ========================================================================
    // drain_capped — T5: the stderr ceiling
    //
    // Surface: pure-function tests over the collector. They exercise NEITHER
    // the rewrite engine NOR the PATH-wrapper dispatch front-end.
    // ========================================================================

    #[test]
    fn test_drain_capped_under_limit_keeps_everything() {
        let reader = CountingReader {
            remaining: 500,
            read_total: 0,
        };
        let (kept, discarded) = drain_capped(reader, 1_000);
        assert_eq!(kept.len(), 500);
        assert!(!discarded, "nothing was dropped, so no marker is warranted");
    }

    #[test]
    fn test_drain_capped_at_exact_limit_is_not_marked_as_discarded() {
        let reader = CountingReader {
            remaining: 1_000,
            read_total: 0,
        };
        let (kept, discarded) = drain_capped(reader, 1_000);
        assert_eq!(kept.len(), 1_000);
        assert!(
            !discarded,
            "exactly at the ceiling nothing is dropped — a marker here would be a false loss claim"
        );
    }

    #[test]
    fn test_drain_capped_over_limit_truncates_and_reports_loss() {
        let reader = CountingReader {
            remaining: 1_500,
            read_total: 0,
        };
        let (kept, discarded) = drain_capped(reader, 1_000);
        assert_eq!(kept.len(), 1_000, "retention is capped at the limit");
        assert!(
            discarded,
            "bytes were dropped — the caller must emit the unconditional \
             elision marker (ADR-011 class 1)"
        );
    }

    /// The collector must consume the pipe to EOF even after its ceiling is
    /// reached.  A collector that stopped reading would leave the child's stderr
    /// pipe full, blocking the child and deadlocking the stdout pump
    /// (PF-023 / AD-STR-8).
    #[test]
    fn test_drain_capped_keeps_reading_past_the_limit() {
        let mut reader = CountingReader {
            remaining: 300_000,
            read_total: 0,
        };
        let (kept, discarded) = drain_capped(&mut reader, 1_000);
        assert_eq!(kept.len(), 1_000);
        assert!(discarded);
        assert_eq!(
            reader.read_total, 300_000,
            "the collector must drain the whole pipe, not stop at its ceiling — \
             stopping is the PF-023 deadlock"
        );
        assert_eq!(reader.remaining, 0);
    }

    #[test]
    fn test_drain_capped_empty_reader() {
        let reader = CountingReader {
            remaining: 0,
            read_total: 0,
        };
        let (kept, discarded) = drain_capped(reader, 1_000);
        assert!(kept.is_empty());
        assert!(!discarded);
    }

    // ========================================================================
    // pump — byte fidelity and the pipe-closed disposition
    // ========================================================================

    /// A writer that accepts `accept` bytes and then fails with `BrokenPipe`,
    /// standing in for a downstream reader that went away mid-stream.
    struct ClosingWriter {
        accept: usize,
        written: Vec<u8>,
    }

    impl Write for ClosingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.written.len() >= self.accept {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_pump_copies_arbitrary_bytes_verbatim() {
        // Deliberately not valid UTF-8: the pump must never decode.
        let source: Vec<u8> = vec![0xff, 0xfe, b'a', 0x09, 0x80, b'\n'];
        let mut reader = source.as_slice();
        let mut sink = ClosingWriter {
            accept: usize::MAX,
            written: Vec::new(),
        };
        let report = pump(&mut reader, &mut sink).unwrap();

        assert_eq!(sink.written, source, "bytes must arrive verbatim");
        assert_eq!(report.stop, PumpStop::Eof);
        assert_eq!(report.bytes, source.len());
        assert_eq!(report.last_byte, Some(b'\n'));
    }

    #[test]
    fn test_pump_reports_pipe_closed_without_erroring() {
        let source = vec![b'x'; PUMP_BUF_BYTES * 3];
        let mut reader = source.as_slice();
        let mut sink = ClosingWriter {
            accept: 1,
            written: Vec::new(),
        };
        let report = pump(&mut reader, &mut sink).unwrap();

        assert_eq!(
            report.stop,
            PumpStop::PipeClosed,
            "a closed reader is a value the caller handles, never an Err — an Err \
             would surface as `Error: Broken pipe` and exit 1"
        );
        assert!(report.bytes < source.len(), "the stream stopped early");
    }

    #[test]
    fn test_pump_empty_stream_reports_no_last_byte() {
        let mut reader: &[u8] = b"";
        let mut sink = ClosingWriter {
            accept: usize::MAX,
            written: Vec::new(),
        };
        let report = pump(&mut reader, &mut sink).unwrap();
        assert_eq!(report.bytes, 0);
        assert_eq!(
            report.last_byte, None,
            "an empty stream must not trigger the trailing-newline guard"
        );
    }

    // ========================================================================
    // write_tail — trailing-newline parity with execution::write_and_flush
    // ========================================================================

    #[test]
    fn test_write_tail_appends_newline_when_missing() {
        let mut out = Vec::new();
        write_tail(&mut out, b"body", true).unwrap();
        assert_eq!(out, b"body\n");
    }

    #[test]
    fn test_write_tail_does_not_double_newline() {
        let mut out = Vec::new();
        write_tail(&mut out, b"body\n", true).unwrap();
        assert_eq!(out, b"body\n");
    }

    #[test]
    fn test_write_tail_leaves_empty_payload_empty() {
        let mut out = Vec::new();
        write_tail(&mut out, b"", true).unwrap();
        assert!(
            out.is_empty(),
            "an empty body must stay empty, not become \\n"
        );
    }

    #[test]
    fn test_write_tail_guard_off_is_byte_exact() {
        let mut out = Vec::new();
        write_tail(&mut out, b"body", false).unwrap();
        assert_eq!(
            out, b"body",
            "with the guard off the payload is byte-exact — this is the \
             `passthrough_raw` (unexpected-failure) shape"
        );
    }
}
