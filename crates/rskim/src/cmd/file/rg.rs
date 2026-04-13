//! ripgrep (rg) parser with three-tier degradation (#116).
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON Lines parsing (inject `--json`)
//! - **Tier 2 (Degraded)**: Regex fallback (same as grep: file:line:content)
//! - **Tier 3 (Passthrough)**: Raw output

use std::collections::BTreeMap;

use crate::cmd::user_has_flag;
use crate::output::canonical::FileResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{
    build_file_result, run_file_tool, try_parse_file_line_content, FileToolConfig, MAX_FILES_SHOWN,
    MAX_INPUT_LINES, MAX_MATCHES_PER_FILE,
};

const CONFIG: FileToolConfig<'static> = FileToolConfig {
    program: "rg",
    env_overrides: &[],
    install_hint: "Install ripgrep: https://github.com/BurntSushi/ripgrep",
};

/// Run `skim file rg [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<std::process::ExitCode> {
    run_file_tool(
        CONFIG,
        args,
        show_stats,
        json_output,
        analytics_enabled,
        prepare_args,
        parse_impl,
    )
}

/// Inject `--json` unless user has conflicting flags.
fn prepare_args(cmd_args: &mut Vec<String>) {
    // Skip injection if user already has JSON or count/files-only modes
    let conflicting = &[
        "--json",
        "-c",
        "--count",
        "-l",
        "--files",
        "--files-with-matches",
    ];
    if user_has_flag(cmd_args, conflicting) {
        return;
    }
    cmd_args.push("--json".to_string());
}

/// Three-tier parse function for rg output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_regex(&output.stdout) {
        return ParseResult::Degraded(
            result,
            vec!["rg: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: JSON Lines
// ============================================================================

/// Extract `(file_path, formatted_match_line)` from a single rg JSON Lines match entry.
///
/// Returns `None` if the entry is not a valid `"match"` type object.
fn extract_match_fields(obj: &serde_json::Value) -> Option<(String, String)> {
    let msg_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if msg_type != "match" {
        return None;
    }
    let data = obj.get("data")?;
    let file_path = data
        .get("path")
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("<unknown>")
        .to_string();
    let lineno = data
        .get("line_number")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let text = data
        .get("lines")
        .and_then(|l| l.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Some((file_path, format!("  :{lineno}: {text}")))
}

/// Parse rg `--json` JSON Lines output.
///
/// rg emits one JSON object per line with a `type` field:
/// `"begin"`, `"match"`, `"end"`, `"summary"`.
fn try_parse_json(stdout: &str) -> Option<FileResult> {
    let mut file_matches: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total_matches = 0usize;
    let mut found_any = false;

    for line in stdout.lines().take(MAX_INPUT_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return None, // Not JSON Lines format
        };
        found_any = true;

        if let Some((file_path, formatted)) = extract_match_fields(&obj) {
            total_matches += 1;
            let file_entry = file_matches.entry(file_path).or_default();
            if file_entry.len() < MAX_MATCHES_PER_FILE {
                file_entry.push(formatted);
            }
        }
    }

    if !found_any {
        return None;
    }

    build_file_result(
        "rg",
        total_matches,
        file_matches,
        MAX_FILES_SHOWN,
        MAX_MATCHES_PER_FILE,
    )
}

// ============================================================================
// Tier 2: grep-compatible text format
// ============================================================================

/// Parse rg text output in `file:line:content` format.
///
/// Delegates to the shared `try_parse_file_line_content` in `file/mod.rs`.
/// `allow_stdin_fallback = false` because rg always includes file paths and
/// line numbers in its text output.
fn try_parse_regex(text: &str) -> Option<FileResult> {
    try_parse_file_line_content("rg", text, false)
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
    fn test_tier1_rg_json() {
        let input = load_fixture("rg_json.jsonl");
        let result = try_parse_json(&input);
        assert!(
            result.is_some(),
            "Expected Tier 1 JSON Lines parse to succeed"
        );
        let result = result.unwrap();
        assert!(result.total_count > 0, "Expected matches in JSON fixture");
    }

    #[test]
    fn test_tier2_rg_text() {
        let input = load_fixture("rg_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert!(result.total_count > 0, "Expected matches in text fixture");
    }

    #[test]
    fn test_parse_impl_json_is_full() {
        let input = load_fixture("rg_json.jsonl");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "rg JSON output should be Full tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_is_degraded() {
        let input = load_fixture("rg_text.txt");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "rg text output should be Degraded tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_empty_is_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty rg output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_prepare_args_injects_json() {
        let mut args: Vec<String> = vec!["fn main".to_string()];
        prepare_args(&mut args);
        assert!(
            args.contains(&"--json".to_string()),
            "Should inject --json when not present"
        );
    }

    #[test]
    fn test_prepare_args_no_inject_when_json_present() {
        let mut args: Vec<String> = vec!["fn main".to_string(), "--json".to_string()];
        prepare_args(&mut args);
        let count = args.iter().filter(|a| *a == "--json").count();
        assert_eq!(count, 1, "Should not double-inject --json");
    }

    #[test]
    fn test_prepare_args_no_inject_when_count_flag() {
        let mut args: Vec<String> = vec!["fn main".to_string(), "-c".to_string()];
        prepare_args(&mut args);
        assert!(
            !args.contains(&"--json".to_string()),
            "Should not inject --json when -c is present"
        );
    }

    #[test]
    fn test_max_matches_per_file_cap() {
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
            "Expected at most {MAX_MATCHES_PER_FILE} match lines, got {match_lines}"
        );
    }

    #[test]
    fn test_tier1_non_json_returns_none() {
        let result = try_parse_json("not json at all");
        assert!(
            result.is_none(),
            "Non-JSON input should return None from Tier 1"
        );
    }
}
