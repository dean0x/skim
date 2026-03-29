//! Cargo audit parser (#105)
//!
//! Parses `cargo audit` output using three-tier degradation:
//! JSON (`--json` flag) -> block-based text parsing -> passthrough.
//!
//! NOTE: This is a DIFFERENT module from `cmd/build/cargo.rs` which handles
//! `cargo build` and `cargo clippy`. No collision: different parent module paths.

use std::process::ExitCode;

use crate::cmd::user_has_flag;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

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
            let safe = super::sanitize_for_display(other);
            eprintln!(
                "skim pkg cargo: unknown subcommand '{safe}'\n\
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
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "cargo",
            subcommand: "audit",
            env_overrides: &[("CARGO_TERM_COLOR", "never")],
            install_hint: "Install cargo-audit via: cargo install cargo-audit",
        },
        args,
        show_stats,
        |cmd_args| {
            if json_output && !user_has_flag(cmd_args, &["--json"]) {
                cmd_args.push("--json".to_string());
            }
        },
        parse_audit,
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
        let (detail, severity) = extract_vuln_detail(vuln);
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
/// Returns `(detail_string, severity_str)`. Missing fields fall back to
/// `"unknown"` / `"?"` so every input produces a result.
fn extract_vuln_detail(vuln: &serde_json::Value) -> (String, &str) {
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
    (detail, severity)
}

fn try_parse_audit_regex(text: &str) -> Option<PkgResult> {
    // Check for clean output first
    if text.contains("No vulnerabilities found") {
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

    // Block-based parsing: split on blank lines to get individual advisory
    // blocks, then extract fields from each block. This keeps fields
    // associated with their block, avoiding the misalignment bug of the
    // old triple-regex-zip approach (where missing fields in one block
    // would shift IDs/titles from later blocks into earlier ones).
    let blocks: Vec<&str> = text
        .split("\n\n")
        .filter(|b| b.contains("Crate:"))
        .collect();
    if blocks.is_empty() {
        return None;
    }

    let mut details: Vec<String> = Vec::new();
    for block in &blocks {
        let crate_name = extract_field(block, "Crate:").unwrap_or("?");
        let id = extract_field(block, "ID:").unwrap_or("?");
        let title = extract_field(block, "Title:").unwrap_or("?");
        details.push(format!("{id} {crate_name}: {title}"));
    }

    let total = details.len();

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

/// Extract a field value from a text block by line prefix.
fn extract_field<'a>(block: &'a str, prefix: &str) -> Option<&'a str> {
    block
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix).map(|v| v.trim()))
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

    #[test]
    fn test_audit_regex_missing_id_field() {
        // First block is MISSING its ID, second block has one.
        // Triple-regex misaligns: the ID from block 2 gets assigned to block 1
        // because regex matches are zipped by index, not by block.
        let text = "\
Crate:   first-crate
Version: 0.1.0
Title:   Some vulnerability

Crate:   second-crate
Version: 0.2.0
Title:   Another vulnerability
ID:      RUSTSEC-2024-0099
";
        let result = try_parse_audit_regex(text);
        assert!(
            result.is_some(),
            "Should still parse blocks with missing fields"
        );
        let result = result.unwrap();
        let display = format!("{result}");
        // Should have 2 vulnerabilities
        assert!(
            display.contains("total: 2"),
            "Expected 2 vulns, got: {display}"
        );
        // The ID must be associated with second-crate, NOT first-crate.
        // Triple-regex would misalign: RUSTSEC-2024-0099 would appear next to first-crate.
        assert!(
            display.contains("RUSTSEC-2024-0099 second-crate"),
            "ID should be on second-crate, not first-crate. Got: {display}"
        );
    }

    #[test]
    fn test_audit_regex_reordered_fields() {
        // Fields appear in non-standard order (ID before Crate)
        let text = "\
ID:      RUSTSEC-2024-0001
Crate:   buffer-utils
Title:   Buffer overflow
Version: 0.3.1
";
        let result = try_parse_audit_regex(text);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("total: 1"));
        assert!(display.contains("RUSTSEC-2024-0001"));
        assert!(display.contains("buffer-utils"));
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
