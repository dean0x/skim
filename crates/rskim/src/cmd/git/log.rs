//! Git log compression — commit log formatting.

use std::process::ExitCode;

use crate::cmd::{extract_output_format, user_has_flag};
use crate::output::canonical::GitResult;

use super::{has_limit_flag, run_parsed_command, run_passthrough};

/// Run `git log` with compression.
///
/// Flag-aware passthrough: if user has `--format` or `--pretty` (custom
/// format strings that cannot be parsed generically), pass through unmodified.
/// `--oneline` is handled by stripping it and injecting the handler's own
/// `--format` flag instead.
pub(super) fn run_log(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--format", "--pretty"]) {
        return run_passthrough(global_flags, "log", args, show_stats);
    }

    // Strip --oneline — handler injects its own --format flag.
    let stripped_args: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--oneline")
        .cloned()
        .collect();

    let (filtered_args, output_format) = extract_output_format(&stripped_args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["log".to_string(), "--format=%h %s (%cr) <%an>".to_string()]);

    if !has_limit_flag(&filtered_args) {
        full_args.extend(["-n".to_string(), "20".to_string()]);
    }

    full_args.extend_from_slice(&filtered_args);

    // Build analytics label lazily from user's original args (before flag injection)
    // to match the convention used by run_show_commit and run_show_file_content.
    let label = if show_stats || crate::analytics::is_analytics_enabled() {
        format!("skim git log {}", args.join(" "))
    } else {
        String::new()
    };

    run_parsed_command(&full_args, show_stats, output_format, false, label, parse_log)
}

/// Parse formatted `git log` output into a compressed GitResult.
fn parse_log(output: &str) -> GitResult {
    let lines: Vec<String> = output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    let count = lines.len();
    let summary = if count == 0 {
        "no commits".to_string()
    } else if count == 1 {
        "1 commit".to_string()
    } else {
        format!("{count} commits")
    };

    GitResult::new("log".to_string(), summary, lines)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // parse_log tests
    // ========================================================================

    #[test]
    fn test_parse_log_format() {
        let output = include_str!("../../../tests/fixtures/cmd/git/log_format.txt");
        let result = parse_log(output);

        assert!(
            result.summary.contains("5 commits"),
            "expected '5 commits' in summary, got: {}",
            result.summary
        );
        assert_eq!(result.details.len(), 5, "expected 5 commit lines");
    }

    #[test]
    fn test_parse_log_single_commit() {
        let output = "abc1234 feat: initial commit (1 day ago) <Author>\n";
        let result = parse_log(output);
        assert_eq!(result.summary, "1 commit");
        assert_eq!(result.details.len(), 1);
    }

    #[test]
    fn test_parse_log_empty() {
        let result = parse_log("");
        assert_eq!(result.summary, "no commits");
        assert!(result.details.is_empty());
    }
}
