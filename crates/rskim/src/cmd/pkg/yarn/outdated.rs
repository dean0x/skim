use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::runner::CommandOutput;

use super::combine_output;

static RE_YARN_OUTDATED_PKG: LazyLock<Regex> = LazyLock::new(|| {
    // yarn outdated text: package name followed by current, wanted, latest
    Regex::new(r"^(\S+)\s+\S+\s+\S+\s+\S+").expect("valid regex")
});

pub(super) fn run_outdated(
    args: &[String],
    show_stats: bool,
    _json_output: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "yarn",
            subcommand: "outdated",
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Yarn: npm install -g yarn",
        },
        args,
        show_stats,
        rec,
        |_cmd_args| {},
        parse_outdated,
    )
}

fn parse_outdated(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: NDJSON
    if let Some(result) = try_parse_ndjson(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex
    let combined = combine_output(output);
    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["yarn outdated: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_ndjson(stdout: &str) -> Option<PkgResult> {
    let mut any_json = false;
    let mut outdated_count = 0usize;
    let mut outdated_packages: Vec<String> = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        any_json = true;

        if v.get("type").and_then(|t| t.as_str()) == Some("table")
            && let Some(body) = v
                .get("data")
                .and_then(|d| d.get("body"))
                .and_then(|b| b.as_array())
        {
            for row in body {
                if let Some(arr) = row.as_array()
                    && let Some(name) = arr.first().and_then(|n| n.as_str())
                {
                    outdated_count += 1;
                    outdated_packages.push(name.to_string());
                }
            }
        }
    }

    if !any_json {
        return None;
    }

    let messages: Vec<String> = outdated_packages
        .iter()
        .map(|p| format!("outdated: {p}"))
        .collect();

    Some(PkgResult::new(
        "yarn".to_string(),
        PkgOperation::Outdated {
            count: outdated_count,
        },
        true,
        messages,
    ))
}

fn try_parse_regex(text: &str) -> Option<PkgResult> {
    // Detect yarn outdated output
    if !text.contains("Package") && !text.contains("Current") && !text.contains("Latest") {
        return None;
    }

    let count = text
        .lines()
        .filter(|l| RE_YARN_OUTDATED_PKG.is_match(l) && !l.trim_start().starts_with("Package"))
        .count();

    if count == 0 && !text.contains("Done in") {
        return None;
    }

    Some(PkgResult::new(
        "yarn".to_string(),
        PkgOperation::Outdated { count },
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

    const YARN_OUTDATED_NDJSON: &str = r#"{"type":"table","data":{"head":["Package","Current","Wanted","Latest","Package Type","URL"],"body":[["lodash","4.17.19","4.17.21","4.17.21","dependencies",""],["react","17.0.2","17.0.2","18.2.0","dependencies",""]]}}"#;

    #[test]
    fn test_yarn_outdated_tier1_ndjson() {
        let result = try_parse_ndjson(YARN_OUTDATED_NDJSON);
        assert!(result.is_some(), "Expected NDJSON parse to succeed");
        let r = result.unwrap();
        let s = format!("{r}");
        assert!(
            s.contains("yarn outdated") || s.contains("2"),
            "Display: {s}"
        );
    }

    #[test]
    fn test_yarn_outdated_tier3_passthrough() {
        let output = CommandOutput {
            stdout: "unrecognized random text".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
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
