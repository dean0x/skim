use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_output;

// ============================================================================
// Static regex patterns
// ============================================================================

static RE_NPM_VULNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+vulnerabilit(?:y|ies)\s*\(([^)]+)\)").unwrap());
static RE_NPM_VULN_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+(critical|high|moderate|low|info)").unwrap());

pub(super) fn run_audit(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "npm",
            subcommand: "audit",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Node.js from https://nodejs.org",
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
    let combined = combine_output(output);
    if let Some(result) = try_parse_audit_regex(&combined) {
        return ParseResult::Degraded(result, vec!["npm audit: JSON parse failed, using regex".to_string()]);
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
}
