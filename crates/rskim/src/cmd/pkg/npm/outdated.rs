use std::process::ExitCode;

use crate::cmd::user_has_flag;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_output;

pub(super) fn run_outdated(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "npm",
            subcommand: "outdated",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Node.js from https://nodejs.org",
        },
        args,
        show_stats,
        analytics_enabled,
        |cmd_args| {
            if json_output && !user_has_flag(cmd_args, &["--json"]) {
                cmd_args.push("--json".to_string());
            }
        },
        parse_outdated,
    )
}

fn parse_outdated(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: JSON
    if let Some(result) = try_parse_outdated_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex (count non-header table lines)
    let combined = combine_output(output);
    if let Some(result) = try_parse_outdated_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["npm outdated: JSON parse failed, using regex".to_string()],
        );
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_outdated_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let obj = value.as_object()?;

    // Empty object = nothing outdated
    if obj.is_empty() {
        return Some(PkgResult::new(
            "npm".to_string(),
            PkgOperation::Outdated { count: 0 },
            true,
            vec![],
        ));
    }

    let mut details: Vec<String> = Vec::new();

    for (name, pkg) in obj {
        let current = pkg.get("current").and_then(|v| v.as_str()).unwrap_or("?");
        let latest = pkg.get("latest").and_then(|v| v.as_str()).unwrap_or("?");
        details.push(format!("{name} {current} -> {latest}"));
    }

    let count = details.len();
    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::Outdated { count },
        true,
        details,
    ))
}

fn try_parse_outdated_regex(text: &str) -> Option<PkgResult> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // npm outdated text output has a header line: "Package  Current  Wanted  Latest  Location"
    let has_header = lines
        .first()
        .is_some_and(|l| l.contains("Package") && l.contains("Current"));

    if !has_header {
        return None;
    }

    let details: Vec<String> = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::Outdated {
            count: details.len(),
        },
        true,
        details,
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
    // npm outdated: JSON
    // ========================================================================

    #[test]
    fn test_outdated_json_parse() {
        let input = load_fixture("npm_outdated.json");
        let result = try_parse_outdated_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG OUTDATED | npm | 3 packages"));
        assert!(display.contains("lodash 4.17.20 -> 4.17.21"));
    }

    #[test]
    fn test_outdated_json_empty() {
        let result = try_parse_outdated_json("{}");
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("0 packages"));
    }

    // ========================================================================
    // Three-tier integration
    // ========================================================================

    #[test]
    fn test_outdated_json_produces_full() {
        let input = load_fixture("npm_outdated.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_outdated(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_outdated_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "completely unparseable output".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_outdated(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
