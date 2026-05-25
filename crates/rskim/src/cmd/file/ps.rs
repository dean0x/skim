//! ps parser.
//!
//! Parses `ps` output (process list) into structured `FileResult`.
//!
//! Tiers:
//! - **Tier 1 (Full)**: Header + process rows, truncated at MAX_DISPLAY_ENTRIES
//! - **Tier 3 (Passthrough)**: Empty output or parse failure

use std::process::ExitCode;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use super::{MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};
use crate::cmd::{ToolRunConfig, run_tool};
use crate::analytics::CommandType;

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "ps",
    env_overrides: &[],
    install_hint: "ps is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
};

/// Run `skim ps [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Three-tier parse function for ps output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    if let Some(result) = try_parse_ps(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: ps output parsing
// ============================================================================

fn try_parse_ps(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES + 1);
    let mut process_count = 0usize;
    let mut header_found = false;

    for (i, line) in stdout.lines().enumerate() {
        if i >= MAX_INPUT_LINES {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }

        if !header_found {
            // Find the header line containing "PID" (case-insensitive)
            if line.to_uppercase().contains("PID") {
                entries.push(line.to_string());
                header_found = true;
            }
            continue;
        }

        // Remaining lines are process rows
        process_count += 1;
        if entries.len() <= MAX_DISPLAY_ENTRIES {
            entries.push(line.to_string());
        }
    }

    if !header_found || process_count == 0 {
        return None;
    }

    let shown_count = entries.len().saturating_sub(1); // exclude header
    let footer = if process_count > MAX_DISPLAY_ENTRIES {
        Some(format!(
            "... and {} more processes",
            process_count - MAX_DISPLAY_ENTRIES
        ))
    } else {
        None
    };

    Some(FileResult::new(
        "ps".to_string(),
        process_count,
        shown_count,
        entries,
        footer,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture as _load_fixture, make_output_full};

    fn load_fixture(name: &str) -> String {
        _load_fixture("file", name)
    }

    fn make_large_ps() -> String {
        let mut lines = vec![
            "USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND".to_string(),
        ];
        for i in 1..=160 {
            lines.push(format!(
                "user     {i:>5}  0.0  0.0      0     0 ?        S    May01   0:00 process-{i}"
            ));
        }
        lines.join("\n")
    }

    fn make_output(stdout: &str, exit_code: i32) -> CommandOutput {
        make_output_full(stdout, "", Some(exit_code))
    }

    #[test]
    fn test_tier1_ps_small() {
        let input = load_fixture("ps_small.txt");
        let result = try_parse_ps(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 11, "11 processes in ps_small.txt");
        assert!(result.footer.is_none(), "Small fixture should not truncate");
    }

    #[test]
    fn test_tier1_ps_large_truncates() {
        let input = make_large_ps();
        let result = try_parse_ps(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert!(
            result.total_count > MAX_DISPLAY_ENTRIES,
            "Large input should exceed cap"
        );
        assert!(result.footer.is_some(), "Should have footer when truncated");
        let footer = result.footer.as_ref().unwrap();
        assert!(
            footer.contains("processes"),
            "Footer should mention processes"
        );
    }

    #[test]
    fn test_tier1_ps_preserves_header() {
        let input = load_fixture("ps_small.txt");
        let result = try_parse_ps(&input).unwrap();
        // First entry should be the header line with PID
        assert!(
            result.entries[0].contains("PID"),
            "First entry should be the header, got: {}",
            result.entries[0]
        );
    }

    #[test]
    fn test_tier1_ps_minimal() {
        let input = load_fixture("ps_minimal.txt");
        let result = try_parse_ps(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 2, "2 processes in ps_minimal.txt");
        assert!(result.entries[0].contains("PID"), "Header preserved");
    }

    #[test]
    fn test_tier3_empty_passthrough() {
        let output = make_output("", 1);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty output should be passthrough, got {}",
            result.tier_name()
        );
    }
}
