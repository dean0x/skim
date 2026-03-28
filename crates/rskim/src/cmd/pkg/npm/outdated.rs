use std::io::IsTerminal;
use std::process::ExitCode;

use crate::cmd::{run_parsed_command_with_mode, user_has_flag, ParsedCommandConfig};
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_output;

pub(super) fn run_outdated(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = vec!["outdated".to_string()];
    cmd_args.extend(args.iter().cloned());

    if json_output && !user_has_flag(&cmd_args, &["--json"]) {
        cmd_args.push("--json".to_string());
    }

    let use_stdin = !std::io::stdin().is_terminal() && args.is_empty();

    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: "npm",
            args: &cmd_args,
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Node.js from https://nodejs.org",
            use_stdin,
            show_stats,
            command_type: crate::analytics::CommandType::Pkg,
        },
        |output, _args| parse_outdated(output),
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
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
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
}
