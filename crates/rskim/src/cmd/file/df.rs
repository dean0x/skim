//! df parser.
//!
//! Parses `df` output (disk filesystem usage) into structured `FileResult`.
//!
//! Tiers:
//! - **Tier 1 (Full)**: Header row + data rows
//! - **Tier 3 (Passthrough)**: Empty output or parse failure

use std::process::ExitCode;

use crate::output::canonical::FileResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{run_file_tool, FileToolConfig, MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};

const CONFIG: FileToolConfig<'static> = FileToolConfig {
    program: "df",
    env_overrides: &[],
    install_hint: "df is typically pre-installed on Unix systems",
};

/// Run `skim df [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_file_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Three-tier parse function for df output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    if let Some(result) = try_parse_df(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: df output parsing
// ============================================================================

fn try_parse_df(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES + 1);
    let mut filesystem_count = 0usize;
    let mut header_found = false;

    for (i, line) in stdout.lines().enumerate() {
        if i >= MAX_INPUT_LINES {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }

        if !header_found {
            // First non-empty line is the header
            entries.push(line.to_string());
            header_found = true;
            continue;
        }

        // Remaining lines are filesystem entries
        filesystem_count += 1;
        if entries.len() <= MAX_DISPLAY_ENTRIES {
            entries.push(line.to_string());
        }
    }

    if !header_found {
        return None;
    }

    // entries includes the header, so shown_count = entries.len() - 1 (data rows)
    let shown_count = entries.len().saturating_sub(1);
    let footer = if filesystem_count > MAX_DISPLAY_ENTRIES {
        Some(format!(
            "... and {} more",
            filesystem_count - MAX_DISPLAY_ENTRIES
        ))
    } else {
        None
    };

    Some(FileResult::new(
        "df".to_string(),
        filesystem_count,
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
    fn test_tier1_df_basic() {
        let input = load_fixture("df_basic.txt");
        let result = try_parse_df(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 5, "5 filesystems in df_basic.txt");
    }

    #[test]
    fn test_tier1_df_human() {
        let input = load_fixture("df_human.txt");
        let result = try_parse_df(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 5, "5 filesystems in df_human.txt");
    }

    #[test]
    fn test_tier1_df_preserves_header() {
        let input = load_fixture("df_basic.txt");
        let result = try_parse_df(&input).unwrap();
        // First entry should be the header line
        assert!(
            result.entries[0].contains("Filesystem"),
            "First entry should be header, got: {}",
            result.entries[0]
        );
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

    #[test]
    fn test_tier1_df_truncates_large() {
        // Build a df output with 150+ filesystem lines, exceeding MAX_DISPLAY_ENTRIES (100).
        let mut lines = String::new();
        lines.push_str("Filesystem     1K-blocks      Used Available Use% Mounted on\n");
        for i in 0..=MAX_DISPLAY_ENTRIES {
            lines.push_str(&format!(
                "/dev/sd{i}     103081248  45234120  52583660  47% /mnt/disk{i}\n"
            ));
        }
        let result = try_parse_df(&lines).unwrap();
        // More than MAX_DISPLAY_ENTRIES filesystem lines were fed in.
        assert!(
            result.total_count > MAX_DISPLAY_ENTRIES,
            "total_count should exceed MAX_DISPLAY_ENTRIES"
        );
        // shown_count is the number of data rows actually stored in entries (excluding header).
        assert_eq!(
            result.shown_count, MAX_DISPLAY_ENTRIES,
            "shown_count should be capped at MAX_DISPLAY_ENTRIES"
        );
        assert!(
            result.footer.is_some(),
            "A footer indicating truncation should be present"
        );
        let footer = result.footer.as_ref().unwrap();
        assert!(
            footer.contains("more"),
            "Footer should mention the omitted count, got: {footer}"
        );
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("df_basic.txt");
        let output = make_output(&input, 0);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "parse_impl with exit code 0 and valid df output should return Full, got {}",
            result.tier_name()
        );
    }
}
