//! Streaming output compression primitive for long-running gh commands.
//!
//! Provides a trait-based streaming parser that processes output line-by-line,
//! enabling compression of commands like `gh run watch` that emit output over
//! an extended period rather than all at once.
//!
//! # DESIGN NOTE (AD-STR-1) — Line-by-line only
//!
//! Each line is processed independently.  No multi-line event batching is
//! performed.  This keeps the parser state machine simple and avoids buffer
//! accumulation for streams that may idle for minutes.
//!
//! # DESIGN NOTE (AD-STR-2) — Backpressure via flush
//!
//! [`run_streamed_stdin`] and [`run_streamed_spawned`] call
//! `BufWriter::flush()` after each emitted line to prevent output from being
//! held in the buffer while the stream is still active.
//!
//! # DESIGN NOTE (AD-STR-3) — Analytics at EOF; Drop guard for partials
//!
//! Analytics are recorded at EOF on the success path.  A [`DropGuard`] holds
//! running totals and a `recorded` flag; when `Drop` fires (e.g., on SIGINT or
//! SIGPIPE), it records partial totals if the success path has not already done
//! so.  This prevents double-recording while ensuring interrupted streams
//! appear in analytics.
//!
//! # DESIGN NOTE (AD-STR-4) — No history retention
//!
//! Parser state is current-step-only.  No history buffer is maintained.  Parsers
//! must be designed to derive context from the current line alone (or from
//! state they accumulate themselves).
//!
//! # DESIGN NOTE (AD-STR-5) — Lives under gh/ until a second caller emerges
//!
//! This module is co-located with `gh/` because `gh run watch` is currently the
//! only caller.  When a second streaming use case appears, promote to
//! `cmd/streaming.rs` and update imports.  YAGNI.
//!
//! # DESIGN NOTE (AD-STR-6) — UTF-8 via from_utf8_lossy
//!
//! The stream reader uses `read_until(b'\n')` followed by
//! `String::from_utf8_lossy` to tolerate rare invalid UTF-8 bytes that
//! `gh run watch` may emit (e.g., from build tool output).  The previous
//! `BufRead::lines()` implementation terminated the stream on the first
//! invalid byte; `read_until` plus `from_utf8_lossy` replaces the offending byte
//! with U+FFFD and continues.
//!
//! # DESIGN NOTE (AD-STR-7) — Blocking reads, no timeouts
//!
//! `gh run watch` may idle for minutes between workflow steps.  Blocking reads
//! are correct here; timeouts would cause premature stream termination.
//!
//! # DESIGN NOTE (AD-STR-8) — Child-spawn drains stderr concurrently
//!
//! [`run_streamed_spawned`] pipes both stdout and stderr.  A background thread
//! drains stderr into a `Vec<String>` while the main thread processes stdout.
//! After the stdout loop completes, the background thread is joined and the
//! collected stderr lines are fed through the parser.  This prevents the pipe
//! deadlock that would occur if the child writes more than 64 KiB to stderr
//! while the main thread is blocked on stdout (PF-023).
//!
//! # DESIGN NOTE (AD-STR-9) — ChildGuard kills and reaps on drop
//!
//! The spawned child is wrapped in [`ChildGuard`], whose `Drop` implementation
//! calls `kill()` followed by `wait()`.  This ensures the child is reaped when
//! the parent exits early (SIGPIPE, SIGINT unwind, or any other panic path).
//! `kill()` on an already-exited child is a no-op on all platforms (PF-025).

use std::io::{self, BufRead, BufWriter, Read, Write};
use std::process::ExitCode;
use std::time::Instant;

use crate::output::strip_ansi;

// ============================================================================
// Constants
// ============================================================================

/// Maximum line length in bytes accepted by the streaming reader.
///
/// The `read_until` reader is capped at `MAX_STREAM_LINE_BYTES + 1` bytes per
/// call via `Read::take`, bounding allocation before the UTF-8 decode step.
/// A `...` marker is appended when a line is truncated.  This prevents unbounded
/// allocation on pathological inputs (e.g., minified JS emitted by a build
/// step) per AD-STR-1.
pub(super) const MAX_STREAM_LINE_BYTES: usize = 64 * 1024; // 64 KiB

// ============================================================================
// Private helpers
// ============================================================================

/// Read one line from `reader` using `read_until` plus `from_utf8_lossy`.
///
/// Implements the AD-STR-6 contract:
/// - Each read is bounded to `MAX_STREAM_LINE_BYTES + 1` bytes via `Read::take`
///   so allocation is capped before the UTF-8 decode step (AD-STR-1 / PF-026).
/// - Invalid UTF-8 bytes are replaced with U+FFFD rather than terminating the
///   stream (AD-STR-6).
/// - Trailing `\r` and `\n` are stripped.
///
/// Returns `None` at EOF (0 bytes read) or on I/O error.
/// A line that fills the cap without a newline gets a truncation marker appended.
fn read_line_lossy(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> Option<String> {
    buf.clear();
    // Cap + 1 so we can detect whether a newline was consumed or we hit the
    // limit first.  If we read exactly MAX+1 bytes, the line was truncated.
    let n = match reader
        .by_ref()
        .take((MAX_STREAM_LINE_BYTES + 1) as u64)
        .read_until(b'\n', buf)
    {
        Ok(0) => return None,
        Ok(n) => n,
        Err(_) => return None,
    };

    // Strip trailing \r\n.
    while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
        buf.pop();
    }

    let truncated = n > MAX_STREAM_LINE_BYTES;
    let mut line = String::from_utf8_lossy(buf).into_owned();
    if truncated {
        line.push('\u{2026}'); // U+2026 HORIZONTAL ELLIPSIS
    }
    Some(line)
}

// ============================================================================
// Public types
// ============================================================================

/// Aggregate byte totals for a streaming parse session.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct StreamTotals {
    /// Total raw bytes received from the stream.
    pub raw_bytes: usize,
    /// Total compressed bytes emitted.
    pub compressed_bytes: usize,
}

/// Configuration for a streaming parse run.
#[derive(Debug, Clone)]
pub(super) struct StreamConfig {
    /// Whether to record analytics for this stream.
    pub analytics_enabled: bool,
    /// Command label for analytics recording.
    pub label: String,
}

/// A streaming line-by-line output parser.
///
/// Implementors process each line of output from a long-running command,
/// emitting compressed output lines or `None` to suppress a line.
///
/// # Contract
///
/// - [`on_line`] is called for every line of input (without trailing newline).
/// - [`finalize`] is called once at EOF to emit any buffered state.
/// - [`totals`] returns the running raw/compressed byte counts.
#[allow(dead_code)]
pub(super) trait StreamingParser: Send {
    /// Process a single input line.
    ///
    /// Returns `Some(output_line)` if a compressed line should be emitted, or
    /// `None` to suppress the line entirely.  The returned string must NOT
    /// include a trailing newline.
    fn on_line(&mut self, line: &str) -> Option<String>;

    /// Finalize the stream at EOF.
    ///
    /// Called once after the last line.  Returns `Some(output_line)` if a
    /// final summary line should be emitted, or `None` if there is nothing
    /// left to output.
    fn finalize(self: Box<Self>) -> Option<String>;

    /// Return the current running byte totals.
    ///
    /// Called by the harness at EOF and in the Drop guard for analytics.
    fn totals(&self) -> StreamTotals;
}

// ============================================================================
// Analytics Drop guard
// ============================================================================

/// Drop guard that records partial analytics on SIGINT/SIGPIPE.
///
/// Holds running totals and a `recorded` flag.  When `Drop` fires, records
/// only if the success path (`record()`) has not already done so.
struct DropGuard {
    label: String,
    start: Instant,
    raw_bytes: usize,
    compressed_bytes: usize,
    analytics_enabled: bool,
    recorded: bool,
}

impl DropGuard {
    fn new(label: String, analytics_enabled: bool) -> Self {
        Self {
            label,
            start: Instant::now(),
            raw_bytes: 0,
            compressed_bytes: 0,
            analytics_enabled,
            recorded: false,
        }
    }

    fn update(&mut self, raw: usize, compressed: usize) {
        self.raw_bytes += raw;
        self.compressed_bytes += compressed;
    }

    /// Record analytics on the success path and set `recorded = true`.
    fn record(&mut self) {
        if !self.recorded && self.analytics_enabled {
            self.do_record();
        }
        self.recorded = true;
    }

    fn do_record(&self) {
        // Streaming parsers don't retain full text, so we use the byte counters
        // tracked by the harness as token-count approximations.  Dividing bytes
        // by 4 gives a rough estimate of GPT tokens for English-like text --
        // close enough for analytics bucketing.  Using
        // `try_record_command_with_counts` avoids a re-tokenization pass on an
        // empty placeholder string (which previously collapsed to <=1 token and
        // silently under-reported every streaming run's savings).
        let raw_tokens = self.raw_bytes / 4;
        let compressed_tokens = self.compressed_bytes / 4;
        crate::analytics::try_record_command_with_counts(
            self.analytics_enabled,
            raw_tokens,
            compressed_tokens,
            self.label.clone(),
            crate::analytics::CommandType::Infra,
            self.start.elapsed(),
            Some("streaming"),
        );
    }
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        if !self.recorded && self.analytics_enabled {
            self.do_record();
        }
    }
}

// ============================================================================
// ChildGuard -- kill + reap on drop (AD-STR-9, PF-025)
// ============================================================================

/// RAII wrapper that kills and reaps a child process on drop.
///
/// When the parent exits early (SIGPIPE, panic, or any unwind path), the
/// `Drop` implementation calls `kill()` followed by `wait()` to prevent the
/// child from becoming a zombie.  `kill()` on an already-exited process is a
/// no-op on Unix (returns `ESRCH`) and returns `InvalidInput` on Windows --
/// both cases are explicitly ignored.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // kill() is a no-op on an already-exited child; ignore the result.
        let _ = self.0.kill();
        // wait() reaps the zombie; ignore result (process may already be reaped).
        let _ = self.0.wait();
    }
}

// ============================================================================
// Public harness functions
// ============================================================================

/// Run a streaming parser over stdin.
///
/// Reads lines from stdin, passes each to `parser.on_line()`, and writes any
/// non-`None` return values to stdout.  Calls `parser.finalize()` at EOF.
///
/// # Exit code semantics
///
/// Always returns `ExitCode::SUCCESS` (stdin sources don't have an exit code).
///
/// # Signal handling
///
/// SIGPIPE is handled gracefully: a `BrokenPipe` write error causes a clean
/// exit via `ExitCode::SUCCESS`.  The Drop guard records partial analytics.
///
/// # Analytics
///
/// Analytics are recorded at EOF via the Drop guard.
#[allow(dead_code)]
pub(super) fn run_streamed_stdin(
    mut parser: Box<dyn StreamingParser>,
    cfg: StreamConfig,
) -> ExitCode {
    let mut stdout = BufWriter::new(io::stdout());
    let stdin = io::stdin();
    let stdin_lock = stdin.lock();
    let mut reader = io::BufReader::new(stdin_lock);
    let mut guard = DropGuard::new(cfg.label, cfg.analytics_enabled);
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    loop {
        let raw_line = match read_line_lossy(&mut reader, &mut buf) {
            Some(l) => l,
            None => break,
        };

        // Strip ANSI escape codes before passing to parser (AD-GRW-1).
        let clean = strip_ansi(&raw_line);
        let clean_line: &str = clean.as_ref();

        guard.update(raw_line.len() + 1, 0); // +1 for newline

        if let Some(output) = parser.on_line(clean_line) {
            match writeln!(stdout, "{output}") {
                Ok(()) => {
                    // Update compressed bytes only after a successful write to
                    // avoid over-reporting on SIGPIPE (PF-026 / AD-STR-3).
                    guard.update(0, output.len() + 1);
                    if stdout.flush().is_err() {
                        // SIGPIPE -- exit gracefully (AD-STR-2).
                        break;
                    }
                }
                Err(_) => {
                    // SIGPIPE -- exit gracefully (AD-STR-2).
                    break;
                }
            }
        }
    }

    // Finalize -- parser is consumed here; totals are already tracked in guard.
    if let Some(output) = parser.finalize() {
        let compressed_len = output.len() + 1;
        guard.update(0, compressed_len);
        let _ = writeln!(stdout, "{output}");
        let _ = stdout.flush();
    }

    guard.record();
    ExitCode::SUCCESS
}

/// Run a streaming parser over a spawned child process.
///
/// Spawns `cmd` with `args`, pipes both stdout and stderr, and feeds each line
/// to `parser.on_line()`.  Stderr is drained concurrently in a background
/// thread to prevent the pipe-full deadlock described in PF-023.  After stdout
/// reaches EOF the background thread is joined and the collected stderr lines
/// are fed through the parser in order.  Calls `parser.finalize()` at EOF.
///
/// # Exit code semantics
///
/// Returns the child process exit code on success, or `ExitCode::FAILURE` if
/// the child could not be spawned.  `gh run watch --exit-status` failure
/// propagates as a non-zero exit code.
///
/// # Signal handling
///
/// The child is wrapped in [`ChildGuard`]; when the parent exits for any
/// reason (SIGPIPE, panic, clean exit) the child is killed and reaped
/// automatically (AD-STR-9, PF-025).  SIGPIPE on writes causes a clean exit.
///
/// # Analytics
///
/// Analytics are recorded at EOF or on early exit via the Drop guard.
///
/// # Error: gh not found
///
/// If `cmd` is not on PATH, prints `error: gh not found on PATH` to stderr
/// and returns exit code 127.
pub(super) fn run_streamed_spawned(
    mut parser: Box<dyn StreamingParser>,
    cmd: &str,
    args: &[String],
    cfg: StreamConfig,
) -> ExitCode {
    use std::process::{Command, Stdio};

    let child_proc = match Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("error: {cmd} not found on PATH");
            return ExitCode::from(127);
        }
        Err(e) => {
            eprintln!("error: failed to spawn {cmd}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Wrap in ChildGuard so the child is killed+reaped on any exit path
    // (SIGPIPE, panic, clean exit) -- AD-STR-9, PF-025.
    let mut child = ChildGuard(child_proc);

    let mut stdout = BufWriter::new(io::stdout());
    let mut guard = DropGuard::new(cfg.label, cfg.analytics_enabled);

    // Spawn a background thread to drain stderr concurrently (AD-STR-8, PF-023).
    // The thread collects all stderr lines into a Vec so we can feed them through
    // the parser after stdout is exhausted -- without risking a pipe deadlock when
    // the child writes > 64 KiB to stderr while the main thread is blocked on
    // stdout.
    let stderr_handle = child.0.stderr.take().map(|err| {
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(err);
            let mut buf: Vec<u8> = Vec::with_capacity(256);
            let mut lines: Vec<String> = Vec::new();
            loop {
                match read_line_lossy(&mut reader, &mut buf) {
                    Some(line) => lines.push(line),
                    None => break,
                }
            }
            lines
        })
    });

    // Read lines from child stdout (AD-STR-6: read_until + from_utf8_lossy).
    if let Some(out) = child.0.stdout.take() {
        let mut reader = io::BufReader::new(out);
        let mut buf: Vec<u8> = Vec::with_capacity(256);

        loop {
            let raw_line = match read_line_lossy(&mut reader, &mut buf) {
                Some(l) => l,
                None => break,
            };

            let clean = strip_ansi(&raw_line);
            let clean_line: &str = clean.as_ref();

            guard.update(raw_line.len() + 1, 0);

            if let Some(output) = parser.on_line(clean_line) {
                match writeln!(stdout, "{output}") {
                    Ok(()) => {
                        // Update compressed bytes only after a successful write
                        // to avoid over-reporting on SIGPIPE (PF-026).
                        guard.update(0, output.len() + 1);
                        if stdout.flush().is_err() {
                            // SIGPIPE -- kill child and exit gracefully.
                            return ExitCode::SUCCESS;
                        }
                    }
                    Err(_) => {
                        // SIGPIPE -- kill child and exit gracefully.
                        return ExitCode::SUCCESS;
                    }
                }
            }
        }
    }

    // Join the stderr background thread and feed its lines through the parser.
    let stderr_lines = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    for raw_line in stderr_lines {
        let clean = strip_ansi(&raw_line);
        let clean_line: &str = clean.as_ref();
        guard.update(raw_line.len() + 1, 0);
        if let Some(output) = parser.on_line(clean_line) {
            match writeln!(stdout, "{output}") {
                Ok(()) => {
                    guard.update(0, output.len() + 1);
                    let _ = stdout.flush();
                }
                Err(_) => break,
            }
        }
    }

    // Finalize.
    if let Some(output) = parser.finalize() {
        guard.update(0, output.len() + 1);
        let _ = writeln!(stdout, "{output}");
        let _ = stdout.flush();
    }

    // Wait for child and propagate exit code.
    match child.0.wait() {
        Ok(status) => {
            guard.record();
            match status.code() {
                Some(0) => ExitCode::SUCCESS,
                Some(code) => ExitCode::from(code as u8),
                None => ExitCode::FAILURE,
            }
        }
        Err(_) => {
            guard.record();
            ExitCode::FAILURE
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Minimal StreamingParser implementation for testing ----

    struct IdentityParser {
        totals: StreamTotals,
    }

    impl IdentityParser {
        fn new() -> Self {
            Self {
                totals: StreamTotals::default(),
            }
        }
    }

    impl StreamingParser for IdentityParser {
        fn on_line(&mut self, line: &str) -> Option<String> {
            self.totals.raw_bytes += line.len() + 1;
            self.totals.compressed_bytes += line.len() + 1;
            Some(line.to_string())
        }

        fn finalize(self: Box<Self>) -> Option<String> {
            None
        }

        fn totals(&self) -> StreamTotals {
            self.totals
        }
    }

    struct SuppressParser {
        totals: StreamTotals,
    }

    impl SuppressParser {
        fn new() -> Self {
            Self {
                totals: StreamTotals::default(),
            }
        }
    }

    impl StreamingParser for SuppressParser {
        fn on_line(&mut self, line: &str) -> Option<String> {
            self.totals.raw_bytes += line.len() + 1;
            // Suppress all lines.
            None
        }

        fn finalize(self: Box<Self>) -> Option<String> {
            Some("summary".to_string())
        }

        fn totals(&self) -> StreamTotals {
            self.totals
        }
    }

    #[test]
    fn test_drop_guard_records_once() {
        let mut guard = DropGuard::new("test".to_string(), false);
        guard.update(100, 50);
        guard.record();
        assert!(guard.recorded);
        // Drop should not double-record (analytics_enabled is false, so no-op).
    }

    #[test]
    fn test_drop_guard_update_accumulates() {
        let mut guard = DropGuard::new("test".to_string(), false);
        guard.update(100, 50);
        guard.update(200, 100);
        assert_eq!(guard.raw_bytes, 300);
        assert_eq!(guard.compressed_bytes, 150);
    }

    #[test]
    fn test_max_stream_line_bytes_is_64_kib() {
        assert_eq!(MAX_STREAM_LINE_BYTES, 64 * 1024);
    }

    #[test]
    fn test_streaming_parser_contract() {
        let mut parser = IdentityParser::new();
        let out1 = parser.on_line("hello");
        let out2 = parser.on_line("world");
        assert_eq!(out1, Some("hello".to_string()));
        assert_eq!(out2, Some("world".to_string()));
        let parser: Box<dyn StreamingParser> = Box::new(parser);
        assert!(parser.finalize().is_none());
    }

    #[test]
    fn test_suppress_parser_finalize_emits_summary() {
        let mut parser = SuppressParser::new();
        assert!(parser.on_line("noise").is_none());
        let parser: Box<dyn StreamingParser> = Box::new(parser);
        assert_eq!(parser.finalize(), Some("summary".to_string()));
    }

    #[test]
    fn test_run_streamed_spawned_gh_not_found() {
        let parser: Box<dyn StreamingParser> = Box::new(IdentityParser::new());
        let cfg = StreamConfig {
            analytics_enabled: false,
            label: "test".to_string(),
        };
        // Use a non-existent binary name to trigger "not found on PATH".
        let code = run_streamed_spawned(parser, "skim_nonexistent_binary_xyz", &[], cfg);
        assert_eq!(code, ExitCode::from(127));
    }

    #[test]
    fn test_run_streamed_spawned_exit_code_propagated() {
        let parser: Box<dyn StreamingParser> = Box::new(IdentityParser::new());
        let cfg = StreamConfig {
            analytics_enabled: false,
            label: "test".to_string(),
        };
        // /bin/sh -c 'exit 3' exits with code 3.
        let args: Vec<String> = vec!["-c".to_string(), "exit 3".to_string()];
        let code = run_streamed_spawned(parser, "/bin/sh", &args, cfg);
        // ExitCode::from(3u8)
        assert_eq!(code, ExitCode::from(3u8));
    }

    #[test]
    fn test_run_streamed_spawned_success() {
        let parser: Box<dyn StreamingParser> = Box::new(IdentityParser::new());
        let cfg = StreamConfig {
            analytics_enabled: false,
            label: "test".to_string(),
        };
        let args: Vec<String> = vec!["-c".to_string(), "echo hello".to_string()];
        let code = run_streamed_spawned(parser, "/bin/sh", &args, cfg);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    // ---- ChildGuard kill-on-drop contract (AD-STR-9, PF-025) ----

    /// Verify that ChildGuard kills a long-running child when dropped.
    ///
    /// Spawns `sleep 60`, immediately drops the guard, then uses `kill -0`
    /// to confirm the child is no longer running.
    #[test]
    fn test_child_guard_kills_on_drop() {
        use std::process::{Command, Stdio};

        let child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        let pid = child.id();
        let guard = ChildGuard(child);

        // Drop the guard: this calls kill() + wait() on the child.
        drop(guard);

        // kill -0 returns non-zero when the process does not exist.
        let status = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status();

        match status {
            Ok(s) => assert!(
                !s.success(),
                "process {pid} should have been killed by ChildGuard"
            ),
            Err(_) => {
                // `kill` binary not found on this platform -- spawn + drop
                // completing without panic is sufficient verification.
            }
        }
    }

    // ---- UTF-8-lossy stream behaviour (AD-STR-6) ----

    /// Verify that `read_line_lossy` tolerates invalid UTF-8 and does not
    /// terminate the stream early.
    #[test]
    fn test_read_line_lossy_tolerates_invalid_utf8() {
        let mut input: Vec<u8> = Vec::new();
        input.extend_from_slice(b"line one\n");
        input.extend_from_slice(b"bad \xFF bytes here\n");
        input.extend_from_slice(b"line three\n");

        let mut reader = io::BufReader::new(input.as_slice());
        let mut buf = Vec::new();

        let l1 = read_line_lossy(&mut reader, &mut buf).expect("first line");
        let l2 = read_line_lossy(&mut reader, &mut buf)
            .expect("second line -- must not be None on invalid UTF-8");
        let l3 = read_line_lossy(&mut reader, &mut buf).expect("third line");
        let eof = read_line_lossy(&mut reader, &mut buf);

        assert_eq!(l1, "line one");
        // Invalid 0xFF must be replaced with U+FFFD, not terminate the stream.
        assert!(
            l2.contains('\u{FFFD}'),
            "expected replacement char in: {l2:?}"
        );
        assert_eq!(l3, "line three");
        assert!(eof.is_none(), "expected EOF");
    }

    /// Verify that a line exceeding `MAX_STREAM_LINE_BYTES` is truncated and
    /// gets a truncation marker suffix, with the content portion bounded by the cap.
    #[test]
    fn test_read_line_lossy_truncates_overlong_line() {
        let long_line: Vec<u8> = vec![b'A'; MAX_STREAM_LINE_BYTES + 100];
        let mut input = long_line;
        input.push(b'\n');

        let mut reader = io::BufReader::new(input.as_slice());
        let mut buf = Vec::new();

        let result = read_line_lossy(&mut reader, &mut buf).expect("line");
        // U+2026 is the truncation marker appended by read_line_lossy.
        assert!(
            result.ends_with('\u{2026}'),
            "truncated line should end with ellipsis (U+2026)"
        );
        let without_marker: &str = result.trim_end_matches('\u{2026}');
        assert!(
            without_marker.len() <= MAX_STREAM_LINE_BYTES,
            "content before marker must be within cap"
        );
    }

    /// Verify that `run_streamed_spawned` handles concurrent stdout+stderr
    /// output without deadlocking.
    #[test]
    fn test_run_streamed_spawned_no_deadlock_with_stderr() {
        let parser: Box<dyn StreamingParser> = Box::new(IdentityParser::new());
        let cfg = StreamConfig {
            analytics_enabled: false,
            label: "test".to_string(),
        };
        let script = r#"for i in $(seq 1 500); do
    echo "stdout line $i"
    echo "stderr line $i" >&2
done"#;
        let args: Vec<String> = vec!["-c".to_string(), script.to_string()];
        let code = run_streamed_spawned(parser, "/bin/sh", &args, cfg);
        assert_eq!(code, ExitCode::SUCCESS);
    }
}
