//! Shared byte pump for skim's streaming raw-passthrough sinks (#495).
//!
//! Two sinks stream a child's stdout straight to skim's stdout:
//!
//! | sink | byte contract |
//! |---|---|
//! | [`crate::cmd::file::passthrough_stream`] | the pure-passthrough file family (`grep`, `rg`, `find`, `ls`, `wc`, `df`, `du`, `ps`); reproduces `emit_raw_passthrough`'s trailing-newline guard |
//! | `execution::stream_passthrough_raw` | the `SKIM_PASSTHROUGH=1` escape hatch, for **every** family; reproduces `passthrough_raw`'s byte-exact contract (no trailing-newline guard on either stream) |
//!
//! They differ only in what they do *after* the stream ends — notices, the
//! trailing-newline guard, analytics.  The part that is easy to get wrong — the
//! spawn/drain/pump/reap ordering — lives here, once, so the two cannot drift.
//!
//! # Why streaming, and not the buffered runner
//!
//! [`crate::runner::read_pipe`] accumulates a child's whole pipe into a `String`
//! and, past [`MAX_OUTPUT_BYTES`], **hard-errors and discards the entire
//! buffer**.  It also decodes with `String::from_utf8(..).unwrap_or_else(lossy)`,
//! so non-UTF-8 bytes reach the reader as U+FFFD.  For a sink that has no
//! compressed view to build, buffering bought nothing and cost four separate
//! fidelity defects (PF-021):
//!
//! 1. **Silent data loss on slow producers.**  A reader that closed early got
//!    everything the raw tool had already emitted and nothing from skim, because
//!    skim's single write took `EPIPE` and threw the whole buffer away.
//! 2. **Latency.**  Nothing reached the reader until the child exited.
//! 3. **Non-UTF-8 corruption.**  See the lossy decode above.
//! 4. **Total loss past 64 MiB.**  The ceiling is a genuine zero-output path.
//!
//! Defect 4 is worst on the escape hatch: `SKIM_PASSTHROUGH=1` is what a user
//! runs *because* compressed output hid something, and past the ceiling it
//! returned nothing at all.  A byte pump has no ceiling on stdout — memory is
//! O(chunk) because each chunk is written out before the next is read — so
//! ADR-002's rule (an oversized input degrades losslessly instead of
//! hard-erroring) is satisfied by construction rather than by a degrade branch.
//!
//! This module is a **raw byte pump**, not a line splitter: there is no parser to
//! feed, so decoding would only reintroduce defect 3.
//!
//! # DESIGN NOTE — stderr is drained concurrently (PF-021 / AD-STR-8)
//!
//! [`crate::runner::CommandRunner::run_with_env`] uses two reader threads, which
//! makes the pipe-full deadlock *structurally impossible*.  Streaming stdout on
//! the calling thread reintroduces it: if nobody drains stderr, the child blocks
//! once that pipe fills, stops writing stdout, and the stdout pump blocks
//! forever.  [`drain_capped`] therefore runs on its own thread, is started
//! **before the first stdout read**, and **never stops draining** — past its
//! ceiling it keeps reading and discarding rather than leaving the pipe full.
//!
//! # DESIGN NOTE — ChildGuard bounds the child's lifetime (ADR-008)
//!
//! The child is wrapped in [`ChildGuard`] **at spawn**.  skim imposes no internal
//! timeout, so kill-on-drop is the *only* thing that stops a child once the
//! reader has left.  Without it `skim grep -rn x / | head -20` would keep
//! scanning the filesystem after `head` exited, where raw `grep` dies at once.
//!
//! # DESIGN NOTE — the early-close path DETACHES the drain, never joins it
//!
//! On the early-close path the child is killed and the drain thread is then
//! **abandoned, not joined**.  Joining there is unbounded: killing the child
//! closes only *its* copy of the stderr write end, and any grandchild that
//! inherited fd 2 keeps the pipe open, so `drain_capped` never reaches EOF.
//! `SKIM_PASSTHROUGH=1 skim cargo build | head` and `skim yarn <sub> | head`
//! are exactly that shape — cargo/yarn/npm spawn tool processes that inherit
//! stderr — and a join would block skim for the grandchild's whole lifetime
//! while raw `cargo build | head` returns at once.  ADR-008 forbids an internal
//! timeout, so the bound has to come from not waiting at all.
//!
//! Abandoning costs nothing: [`StreamOutcome::PipeClosed`] carries no payload,
//! so the drain's result was already discarded.  The detached thread holds at
//! most [`MAX_OUTPUT_BYTES`] and dies with the process.  This matches the
//! pre-existing precedent in `cmd::infra::gh::streaming`, whose early-return
//! write-failure paths also drop their stderr `JoinHandle` rather than join it.

use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use crate::cmd::execution::is_broken_pipe;
use crate::runner::{ChildGuard, MAX_OUTPUT_BYTES};

/// Chunk size for the stdout pump and the stderr drain (64 KiB).
///
/// Matched to a typical pipe-buffer size so a saturated producer is consumed in
/// whole-buffer reads, and to the `BufWriter` capacity callers use so a full
/// chunk bypasses the buffer entirely (`BufWriter` writes straight through when
/// the incoming slice is at least its capacity).
pub(crate) const PUMP_BUF_BYTES: usize = 64 * 1024;

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
/// [`ChildGuard`]'s kill-on-drop guarantees on every early-return path.  There is
/// **no byte ceiling**: memory is O(chunk) because each chunk is written out
/// before the next is read, so the [`MAX_OUTPUT_BYTES`] ceiling that the buffered
/// path needs does not apply here at all.
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
/// reading would deadlock the stdout pump (PF-021 / AD-STR-8).  Discarding is
/// loss-bearing and the caller must emit an unconditional elision marker for it
/// (ADR-011 class 1).
///
/// `limit` is parameterised for unit testing with sub-ceiling sizes, following
/// the `runner::read_pipe_degrade_impl(reader, limit)` precedent;
/// [`stream_child`] passes [`MAX_OUTPUT_BYTES`].
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

/// Write `bytes` to `out`, appending a newline when `ensure_trailing_newline` is
/// set and the payload is non-empty and does not already end with one.
///
/// Mirrors `execution::write_and_flush` so the streamed and buffered sinks cannot
/// drift on the trailing-newline guard.  Callers that must be byte-exact (the
/// `SKIM_PASSTHROUGH=1` escape hatch) pass `false`.
pub(crate) fn write_tail(
    out: &mut impl Write,
    bytes: &[u8],
    ensure_trailing_newline: bool,
) -> io::Result<()> {
    out.write_all(bytes)?;
    if ensure_trailing_newline && !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// What to spawn and stream.
pub(crate) struct StreamSpec<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    /// Environment overrides applied to the child, matching
    /// `CommandRunner::run_with_env`.  Pass `&[]` when there are none.
    pub env_overrides: &'a [(&'a str, &'a str)],
}

/// A run whose stdout reached the reader in full.
pub(crate) struct StreamCompleted {
    /// The child's exit code; `None` when it was killed by a signal.
    pub exit_code: Option<i32>,
    /// Bytes delivered downstream.
    pub stdout_bytes: usize,
    /// Last stdout byte delivered — drives the caller's trailing-newline guard.
    pub last_stdout_byte: Option<u8>,
    /// Child stderr, captured up to [`MAX_OUTPUT_BYTES`].
    pub stderr: Vec<u8>,
    /// `true` when stderr exceeded the capture ceiling and bytes were dropped.
    /// Loss-bearing: the caller **must** emit an unconditional elision marker
    /// (ADR-011 class 1).
    pub stderr_discarded: bool,
}

/// How a streamed run ended.
#[must_use]
pub(crate) enum StreamOutcome {
    /// The child could not be spawned.  The caller owns the diagnostic, which
    /// differs per family: the passthrough sinks print their own "not found"
    /// message plus an install hint and ignore the payload, while
    /// `dispatch::run_raw_passthrough` rebuilds `RunnerError::SpawnFailed` from
    /// it so its error text stays byte-identical to the buffered runner's.
    SpawnFailed(io::Error),
    /// The downstream reader closed the pipe mid-stream.  The child has already
    /// been killed and the drain thread joined; the caller must stop producing
    /// output and return `pipe_closed_exit()`.
    PipeClosed,
    /// The child ran to completion and its whole stdout was delivered.
    Completed(StreamCompleted),
}

/// Spawn `spec` and stream its stdout to `sink`, draining stderr concurrently.
///
/// This function owns every piece of ordering that is load-bearing for
/// correctness — see the two DESIGN NOTEs at the top of the module:
///
/// 1. [`ChildGuard`] wraps the child **at spawn**, so every early return kills it.
/// 2. The stderr drain thread starts **before the first stdout read**.
/// 3. On a closed reader the child is killed and the drain thread is **abandoned,
///    never joined** — a surviving grandchild that inherited fd 2 keeps the
///    stderr pipe open, so a join there has no bound.
///
/// `sink` is borrowed rather than owned so the caller can keep writing to the
/// same buffered stdout handle afterwards (the trailing-newline guard).  The pump
/// flushes per chunk, so nothing is left pending in the caller's buffer when this
/// returns.
pub(crate) fn stream_child(
    spec: &StreamSpec<'_>,
    sink: &mut impl Write,
) -> io::Result<StreamOutcome> {
    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for &(key, value) in spec.env_overrides {
        cmd.env(key, value);
    }

    // ChildGuard AT SPAWN (ADR-008): on every early return below, kill-on-drop is
    // what stops a tool that would otherwise keep working after the reader has
    // gone.
    let mut child = match cmd.spawn() {
        Ok(c) => ChildGuard(c),
        Err(e) => return Ok(StreamOutcome::SpawnFailed(e)),
    };

    // Started BEFORE the first stdout read — see the PF-021 design note above.
    let stderr_drain = child
        .0
        .stderr
        .take()
        .map(|err| std::thread::spawn(move || drain_capped(err, MAX_OUTPUT_BYTES)));

    let report = match child.0.stdout.take() {
        Some(mut out) => pump(&mut out, sink)?,
        None => PumpReport {
            stop: PumpStop::Eof,
            bytes: 0,
            last_byte: None,
        },
    };

    if report.stop == PumpStop::PipeClosed {
        // Kill the child, then ABANDON the drain thread — see the design note
        // above.  Killing closes only the child's own copy of the stderr write
        // end; a grandchild that inherited fd 2 (cargo -> rustc, yarn -> node)
        // keeps the pipe open, so `drain_capped` may never reach EOF and a join
        // here would block for the grandchild's whole lifetime while the raw
        // tool returns at once.  `StreamOutcome::PipeClosed` carries no payload,
        // so the drain's result was already discarded; dropping the handle
        // detaches the thread, which dies with the process.
        let _ = child.0.kill();
        drop(stderr_drain);
        return Ok(StreamOutcome::PipeClosed);
    }

    let status = child.0.wait()?;
    let (stderr, stderr_discarded) = stderr_drain
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    Ok(StreamOutcome::Completed(StreamCompleted {
        exit_code: status.code(),
        stdout_bytes: report.bytes,
        last_stdout_byte: report.last_byte,
        stderr,
        stderr_discarded,
    }))
}

// ============================================================================
// Unit tests
//
// SCOPE — pure-function tests over the pump primitives.  They exercise NEITHER
// the rewrite engine NOR the PATH-wrapper dispatch front-end; the e2e coverage
// of both streaming sinks lives in tests/cli_e2e_pipe_fidelity.rs.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that yields `remaining` bytes of `fill` and counts how many bytes
    /// it was actually asked for, so a test can prove a consumer kept reading
    /// past a ceiling.
    struct CountingReader {
        remaining: usize,
        read_total: usize,
        fill: u8,
    }

    impl CountingReader {
        fn new(remaining: usize, fill: u8) -> Self {
            Self {
                remaining,
                read_total: 0,
                fill,
            }
        }
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.remaining);
            buf[..n].fill(self.fill);
            self.remaining -= n;
            self.read_total += n;
            Ok(n)
        }
    }

    /// A writer that accepts `accept` bytes and then fails with `BrokenPipe`,
    /// standing in for a downstream reader that went away mid-stream.
    struct ClosingWriter {
        accept: usize,
        written: Vec<u8>,
    }

    impl ClosingWriter {
        fn open() -> Self {
            Self {
                accept: usize::MAX,
                written: Vec::new(),
            }
        }
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

    // ========================================================================
    // drain_capped — the stderr ceiling
    // ========================================================================

    #[test]
    fn test_drain_capped_under_limit_keeps_everything() {
        let (kept, discarded) = drain_capped(CountingReader::new(500, b'e'), 1_000);
        assert_eq!(kept.len(), 500);
        assert!(!discarded, "nothing was dropped, so no marker is warranted");
    }

    #[test]
    fn test_drain_capped_at_exact_limit_is_not_marked_as_discarded() {
        let (kept, discarded) = drain_capped(CountingReader::new(1_000, b'e'), 1_000);
        assert_eq!(kept.len(), 1_000);
        assert!(
            !discarded,
            "exactly at the ceiling nothing is dropped — a marker here would be a false loss claim"
        );
    }

    #[test]
    fn test_drain_capped_over_limit_truncates_and_reports_loss() {
        let (kept, discarded) = drain_capped(CountingReader::new(1_500, b'e'), 1_000);
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
    /// (PF-021 / AD-STR-8).
    #[test]
    fn test_drain_capped_keeps_reading_past_the_limit() {
        let mut reader = CountingReader::new(300_000, b'e');
        let (kept, discarded) = drain_capped(&mut reader, 1_000);
        assert_eq!(kept.len(), 1_000);
        assert!(discarded);
        assert_eq!(
            reader.read_total, 300_000,
            "the collector must drain the whole pipe, not stop at its ceiling — \
             stopping is the PF-021 deadlock"
        );
        assert_eq!(reader.remaining, 0);
    }

    #[test]
    fn test_drain_capped_empty_reader() {
        let (kept, discarded) = drain_capped(CountingReader::new(0, b'e'), 1_000);
        assert!(kept.is_empty());
        assert!(!discarded);
    }

    // ========================================================================
    // pump — byte fidelity, the pipe-closed disposition, and the ABSENT ceiling
    // ========================================================================

    #[test]
    fn test_pump_copies_arbitrary_bytes_verbatim() {
        // Deliberately not valid UTF-8: the pump must never decode.
        let source: Vec<u8> = vec![0xff, 0xfe, b'a', 0x09, 0x80, b'\n'];
        let mut reader = source.as_slice();
        let mut sink = ClosingWriter::open();
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
        let mut sink = ClosingWriter::open();
        let report = pump(&mut reader, &mut sink).unwrap();
        assert_eq!(report.bytes, 0);
        assert_eq!(
            report.last_byte, None,
            "an empty stream must not trigger the trailing-newline guard"
        );
    }

    /// **The headline A2 property.**  The pump has no byte ceiling, so a stream
    /// far larger than a buffered collector's limit is *delivered*, not discarded.
    ///
    /// `runner::read_pipe` returns `Err("output exceeded … byte limit")` past
    /// [`MAX_OUTPUT_BYTES`] and throws the accumulated buffer away — a genuine
    /// zero-output path that `SKIM_PASSTHROUGH=1` shared, so the documented
    /// escape hatch lost *all* data precisely when a user reached for it.
    ///
    /// The ceiling is injected as `CEILING` rather than using the real 64 MiB
    /// constant, following the `read_pipe_degrade_impl(reader, limit)` precedent:
    /// the property under test is "no ceiling applies", which a 4 KiB stand-in
    /// demonstrates without moving 64 MiB through a unit test.  The real-size e2e
    /// variant is `t11_escape_hatch_delivers_past_the_buffered_ceiling`
    /// (`#[ignore]`d).
    #[test]
    fn test_pump_delivers_everything_past_a_buffered_style_ceiling() {
        const CEILING: usize = 4 * 1024;
        const TOTAL: usize = CEILING * 5;

        let mut reader = CountingReader::new(TOTAL, b'x');
        let mut sink = ClosingWriter::open();
        let report = pump(&mut reader, &mut sink).unwrap();

        assert_eq!(
            report.stop,
            PumpStop::Eof,
            "the stream must run to EOF, not stop at any ceiling"
        );
        assert_eq!(
            sink.written.len(),
            TOTAL,
            "every byte past the stand-in {CEILING}-byte ceiling must be DELIVERED — \
             the buffered collector discards its whole buffer here and emits nothing"
        );
        assert_eq!(report.bytes, TOTAL);
        assert!(sink.written.iter().all(|&b| b == b'x'), "and unmodified");
    }

    /// Memory stays O(chunk) no matter how large the stream is.
    ///
    /// This is *why* there is no ceiling: the buffered path needs one because it
    /// accumulates, and the pump does not accumulate at all.  A writer that
    /// counts instead of retaining proves the pump itself holds nothing.
    #[test]
    fn test_pump_retains_no_bytes_of_its_own() {
        /// Discards everything, remembering only how much it saw.
        struct CountingWriter(usize);
        impl Write for CountingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0 += buf.len();
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let total = PUMP_BUF_BYTES * 40; // 2.5 MiB through a 64 KiB pump buffer
        let mut reader = CountingReader::new(total, b'z');
        let mut sink = CountingWriter(0);
        let report = pump(&mut reader, &mut sink).unwrap();

        assert_eq!(sink.0, total);
        assert_eq!(report.bytes, total);
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
             `passthrough_raw` / SKIM_PASSTHROUGH=1 escape-hatch shape"
        );
    }
}
