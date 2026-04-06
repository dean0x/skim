//! grep parser (#116).
//!
//! Parses `grep` output into structured `FileResult`.
//! grep has no JSON output mode, so regex is the best (and only) structured tier.
//!
//! Tiers:
//! - **Tier 1 (Full)**: Parse `file:line:content` format, group by file
//! - **Tier 2 (Passthrough)**: Raw output

use crate::output::canonical::FileResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{run_file_tool, try_parse_file_line_content, FileToolConfig, MAX_MATCHES_PER_FILE};

const CONFIG: FileToolConfig<'static> = FileToolConfig {
    program: "grep",
    env_overrides: &[],
    install_hint: "grep is typically pre-installed. For better compression, install ripgrep: https://github.com/BurntSushi/ripgrep",
};

/// Run `skim file grep [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection for grep -- flags are too varied
    run_file_tool(CONFIG, args, show_stats, json_output, |_| {}, parse_impl)
}

/// Two-tier parse function: Tier 1 regex -> Passthrough.
///
/// grep has no JSON output mode, so regex is the best available format
/// and is returned as `Full` (not Degraded).
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    if let Some(result) = try_parse_regex(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: file:line:content regex
// ============================================================================

/// Group grep matches by file, limit per-file and total-file counts.
///
/// Delegates to the shared `try_parse_file_line_content` in `file/mod.rs`.
/// `allow_stdin_fallback = true` so that plain lines (grep without `-n`) are
/// bucketed under `<stdin>` rather than silently dropped.
fn try_parse_regex(text: &str) -> Option<FileResult> {
    try_parse_file_line_content("grep", text, true)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/file");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    fn make_output(stdout: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: Duration::ZERO,
        }
    }

    #[test]
    fn test_tier1_grep_basic() {
        let input = load_fixture("grep_basic.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 1 grep parse to succeed");
        let result = result.unwrap();
        assert!(result.total_count > 0);
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("grep_basic.txt");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "grep regex output should be Full tier (best available), got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_empty_is_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty grep output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_file_grouping() {
        let input = "src/a.rs:1:fn main() {}\nsrc/a.rs:2:    println!()\nsrc/b.rs:5:fn run() {}";
        let result = try_parse_regex(input).unwrap();
        let rendered = format!("{result}");
        assert!(rendered.contains("src/a.rs"), "Should include file a.rs");
        assert!(rendered.contains("src/b.rs"), "Should include file b.rs");
    }

    #[test]
    fn test_max_matches_per_file_cap() {
        // Build 10 matches for same file -- should cap at MAX_MATCHES_PER_FILE
        let input: String = (1..=10)
            .map(|i| format!("src/big.rs:{i}:match line {i}\n"))
            .collect();
        let result = try_parse_regex(&input).unwrap();
        let rendered = format!("{result}");
        let match_lines: usize = rendered
            .lines()
            .filter(|l| l.trim().starts_with(':'))
            .count();
        assert!(
            match_lines <= MAX_MATCHES_PER_FILE,
            "Expected at most {MAX_MATCHES_PER_FILE} match lines per file, got {match_lines}"
        );
    }

    #[test]
    fn test_summary_line_present() {
        let input = "src/a.rs:1:hello world\n";
        let result = try_parse_regex(input).unwrap();
        let rendered = format!("{result}");
        assert!(
            rendered.contains("GREP:"),
            "Should contain GREP summary, got: {rendered}"
        );
        assert!(rendered.contains("matches in"));
    }

    #[test]
    fn test_display_format() {
        let input = "src/a.rs:1:fn main() {}\nsrc/b.rs:2:fn run() {}";
        let result = try_parse_regex(input).unwrap();
        let rendered = format!("{result}");
        assert!(
            rendered.contains("GREP: grep |"),
            "Header should start with GREP:"
        );
    }
}
