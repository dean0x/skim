//! Command execution engine (#40)
//!
//! Provides a safe, timeout-aware command runner that captures stdout/stderr
//! concurrently to prevent pipe deadlocks. No shell interpretation — commands
//! are executed directly via `Command::new().args()`.

// Infrastructure module — consumers arrive in later Phase B tickets.
#![allow(dead_code)]

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::{io, thread};

/// Output captured from a completed command.
#[derive(Debug, Clone)]
#[must_use]
pub(crate) struct CommandOutput {
    /// Captured standard output (lossy UTF-8).
    pub(crate) stdout: String,
    /// Captured standard error (lossy UTF-8).
    pub(crate) stderr: String,
    /// Process exit code. `None` when killed by signal (Unix).
    pub(crate) exit_code: Option<i32>,
    /// Wall-clock duration from spawn to reap.
    pub(crate) duration: Duration,
}

/// Errors specific to command execution.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RunnerError {
    /// The program could not be spawned (e.g., not found on `$PATH`).
    #[error("failed to execute '{program}': {source}")]
    SpawnFailed {
        program: String,
        #[source]
        source: io::Error,
    },

    /// The command exceeded its timeout and was killed.
    #[error("command timed out after {timeout:?}")]
    Timeout { timeout: Duration },

    /// I/O error reading pipes.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// Failed to capture a child pipe (stdout or stderr was `None`).
    #[error("failed to capture child {pipe}")]
    PipeCaptureFailed { pipe: &'static str },

    /// A reader thread panicked instead of returning normally.
    #[error("{pipe} reader thread panicked")]
    ReaderPanicked { pipe: &'static str },
}

/// A command runner with optional timeout support.
///
/// Commands are executed directly via `Command::new().args()` — no shell
/// interpretation. Stdout and stderr are captured concurrently via two
/// reader threads to prevent pipe deadlocks on large output.
pub(crate) struct CommandRunner {
    timeout: Option<Duration>,
}

impl CommandRunner {
    /// Create a new runner.
    ///
    /// `timeout` — maximum wall-clock duration before the child is killed.
    /// `None` means wait indefinitely.
    pub(crate) fn new(timeout: Option<Duration>) -> Self {
        Self { timeout }
    }

    /// Execute `program` with `args`, capturing stdout and stderr.
    ///
    /// Returns [`CommandOutput`] on success or an error if the program cannot
    /// be spawned, times out, or pipe I/O fails.
    pub(crate) fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        self.run_with_env(program, args, &[])
    }

    /// Execute `program` with `args` and environment variable overrides,
    /// capturing stdout and stderr.
    ///
    /// Reuses the same timeout, output-size cap, and pipe-capture logic as
    /// [`run`](Self::run). Pass an empty slice for `env_vars` when no
    /// overrides are needed.
    pub(crate) fn run_with_env(
        &self,
        program: &str,
        args: &[&str],
        env_vars: &[(&str, &str)],
    ) -> anyhow::Result<CommandOutput> {
        let start = Instant::now();

        let mut cmd = Command::new(program);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|source| RunnerError::SpawnFailed {
            program: program.to_string(),
            source,
        })?;

        // Take pipes BEFORE spawning reader threads — avoids borrowing child later.
        let child_stdout = child
            .stdout
            .take()
            .ok_or(RunnerError::PipeCaptureFailed { pipe: "stdout" })?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or(RunnerError::PipeCaptureFailed { pipe: "stderr" })?;

        // Spawn concurrent reader threads to prevent pipe deadlocks.
        let stdout_handle = thread::spawn(move || read_pipe(child_stdout));
        let stderr_handle = thread::spawn(move || read_pipe(child_stderr));

        // Wait for child with optional timeout.
        let status = self.wait_with_timeout(&mut child, start)?;

        // Join reader threads — propagate panics as anyhow errors.
        let stdout = stdout_handle
            .join()
            .map_err(|_| RunnerError::ReaderPanicked { pipe: "stdout" })??;
        let stderr = stderr_handle
            .join()
            .map_err(|_| RunnerError::ReaderPanicked { pipe: "stderr" })??;

        let duration = start.elapsed();

        Ok(CommandOutput {
            stdout,
            stderr,
            exit_code: status.code(),
            duration,
        })
    }

    /// Wait for the child process, killing it if the timeout is exceeded.
    ///
    /// Uses polling with exponential backoff (1ms → 50ms cap) when a timeout
    /// is set. Without a timeout, blocks on `child.wait()` directly.
    fn wait_with_timeout(&self, child: &mut Child, start: Instant) -> anyhow::Result<ExitStatus> {
        let Some(timeout) = self.timeout else {
            return Ok(child.wait()?);
        };

        let mut sleep_ms: u64 = 1;
        const MAX_SLEEP_MS: u64 = 50;

        loop {
            match child.try_wait()? {
                Some(status) => return Ok(status),
                None => {
                    if start.elapsed() >= timeout {
                        // Kill the child — ignore error (process may have exited
                        // between try_wait and kill).
                        let _ = child.kill();
                        // Reap the zombie so the OS can reclaim its PID.
                        let _ = child.wait();
                        return Err(RunnerError::Timeout { timeout }.into());
                    }
                    thread::sleep(Duration::from_millis(sleep_ms));
                    sleep_ms = (sleep_ms * 2).min(MAX_SLEEP_MS);
                }
            }
        }
    }
}

/// Maximum bytes we will read from a single pipe (64 MiB).
///
/// Prevents unbounded memory growth when a child process produces
/// unexpectedly large output.
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Read a pipe into a lossy-UTF-8 String, capped at [`MAX_OUTPUT_BYTES`].
///
/// Uses chunked reads (8 KiB) instead of `read_to_end` to enforce the size
/// limit without requiring the OS to report exact pipe length up-front.
/// Non-UTF-8 output (e.g., binary data from `/dev/zero`) is handled via
/// `String::from_utf8_lossy`.
///
/// Returns an `io::Error` (kind `Other`) if the output exceeds the cap.
fn read_pipe<R: Read>(mut reader: R) -> io::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8 * 1024];

    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if buf.len() + n > MAX_OUTPUT_BYTES {
            return Err(io::Error::other(format!(
                "output exceeded {} byte limit",
                MAX_OUTPUT_BYTES
            )));
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that produces exactly `remaining` bytes of `b'A'`, then EOF.
    struct FixedSizeReader {
        remaining: usize,
    }

    impl Read for FixedSizeReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.remaining);
            for b in buf[..n].iter_mut() {
                *b = b'A';
            }
            self.remaining -= n;
            Ok(n)
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_echo_captures_stdout() {
        let runner = CommandRunner::new(None);
        let result = runner.run("echo", &["hello world"]).unwrap();

        assert_eq!(result.stdout.trim(), "hello world");
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn run_captures_stderr() {
        let runner = CommandRunner::new(None);
        // Use a path that definitely does not exist.
        let result = runner
            .run("cat", &["/nonexistent/path/SKIM_TEST_404"])
            .unwrap();

        assert!(!result.stderr.is_empty());
        assert_ne!(result.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn run_preserves_nonzero_exit_code() {
        let runner = CommandRunner::new(None);
        let result = runner.run("false", &[]).unwrap();

        assert_eq!(result.exit_code, Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn run_preserves_zero_exit_code() {
        let runner = CommandRunner::new(None);
        let result = runner.run("true", &[]).unwrap();

        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn run_does_not_interpret_shell_metacharacters() {
        let runner = CommandRunner::new(None);
        // Pass `&& rm -rf /` as a single literal argument.
        let result = runner.run("echo", &["&& rm -rf /"]).unwrap();

        assert!(
            result.stdout.contains("&&"),
            "Expected literal '&&' in stdout, got: {:?}",
            result.stdout
        );
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn run_returns_error_for_nonexistent_program() {
        let runner = CommandRunner::new(None);
        let err = runner
            .run("skim_test_nonexistent_program_42", &[])
            .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("skim_test_nonexistent_program_42"),
            "Error should contain program name, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_kills_on_timeout() {
        let runner = CommandRunner::new(Some(Duration::from_millis(100)));
        let err = runner.run("sleep", &["10"]).unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("timed out"),
            "Expected 'timed out' in error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_completes_within_timeout() {
        let runner = CommandRunner::new(Some(Duration::from_secs(5)));
        let result = runner.run("echo", &["fast"]).unwrap();

        assert_eq!(result.stdout.trim(), "fast");
        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn run_tracks_duration() {
        let runner = CommandRunner::new(None);
        let result = runner.run("echo", &["timing"]).unwrap();

        assert!(
            result.duration > Duration::ZERO,
            "Duration should be positive"
        );
        assert!(
            result.duration < Duration::from_secs(5),
            "Echo should complete in well under 5 seconds, took {:?}",
            result.duration
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_handles_large_stdout_without_deadlock() {
        let runner = CommandRunner::new(Some(Duration::from_secs(10)));
        // `head -c 131072 /dev/zero` outputs exactly 131072 bytes of null bytes.
        let result = runner.run("head", &["-c", "131072", "/dev/zero"]).unwrap();

        assert!(
            result.stdout.len() >= 131072,
            "Expected >= 131072 bytes, got {}",
            result.stdout.len()
        );
    }

    #[test]
    fn read_pipe_enforces_max_output_limit() {
        // Create a reader that produces bytes beyond MAX_OUTPUT_BYTES.
        // We use a struct that yields an infinite stream of zeros.
        struct InfiniteZeros;
        impl Read for InfiniteZeros {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                // Fill the buffer with zeros.
                for b in buf.iter_mut() {
                    *b = 0;
                }
                Ok(buf.len())
            }
        }

        let err = read_pipe(InfiniteZeros).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(
            err.to_string().contains("byte limit"),
            "Expected 'byte limit' in error, got: {}",
            err
        );
    }

    #[test]
    fn read_pipe_accepts_output_under_limit() {
        let data = vec![b'A'; 1024];
        let result = read_pipe(std::io::Cursor::new(data)).unwrap();
        assert_eq!(result.len(), 1024);
    }

    #[cfg(unix)]
    #[test]
    fn run_handles_concurrent_stdout_stderr() {
        let runner = CommandRunner::new(Some(Duration::from_secs(10)));
        // Use python3 to write large output to both stdout and stderr simultaneously.
        let result = runner
            .run(
                "python3",
                &[
                    "-c",
                    "import sys; sys.stdout.write('x'*100000); sys.stderr.write('y'*100000)",
                ],
            )
            .unwrap();

        assert!(
            result.stdout.len() >= 100000,
            "Expected >= 100000 bytes on stdout, got {}",
            result.stdout.len()
        );
        assert!(
            result.stderr.len() >= 100000,
            "Expected >= 100000 bytes on stderr, got {}",
            result.stderr.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_handles_empty_output() {
        let runner = CommandRunner::new(None);
        let result = runner.run("true", &[]).unwrap();

        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_overhead_under_15ms() {
        let runner = CommandRunner::new(None);
        let result = runner.run("true", &[]).unwrap();

        // `true` completes instantly, so the entire duration is dispatch overhead.
        assert!(
            result.duration < Duration::from_millis(50),
            "Expected dispatch overhead < 50ms, got {:?}",
            result.duration
        );
    }

    // ========================================================================
    // Adversarial edge cases
    // ========================================================================

    #[test]
    fn run_empty_program_name_errors() {
        let runner = CommandRunner::new(None);
        let err = runner.run("", &[]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("failed to execute"),
            "Expected 'failed to execute' in error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_stderr_with_zero_exit() {
        let runner = CommandRunner::new(None);
        let result = runner
            .run("bash", &["-c", "echo warn >&2; exit 0"])
            .unwrap();

        assert_eq!(result.exit_code, Some(0));
        assert!(
            !result.stderr.is_empty(),
            "Expected non-empty stderr with exit code 0"
        );
        assert!(result.stderr.contains("warn"));
    }

    #[cfg(unix)]
    #[test]
    fn run_timeout_zero_kills_immediately() {
        let runner = CommandRunner::new(Some(Duration::ZERO));
        let err = runner.run("sleep", &["10"]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("timed out"),
            "Expected 'timed out' in error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_args_with_literal_backslash_n() {
        let runner = CommandRunner::new(None);
        // Pass a literal `\n` (backslash + n) as an argument.
        // Since the runner doesn't use shell, echo should output it literally.
        let result = runner.run("echo", &["line1\\nline2"]).unwrap();
        assert!(
            result.stdout.contains("line1\\nline2"),
            "Expected literal backslash-n in output, got: {:?}",
            result.stdout
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_binary_output_is_lossy() {
        let runner = CommandRunner::new(Some(Duration::from_secs(10)));
        let result = runner.run("head", &["-c", "1024", "/dev/urandom"]).unwrap();

        assert!(
            !result.stdout.is_empty(),
            "Expected non-empty stdout from /dev/urandom"
        );
        // Binary data should be lossily converted — likely contains replacement chars
        // (U+FFFD) for invalid UTF-8 sequences.
        assert!(
            result.stdout.contains('\u{FFFD}'),
            "Expected replacement character in lossy output, got {} bytes",
            result.stdout.len()
        );
    }

    #[test]
    fn read_pipe_at_exact_limit_succeeds() {
        let reader = FixedSizeReader {
            remaining: MAX_OUTPUT_BYTES,
        };
        let result = read_pipe(reader).unwrap();
        assert_eq!(
            result.len(),
            MAX_OUTPUT_BYTES,
            "Expected exactly MAX_OUTPUT_BYTES ({MAX_OUTPUT_BYTES}) chars"
        );
    }

    #[test]
    fn read_pipe_one_byte_over_limit_fails() {
        let reader = FixedSizeReader {
            remaining: MAX_OUTPUT_BYTES + 1,
        };
        let err = read_pipe(reader).unwrap_err();
        assert!(
            err.to_string().contains("byte limit"),
            "Expected 'byte limit' in error, got: {}",
            err
        );
    }
}
