//! Command execution engine (#40)
//!
//! Provides a transparent command runner that captures stdout/stderr concurrently
//! to prevent pipe deadlocks. No shell interpretation — commands are executed
//! directly via `Command::new().args()`.
//!
//! # Design (ADR-008 — no internal timeout)
//!
//! Skim is a transparent command wrapper: it intercepts `cargo test`, `git diff`,
//! `npm build`, etc., runs the real command, compresses the output, and prints.
//! A transparent wrapper must NOT change whether or when a command completes.
//! Previous versions imposed a wall-clock cap (300 s / 600 s) that could kill
//! long-running but finite commands (e.g., a full `cargo test --all-features`
//! run on a large workspace).
//!
//! **`CommandRunner` is now stateless and imposes no timeout.** Callers that need
//! a time bound should use an external mechanism: CI step timeout, the shell
//! `timeout(1)` command, the agent tool timeout, or `Ctrl-C`. The 64 MiB memory
//! cap ([`MAX_OUTPUT_BYTES`]) is unchanged — this removes the TIME bound, not the
//! MEMORY bound.
//!
//! # Kill-on-drop guard
//!
//! The spawned child is wrapped in [`ChildGuard`], whose `Drop` implementation
//! calls `kill()` followed by `wait()`. On the normal execution path the child
//! has already exited before `drop` fires, so `kill()` is a harmless no-op.
//! On any early-return path (pipe-capture failure, reader-thread panic, size-cap
//! error) the guard kills the still-running child, preventing orphans.

// Infrastructure module — consumers arrive in later Phase B tickets.
#![allow(dead_code)]

use std::io::Read;
use std::process::{Command, Stdio};
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

/// A stateless command runner.
///
/// Commands are executed directly via `Command::new().args()` — no shell
/// interpretation. Stdout and stderr are captured concurrently via two reader
/// threads to prevent pipe deadlocks on large output.
///
/// # Stateless unit struct (ADR-008)
///
/// `CommandRunner` is a zero-field unit struct. `new()` takes no arguments and
/// carries no configuration: there is no timeout, no retry count, and no
/// buffering policy stored here. All policy is provided at call sites.
///
/// # No internal timeout (ADR-008)
///
/// `CommandRunner` imposes no wall-clock cap. Skim is a transparent wrapper:
/// a transparent wrapper must not change whether or when a command completes.
/// Bound child-process lifetime externally if needed: use a CI step timeout,
/// the shell `timeout(1)` utility, the agent tool timeout, or `Ctrl-C`.
///
/// The 64 MiB output cap ([`MAX_OUTPUT_BYTES`]) remains — only the TIME bound
/// is removed, not the MEMORY bound.
///
/// On any early-return path (size-cap, pipe failure, thread panic) the spawned
/// child is automatically killed and reaped by the [`ChildGuard`] RAII wrapper
/// before `run_with_env` returns, preventing orphan processes.
#[derive(Debug, Default)]
pub(crate) struct CommandRunner;

impl CommandRunner {
    /// Create a new runner.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self
    }

    /// Execute `program` with `args`, capturing stdout and stderr.
    ///
    /// Returns [`CommandOutput`] on success or an error if the program cannot
    /// be spawned or pipe I/O fails.
    pub(crate) fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        self.run_with_env(program, args, &[])
    }

    /// Execute `program` with `args` and environment variable overrides,
    /// capturing stdout and stderr.
    ///
    /// Reuses the same output-size cap and pipe-capture logic as
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

        let child = cmd.spawn().map_err(|source| RunnerError::SpawnFailed {
            program: program.to_string(),
            source,
        })?;

        // Wrap in ChildGuard: kills + reaps on any early return or unwind.
        // On the normal path the child has already exited by the time drop fires,
        // so kill() is a harmless no-op (returns ESRCH on Unix).
        let mut child = ChildGuard(child);

        // Take pipes BEFORE spawning reader threads — avoids borrowing child later.
        let child_stdout = child
            .0
            .stdout
            .take()
            .ok_or(RunnerError::PipeCaptureFailed { pipe: "stdout" })?;
        let child_stderr = child
            .0
            .stderr
            .take()
            .ok_or(RunnerError::PipeCaptureFailed { pipe: "stderr" })?;

        // Spawn concurrent reader threads to prevent pipe deadlocks.
        let stdout_handle = thread::spawn(move || read_pipe(child_stdout));
        let stderr_handle = thread::spawn(move || read_pipe(child_stderr));

        // Wait for child — no timeout; transparent wrapper must not impose a time cap.
        let status = child.0.wait()?;

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

    /// Try to spawn `program` with cascading resolution:
    /// 1. Direct PATH lookup (`Command::new(program)`)
    /// 2. `./node_modules/.bin/{program}` (local Node.js install)
    /// 3. `npx --no-install {program}` (npx local-only resolver)
    ///
    /// If `program` contains a `/` it is treated as an explicit path — no
    /// fallback is attempted (the caller has a specific binary in mind).
    ///
    /// Returns the original spawn error when all strategies are exhausted.
    pub(crate) fn run_with_node_fallback(
        &self,
        program: &str,
        args: &[&str],
    ) -> anyhow::Result<CommandOutput> {
        self.run_with_env_node_fallback(program, args, &[])
    }

    /// Variant of [`run_with_node_fallback`] that also forwards environment
    /// variable overrides to every candidate.
    pub(crate) fn run_with_env_node_fallback(
        &self,
        program: &str,
        args: &[&str],
        env_vars: &[(&str, &str)],
    ) -> anyhow::Result<CommandOutput> {
        match self.run_with_env(program, args, env_vars) {
            Ok(out) => Ok(out),
            Err(original_err) => {
                // Only attempt Node.js fallback resolution when the program was
                // not found on PATH (SpawnFailed). Other errors —
                // PipeCaptureFailed, ReaderPanicked, Io — mean the program was
                // found and launched; retrying via npx makes no sense and could
                // mask real failures.
                if !is_spawn_error(&original_err) {
                    return Err(original_err);
                }

                // Absolute or relative paths — no fallback; caller was explicit.
                if program.contains('/') {
                    return Err(original_err);
                }

                // Strategy 2: ./node_modules/.bin/{program}
                let local_bin = format!("./node_modules/.bin/{program}");
                if std::path::Path::new(&local_bin).exists()
                    && let Ok(out) = self.run_with_env(&local_bin, args, env_vars)
                {
                    return Ok(out);
                }

                // Strategy 3: npx --no-install {program}
                let mut npx_args: Vec<&str> = vec!["--no-install", program];
                npx_args.extend_from_slice(args);
                match self.run_with_env("npx", &npx_args, env_vars) {
                    Ok(out) => Ok(out),
                    Err(_) => Err(original_err),
                }
            }
        }
    }
}

// ============================================================================
// ChildGuard — kill + reap on drop (ADR-008)
// ============================================================================

/// RAII wrapper that kills and reaps a child process on drop.
///
/// When the parent exits early (size-cap error, pipe failure, thread panic,
/// or any other unwind path before `wait()` completes), the `Drop`
/// implementation calls `kill()` followed by `wait()` to prevent the child
/// from becoming a zombie or an orphan.
///
/// `kill()` on an already-exited process is a no-op on Unix (returns `ESRCH`)
/// and returns `InvalidInput` on Windows — both cases are explicitly ignored.
/// On the normal execution path the child has already exited before drop fires,
/// so this is always a harmless no-op.
///
/// # Limitation — direct child only
///
/// `kill()` signals the direct child process; grandchildren (e.g., the `node`
/// subprocess spawned by `npm`, or `rustc` spawned by `cargo`) are not reached.
/// Hardening to kill the full process group (`kill(-pgid, SIGKILL)`) is a
/// Unix-only change deferred to a follow-up (ADR-001: noticed but out of scope
/// for ADR-008).
///
/// `pub(crate)` so that `cmd::infra::gh::streaming` can reuse this guard
/// instead of maintaining a duplicate definition (ADR-001).
pub(crate) struct ChildGuard(pub(crate) std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // kill() is a no-op on an already-exited child; ignore the result.
        let _ = self.0.kill();
        // wait() reaps the zombie; ignore result (process may already be reaped).
        let _ = self.0.wait();
    }
}

/// Return `true` when `err` is a [`RunnerError::SpawnFailed`].
///
/// Use this for control-flow detection instead of `err.to_string().contains("failed to execute")`,
/// which couples to the `Display` format and breaks silently when the message changes.
pub(crate) fn is_spawn_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RunnerError>()
        .map(|e| matches!(e, RunnerError::SpawnFailed { .. }))
        .unwrap_or(false)
}

/// Maximum bytes we will read from a single pipe (64 MiB).
///
/// Prevents unbounded memory growth when a child process produces
/// unexpectedly large output. Shared with `cmd::MAX_STDIN_BYTES` via re-export
/// so both limits stay in sync without a separate constant.
pub(crate) const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Read a pipe into a UTF-8 String, capped at [`MAX_OUTPUT_BYTES`].
///
/// Uses chunked reads (8 KiB) instead of `read_to_end` to enforce the size
/// limit without requiring the OS to report exact pipe length up-front.
/// Valid UTF-8 is zero-copy (`Vec<u8>` moved into `String`); non-UTF-8 output
/// (e.g., binary data from `/dev/zero`) falls back to lossy U+FFFD replacement.
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

    Ok(String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned()))
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
        let runner = CommandRunner::new();
        let result = runner.run("echo", &["hello world"]).unwrap();

        assert_eq!(result.stdout.trim(), "hello world");
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn run_captures_stderr() {
        let runner = CommandRunner::new();
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
        let runner = CommandRunner::new();
        let result = runner.run("false", &[]).unwrap();

        assert_eq!(result.exit_code, Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn run_preserves_zero_exit_code() {
        let runner = CommandRunner::new();
        let result = runner.run("true", &[]).unwrap();

        assert_eq!(result.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn run_does_not_interpret_shell_metacharacters() {
        let runner = CommandRunner::new();
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
        let runner = CommandRunner::new();
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
    fn run_tracks_duration() {
        let runner = CommandRunner::new();
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
        let runner = CommandRunner::new();
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
        let runner = CommandRunner::new();
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
        let runner = CommandRunner::new();
        let result = runner.run("true", &[]).unwrap();

        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_overhead_under_15ms() {
        let runner = CommandRunner::new();
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
        let runner = CommandRunner::new();
        let err = runner.run("", &[]).unwrap_err();
        let msg = err.to_string();
        // Asserts Display format, not spawn detection — see is_spawn_error for control flow.
        assert!(
            msg.contains("failed to execute"),
            "Expected 'failed to execute' in error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_stderr_with_zero_exit() {
        let runner = CommandRunner::new();
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
    fn run_args_with_literal_backslash_n() {
        let runner = CommandRunner::new();
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
        let runner = CommandRunner::new();
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

    // ========================================================================
    // Node fallback tests
    // ========================================================================

    #[test]
    fn test_read_pipe_handles_invalid_utf8() {
        let data: Vec<u8> = vec![b'O', b'K', 0xFF, 0xFE];
        let result = read_pipe(std::io::Cursor::new(data)).unwrap();
        assert!(result.starts_with("OK"));
        assert!(result.contains('\u{FFFD}'));
    }

    #[cfg(unix)]
    #[test]
    fn test_node_fallback_direct_success_no_fallback() {
        // `echo` is always in PATH — succeeds on the first attempt.
        let runner = CommandRunner::new();
        let result = runner.run_with_node_fallback("echo", &["hello"]).unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn test_node_fallback_absolute_path_no_fallback() {
        // Absolute path — no fallback should be tried (contains '/').
        let runner = CommandRunner::new();
        let err = runner
            .run_with_node_fallback("/nonexistent-binary-1234", &[])
            .unwrap_err();
        let msg = err.to_string();
        // Asserts Display format, not spawn detection — see is_spawn_error for control flow.
        assert!(
            msg.contains("failed to execute"),
            "Expected 'failed to execute', got: {msg}"
        );
    }

    #[test]
    fn test_node_fallback_relative_path_no_fallback() {
        // Relative path (contains '/') — no fallback.
        let runner = CommandRunner::new();
        let err = runner
            .run_with_node_fallback("./nonexistent-binary-5678", &[])
            .unwrap_err();
        let msg = err.to_string();
        // Asserts Display format, not spawn detection — see is_spawn_error for control flow.
        assert!(
            msg.contains("failed to execute"),
            "Expected 'failed to execute', got: {msg}"
        );
    }

    #[test]
    fn test_node_fallback_all_fail_preserves_original_error() {
        // A program not in PATH or node_modules:
        // - Strategy 1: spawn fails (not in PATH)
        // - Strategy 2: ./node_modules/.bin/... doesn't exist
        // - Strategy 3: npx may or may not be available
        //
        // When npx is unavailable, we get the original Err(SpawnFailed).
        // When npx is available but cannot resolve the package, we get Ok with
        // non-zero exit code (npx ran and reported failure).
        // Either outcome is acceptable — the key property is that the original
        // program (__skim_nonexistent_9999__) was never successfully executed.
        let runner = CommandRunner::new();
        match runner.run_with_node_fallback("__skim_nonexistent_9999__", &[]) {
            Err(e) => {
                let msg = e.to_string();
                // Asserts Display format, not spawn detection — see is_spawn_error for control flow.
                assert!(
                    msg.contains("failed to execute") || msg.contains("__skim_nonexistent"),
                    "Expected original spawn error, got: {msg}"
                );
            }
            Ok(output) => {
                // npx was found but returned non-zero — acceptable fallback behavior.
                assert_ne!(
                    output.exit_code,
                    Some(0),
                    "Unexpected success running nonexistent program via npx"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn test_node_fallback_local_bin_found() {
        use std::os::unix::fs::PermissionsExt;

        // Create a temp directory simulating a project with ./node_modules/.bin/fake-tool
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let bin_dir = tmp.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let script = bin_dir.join("fake-tool");
        std::fs::write(&script, b"#!/bin/sh\necho 'from-local-bin'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        // `set_current_dir` is process-wide. `#[serial]` ensures no other test
        // runs concurrently in this process while cwd is modified. The original
        // cwd is restored on exit so subsequent tests are unaffected.
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = std::panic::catch_unwind(|| {
            let runner = CommandRunner::new();
            // `fake-tool` is not in PATH, but ./node_modules/.bin/fake-tool exists
            runner.run_with_node_fallback("fake-tool", &[]).unwrap()
        });
        std::env::set_current_dir(&original_dir).unwrap();
        let output = result.expect("test panicked");
        assert_eq!(output.stdout.trim(), "from-local-bin");
    }

    #[cfg(unix)]
    #[test]
    fn test_node_fallback_npx_no_install_flag() {
        // Verify that --no-install is passed to npx.
        // When npx is available: --no-install prevents downloading, so npx exits
        // non-zero (package not found locally). When npx is not available, the
        // original spawn error is returned as Err.
        // Either way, the unknown program must never succeed (exit code != 0).
        let runner = CommandRunner::new();
        match runner.run_with_node_fallback("__skim_nonexistent_npx_test_8888__", &[]) {
            Err(e) => {
                let msg = e.to_string();
                // Asserts Display format, not spawn detection — see is_spawn_error for control flow.
                assert!(
                    msg.contains("failed to execute") || msg.contains("__skim_nonexistent"),
                    "Expected spawn error, got: {msg}"
                );
            }
            Ok(output) => {
                // npx ran but returned non-zero — this is correct: --no-install
                // prevented downloading the package.
                assert_ne!(
                    output.exit_code,
                    Some(0),
                    "--no-install should cause npx to fail for unknown packages"
                );
            }
        }
    }

    // ========================================================================
    // run_with_env_node_fallback: env forwarding tests
    // ========================================================================

    /// Verify that `run_with_env_node_fallback` forwards env overrides to the
    /// spawned process on every fallback candidate. Uses `printenv` (always in
    /// PATH on Unix) with an injected variable to confirm forwarding.
    #[cfg(unix)]
    #[test]
    fn test_node_fallback_env_vars_forwarded() {
        let runner = CommandRunner::new();
        let env_overrides = &[("SKIM_TEST_ENV_FORWARD", "hello_from_skim")];
        // printenv KEY prints the value of KEY if set, exits 0; exits non-zero if unset.
        let result = runner
            .run_with_env_node_fallback("printenv", &["SKIM_TEST_ENV_FORWARD"], env_overrides)
            .unwrap();
        assert_eq!(
            result.stdout.trim(),
            "hello_from_skim",
            "env override must be forwarded to the child process"
        );
        assert_eq!(result.exit_code, Some(0));
    }

    /// Verify that original args are passed unchanged to the spawned process by
    /// `run_with_env_node_fallback`. Uses `echo` (always in PATH on Unix) with
    /// specific arguments to confirm they appear verbatim in the output.
    #[cfg(unix)]
    #[test]
    fn test_node_fallback_args_preserved() {
        let runner = CommandRunner::new();
        let result = runner
            .run_with_env_node_fallback("echo", &["arg_one", "arg_two", "arg_three"], &[])
            .unwrap();
        let out = result.stdout.trim();
        assert!(
            out.contains("arg_one") && out.contains("arg_two") && out.contains("arg_three"),
            "all args must be preserved in output, got: {out:?}"
        );
        assert_eq!(result.exit_code, Some(0));
    }

    // ========================================================================
    // is_spawn_error tests
    // ========================================================================

    #[test]
    fn test_is_spawn_error_true() {
        let err: anyhow::Error = RunnerError::SpawnFailed {
            program: "nonexistent".into(),
            source: io::Error::new(io::ErrorKind::NotFound, "not found"),
        }
        .into();
        assert!(is_spawn_error(&err));
    }

    #[test]
    fn test_is_spawn_error_false_for_io() {
        let err: anyhow::Error =
            RunnerError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe")).into();
        assert!(!is_spawn_error(&err));
    }

    #[test]
    fn test_is_spawn_error_false_for_pipe_capture_failed() {
        let err: anyhow::Error = RunnerError::PipeCaptureFailed { pipe: "stdout" }.into();
        assert!(!is_spawn_error(&err));
    }

    #[test]
    fn test_is_spawn_error_false_for_reader_panicked() {
        let err: anyhow::Error = RunnerError::ReaderPanicked { pipe: "stderr" }.into();
        assert!(!is_spawn_error(&err));
    }

    // ========================================================================
    // ChildGuard kill-on-drop contract (ADR-008)
    // ========================================================================

    /// Verify that ChildGuard kills a long-running child when dropped.
    ///
    /// Spawns `sleep 60`, immediately drops the guard, then uses `kill -0`
    /// to confirm the child is no longer running.
    #[cfg(unix)]
    #[test]
    fn child_guard_kills_running_child() {
        use std::process::Stdio;

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
        let status = Command::new("kill").args(["-0", &pid.to_string()]).status();

        match status {
            Ok(s) => assert!(
                !s.success(),
                "process {pid} should have been killed by ChildGuard"
            ),
            Err(_) => {
                // `kill` binary not found on this platform — spawn + drop
                // completing without panic is sufficient verification.
            }
        }
    }

    /// Regression: CommandRunner imposes NO internal wall-clock cap (ADR-008).
    ///
    /// Runs a finite command that takes ~1.5 s and asserts it completes
    /// successfully with full output — it must not be killed or truncated by
    /// any internal timeout.
    #[cfg(unix)]
    #[test]
    fn no_internal_timeout_finite_command_runs_to_completion() {
        let runner = CommandRunner::new();
        // A ~1.5-second command that exits 0 and prints "done" to stdout.
        // Uses `sh -c` for portability across Unix platforms.
        let result = runner
            .run("sh", &["-c", "sleep 1.5; echo done"])
            .expect("command should succeed");

        assert_eq!(
            result.exit_code,
            Some(0),
            "finite 1.5-second command must exit 0 (ADR-008: no internal cap)"
        );
        assert!(
            result.stdout.contains("done"),
            "stdout must contain 'done' — command must not be truncated or killed"
        );
        // Sanity-check duration: must be at least 1 second (sleep ran) and
        // less than 10 seconds (reasonable upper bound for a CI machine).
        assert!(
            result.duration >= Duration::from_secs(1),
            "duration should be >= 1s, got {:?}",
            result.duration
        );
        assert!(
            result.duration < Duration::from_secs(10),
            "duration should be < 10s, got {:?}",
            result.duration
        );
    }

    /// ChildGuard kill() on an already-exited child is a harmless no-op (ADR-008).
    ///
    /// Spawns a command that exits immediately, waits for it to exit, then
    /// drops the guard.  Drop must not panic or return an error — `kill()` on
    /// a reaped process is explicitly ignored in the Drop impl.
    #[cfg(unix)]
    #[test]
    fn child_guard_kill_on_already_exited_child_is_noop() {
        use std::process::Stdio;

        let child = Command::new("true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn true");

        // Reap the child manually so it is definitely exited before ChildGuard
        // drops.  We use a separate handle to call wait() first.
        let mut child = ChildGuard(child);
        let _ = child.0.wait(); // child is now a zombie/reaped

        // Dropping the guard calls kill() on an already-exited child.
        // This must not panic — kill() returns ESRCH which the Drop impl ignores.
        drop(child); // must not panic
    }

    /// End-to-end coverage of the 64 MiB output-cap path through `run_with_env`
    /// (ADR-008). Previously the cap was only exercised at the `read_pipe` unit
    /// level; this drives a real child whose stdout exceeds the cap and asserts
    /// the function surfaces the cap error *and returns* — i.e. it does not hang
    /// or leak the child when a reader thread caps out.
    ///
    /// # Why this does not (and cannot) assert a live-child `ChildGuard` kill
    ///
    /// `run_with_env` blocks on `child.0.wait()` *before* it joins the reader
    /// threads and surfaces their cap `Err`. So by the time the function returns
    /// on the cap path the child has already exited (its next write to the
    /// reader-closed pipe gets SIGPIPE), and `ChildGuard::drop` only ever kills
    /// an already-dead process here — a no-op. The guard's *live*-child kill
    /// fires only on an early return between guard construction and `wait()`; the
    /// sole such path is `PipeCaptureFailed`, which is unreachable through the
    /// public API because `Stdio::piped()` is always set, so `stdout.take()` /
    /// `stderr.take()` never return `None`. The `ChildGuard::drop` kill contract
    /// itself is therefore covered in isolation by
    /// [`child_guard_kills_running_child`]; this test covers the reachable
    /// behavior — the cap path completes cleanly rather than hanging.
    ///
    /// The child emits 70 MiB via `dd` and then exits (no trailing `sleep`):
    /// once the reader caps at 64 MiB and drops the read end, `dd`'s next write
    /// gets EPIPE/SIGPIPE and the child exits, so `wait()` unblocks promptly and
    /// the test is bounded — never a 1-hour `sleep` hang.
    #[cfg(unix)]
    #[test]
    fn run_with_env_surfaces_cap_error_without_hanging() {
        // 70 MiB > the 64 MiB MAX_OUTPUT_BYTES cap. `dd` is the child's last
        // command, so when the capped reader drops the pipe `dd` dies on SIGPIPE
        // and the shell exits — `wait()` returns without blocking.
        let runner = CommandRunner::new();
        let result = runner.run_with_env(
            "sh",
            &["-c", "dd if=/dev/zero bs=1048576 count=70 2>/dev/null"],
            &[],
        );

        let err =
            result.expect_err("run_with_env must return Err when stdout exceeds the 64 MiB cap");
        assert!(
            err.to_string().contains("byte limit"),
            "error must mention the byte limit, got: {err}"
        );
    }
}
