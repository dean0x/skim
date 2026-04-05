//! pip package manager parser (#105)
//!
//! Parses `pip install`, `pip check`, and `pip list --outdated` output.
//! pip install and check are text-only (no JSON mode), so they start at
//! regex tier. pip list supports `--outdated --format=json`.

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

// ============================================================================
// Static regex patterns
// ============================================================================

static RE_PIP_INSTALLED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Successfully installed\s+(.+)").unwrap());
static RE_PIP_WARNING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^WARNING:").unwrap());
static RE_PIP_REQUIREMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\S+\s+\S+\s+has\s+requirement\s+").unwrap());

// ============================================================================
// Public entry point
// ============================================================================

/// Run `skim pkg pip <subcmd> [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Safe: args.is_empty() is handled above.
    let (subcmd, subcmd_args) = args.split_first().expect("already verified non-empty");

    match subcmd.as_str() {
        "install" => run_install(subcmd_args, show_stats, json_output),
        "check" => run_check(subcmd_args, show_stats, json_output),
        "list" => run_list(subcmd_args, show_stats, json_output),
        other => {
            let safe = crate::cmd::sanitize_for_display(other);
            eprintln!(
                "skim pkg pip: unknown subcommand '{safe}'\n\
                 Available: install, check, list\n\
                 Run 'skim pkg pip --help' for usage"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim pkg pip <subcmd> [args...]");
    println!();
    println!("  Parse pip output for AI context windows.");
    println!();
    println!("Subcommands:");
    println!("  install    Parse pip install output");
    println!("  check      Parse pip check output");
    println!("  list       Parse pip list --outdated output");
    println!();
    println!("Examples:");
    println!("  skim pkg pip install flask");
    println!("  skim pkg pip check");
    println!("  skim pkg pip list");
    println!("  pip install flask 2>&1 | skim pkg pip install");
}

// ============================================================================
// pip install (text-only, regex tier 1)
// ============================================================================

fn run_install(args: &[String], show_stats: bool, _json_output: bool) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "pip",
            subcommand: "install",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Python from https://python.org",
        },
        args,
        show_stats,
        |_cmd_args| {},
        parse_install,
    )
}

fn parse_install(output: &CommandOutput) -> ParseResult<PkgResult> {
    let combined = super::combine_output(output);

    // Tier 1: Regex (pip install has no JSON mode)
    if let Some(result) = try_parse_install_regex(&combined) {
        return ParseResult::Full(result);
    }

    // Tier 2: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_install_regex(text: &str) -> Option<PkgResult> {
    // Match "Successfully installed pkg1-1.0 pkg2-2.0"
    let added = if let Some(caps) = RE_PIP_INSTALLED.captures(text) {
        caps[1].split_whitespace().count()
    } else if text.contains("already satisfied") {
        0
    } else {
        return None;
    };

    let warnings = RE_PIP_WARNING.find_iter(text).count();

    Some(PkgResult::new(
        "pip".to_string(),
        PkgOperation::Install {
            added,
            removed: 0,
            changed: 0,
            warnings,
        },
        true,
        vec![],
    ))
}

// ============================================================================
// pip check (text-only, regex)
// ============================================================================

fn run_check(args: &[String], show_stats: bool, _json_output: bool) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "pip",
            subcommand: "check",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Python from https://python.org",
        },
        args,
        show_stats,
        |_cmd_args| {},
        parse_check,
    )
}

fn parse_check(output: &CommandOutput) -> ParseResult<PkgResult> {
    let combined = super::combine_output(output);

    // Tier 1: Regex
    if let Some(result) = try_parse_check_regex(&combined) {
        return ParseResult::Full(result);
    }

    // Tier 2: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_check_regex(text: &str) -> Option<PkgResult> {
    if text.contains("No broken requirements found") {
        return Some(PkgResult::new(
            "pip".to_string(),
            PkgOperation::Check { issues: 0 },
            true,
            vec![],
        ));
    }

    // Count lines matching the structured requirement pattern first,
    // then fall back to looser "has requirement" / "which is not installed" matching.
    let issues = RE_PIP_REQUIREMENT.find_iter(text).count();
    let issues = if issues > 0 {
        issues
    } else {
        let fallback = text
            .lines()
            .filter(|l| l.contains("has requirement") || l.contains("which is not installed"))
            .count();
        if fallback == 0 {
            return None;
        }
        fallback
    };

    let details: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();

    Some(PkgResult::new(
        "pip".to_string(),
        PkgOperation::Check { issues },
        false,
        details,
    ))
}

// ============================================================================
// pip list --outdated
// ============================================================================

fn run_list(args: &[String], show_stats: bool, json_output: bool) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "pip",
            subcommand: "list",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Python from https://python.org",
        },
        args,
        show_stats,
        |cmd_args| {
            if json_output {
                if !user_has_flag(cmd_args, &["--outdated"]) {
                    cmd_args.push("--outdated".to_string());
                }
                if !user_has_flag(cmd_args, &["--format"]) {
                    cmd_args.push("--format=json".to_string());
                }
            }
        },
        parse_list,
    )
}

fn parse_list(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: JSON
    if let Some(result) = try_parse_list_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex
    let combined = super::combine_output(output);
    if let Some(result) = try_parse_list_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_list_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let arr = value.as_array()?;

    let mut details: Vec<String> = Vec::new();

    for pkg in arr {
        let name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("?");
        let latest = pkg
            .get("latest_version")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        details.push(format!("{name} {version} -> {latest}"));
    }

    let count = details.len();
    Some(PkgResult::new(
        "pip".to_string(),
        PkgOperation::Outdated { count },
        true,
        details,
    ))
}

fn try_parse_list_regex(text: &str) -> Option<PkgResult> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 3 {
        return None;
    }

    // pip list --outdated text format:
    // Package    Version Latest Type
    // ---------- ------- ------ -----
    // flask      2.3.0   3.0.0  wheel
    let has_header = lines.first().is_some_and(|l| l.contains("Package"));
    let has_separator = lines.get(1).is_some_and(|l| l.starts_with("---"));

    if !has_header || !has_separator {
        return None;
    }

    let details: Vec<String> = lines
        .iter()
        .skip(2)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();

    Some(PkgResult::new(
        "pip".to_string(),
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
    // pip install: Regex
    // ========================================================================

    #[test]
    fn test_install_regex_parse() {
        let input = load_fixture("pip_install.txt");
        let result = try_parse_install_regex(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG INSTALL | pip"));
        assert!(display.contains("added: 3"));
    }

    #[test]
    fn test_install_already_satisfied() {
        let text =
            "Requirement already satisfied: flask in ./venv/lib/python3.11/site-packages (3.0.0)";
        let result = try_parse_install_regex(text);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("added: 0"));
    }

    // ========================================================================
    // pip check: Regex
    // ========================================================================

    #[test]
    fn test_check_clean() {
        let input = load_fixture("pip_check_clean.txt");
        let result = try_parse_check_regex(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG CHECK | pip | 0 issues"));
    }

    #[test]
    fn test_check_issues() {
        let input = load_fixture("pip_check_issues.txt");
        let result = try_parse_check_regex(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG CHECK | pip"));
        assert!(display.contains("2 issues"));
    }

    // ========================================================================
    // pip list --outdated: JSON
    // ========================================================================

    #[test]
    fn test_list_json_parse() {
        let input = load_fixture("pip_outdated.json");
        let result = try_parse_list_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG OUTDATED | pip | 2 packages"));
        assert!(display.contains("flask 2.3.0 -> 3.0.0"));
    }

    #[test]
    fn test_list_json_empty() {
        let result = try_parse_list_json("[]");
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("0 packages"));
    }

    // ========================================================================
    // Three-tier integration
    // ========================================================================

    #[test]
    fn test_install_produces_full() {
        let input = load_fixture("pip_install.txt");
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
    fn test_check_produces_full() {
        let input = load_fixture("pip_check_clean.txt");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_check(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_garbage_produces_passthrough() {
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
