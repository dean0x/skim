//! curl parser with three-tier degradation (#116).
//!
//! Executes `curl` and parses the output into structured `InfraResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Detect JSON body in output and parse it
//! - **Tier 2 (Degraded)**: Strip verbose lines, extract HTTP status
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::sync::LazyLock;

use regex::Regex;

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, run_infra_tool, InfraToolConfig};

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "curl",
    env_overrides: &[],
    install_hint: "Install curl: https://curl.se/",
};

static RE_HTTP_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^< HTTP/[\d.]+ (\d{3})\s*(.*)").unwrap());

static RE_VERBOSE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[*><{}\s]").unwrap());

/// Run `skim infra curl [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection for curl — flags are too varied
    run_infra_tool(CONFIG, args, show_stats, json_output, |_| {}, parse_impl)
}

/// Three-tier parse function for curl output.
fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    // Check stderr for HTTP status (curl -v outputs headers to stderr)
    let http_status = extract_http_status(&output.stderr);

    if let Some(result) = try_parse_json_body(&output.stdout, http_status.as_deref()) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_verbose(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON body detection
// ============================================================================

/// Parse JSON body from curl response and summarize.
fn try_parse_json_body(
    stdout: &str,
    http_status: Option<&str>,
) -> Option<InfraResult> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }

    let json_val: serde_json::Value = serde_json::from_str(trimmed).ok()?;

    let mut items: Vec<InfraItem> = Vec::new();

    // Add HTTP status if available
    if let Some(status) = http_status {
        items.push(InfraItem {
            label: "status".to_string(),
            value: status.to_string(),
        });
    }

    let summary_str = match &json_val {
        serde_json::Value::Array(arr) => {
            let count = arr.len();
            items.push(InfraItem {
                label: "count".to_string(),
                value: count.to_string(),
            });
            format!("array with {count} element{}", if count == 1 { "" } else { "s" })
        }
        serde_json::Value::Object(map) => {
            let count = map.len();
            // Add top-level fields as items (up to 5)
            for (k, v) in map.iter().take(5) {
                let v_str = v.as_str().map(|s| s.to_string()).unwrap_or_else(|| {
                    v.as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| v.to_string())
                });
                items.push(InfraItem {
                    label: k.clone(),
                    value: v_str,
                });
            }
            format!("object with {count} key{}", if count == 1 { "" } else { "s" })
        }
        _ => return None,
    };

    Some(InfraResult::new(
        "curl".to_string(),
        "response".to_string(),
        summary_str,
        items,
    ))
}

// ============================================================================
// Tier 2: verbose output fallback
// ============================================================================

/// Extract HTTP status line from curl verbose stderr output.
fn extract_http_status(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        if let Some(caps) = RE_HTTP_STATUS.captures(line) {
            let code = &caps[1];
            let reason = caps[2].trim();
            return Some(if reason.is_empty() {
                code.to_string()
            } else {
                format!("{code} {reason}")
            });
        }
    }
    None
}

/// Parse curl verbose output by extracting non-verbose lines.
fn try_parse_verbose(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();

    // Extract HTTP status
    for line in text.lines() {
        if let Some(caps) = RE_HTTP_STATUS.captures(line) {
            items.push(InfraItem {
                label: "status".to_string(),
                value: format!("{} {}", &caps[1], caps[2].trim()),
            });
        }
    }

    // Extract body lines (non-verbose, non-header lines)
    let body_lines: Vec<&str> = text
        .lines()
        .filter(|line| !RE_VERBOSE_LINE.is_match(line) && !line.is_empty())
        .take(3)
        .collect();

    if !body_lines.is_empty() {
        items.push(InfraItem {
            label: "body_preview".to_string(),
            value: body_lines.join(" ").chars().take(200).collect(),
        });
    }

    if items.is_empty() {
        return None;
    }

    let summary = items
        .iter()
        .find(|i| i.label == "status")
        .map(|i| i.value.clone())
        .unwrap_or_else(|| "response received".to_string());

    Some(InfraResult::new(
        "curl".to_string(),
        "response".to_string(),
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
    fn test_tier1_curl_json_response() {
        let input = load_fixture("curl_json_response.txt");
        let result = try_parse_json_body(&input, Some("200 OK"));
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("INFRA: curl response"));
    }

    #[test]
    fn test_tier1_curl_non_json_fails() {
        let result = try_parse_json_body("not json at all", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_tier2_curl_regex() {
        let input = load_fixture("curl_verbose.txt");
        let result = try_parse_verbose(&input);
        assert!(result.is_some(), "Expected Tier 2 parse to succeed");
        let result = result.unwrap();
        assert!(result.items.iter().any(|i| i.label == "status"));
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("curl_json_response.txt");
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
        // Output with no JSON and no HTTP status lines falls through to passthrough
        // (empty stdout, empty stderr, non-zero exit)
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(7),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
