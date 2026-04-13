//! pnpm package manager parser (#105)
//!
//! Parses `pnpm install`, `pnpm audit`, and `pnpm outdated` output.
//! pnpm install is text-only (no JSON mode for install output).
//! pnpm audit supports `--json`, pnpm outdated supports `--format json`.

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

static RE_PNPM_PACKAGES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Packages:\s*\+(\d+)(?:\s+-(\d+))?").unwrap());

// ============================================================================
// Public entry point
// ============================================================================

/// Run `skim pkg pnpm <subcmd> [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Safe: args.is_empty() is handled above.
    let (subcmd, subcmd_args) = args.split_first().expect("already verified non-empty");

    match subcmd.as_str() {
        "install" | "i" => run_install(subcmd_args, show_stats, json_output, analytics_enabled),
        "audit" => run_audit(subcmd_args, show_stats, json_output, analytics_enabled),
        "outdated" => run_outdated(subcmd_args, show_stats, json_output, analytics_enabled),
        other => {
            let safe = crate::cmd::sanitize_for_display(other);
            eprintln!(
                "skim pkg pnpm: unknown subcommand '{safe}'\n\
                 Available: install, audit, outdated\n\
                 Run 'skim pkg pnpm --help' for usage"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim pkg pnpm <subcmd> [args...]");
    println!();
    println!("  Parse pnpm output for AI context windows.");
    println!();
    println!("Subcommands:");
    println!("  install    Parse pnpm install output");
    println!("  audit      Parse pnpm audit output");
    println!("  outdated   Parse pnpm outdated output");
    println!();
    println!("Examples:");
    println!("  skim pkg pnpm install");
    println!("  skim pkg pnpm audit");
    println!("  skim pkg pnpm outdated");
    println!("  pnpm install 2>&1 | skim pkg pnpm install");
}

// ============================================================================
// pnpm install (text-only, regex tier)
// ============================================================================

fn run_install(
    args: &[String],
    show_stats: bool,
    _json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "pnpm",
            subcommand: "install",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install pnpm via https://pnpm.io/installation",
        },
        args,
        show_stats,
        analytics_enabled,
        |_cmd_args| {},
        parse_install,
    )
}

fn parse_install(output: &CommandOutput) -> ParseResult<PkgResult> {
    let combined = super::combine_output(output);

    // Tier 1: Regex (pnpm install has no JSON mode)
    if let Some(result) = try_parse_install_regex(&combined) {
        return ParseResult::Full(result);
    }

    // Tier 2: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_install_regex(text: &str) -> Option<PkgResult> {
    // Match "Packages: +127 -3"
    if let Some(caps) = RE_PNPM_PACKAGES.captures(text) {
        let added = caps[1].parse::<usize>().unwrap_or(0);
        let removed = caps
            .get(2)
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .unwrap_or(0);

        return Some(PkgResult::new(
            "pnpm".to_string(),
            PkgOperation::Install {
                added,
                removed,
                changed: 0,
                warnings: 0,
            },
            true,
            vec![],
        ));
    }

    // Fallback: "Already up to date" or "Done in"
    if text.contains("Already up to date") || text.contains("Nothing to do") {
        return Some(PkgResult::new(
            "pnpm".to_string(),
            PkgOperation::Install {
                added: 0,
                removed: 0,
                changed: 0,
                warnings: 0,
            },
            true,
            vec![],
        ));
    }

    None
}

// ============================================================================
// pnpm audit
// ============================================================================

fn run_audit(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "pnpm",
            subcommand: "audit",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install pnpm via https://pnpm.io/installation",
        },
        args,
        show_stats,
        analytics_enabled,
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

    // Tier 2: Passthrough (pnpm audit text is harder to regex reliably)
    let combined = super::combine_output(output);
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_audit_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;

    // pnpm audit JSON format: { advisories: {...}, metadata: { vulnerabilities: {...} } }
    let advisories = value.get("advisories")?.as_object()?;

    let mut critical: usize = 0;
    let mut high: usize = 0;
    let mut moderate: usize = 0;
    let mut low: usize = 0;
    let mut details: Vec<String> = Vec::new();

    // AD-18 (2026-04-11): advisory ID extraction mirrors `cargo audit` (see npm/audit.rs).
    // The pnpm audit JSON object key IS the advisory ID — it may be a GHSA string
    // (e.g. "GHSA-1234-abcd-xyz0") or a numeric string (e.g. "1234").
    // We use the key directly as the ID without transformation so that both forms
    // are preserved accurately. pnpm IDs sometimes numeric strings — preserve as-is.
    for (id, advisory) in advisories {
        let severity = advisory
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let module_name = advisory
            .get("module_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let title = advisory
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        match severity {
            "critical" => critical += 1,
            "high" => high += 1,
            "moderate" => moderate += 1,
            "low" => low += 1,
            _ => {}
        }

        details.push(format!("{id} {module_name}: {title} ({severity})"));
    }

    // Use details.len() instead of summing severity buckets so entries with
    // unknown/unrecognised severity are still counted (consistent with npm/cargo).
    let total = details.len();

    Some(PkgResult::new(
        "pnpm".to_string(),
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

// ============================================================================
// pnpm outdated
// ============================================================================

fn run_outdated(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "pnpm",
            subcommand: "outdated",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install pnpm via https://pnpm.io/installation",
        },
        args,
        show_stats,
        analytics_enabled,
        |cmd_args| {
            if json_output && !user_has_flag(cmd_args, &["--format"]) {
                cmd_args.push("--format".to_string());
                cmd_args.push("json".to_string());
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

    // Tier 2: Passthrough
    let combined = super::combine_output(output);
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_outdated_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let obj = value.as_object()?;

    if obj.is_empty() {
        return Some(PkgResult::new(
            "pnpm".to_string(),
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
        "pnpm".to_string(),
        PkgOperation::Outdated { count },
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
    // pnpm install: Regex
    // ========================================================================

    #[test]
    fn test_install_regex_parse() {
        let input = load_fixture("pnpm_install.txt");
        let result = try_parse_install_regex(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG INSTALL | pnpm"));
        assert!(display.contains("added: 127"));
        assert!(display.contains("removed: 3"));
    }

    // ========================================================================
    // pnpm audit: JSON
    // ========================================================================

    #[test]
    fn test_audit_json_parse() {
        let input = load_fixture("pnpm_audit.json");
        let result = try_parse_audit_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG AUDIT | pnpm"));
        assert!(display.contains("critical: 1"));
        assert!(display.contains("high: 1"));
        assert!(display.contains("total: 2"));
    }

    // ========================================================================
    // pnpm outdated: JSON
    // ========================================================================

    #[test]
    fn test_outdated_json_parse() {
        let input = load_fixture("pnpm_outdated.json");
        let result = try_parse_outdated_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("PKG OUTDATED | pnpm | 2 packages"));
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
    fn test_install_produces_full() {
        let input = load_fixture("pnpm_install.txt");
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

    // ========================================================================
    // Advisory ID extraction tests (AD-18, 2026-04-11)
    // ========================================================================

    #[test]
    fn test_pnpm_audit_uses_key_as_id() {
        let input = load_fixture("pnpm_audit_ghsa_key.json");
        let result = try_parse_audit_json(&input).expect("must parse");
        let display = format!("{result}");
        // The GHSA-1234-abcd-xyz0 key must appear in the detail string.
        assert!(
            display.contains("GHSA-1234-abcd-xyz0"),
            "must include the pnpm advisory key as the ID: {display}"
        );
    }
}
