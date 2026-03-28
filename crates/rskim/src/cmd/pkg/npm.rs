//! npm package manager parser (#105)
//!
//! Parses `npm install`, `npm audit`, `npm outdated`, and `npm ls` output
//! using three-tier degradation: JSON -> regex -> passthrough.
//!
//! npm 7+ JSON schemas are supported. If JSON fails to parse, tier 2 regex
//! is attempted on plain text output.

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::{run_parsed_command_with_mode, user_has_flag, ParsedCommandConfig};
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

// ============================================================================
// Static regex patterns
// ============================================================================

static RE_NPM_ADDED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"added\s+(\d+)\s+packages?").unwrap());
static RE_NPM_REMOVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"removed\s+(\d+)\s+packages?").unwrap());
static RE_NPM_CHANGED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"changed\s+(\d+)\s+packages?").unwrap());
static RE_NPM_VULNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+vulnerabilit(?:y|ies)\s*\(([^)]+)\)").unwrap());
static RE_NPM_VULN_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+(critical|high|moderate|low|info)").unwrap());
static RE_NPM_FOUND_VULNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"found\s+(\d+)\s+vulnerabilit").unwrap());

// ============================================================================
// Public entry point
// ============================================================================

/// Run `skim pkg npm <subcmd> [args...]`.
///
/// Sub-dispatches to install, audit, outdated, or ls based on the first arg.
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
        "install" | "i" | "ci" => run_install(subcmd_args, show_stats, json_output),
        "audit" => run_audit(subcmd_args, show_stats, json_output),
        "outdated" => run_outdated(subcmd_args, show_stats, json_output),
        "ls" | "list" => run_ls(subcmd_args, show_stats, json_output),
        other => {
            eprintln!(
                "skim pkg npm: unknown subcommand '{other}'\n\
                 Available: install, audit, outdated, ls\n\
                 Run 'skim pkg npm --help' for usage"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim pkg npm <subcmd> [args...]");
    println!();
    println!("  Parse npm output for AI context windows.");
    println!();
    println!("Subcommands:");
    println!("  install    Parse npm install output");
    println!("  audit      Parse npm audit output");
    println!("  outdated   Parse npm outdated output");
    println!("  ls         Parse npm ls output");
    println!();
    println!("Examples:");
    println!("  skim pkg npm install");
    println!("  skim pkg npm audit");
    println!("  skim pkg npm outdated");
    println!("  skim pkg npm ls");
    println!("  npm install 2>&1 | skim pkg npm install");
}

// ============================================================================
// npm install
// ============================================================================

fn run_install(args: &[String], show_stats: bool, json_output: bool) -> anyhow::Result<ExitCode> {
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
    let combined = super::combine_output(output);
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
// npm audit
// ============================================================================

fn run_audit(args: &[String], show_stats: bool, json_output: bool) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = vec!["audit".to_string()];
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
        |output, _args| parse_audit(output),
    )
}

fn parse_audit(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: JSON
    if let Some(result) = try_parse_audit_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex
    let combined = super::combine_output(output);
    if let Some(result) = try_parse_audit_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_audit_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;

    // npm 7+ audit format
    let vulns = value.get("vulnerabilities")?.as_object()?;

    let mut critical: usize = 0;
    let mut high: usize = 0;
    let mut moderate: usize = 0;
    let mut low: usize = 0;
    let mut details: Vec<String> = Vec::new();

    for (name, vuln) in vulns {
        let severity = vuln
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        match severity {
            "critical" => critical += 1,
            "high" => high += 1,
            "moderate" => moderate += 1,
            "low" => low += 1,
            _ => {}
        }

        // Extract advisory title from via array. Entries can be either
        // objects (advisories with a `title` field) or plain strings
        // (transitive dependency names).
        let title = vuln
            .get("via")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find_map(|entry| {
                        // Object entry: { "title": "...", ... }
                        entry
                            .get("title")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .or_else(|| {
                        // String entry: transitive dep name (e.g. "lodash")
                        arr.first()
                            .and_then(|v| v.as_str())
                            .map(|s| format!("via {s}"))
                    })
            })
            .unwrap_or_else(|| "unknown".to_string());

        details.push(format!("{name}: {title} ({severity})"));
    }

    // Use details.len() instead of summing severity buckets so entries with
    // unknown/unrecognised severity are still counted.
    let total = details.len();

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::Audit {
            critical,
            high,
            moderate,
            low,
            total,
        },
        true,
        details,
    ))
}

fn try_parse_audit_regex(text: &str) -> Option<PkgResult> {
    // Match "N vulnerabilities (N critical, N high, N moderate, N low)"
    if let Some(caps) = RE_NPM_VULNS.captures(text) {
        let total = caps[1].parse::<usize>().unwrap_or(0);
        let breakdown = &caps[2];

        let mut critical: usize = 0;
        let mut high: usize = 0;
        let mut moderate: usize = 0;
        let mut low: usize = 0;

        for cap in RE_NPM_VULN_COUNT.captures_iter(breakdown) {
            let count = cap[1].parse::<usize>().unwrap_or(0);
            match &cap[2] {
                "critical" => critical = count,
                "high" => high = count,
                "moderate" => moderate = count,
                "low" => low = count,
                _ => {}
            }
        }

        return Some(PkgResult::new(
            "npm".to_string(),
            PkgOperation::Audit {
                critical,
                high,
                moderate,
                low,
                total,
            },
            true,
            vec![],
        ));
    }

    // Match "found 0 vulnerabilities"
    if text.contains("found 0 vulnerabilities") || text.contains("0 vulnerabilities") {
        return Some(PkgResult::new(
            "npm".to_string(),
            PkgOperation::Audit {
                critical: 0,
                high: 0,
                moderate: 0,
                low: 0,
                total: 0,
            },
            true,
            vec![],
        ));
    }

    None
}

// ============================================================================
// npm outdated
// ============================================================================

fn run_outdated(args: &[String], show_stats: bool, json_output: bool) -> anyhow::Result<ExitCode> {
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
    let combined = super::combine_output(output);
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

    // Count non-header, non-empty lines
    let count = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .count();

    let details: Vec<String> = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::Outdated { count },
        true,
        details,
    ))
}

// ============================================================================
// npm ls
// ============================================================================

fn run_ls(args: &[String], show_stats: bool, json_output: bool) -> anyhow::Result<ExitCode> {
    let mut cmd_args: Vec<String> = vec!["ls".to_string()];
    cmd_args.extend(args.iter().cloned());

    if json_output && !user_has_flag(&cmd_args, &["--json"]) {
        cmd_args.push("--json".to_string());
    }
    if json_output && !user_has_flag(&cmd_args, &["--depth"]) {
        cmd_args.push("--depth=0".to_string());
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
        |output, _args| parse_ls(output),
    )
}

fn parse_ls(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: JSON
    if let Some(result) = try_parse_ls_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex (count package lines)
    let combined = super::combine_output(output);
    if let Some(result) = try_parse_ls_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
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
    // npm audit: JSON
    // ========================================================================

    #[test]
    fn test_audit_json_parse() {
        let input = load_fixture("npm_audit.json");
        let result = try_parse_audit_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG AUDIT | npm"));
        assert!(display.contains("critical: 1"));
        assert!(display.contains("high: 1"));
        assert!(display.contains("moderate: 1"));
        assert!(display.contains("total: 3"));
        assert!(display.contains("lodash: Prototype Pollution (high)"));
    }

    #[test]
    fn test_audit_json_clean() {
        let input = load_fixture("npm_audit_clean.json");
        let result = try_parse_audit_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("total: 0"));
    }

    // ========================================================================
    // npm audit: Regex
    // ========================================================================

    #[test]
    fn test_audit_regex_vulns() {
        let text = "3 vulnerabilities (1 critical, 1 high, 1 moderate)";
        let result = try_parse_audit_regex(text);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("total: 3"));
        assert!(display.contains("critical: 1"));
    }

    #[test]
    fn test_audit_regex_clean() {
        let text = "found 0 vulnerabilities";
        let result = try_parse_audit_regex(text);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("total: 0"));
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
