//! Cargo audit parser (#105)
//!
//! Parses `cargo audit` output using three-tier degradation:
//! JSON (`--json` flag) -> regex on text blocks -> passthrough.
//!
//! NOTE: This is a DIFFERENT module from `cmd/build/cargo.rs` which handles
//! `cargo build` and `cargo clippy`. No collision: different parent module paths.

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

static RE_CRATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Crate:\s+(\S+)").unwrap());
static RE_TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Title:\s+(.+)").unwrap());
static RE_ADVISORY_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^ID:\s+(RUSTSEC-\S+)").unwrap());
static RE_NO_VULNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"No\s+vulnerabilities\s+found").unwrap());

// ============================================================================
// Public entry point
// ============================================================================

/// Run `skim pkg cargo <subcmd> [args...]`.
///
/// Currently only `audit` is supported.
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
        "audit" => run_audit(subcmd_args, show_stats, json_output),
        other => {
            eprintln!(
                "skim pkg cargo: unknown subcommand '{other}'\n\
                 Available: audit\n\
                 Run 'skim pkg cargo --help' for usage"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim pkg cargo <subcmd> [args...]");
    println!();
    println!("  Parse cargo package manager output for AI context windows.");
    println!();
    println!("Subcommands:");
    println!("  audit    Parse cargo audit output");
    println!();
    println!("Examples:");
    println!("  skim pkg cargo audit");
    println!("  cargo audit --json | skim pkg cargo audit");
}

// ============================================================================
// cargo audit
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
            program: "cargo",
            args: &cmd_args,
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install cargo-audit via: cargo install cargo-audit",
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

    let vulns = value.get("vulnerabilities")?;
    let found = vulns
        .get("found")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !found {
        return Some(PkgResult::new(
            "cargo".to_string(),
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

    let list = vulns
        .get("list")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or_default();

    let mut critical: usize = 0;
    let mut high: usize = 0;
    let mut moderate: usize = 0;
    let mut low: usize = 0;
    let mut details: Vec<String> = Vec::new();

    for vuln in list {
        let Some((detail, severity)) = extract_vuln_detail(vuln) else {
            continue;
        };
        match severity {
            "critical" => critical += 1,
            "high" => high += 1,
            "moderate" | "medium" => moderate += 1,
            "low" => low += 1,
            _ => {}
        }
        details.push(detail);
    }

    // Use details.len() instead of summing severity buckets so entries with
    // unknown/unrecognised severity are still counted.
    let total = details.len();

    Some(PkgResult::new(
        "cargo".to_string(),
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

/// Extract a single vulnerability entry from cargo audit JSON.
/// Returns `(detail_string, severity_str)` or `None` if the entry is malformed.
fn extract_vuln_detail(vuln: &serde_json::Value) -> Option<(String, &str)> {
    let advisory = vuln.get("advisory");
    let package = vuln.get("package");

    let severity = advisory
        .and_then(|a| a.get("severity"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let title = advisory
        .and_then(|a| a.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let id = advisory
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let pkg_name = package
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let pkg_version = package
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");

    let detail = format!("{id} {pkg_name}@{pkg_version}: {title} ({severity})");
    Some((detail, severity))
}

fn try_parse_audit_regex(text: &str) -> Option<PkgResult> {
    // Check for clean output first
    if RE_NO_VULNS.is_match(text) {
        return Some(PkgResult::new(
            "cargo".to_string(),
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

    // Parse advisory blocks: Crate:, Version:, Title:, ID:
    let crates: Vec<&str> = RE_CRATE
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let ids: Vec<&str> = RE_ADVISORY_ID
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let titles: Vec<&str> = RE_TITLE
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();

    let total = crates.len();
    if total == 0 {
        return None;
    }

    let mut details: Vec<String> = Vec::new();
    for i in 0..total {
        let crate_name = crates.get(i).unwrap_or(&"?");
        let id = ids.get(i).unwrap_or(&"?");
        let title = titles.get(i).unwrap_or(&"?");
        details.push(format!("{id} {crate_name}: {title}"));
    }

    // cargo audit text doesn't reliably include severity in text mode,
    // so count everything as moderate
    Some(PkgResult::new(
        "cargo".to_string(),
        PkgOperation::Audit {
            critical: 0,
            high: 0,
            moderate: total,
            low: 0,
            total,
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
    // cargo audit: JSON
    // ========================================================================

    #[test]
    fn test_audit_json_parse() {
        let input = load_fixture("cargo_audit.json");
        let result = try_parse_audit_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG AUDIT | cargo"));
        assert!(display.contains("critical: 1"));
        assert!(display.contains("high: 1"));
        assert!(display.contains("total: 2"));
        assert!(display.contains("RUSTSEC-2024-0001"));
        assert!(display.contains("buffer-utils"));
    }

    #[test]
    fn test_audit_json_clean() {
        let input = load_fixture("cargo_audit_clean.json");
        let result = try_parse_audit_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("total: 0"));
    }

    // ========================================================================
    // cargo audit: Regex
    // ========================================================================

    #[test]
    fn test_audit_regex_no_vulns() {
        let text = "No vulnerabilities found!\n250 dependencies checked";
        let result = try_parse_audit_regex(text);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("total: 0"));
    }

    #[test]
    fn test_audit_regex_with_blocks() {
        let text = "\
Crate:   buffer-utils
Version: 0.3.1
Title:   Buffer overflow in buffer-utils
ID:      RUSTSEC-2024-0001

Crate:   unsafe-lib
Version: 1.0.0
Title:   Memory safety issue
ID:      RUSTSEC-2024-0002
";
        let result = try_parse_audit_regex(text);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("total: 2"));
        assert!(display.contains("RUSTSEC-2024-0001"));
    }

    // ========================================================================
    // Three-tier integration
    // ========================================================================

    #[test]
    fn test_audit_json_produces_full() {
        let input = load_fixture("cargo_audit.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_audit(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_audit_text_produces_degraded() {
        let text =
            "Crate:   buffer-utils\nVersion: 0.3.1\nTitle:   overflow\nID:      RUSTSEC-2024-0001";
        let output = CommandOutput {
            stdout: text.to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_audit(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded, got {}",
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
        let result = parse_audit(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
