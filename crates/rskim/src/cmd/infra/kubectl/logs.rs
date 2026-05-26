//! `kubectl logs` parser.
//!
//! Delegates to the shared log compression pipeline (`compress_log`).
//! Converts `ParseResult<LogResult>` → `ParseResult<InfraResult>`.

use crate::cmd::log::{LogFlags, compress_log};
use crate::output::ParseResult;
use crate::output::canonical::InfraResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, log_result_to_infra};

/// No-op: `kubectl logs` has no format flag to inject.
pub(crate) fn prepare_args(_args: &mut Vec<String>) {
    // Intentionally empty.
}

/// Parse function for `kubectl logs` output.
///
/// Delegates to the shared log compression pipeline.
pub(crate) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    let flags = LogFlags::default();
    match compress_log(text, &flags) {
        ParseResult::Full(log_result) => {
            ParseResult::Full(log_result_to_infra(log_result, "kubectl", "logs"))
        }
        ParseResult::Degraded(log_result, warnings) => {
            ParseResult::Degraded(log_result_to_infra(log_result, "kubectl", "logs"), warnings)
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

    #[test]
    fn test_logs_structured() {
        let fixture = load_fixture("infra", "kubectl_logs.txt");
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            !matches!(result, ParseResult::Passthrough(_)),
            "expected structured result for log content"
        );
    }

    #[test]
    fn test_logs_result_contains_kubectl() {
        let fixture = load_fixture("infra", "kubectl_logs.txt");
        let output = make_output(&fixture);
        match parse_impl(&output) {
            ParseResult::Full(r) | ParseResult::Degraded(r, _) => {
                assert!(r.to_string().contains("kubectl"));
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
    fn test_prepare_args_is_noop() {
        let mut args = vec!["logs".to_string(), "my-pod".to_string()];
        let original = args.clone();
        prepare_args(&mut args);
        assert_eq!(args, original);
    }
}
