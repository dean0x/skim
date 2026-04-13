//! wget parser with three-tier degradation (#116).
//!
//! Executes `wget` and parses the output into structured `InfraResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Regex for HTTP status, filename, and size from wget output
//! - **Tier 2 (Degraded)**: Simpler regex fallback
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! Note: wget outputs to stderr by default, so `combine_stdout_stderr` is used.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, run_infra_tool, InfraToolConfig};

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "wget",
    env_overrides: &[],
    install_hint: "Install wget via your package manager",
};

static RE_WGET_HTTP_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"HTTP request sent.*?(\d{3})\s+(.+)").unwrap());

static RE_WGET_SAVING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Saving to:\s+'([^']+)'").unwrap());

static RE_WGET_LENGTH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Length:\s+\d+\s+\(([^)]+)\)").unwrap());

static RE_WGET_SAVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'(.+)' saved \[(\d+)/(\d+)\]").unwrap());

static RE_WGET_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ERROR\s+(\d+):\s+(.+)").unwrap());

static RE_WGET_ANY_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:HTTP|response|status)[^\n]*?\b([1-5]\d{2})\b").unwrap());

/// Run `skim infra wget [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    analytics_enabled: bool,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection for wget — flags are too varied
    run_infra_tool(CONFIG, args, show_stats, json_output, analytics_enabled, |_| {}, parse_impl)
}

/// Three-tier parse function for wget output.
fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    // wget outputs to stderr, so use combined output
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_structured(&combined) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["wget: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: Full regex parsing
// ============================================================================

/// Parse wget output extracting HTTP status, filename, and size.
fn try_parse_structured(text: &str) -> Option<InfraResult> {
    if !text.contains("HTTP request") && !text.contains("ERROR") {
        return None;
    }

    let mut items: Vec<InfraItem> = Vec::new();

    append_http_status(text, &mut items);

    match try_build_error_result(text, items) {
        Ok(result) => return Some(result),
        Err(returned_items) => items = returned_items,
    }

    append_size_and_file(text, &mut items);

    if items.is_empty() {
        return None;
    }

    let summary = items
        .iter()
        .find(|i| i.label == "status")
        .map(|i| i.value.clone())
        .unwrap_or_else(|| "download complete".to_string());

    Some(InfraResult::new(
        "wget".to_string(),
        "download".to_string(),
        summary,
        items,
    ))
}

/// Append an HTTP status item if a status line is present.
fn append_http_status(text: &str, items: &mut Vec<InfraItem>) {
    if let Some(caps) = RE_WGET_HTTP_STATUS.captures(text) {
        items.push(InfraItem {
            label: "status".to_string(),
            value: format!("{} {}", &caps[1], caps[2].trim()),
        });
    }
}

/// If an ERROR line is present, append the error item and return the finished result.
/// Takes ownership of `items`; on success returns `Ok(result)`, on no-match returns
/// `Err(items)` so the caller can continue using the vector.
fn try_build_error_result(
    text: &str,
    mut items: Vec<InfraItem>,
) -> Result<InfraResult, Vec<InfraItem>> {
    let Some(caps) = RE_WGET_ERROR.captures(text) else {
        return Err(items);
    };
    items.push(InfraItem {
        label: "error".to_string(),
        value: format!("{} {}", &caps[1], caps[2].trim()),
    });
    let summary = format!("ERROR {}", &caps[1]);
    Ok(InfraResult::new(
        "wget".to_string(),
        "download".to_string(),
        summary,
        items,
    ))
}

/// Append file size and saved-filename items.
fn append_size_and_file(text: &str, items: &mut Vec<InfraItem>) {
    if let Some(caps) = RE_WGET_LENGTH.captures(text) {
        items.push(InfraItem {
            label: "size".to_string(),
            value: caps[1].to_string(),
        });
    }

    if let Some(caps) = RE_WGET_SAVING.captures(text) {
        items.push(InfraItem {
            label: "file".to_string(),
            value: caps[1].to_string(),
        });
    } else if let Some(caps) = RE_WGET_SAVED.captures(text) {
        items.push(InfraItem {
            label: "file".to_string(),
            value: caps[1].to_string(),
        });
    }
}

// ============================================================================
// Tier 2: Simple fallback
// ============================================================================

/// Simpler fallback: look for any HTTP status code in output.
fn try_parse_regex(text: &str) -> Option<InfraResult> {
    for line in text.lines() {
        if let Some(caps) = RE_WGET_ANY_STATUS.captures(line) {
            let code = &caps[1];
            let items = vec![InfraItem {
                label: "status".to_string(),
                value: code.to_string(),
            }];
            return Some(InfraResult::new(
                "wget".to_string(),
                "download".to_string(),
                format!("HTTP {code}"),
                items,
            ));
        }
    }

    None
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
    fn test_tier1_wget_download() {
        let input = load_fixture("wget_download.txt");
        let result = try_parse_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("INFRA: wget download"));
        assert!(result.items.iter().any(|i| i.label == "status"));
    }

    #[test]
    fn test_tier1_wget_error() {
        let input = load_fixture("wget_error.txt");
        let result = try_parse_structured(&input);
        assert!(
            result.is_some(),
            "Expected Tier 1 parse to succeed for error"
        );
        let result = result.unwrap();
        assert!(result.items.iter().any(|i| i.label == "error"));
    }

    #[test]
    fn test_tier2_wget_regex() {
        // Tier 2 requires HTTP context before the status code
        let input = "HTTP response 200\n";
        let result = try_parse_regex(input);
        assert!(result.is_some(), "Expected Tier 2 fallback to succeed");
    }

    #[test]
    fn test_tier2_wget_no_false_positive() {
        // A bare number without HTTP context must NOT match (e.g. "Downloaded 256 bytes")
        let input = "Downloaded 256 bytes\n";
        let result = try_parse_regex(input);
        assert!(
            result.is_none(),
            "Expected no match for bare number without HTTP context"
        );
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("wget_download.txt");
        let output = CommandOutput {
            stdout: String::new(),
            stderr: input,
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
            stdout: String::new(),
            stderr: "no output at all".to_string(),
            exit_code: Some(1),
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
        // Tier 2 input: wget progress output containing an HTTP status code with
        // HTTP context (matches RE_WGET_ANY_STATUS) but does NOT contain
        // "HTTP request" or "ERROR" (which would trigger Tier 1).
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "HTTP response 200\n".to_string(),
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
