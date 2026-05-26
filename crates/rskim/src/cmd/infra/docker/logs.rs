//! `docker logs` parser.
//!
//! SAFETY INVARIANT: `docker logs` has no `--format` flag — do not inject anything.
//!
//! Delegates to `compress_log()` from the `log` subcommand for three-tier
//! log compression (JSON, regex, passthrough).
//!
//! Converts `ParseResult<LogResult>` → `ParseResult<InfraResult>` using
//! the rendered log summary text as the InfraResult value.

use crate::cmd::log::{LogFlags, compress_log};
use crate::output::ParseResult;
use crate::output::canonical::InfraResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, log_result_to_infra};

/// No-op: `docker logs` has no `--format` flag.
///
/// # Safety invariant
/// Do not inject any format flag for `docker logs`.
pub(crate) fn prepare_args(_args: &mut Vec<String>) {
    // Intentionally empty: no format injection for logs.
}

/// Parse function for `docker logs` output.
///
/// Delegates to the shared log compression pipeline and wraps the result
/// as an `InfraResult`.
pub(crate) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    let flags = LogFlags::default();
    match compress_log(text, &flags) {
        ParseResult::Full(log_result) => {
            ParseResult::Full(log_result_to_infra(log_result, "docker", "logs"))
        }
        ParseResult::Degraded(log_result, warnings) => {
            ParseResult::Degraded(log_result_to_infra(log_result, "docker", "logs"), warnings)
        }
        ParseResult::Passthrough(raw) => ParseResult::Passthrough(raw),
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture, make_output};

    fn logs_fixture() -> String {
        load_fixture("infra", "docker_logs.txt")
    }

    #[test]
    fn test_logs_parses_timestamped_lines() {
        let fixture = logs_fixture();
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        // Should produce Full or Degraded (not passthrough for structured log content)
        assert!(
            !matches!(result, ParseResult::Passthrough(_)),
            "expected Full or Degraded for log content"
        );
    }

    #[test]
    fn test_logs_result_contains_summary() {
        let fixture = logs_fixture();
        let output = make_output(&fixture);
        match parse_impl(&output) {
            ParseResult::Full(r) | ParseResult::Degraded(r, _) => {
                let display = r.to_string();
                assert!(display.contains("docker"), "should contain tool name");
                assert!(display.contains("logs"), "should contain operation");
            }
            ParseResult::Passthrough(_) => panic!("should not passthrough structured logs"),
        }
    }

    #[test]
    fn test_empty_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_garbage_passthrough() {
        let output = make_output("not-a-log-line-at-all-just-random-text");
        let result = parse_impl(&output);
        // Unstructured content without timestamps falls to passthrough
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    /// Safety invariant: prepare_args must never inject any flag for `docker logs`.
    #[test]
    fn test_prepare_args_is_noop() {
        let mut args = vec!["logs".to_string(), "--tail".to_string(), "100".to_string()];
        let original = args.clone();
        prepare_args(&mut args);
        assert_eq!(
            args, original,
            "prepare_args must not modify args for docker logs"
        );
    }
}
