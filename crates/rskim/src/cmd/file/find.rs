//! find parser (#116).
//!
//! Parses `find` output (line-per-path) into structured `FileResult`.
//!
//! Tiers:
//! - **Tier 1 (Full)**: Line-per-path counting with streaming truncation
//! - **Tier 3 (Passthrough)**: Empty output on non-zero exit

use std::process::ExitCode;

use crate::output::canonical::FileResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{run_file_tool, FileToolConfig, MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};

const CONFIG: FileToolConfig<'static> = FileToolConfig {
    program: "find",
    env_overrides: &[],
    install_hint: "find is typically pre-installed on Unix systems",
};

/// Run `skim file find [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    // find has no useful flag injections — its output format is always line-per-path
    run_file_tool(
        CONFIG,
        args,
        show_stats,
        json_output,
        analytics_enabled,
        |_| {},
        parse_impl,
    )
}

/// Three-tier parse function for find output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    if let Some(result) = try_parse_lines(&output.stdout, output.exit_code) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: line-per-path parsing
// ============================================================================

/// Parse find output line-by-line: count total, keep first MAX_DISPLAY_ENTRIES.
///
/// Returns None only when there is literally nothing to show (empty output
/// on a successful run).
fn try_parse_lines(stdout: &str, exit_code: Option<i32>) -> Option<FileResult> {
    // Empty output on non-zero exit is passthrough (error condition)
    if stdout.trim().is_empty() && exit_code != Some(0) {
        return None;
    }

    let mut total_count = 0usize;
    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);

    for (i, line) in stdout.lines().enumerate() {
        if i >= MAX_INPUT_LINES {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        total_count += 1;
        if entries.len() < MAX_DISPLAY_ENTRIES {
            entries.push(trimmed.to_string());
        }
    }

    let shown_count = entries.len();
    let footer = if total_count > MAX_DISPLAY_ENTRIES {
        Some(format!(
            "... and {} more",
            total_count - MAX_DISPLAY_ENTRIES
        ))
    } else {
        None
    };

    Some(FileResult::new(
        "find".to_string(),
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
    use std::time::Duration;

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/file");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    fn make_output(stdout: &str, exit_code: i32) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(exit_code),
            duration: Duration::ZERO,
        }
    }

    #[test]
    fn test_tier1_find_small() {
        let input = load_fixture("find_small.txt");
        let result = try_parse_lines(&input, Some(0));
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert!(result.total_count > 0);
        assert!(
            result.total_count <= 10,
            "Small fixture should have <=10 entries"
        );
        assert!(
            result.footer.is_none(),
            "Small fixture should not be truncated"
        );
    }

    #[test]
    fn test_tier1_find_large_truncates() {
        let input = load_fixture("find_large.txt");
        let result = try_parse_lines(&input, Some(0));
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert!(
            result.total_count > MAX_DISPLAY_ENTRIES,
            "Large fixture should exceed cap"
        );
        assert_eq!(
            result.shown_count, MAX_DISPLAY_ENTRIES,
            "Shown should be capped at 100"
        );
        assert!(result.footer.is_some(), "Large fixture should have footer");
        let footer = result.footer.unwrap();
        assert!(
            footer.contains("more"),
            "Footer should mention remaining count"
        );
    }

    #[test]
    fn test_tier1_empty_on_success_returns_zero_result() {
        let result = try_parse_lines("", Some(0));
        // Empty output on exit code 0: return a zero-entry FileResult
        assert!(result.is_some(), "Empty-on-success should produce a result");
        let result = result.unwrap();
        assert_eq!(result.total_count, 0);
        assert_eq!(result.shown_count, 0);
    }

    #[test]
    fn test_tier3_empty_on_error_is_passthrough() {
        let output = make_output("", 1);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty output on error should fall to passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_full_on_small_input() {
        let input = load_fixture("find_small.txt");
        let output = make_output(&input, 0);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Small find output should be Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_display_format() {
        let input = "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n";
        let result = try_parse_lines(input, Some(0)).unwrap();
        let rendered = format!("{result}");
        assert!(
            rendered.contains("FIND: find |"),
            "Header should start with FIND:"
        );
        assert!(
            rendered.contains("./src/main.rs"),
            "Entries should appear in output"
        );
    }

    #[test]
    fn test_parse_impl_error_with_output_still_parses() {
        // find returns non-zero exit when some paths are inaccessible but still outputs results
        let input = "./src/main.rs\n./src/lib.rs\n";
        let output = make_output(input, 1);
        let result = parse_impl(&output);
        // Has output, so should parse (not passthrough)
        assert!(
            result.is_full(),
            "Non-empty output even on error should produce Full result, got {}",
            result.tier_name()
        );
    }
}
