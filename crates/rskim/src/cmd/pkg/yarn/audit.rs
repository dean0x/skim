use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::runner::CommandOutput;

use super::combine_output;

static RE_YARN_AUDIT_VULN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+vulnerabilit").expect("valid regex"));

pub(super) fn run_audit(
    args: &[String],
    show_stats: bool,
    _json_output: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "yarn",
            subcommand: "audit",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Yarn: npm install -g yarn",
        },
        args,
        show_stats,
        rec,
        |_cmd_args| {},
        parse_audit,
    )
}

fn parse_audit(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: NDJSON
    if let Some(result) = try_parse_ndjson(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex
    let combined = combine_output(output);
    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["yarn audit: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_ndjson(stdout: &str) -> Option<PkgResult> {
    let mut any_json = false;
    let mut total = 0usize;
    let mut critical = 0usize;
    let mut high = 0usize;
    let mut moderate = 0usize;
    let mut low = 0usize;
    // When auditSummary is found, its counts are authoritative. Advisory
    // increments after that point would double-count, so we suppress them.
    let mut found_summary = false;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        any_json = true;

        let type_str = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if type_str == "auditSummary" {
            if let Some(data) = v.get("data") {
                let vuln_count = data
                    .get("vulnerabilities")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                // Use summary counts if available, else fall back to advisory counts
                if vuln_count > 0 {
                    found_summary = true;
                    total = vuln_count;
                    critical = data.get("critical").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    high = data.get("high").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    moderate = data.get("moderate").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    low = data.get("low").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                }
            }
        } else if type_str == "auditAdvisory" && !found_summary {
            total += 1;
            match v
                .get("data")
                .and_then(|d| d.get("advisory"))
                .and_then(|a| a.get("severity"))
                .and_then(|s| s.as_str())
            {
                Some("critical") => critical += 1,
                Some("high") => high += 1,
                Some("moderate") => moderate += 1,
                Some("low") => low += 1,
                _ => {}
            }
        }
    }

    if !any_json {
        return None;
    }

    let success = total == 0;
    Some(PkgResult::new(
        "yarn".to_string(),
        PkgOperation::Audit {
            critical,
            high,
            moderate,
            low,
            total,
        },
        success,
        vec![],
    ))
}

fn try_parse_regex(text: &str) -> Option<PkgResult> {
    if !text.contains("vulnerabilit") && !text.contains("audit") {
        return None;
    }

    let total = RE_YARN_AUDIT_VULN
        .captures(text)
        .and_then(|c| c[1].parse::<usize>().ok())
        .unwrap_or(0);

    Some(PkgResult::new(
        "yarn".to_string(),
        PkgOperation::Audit {
            critical: 0,
            high: 0,
            moderate: 0,
            low: 0,
            total,
        },
        total == 0,
        vec![],
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const YARN_AUDIT_NDJSON: &str = r#"{"type":"auditAdvisory","data":{"resolution":{"id":1234,"path":"lodash>proto"},"advisory":{"module_name":"lodash","severity":"high","title":"Prototype Pollution"}}}
{"type":"auditSummary","data":{"vulnerabilities":2,"critical":0,"high":2,"moderate":0,"low":0,"info":0}}"#;

    const YARN_AUDIT_CLEAN: &str = r#"{"type":"auditSummary","data":{"vulnerabilities":0,"critical":0,"high":0,"moderate":0,"low":0,"info":0}}"#;

    #[test]
    fn test_yarn_audit_tier1_fail() {
        let result = try_parse_ndjson(YARN_AUDIT_NDJSON);
        assert!(result.is_some(), "Expected NDJSON parse to succeed");
        let r = result.unwrap();
        // vulnerabilities > 0 means not success
        assert!(!r.success);
    }

    #[test]
    fn test_yarn_audit_tier1_pass() {
        let result = try_parse_ndjson(YARN_AUDIT_CLEAN);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.success);
    }

    #[test]
    fn test_yarn_audit_tier3_passthrough() {
        let output = CommandOutput {
            stdout: "unrecognized garbage".to_string(),
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
