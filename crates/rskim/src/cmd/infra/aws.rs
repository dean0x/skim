//! AWS CLI parser with three-tier degradation (#116).
//!
//! Executes `aws` and parses the output into structured `InfraResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON parsing (inject `--output json` for most commands)
//! - **Tier 2 (Degraded)**: Regex on text/table formatted output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, run_infra_tool, InfraToolConfig};

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "aws",
    env_overrides: &[],
    install_hint: "Install AWS CLI: https://aws.amazon.com/cli/",
};

/// Keys stripped from AWS JSON responses (metadata, not useful data).
const METADATA_KEYS: &[&str] = &["ResponseMetadata", "NextToken", "RequestId"];

/// Maximum number of items surfaced from arrays or tables (prevents runaway output).
const MAX_ITEMS: usize = 100;

/// Maximum byte length of JSON input accepted for Tier 1 parsing.
///
/// Inputs larger than this are skipped and fall through to the regex tier,
/// preventing unbounded allocation on pathological or adversarial responses.
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

static RE_AWS_TABLE_ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\|\s+(\S[^|]+\S)\s+\|").unwrap());

/// Run `skim infra aws [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_infra_tool(CONFIG, args, ctx, prepare_args, parse_impl)
}

/// Inject `--output json` if not already present and subcommand supports it.
fn prepare_args(cmd_args: &mut Vec<String>) {
    if user_has_flag(cmd_args, &["--output"]) {
        return;
    }

    // Skip flag injection for s3 subcommands — they have different output semantics
    if cmd_args.first().map(|s| s.as_str()) == Some("s3") {
        return;
    }

    cmd_args.push("--output".to_string());
    cmd_args.push("json".to_string());
}

/// Three-tier parse function for aws output.
fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["aws: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse AWS JSON output, stripping metadata keys and summarizing results.
fn try_parse_json(stdout: &str) -> Option<InfraResult> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || (!trimmed.starts_with('{') && !trimmed.starts_with('[')) {
        return None;
    }
    if trimmed.len() > MAX_JSON_BYTES {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;

    match &value {
        serde_json::Value::Array(arr) => {
            let count = arr.len();
            let items = extract_array_items(arr);
            let summary = format!("{count} item{}", if count == 1 { "" } else { "s" });
            Some(InfraResult::new(
                "aws".to_string(),
                "result".to_string(),
                summary,
                items,
            ))
        }
        serde_json::Value::Object(map) => parse_json_object(map),
        _ => None,
    }
}

/// Parse an AWS JSON object response, routing on the primary data key.
fn parse_json_object(map: &serde_json::Map<String, serde_json::Value>) -> Option<InfraResult> {
    // Find the primary data key (skip metadata keys)
    let data_key = map.keys().find(|k| !METADATA_KEYS.contains(&k.as_str()))?;

    let data = &map[data_key];
    let (count, items) = match data {
        serde_json::Value::Array(arr) => (arr.len(), extract_array_items(arr)),
        _ => {
            let summary_val = data
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| data.to_string());
            let items = vec![InfraItem {
                label: data_key.to_string(),
                value: summary_val,
            }];
            (1, items)
        }
    };

    let summary = format!("{count} item{}", if count == 1 { "" } else { "s" });
    Some(InfraResult::new(
        "aws".to_string(),
        data_key.to_string(),
        summary,
        items,
    ))
}

/// Extract display items from a JSON array, capped at MAX_ITEMS.
fn extract_array_items(arr: &[serde_json::Value]) -> Vec<InfraItem> {
    arr.iter()
        .enumerate()
        .take(MAX_ITEMS)
        .map(|(i, entry)| {
            // Try to find a meaningful identifier (Name, Id, Arn, etc.)
            let label = find_identifier(entry).unwrap_or_else(|| format!("item-{}", i + 1));
            let value = summarize_object(entry);
            InfraItem { label, value }
        })
        .collect()
}

/// Find a meaningful identifier in a JSON object.
fn find_identifier(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    for key in &["Name", "Id", "Arn", "InstanceId", "BucketName"] {
        if let Some(v) = obj.get(*key).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

/// Summarize a JSON object into a brief human-readable string.
fn summarize_object(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            // Take up to 3 key-value pairs for the summary
            map.iter()
                .filter(|(k, _)| !METADATA_KEYS.contains(&k.as_str()))
                .take(3)
                .map(|(k, v)| {
                    let v_str = v.as_str().map(|s| s.to_string()).unwrap_or_else(|| {
                        v.as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| v.to_string())
                    });
                    format!("{k}: {v_str}")
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ============================================================================
// Tier 2: table format fallback
// ============================================================================

/// Parse AWS table/text formatted output via regex.
fn try_parse_regex(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    'lines: for line in text.lines() {
        // AWS table format: | value1  | value2 |
        if line.trim_start().starts_with('|') {
            for caps in RE_AWS_TABLE_ROW.captures_iter(line) {
                if items.len() >= MAX_ITEMS {
                    break 'lines;
                }
                let cell = caps[1].trim().to_string();
                // Skip header-like rows and separators
                if cell.chars().all(|c| c == '-' || c == '+') {
                    continue;
                }
                if seen.insert(cell.clone()) && !cell.is_empty() {
                    items.push(InfraItem {
                        label: format!("item-{}", items.len() + 1),
                        value: cell,
                    });
                }
            }
        }
    }

    if items.is_empty() {
        return None;
    }

    let count = items.len();
    let summary = format!("{count} item{}", if count == 1 { "" } else { "s" });
    Some(InfraResult::new(
        "aws".to_string(),
        "result".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/infra");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    #[test]
    fn test_tier1_aws_s3_ls() {
        let input = load_fixture("aws_s3_ls.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("INFRA: aws"));
        assert!(!result.items.is_empty());
    }

    #[test]
    fn test_tier1_aws_ec2_describe() {
        let input = load_fixture("aws_ec2_describe.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
    }

    #[test]
    fn test_tier2_aws_regex() {
        let input = "| i-0abc123  | t3.micro  | running |\n| i-0def456  | t3.small  | stopped |";
        let result = try_parse_regex(input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("aws_s3_ls.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "An error occurred: Access Denied".to_string(),
            stderr: String::new(),
            exit_code: Some(255),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        // Tier 2 input: AWS table-formatted output (not JSON) that matches the
        // `| value |` pipe-delimited table regex.
        let output = CommandOutput {
            stdout:
                "| i-0abc123def  | t3.micro  | running |\n| i-0def456ghi  | t3.small  | stopped |\n"
                    .to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded parse result, got {}",
            result.tier_name()
        );
    }
}
