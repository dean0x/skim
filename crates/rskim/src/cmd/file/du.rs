//! du parser.
//!
//! Parses `du` output (disk usage per path) into structured `FileResult`.
//!
//! Tiers:
//! - **Tier 1 (Full)**: Tab-separated size/path lines
//! - **Tier 3 (Passthrough)**: Empty output or parse failure

use std::process::ExitCode;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use super::{MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "du",
    env_overrides: &[],
    install_hint: "du is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
};

/// Run `skim du [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Three-tier parse function for du output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    if let Some(result) = try_parse_du(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: du output parsing
// ============================================================================

fn try_parse_du(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut total_count = 0usize;

    for (i, line) in stdout.lines().enumerate() {
        if i >= MAX_INPUT_LINES {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }

        // du output: size<TAB>path
        // Split on first tab only
        if let Some(tab_pos) = line.find('\t') {
            let size = line[..tab_pos].trim();
            let path = line[tab_pos + 1..].trim();
            if !size.is_empty() && !path.is_empty() {
                total_count += 1;
                if entries.len() < MAX_DISPLAY_ENTRIES {
                    entries.push(format!("{size}\t{path}"));
                }
            }
        }
    }

    if total_count == 0 {
        return None;
    }

    let shown_count = entries.len();
    let footer = crate::output::elision_marker(shown_count, total_count, "entries");

    Some(FileResult::new(
        "du".to_string(),
        total_count,
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
    use crate::cmd::test_utils::{load_fixture, make_output_full};

    #[test]
    fn test_tier1_du_block_counts() {
        let input = load_fixture("file", "du_small.txt");
        let result = try_parse_du(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 10, "10 entries in du_small.txt");
        assert!(result.footer.is_none(), "Small fixture should not truncate");
    }

    #[test]
    fn test_tier1_du_human_readable() {
        let input = load_fixture("file", "du_human.txt");
        let result = try_parse_du(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert!(
            result.entries.iter().any(|e| e.contains("4.0K")),
            "Human-readable sizes should be preserved"
        );
    }

    #[test]
    fn test_tier1_du_single_summary() {
        // Single directory summary line
        let input = "100\t.\n";
        let result = try_parse_du(input);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.total_count, 1);
        assert_eq!(result.entries[0], "100\t.");
    }

    #[test]
    fn test_tier1_du_truncates_large() {
        // Build large du output exceeding MAX_DISPLAY_ENTRIES
        let mut lines = String::new();
        for i in 0..=MAX_DISPLAY_ENTRIES {
            lines.push_str(&format!("4\t./file{i}.txt\n"));
        }
        let result = try_parse_du(&lines).unwrap();
        assert!(result.total_count > MAX_DISPLAY_ENTRIES);
        assert_eq!(result.shown_count, MAX_DISPLAY_ENTRIES);
        assert!(result.footer.is_some(), "Should have footer when truncated");
    }

    #[test]
    fn test_tier3_empty_passthrough() {
        let output = make_output_full("", "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty output should be passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("file", "du_small.txt");
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "parse_impl with exit code 0 and valid du output should return Full, got {}",
            result.tier_name()
        );
    }
}
