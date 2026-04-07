use std::process::ExitCode;

use crate::cmd::user_has_flag;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_output;

pub(super) fn run_ls(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "npm",
            subcommand: "ls",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Node.js from https://nodejs.org",
        },
        args,
        show_stats,
        |cmd_args| {
            if json_output {
                if !user_has_flag(cmd_args, &["--json"]) {
                    cmd_args.push("--json".to_string());
                }
                if !user_has_flag(cmd_args, &["--depth"]) {
                    cmd_args.push("--depth=0".to_string());
                }
            }
        },
        parse_ls,
    )
}

fn parse_ls(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: JSON
    if let Some(result) = try_parse_ls_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex (count package lines)
    let combined = combine_output(output);
    if let Some(result) = try_parse_ls_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["npm ls: JSON parse failed, using regex".to_string()],
        );
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_ls_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let deps = value.get("dependencies")?.as_object()?;

    let total = deps.len();
    let mut flagged: usize = 0;
    let mut details: Vec<String> = Vec::new();

    for (name, dep) in deps {
        let version = dep.get("version").and_then(|v| v.as_str()).unwrap_or("?");

        if let Some(problems) = dep.get("problems").and_then(|v| v.as_array()) {
            if !problems.is_empty() {
                flagged += 1;
                for problem in problems {
                    if let Some(msg) = problem.as_str() {
                        details.push(format!("{name}@{version}: {msg}"));
                    }
                }
            }
        }
    }

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::List { total, flagged },
        true,
        details,
    ))
}

fn try_parse_ls_regex(text: &str) -> Option<PkgResult> {
    // npm ls text output is a tree: lines starting with non-empty package refs
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    // First line is project name, rest are dependencies
    let total = lines.len().saturating_sub(1);
    if total == 0 {
        return None;
    }

    // Count lines with "invalid" or "UNMET" markers
    let flagged = lines
        .iter()
        .filter(|l| l.contains("invalid") || l.contains("UNMET"))
        .count();

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::List { total, flagged },
        true,
        vec![],
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(name: &str) -> std::path::PathBuf {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/pkg");
        path.push(name);
        path
    }

    fn load_fixture(name: &str) -> String {
        std::fs::read_to_string(fixture_path(name))
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    // ========================================================================
    // npm ls: JSON
    // ========================================================================

    #[test]
    fn test_ls_json_parse() {
        let input = load_fixture("npm_ls.json");
        let result = try_parse_ls_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG LIST | npm"));
        assert!(display.contains("4 total"));
        assert!(display.contains("1 flagged"));
        assert!(display.contains("debug@4.3.4"));
    }

    // ========================================================================
    // Three-tier integration
    // ========================================================================

    #[test]
    fn test_ls_json_produces_full() {
        let input = load_fixture("npm_ls.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_ls(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_ls_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "completely unparseable output".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_ls(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
