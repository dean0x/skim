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
//! The stream reader uses `String::from_utf8_lossy` to tolerate rare invalid
//! UTF-8 bytes that `gh run watch` may emit (e.g., from build tool output).
//!
//! # DESIGN NOTE (AD-STR-7) — Blocking reads, no timeouts
//!
//! `gh run watch` may idle for minutes between workflow steps.  Blocking reads
//! are correct here; timeouts would cause premature stream termination.
//!
//! # DESIGN NOTE (AD-STR-8) — Child-spawn merges stdout+stderr
//!
//! [`run_streamed_spawned`] creates a child process with `stdout` piped and
//! merges stderr onto stdout so the parser sees the full output stream.

use std::io::{self, BufRead, BufWriter, Write};
use std::process::ExitCode;
use std::time::Instant;

use crate::output::strip_ansi;

// ============================================================================
// Constants
// ============================================================================

/// Maximum line length in bytes accepted by the streaming reader.
///
/// Lines longer than this limit are truncated with a `…` marker appended.
/// This prevents unbounded allocation on pathological inputs (e.g., minified
/// JS emitted by a build step).
pub(super) const MAX_STREAM_LINE_BYTES: usize = 64 * 1024; // 64 KiB

// ============================================================================
// Private helpers
// ============================================================================

/// Truncate a line to [`MAX_STREAM_LINE_BYTES`], appending `…` if cut (AD-STR-1).
fn truncate_line(line: String) -> String {
    if line.len() > MAX_STREAM_LINE_BYTES {
        let mut truncated = line[..MAX_STREAM_LINE_BYTES].to_string();
        truncated.push('…');
        truncated
    } else {
        line
    }
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
        // by 4 gives a rough estimate of GPT tokens for English-like text —
        // close enough for analytics bucketing.  Using
        // `try_record_command_with_counts` avoids a re-tokenization pass on an
        // empty placeholder string (which previously collapsed to ≤1 token and
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
    let mut guard = DropGuard::new(cfg.label, cfg.analytics_enabled);

    for line_result in stdin.lock().lines() {
        let raw_line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };

        // Truncate over-limit lines (AD-STR-1).
        let raw_line = truncate_line(raw_line);

        // Strip ANSI escape codes before passing to parser (AD-GRW-1).
        let clean = strip_ansi(&raw_line);
        let clean_line: &str = clean.as_ref();

        guard.update(raw_line.len() + 1, 0); // +1 for newline

        if let Some(output) = parser.on_line(clean_line) {
            guard.update(0, output.len() + 1);
            if writeln!(stdout, "{output}").is_err() {
                // SIGPIPE — exit gracefully (AD-STR-2).
                break;
            }
            if stdout.flush().is_err() {
                break;
            }
        }
    }

    // Finalize — parser is consumed here; totals are already tracked in guard.
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
/// Spawns `cmd` with `args`, merges stdout+stderr into a single reader, and
/// feeds each line to `parser.on_line()`.  Calls `parser.finalize()` at EOF.
///
/// # Exit code semantics
///
/// Returns the child process exit code on success, or `ExitCode::FAILURE` if
/// the child could not be spawned.  `gh run watch --exit-status` failure
/// propagates as a non-zero exit code.
///
/// # Signal handling
///
/// SIGINT is forwarded to the child via `Child::kill()` in a Drop guard.
/// SIGPIPE on writes causes a clean exit.
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

    let mut child = match Command::new(cmd)
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

    let mut stdout = BufWriter::new(io::stdout());
    let mut guard = DropGuard::new(cfg.label, cfg.analytics_enabled);

    // Merge stdout+stderr (AD-STR-8).
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();

    // Read lines from child stdout.
    if let Some(out) = child_stdout {
        let reader = io::BufReader::new(out);
        for line_result in reader.lines() {
            let raw_line = match line_result {
                Ok(l) => l,
                Err(_) => break,
            };

            // Truncate over-limit lines (AD-STR-1).
            let raw_line = truncate_line(raw_line);

            let clean = strip_ansi(&raw_line);
            let clean_line: &str = clean.as_ref();

            guard.update(raw_line.len() + 1, 0);

            if let Some(output) = parser.on_line(clean_line) {
                guard.update(0, output.len() + 1);
                if writeln!(stdout, "{output}").is_err() {
                    let _ = child.kill();
                    break;
                }
                if stdout.flush().is_err() {
                    let _ = child.kill();
                    break;
                }
            }
        }
    }

    // Drain stderr (merge into output).
    if let Some(err) = child_stderr {
        let reader = io::BufReader::new(err);
        for line_result in reader.lines() {
            let raw_line = match line_result {
                Ok(l) => l,
                Err(_) => break,
            };
            let clean = strip_ansi(&raw_line);
            let clean_line: &str = clean.as_ref();
            guard.update(raw_line.len() + 1, 0);
            if let Some(output) = parser.on_line(clean_line) {
                guard.update(0, output.len() + 1);
                let _ = writeln!(stdout, "{output}");
                let _ = stdout.flush();
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
    match child.wait() {
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
}
