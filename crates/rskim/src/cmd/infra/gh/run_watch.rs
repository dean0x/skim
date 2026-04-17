//! `gh run watch` streaming output compression.
//!
//! Parses the live workflow run stream from `gh run watch`, emitting compressed
//! status lines as each job transitions through its lifecycle.
//!
//! # State machine
//!
//! The parser tracks per-job state in a HashMap (capped at [`MAX_STREAM_JOBS`]).
//! Each job entry records the job name and current status.  When a status
//! transition is detected (job start, completion, failure), a compressed line
//! is emitted.
//!
//! State machine rules:
//! 1. New job line (`In progress`, `Queued`, `Waiting`) → emit `⏳ {name}`.
//! 2. Job completion (`Completed`, `Success`) → emit `✓ {name}`.
//! 3. Job failure (`Failure`, `Failed`) → emit `✗ {name} [FAILED]`.
//! 4. Progress/noise lines (dots, percentages, unchanged status) → suppressed.
//! 5. Error lines → passed through.
//!
//! # Non-retention design (AD-STR-4)
//!
//! No history buffer is maintained.  Only the current step state is tracked.
//! Parsers must be stateless across lines except for the jobs HashMap.
//!
//! # DESIGN NOTE (AD-GRW-1) — ANSI strip in reader, not parser
//!
//! `gh run watch` uses `\r` cursor rewrites for in-place status updates.
//! The streaming harness splits on `\n` and strips trailing `\r` after
//! splitting, so the parser never sees `\r`.  ANSI escape codes are stripped
//! by `strip_ansi` in the streaming reader (see `streaming.rs`) before lines
//! reach this parser.  The parser does NOT call `strip_ansi` itself.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::process::ExitCode;

use super::streaming::{run_streamed_spawned, run_streamed_stdin, StreamConfig, StreamTotals, StreamingParser};

// ============================================================================
// Constants
// ============================================================================

/// Maximum number of concurrent jobs tracked in the streaming state.
///
/// gh run watch may expand matrices to many jobs.  Capping at 64 prevents
/// unbounded HashMap growth on pathological matrix configurations.
pub(super) const MAX_STREAM_JOBS: usize = 64;

// ============================================================================
// Public entry point
// ============================================================================

/// Detect whether to read from piped stdin.
///
/// Returns `true` when stdin is not a terminal AND `args` is empty —
/// the same heuristic used by [`super::super::run_infra_tool`].  Shared
/// by `run_watch` and the `gh api` dispatch arm in `gh/mod.rs` so that
/// the detection logic lives in exactly one place per module.
pub(super) fn should_use_stdin(args: &[String]) -> bool {
    !std::io::stdin().is_terminal() && args.is_empty()
}

/// Run `gh run watch` with streaming compression.
///
/// Supports two modes:
/// - **Pipe mode** (`gh run watch | skim infra gh run watch`): when stdin is
///   piped and no args are provided, reads from stdin via
///   [`run_streamed_stdin`].
/// - **Spawn mode** (`skim infra gh run watch <id>`): spawns `gh run watch
///   [args]` as a child process via [`run_streamed_spawned`].
///
/// `--exit-status` flag is propagated to `gh` in spawn mode; non-zero
/// workflow exit is forwarded as the process exit code.
pub(super) fn run_watch(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<ExitCode> {
    let parser = Box::new(RunWatchParser::new());

    let label = if ctx.show_stats || ctx.analytics_enabled {
        format!("skim infra gh run watch {}", args.join(" "))
    } else {
        String::new()
    };

    let cfg = StreamConfig {
        analytics_enabled: ctx.analytics_enabled,
        label,
    };

    // Pipe mode: stdin is piped and no run-ID args were given (AD-STR-2).
    if should_use_stdin(args) {
        return Ok(run_streamed_stdin(parser, cfg));
    }

    // Spawn mode: build `gh run watch [args]` and stream its output.
    let mut gh_args = vec!["run".to_string(), "watch".to_string()];
    gh_args.extend_from_slice(args);

    Ok(run_streamed_spawned(parser, "gh", &gh_args, cfg))
}

// ============================================================================
// Parser implementation
// ============================================================================

/// Job status as tracked by the streaming parser.
#[derive(Debug, Clone, PartialEq)]
enum JobStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
}

/// Streaming parser for `gh run watch` output.
///
/// Tracks job state transitions and emits one summary line per meaningful
/// state change.  Progress dots and unchanged status lines are suppressed.
pub(super) struct RunWatchParser {
    jobs: HashMap<String, JobStatus>,
    totals: StreamTotals,
    any_failure: bool,
}

impl RunWatchParser {
    pub(super) fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            totals: StreamTotals::default(),
            any_failure: false,
        }
    }

    /// Attempt to parse a job status line from `gh run watch` output.
    ///
    /// `gh run watch` emits lines like:
    /// - `  ✓ build (ubuntu-latest)  Completed`
    /// - `  * build (ubuntu-latest)  In progress`
    /// - `  X test  Failed`
    ///
    /// We extract the job name (trimming status symbols/whitespace) and derive
    /// the new status from the trailing word.
    fn try_parse_job_line(&self, line: &str) -> Option<(String, JobStatus)> {
        let trimmed = line.trim();

        // Must be indented (job lines have leading spaces/symbols).
        if trimmed.is_empty() {
            return None;
        }

        // Check for status indicators.
        let (status, rest) = if trimmed.starts_with('✓') || trimmed.starts_with("Pass") {
            (JobStatus::Completed, trimmed.trim_start_matches('✓').trim())
        } else if trimmed.starts_with('✗') || trimmed.starts_with('X') || trimmed.starts_with("Fail") {
            let rest = trimmed
                .trim_start_matches('✗')
                .trim_start_matches('X')
                .trim();
            (JobStatus::Failed, rest)
        } else if trimmed.starts_with('*') || trimmed.contains("In progress") {
            let rest = trimmed.trim_start_matches('*').trim();
            (JobStatus::InProgress, rest)
        } else if trimmed.contains("Queued") || trimmed.contains("Waiting") {
            (JobStatus::Queued, trimmed)
        } else {
            return None;
        };

        // Extract job name: everything before the last status word.
        let name = rest
            .trim_end_matches("Completed")
            .trim_end_matches("Success")
            .trim_end_matches("Failed")
            .trim_end_matches("Failure")
            .trim_end_matches("In progress")
            .trim_end_matches("Queued")
            .trim_end_matches("Waiting")
            .trim()
            .to_string();

        if name.is_empty() {
            return None;
        }

        Some((name, status))
    }
}

impl StreamingParser for RunWatchParser {
    /// Process one line from `gh run watch` output.
    ///
    /// Returns a compressed summary line on meaningful status transitions,
    /// `None` for noise (progress dots, unchanged status, empty lines).
    fn on_line(&mut self, line: &str) -> Option<String> {
        self.totals.raw_bytes += line.len() + 1;

        // Pass through error lines.
        if line.contains("error:") || line.contains("Error:") {
            let out = line.to_string();
            self.totals.compressed_bytes += out.len() + 1;
            return Some(out);
        }

        // Try to parse a job status transition.
        if let Some((name, new_status)) = self.try_parse_job_line(line) {
            // Cap at MAX_STREAM_JOBS.
            if self.jobs.len() >= MAX_STREAM_JOBS && !self.jobs.contains_key(&name) {
                return None;
            }

            let old_status = self.jobs.get(&name).cloned();
            let changed = old_status.as_ref() != Some(&new_status);

            if changed {
                self.jobs.insert(name.clone(), new_status.clone());

                let output = match &new_status {
                    JobStatus::Completed => format!("✓ {name}"),
                    JobStatus::Failed => {
                        self.any_failure = true;
                        format!("✗ {name} [FAILED]")
                    }
                    JobStatus::InProgress => format!("⏳ {name}"),
                    JobStatus::Queued => format!("⏸ {name} [queued]"),
                };
                self.totals.compressed_bytes += output.len() + 1;
                return Some(output);
            }
        }

        None // Suppress noise
    }

    /// Emit a final summary line at EOF.
    fn finalize(self: Box<Self>) -> Option<String> {
        let completed = self
            .jobs
            .values()
            .filter(|s| **s == JobStatus::Completed)
            .count();
        let failed = self
            .jobs
            .values()
            .filter(|s| **s == JobStatus::Failed)
            .count();
        let total = self.jobs.len();

        if total == 0 {
            return None;
        }

        let summary = if failed > 0 {
            format!("Run complete: {completed}/{total} succeeded, {failed} FAILED")
        } else {
            format!("Run complete: {total}/{total} succeeded")
        };

        Some(summary)
    }

    fn totals(&self) -> StreamTotals {
        self.totals
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parser() -> RunWatchParser {
        RunWatchParser::new()
    }

    #[test]
    fn test_completed_job_emits_checkmark() {
        let mut p = make_parser();
        let out = p.on_line("  ✓ build (ubuntu-latest)  Completed");
        assert!(out.is_some(), "should emit on completion");
        let line = out.unwrap();
        assert!(line.starts_with('✓'), "line: {line}");
        assert!(line.contains("build"), "line: {line}");
    }

    #[test]
    fn test_failed_job_emits_failure() {
        let mut p = make_parser();
        let out = p.on_line("  X test Failed");
        assert!(out.is_some());
        let line = out.unwrap();
        assert!(line.contains("FAILED"), "line: {line}");
        assert!(p.any_failure);
    }

    #[test]
    fn test_in_progress_job_emits_hourglass() {
        let mut p = make_parser();
        let out = p.on_line("  * build In progress");
        assert!(out.is_some());
        let line = out.unwrap();
        assert!(line.contains('⏳'), "line: {line}");
    }

    #[test]
    fn test_noise_suppressed() {
        let mut p = make_parser();
        // Empty lines and irrelevant text are suppressed.
        assert!(p.on_line("").is_none());
        assert!(p.on_line("...").is_none());
        assert!(p.on_line("GitHub Actions").is_none());
    }

    #[test]
    fn test_no_duplicate_transition() {
        let mut p = make_parser();
        // First in-progress transition emits.
        assert!(p.on_line("  * build In progress").is_some());
        // Same status again → suppressed.
        assert!(p.on_line("  * build In progress").is_none());
    }

    #[test]
    fn test_finalize_all_success() {
        let mut p = make_parser();
        p.on_line("  ✓ build Completed");
        p.on_line("  ✓ test Completed");
        let summary = Box::new(p).finalize().unwrap();
        assert!(summary.contains("2/2 succeeded"), "summary: {summary}");
    }

    #[test]
    fn test_finalize_with_failures() {
        let mut p = make_parser();
        p.on_line("  ✓ build Completed");
        p.on_line("  X test Failed");
        let summary = Box::new(p).finalize().unwrap();
        assert!(summary.contains("FAILED"), "summary: {summary}");
    }

    #[test]
    fn test_finalize_empty_no_output() {
        let p = make_parser();
        assert!(Box::new(p).finalize().is_none());
    }

    #[test]
    fn test_max_jobs_cap() {
        let mut p = make_parser();
        // Fill up to MAX_STREAM_JOBS.
        for i in 0..MAX_STREAM_JOBS {
            p.on_line(&format!("  ✓ job{i} Completed"));
        }
        // Next job should be suppressed (cap reached).
        let out = p.on_line("  * overflow_job In progress");
        assert!(out.is_none(), "should suppress when cap reached");
    }

    #[test]
    fn test_error_line_passes_through() {
        let mut p = make_parser();
        let out = p.on_line("error: workflow run failed");
        assert!(out.is_some());
        assert!(out.unwrap().contains("error:"));
    }

    #[test]
    fn test_already_finished_run() {
        // An already-finished run may emit no job lines at all.
        let p = make_parser();
        // finalize on zero state should not panic.
        let result = std::panic::catch_unwind(|| {
            Box::new(p).finalize()
        });
        assert!(result.is_ok(), "finalize on empty state should not panic");
    }

    // ---- should_use_stdin helper ----

    #[test]
    fn test_should_use_stdin_returns_false_when_args_present() {
        // When args are non-empty, stdin mode must not be selected regardless
        // of terminal state (the tty check is moot at the unit level; we test
        // the args gate here).
        let args: Vec<String> = vec!["12345".to_string()];
        // We can't mock IsTerminal in a unit test, but we can verify the
        // logic short-circuits on args.is_empty().  The helper must return
        // false whenever args is non-empty because the IS_TERMINAL check is
        // AND-joined with args.is_empty().
        // Both branches must be true for stdin mode; a non-empty args slice
        // makes the result false regardless of terminal state.
        //
        // Note: `should_use_stdin` returns false in unit tests because the
        // test binary's stdin IS a terminal (cargo test does not pipe stdin).
        // That is the intended behaviour — no false positives in unit tests.
        assert!(
            !should_use_stdin(&args),
            "non-empty args must not trigger stdin mode"
        );
    }

    #[test]
    fn test_should_use_stdin_args_gate_short_circuits() {
        // Verify that any non-empty args slice always prevents stdin mode,
        // regardless of the terminal state.  This tests the args.is_empty()
        // gate in isolation: the AND condition means a non-empty args slice
        // short-circuits to false before the is_terminal() check runs.
        //
        // We use several non-empty arg scenarios to confirm the gate.
        let cases: &[&[&str]] = &[
            &["12345"],
            &["--exit-status"],
            &["12345", "--exit-status"],
        ];
        for args_strs in cases {
            let args: Vec<String> = args_strs.iter().map(|s| s.to_string()).collect();
            assert!(
                !should_use_stdin(&args),
                "non-empty args {:?} must not trigger stdin mode",
                args
            );
        }
    }

    // ---- Parser integration (pipe path simulation) ----

    #[test]
    fn test_parser_processes_pipe_scenario_lines() {
        // Mirrors the Tester's scenario:
        //   printf "workflow step 1\nworkflow step 2\ncompleted\n" | skim infra gh run watch
        //
        // The RunWatchParser receives these lines via on_line().  None match
        // job-status patterns, so all are suppressed; finalize() on empty
        // state returns None (no output, clean exit — not a crash).
        let mut p = make_parser();
        assert!(p.on_line("workflow step 1").is_none());
        assert!(p.on_line("workflow step 2").is_none());
        assert!(p.on_line("completed").is_none());
        let summary = Box::new(p).finalize();
        // Empty jobs map → no summary (None is valid; not an error).
        assert!(
            summary.is_none(),
            "empty job state should produce no summary"
        );
    }

    #[test]
    fn test_parser_pipe_with_job_lines_emits_output() {
        // Validates that the streaming parser correctly handles a mix of real
        // job-status lines delivered via stdin (pipe path).
        let mut p = make_parser();
        let out1 = p.on_line("  * build In progress");
        let out2 = p.on_line("  ✓ build Completed");
        assert!(out1.is_some(), "in-progress line should emit output");
        assert!(out2.is_some(), "completion line should emit output");
        let summary = Box::new(p).finalize().unwrap();
        assert!(
            summary.contains("1/1 succeeded"),
            "summary should report success: {summary}"
        );
    }
}
