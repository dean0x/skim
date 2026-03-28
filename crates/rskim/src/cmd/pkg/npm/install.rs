use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::{run_parsed_command_with_mode, user_has_flag, ParsedCommandConfig};
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_output;

// ============================================================================
// Static regex patterns
// ============================================================================

static RE_NPM_ADDED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"added\s+(\d+)\s+packages?").unwrap());
static RE_NPM_REMOVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"removed\s+(\d+)\s+packages?").unwrap());
static RE_NPM_CHANGED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"changed\s+(\d+)\s+packages?").unwrap());
static RE_NPM_FOUND_VULNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"found\s+(\d+)\s+vulnerabilit").unwrap());

pub(super) fn run_install(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = vec!["install".to_string()];
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
        |output, _args| parse_install(output),
    )
}

fn parse_install(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: JSON
    if let Some(result) = try_parse_install_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex
    let combined = combine_output(output);
    if let Some(result) = try_parse_install_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_install_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;

    let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let removed = value.get("removed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let changed = value.get("changed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    // Count audit warnings from the embedded audit report
    let warnings = value
        .get("audit")
        .and_then(|a| a.get("vulnerabilities"))
        .and_then(|v| v.as_object())
        .map(|obj| obj.len())
        .unwrap_or(0);

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::Install {
            added,
            removed,
            changed,
            warnings,
        },
        true,
        vec![],
    ))
}

fn try_parse_install_regex(text: &str) -> Option<PkgResult> {
    let added = RE_NPM_ADDED
        .captures(text)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);
    let removed = RE_NPM_REMOVED
        .captures(text)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);
    let changed = RE_NPM_CHANGED
        .captures(text)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);

    // Only succeed if we found at least one count
    if added == 0 && removed == 0 && changed == 0 {
        // Check for "found 0 vulnerabilities" as a sign this is npm output
        if !RE_NPM_FOUND_VULNS.is_match(text) && !text.contains("up to date") {
            return None;
        }
    }

    let warnings = RE_NPM_FOUND_VULNS
        .captures(text)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::Install {
            added,
            removed,
            changed,
            warnings,
        },
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
    // npm install: JSON
    // ========================================================================

    #[test]
    fn test_install_json_parse() {
        let input = load_fixture("npm_install.json");
        let result = try_parse_install_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG INSTALL | npm"));
        assert!(display.contains("added: 127"));
        assert!(display.contains("removed: 3"));
        assert!(display.contains("changed: 14"));
    }

    // ========================================================================
    // npm install: Regex
    // ========================================================================

    #[test]
    fn test_install_regex_parse() {
        let input = load_fixture("npm_install_text.txt");
        let result = try_parse_install_regex(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("added: 127"));
        assert!(display.contains("removed: 3"));
        assert!(display.contains("changed: 14"));
    }

    // ========================================================================
    // Three-tier integration
    // ========================================================================

    #[test]
    fn test_install_json_produces_full() {
        let input = load_fixture("npm_install.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_install(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_install_text_produces_degraded() {
        let input = load_fixture("npm_install_text.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_install(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_install_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "completely unparseable output".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_install(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
