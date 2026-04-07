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
use serde_json::Value;

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, run_infra_tool, InfraToolConfig};

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "curl",
    env_overrides: &[],
    install_hint: "Install curl: https://curl.se/",
};

/// Maximum number of source fields in a JSON response before a truncation notice is added.
const MAX_ITEMS: usize = 100;

/// Maximum byte length of JSON input accepted for Tier 1 parsing.
///
/// Inputs larger than this are skipped and fall through to the regex tier,
/// preventing unbounded allocation on pathological or adversarial responses.
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

static RE_CURL_HTTP_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^< HTTP/[\d.]+ (\d{3})\s*(.*)").unwrap());

/// Matches lines that are curl verbose metadata (not response body).
/// Uses literal space instead of \s so indented body content is preserved.
static RE_CURL_VERBOSE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[*><{} ]").unwrap());

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

    if let Some(result) = try_parse_json(&output.stdout, http_status.as_deref()) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["curl: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON body detection
// ============================================================================

/// Summarize a JSON object: collect top-level key-value items (up to 5).
/// Returns a human-readable summary string and the collected items.
fn summarize_json_object(map: &serde_json::Map<String, Value>) -> (String, Vec<InfraItem>) {
    let count = map.len();
    let items: Vec<InfraItem> = map
        .iter()
        .take(5)
        .map(|(k, v)| InfraItem {
            label: k.clone(),
            value: json_value_to_string(v),
        })
        .collect();
    let summary = format!(
        "object with {count} key{}",
        if count == 1 { "" } else { "s" }
    );
    (summary, items)
}

/// Summarize a JSON array: record element count.
fn summarize_json_array(arr: &[Value]) -> (String, Vec<InfraItem>) {
    let count = arr.len();
    let items = vec![InfraItem {
        label: "count".to_string(),
        value: count.to_string(),
    }];
    let summary = format!(
        "array with {count} element{}",
        if count == 1 { "" } else { "s" }
    );
    (summary, items)
}

/// Convert a JSON value to a compact display string.
fn json_value_to_string(val: &Value) -> String {
    val.as_str()
        .map(|s| s.to_string())
        .or_else(|| val.as_u64().map(|n| n.to_string()))
        .unwrap_or_else(|| val.to_string())
}

/// Parse JSON body from curl response and summarize (slim dispatcher).
fn try_parse_json(stdout: &str, http_status: Option<&str>) -> Option<InfraResult> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_JSON_BYTES {
        return None;
    }

    let json_val: serde_json::Value = serde_json::from_str(trimmed).ok()?;

    let mut items: Vec<InfraItem> = Vec::new();
    if let Some(status) = http_status {
        items.push(InfraItem {
            label: "status".to_string(),
            value: status.to_string(),
        });
    }

    let summary_str = match &json_val {
        Value::Array(arr) => {
            let (summary, extra) = summarize_json_array(arr);
            items.extend(extra);
            // Truncation notice when source array exceeds cap
            if arr.len() > MAX_ITEMS {
                items.push(InfraItem {
                    label: "truncated".to_string(),
                    value: format!("output capped at {MAX_ITEMS} items"),
                });
            }
            summary
        }
        Value::Object(map) => {
            let (summary, extra) = summarize_json_object(map);
            items.extend(extra);
            // Truncation notice when source object exceeds cap
            if map.len() > MAX_ITEMS {
                items.push(InfraItem {
                    label: "truncated".to_string(),
                    value: format!("output capped at {MAX_ITEMS} items"),
                });
            }
            summary
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
        if let Some(caps) = RE_CURL_HTTP_STATUS.captures(line) {
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
fn try_parse_regex(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();

    // Extract HTTP status
    for line in text.lines() {
        if let Some(caps) = RE_CURL_HTTP_STATUS.captures(line) {
            items.push(InfraItem {
                label: "status".to_string(),
                value: format!("{} {}", &caps[1], caps[2].trim()),
            });
        }
    }

    // Extract body lines (non-verbose, non-header lines)
    let body_lines: Vec<&str> = text
        .lines()
        .filter(|line| !RE_CURL_VERBOSE_LINE.is_match(line) && !line.is_empty())
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
        let result = try_parse_json(&input, Some("200 OK"));
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("INFRA: curl response"));
    }

    #[test]
    fn test_tier1_curl_non_json_fails() {
        let result = try_parse_json("not json at all", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_tier2_curl_regex() {
        let input = load_fixture("curl_verbose.txt");
        let result = try_parse_regex(&input);
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

    #[test]
    fn test_tier2_preserves_indented_body_content() {
        // Regression: RE_CURL_VERBOSE_LINE must not drop tab-indented lines.
        // \s was incorrectly filtering indented body content in Tier 2.
        let text = "< HTTP/1.1 200 OK\n<\n\t<html>\n\t<body>hello</body>\n";
        let result = try_parse_regex(text);
        let result = result.unwrap();
        let body = result.items.iter().find(|i| i.label == "body_preview");
        assert!(
            body.is_some(),
            "Expected indented body lines to be captured"
        );
        let val = &body.unwrap().value;
        assert!(
            val.contains("<html>") || val.contains("<body>"),
            "Indented body content should be preserved, got: {val}"
        );
    }

    #[test]
    fn test_tier1_max_items_cap() {
        // Build a JSON object with more than MAX_ITEMS fields to verify truncation.
        let fields: String = (0..200)
            .map(|i| format!("\"key{i}\": \"val{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let json = format!("{{{fields}}}");
        let result = try_parse_json(&json, None);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(
            result.items.iter().any(|i| i.label == "truncated"),
            "Expected truncation notice item when JSON has > {MAX_ITEMS} fields"
        );
    }

    #[test]
    fn test_summarize_json_object() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"a":1,"b":"two","c":true}"#).unwrap();
        let map = json.as_object().unwrap();
        let (summary, items) = summarize_json_object(map);
        assert!(summary.contains("3 key"), "Got: {summary}");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn test_summarize_json_array() {
        let json: serde_json::Value = serde_json::from_str(r#"[1,2,3]"#).unwrap();
        let arr = json.as_array().unwrap();
        let (summary, items) = summarize_json_array(arr);
        assert!(summary.contains("3 element"), "Got: {summary}");
        assert_eq!(items[0].value, "3");
    }

    #[test]
    fn test_json_value_to_string() {
        assert_eq!(
            json_value_to_string(&Value::String("hello".into())),
            "hello"
        );
        assert_eq!(json_value_to_string(&Value::Number(42u64.into())), "42");
        assert_eq!(json_value_to_string(&Value::Bool(true)), "true");
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        // Tier 2 input: curl verbose output with HTTP status in stderr and no JSON
        // in stdout. The `< HTTP/...` line triggers try_parse_verbose (Tier 2) but
        // try_parse_json_body returns None because stdout is not valid JSON.
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "* Connected to api.example.com\n> GET / HTTP/1.1\n< HTTP/1.1 200 OK\n<\n"
                .to_string(),
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
