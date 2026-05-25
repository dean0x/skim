//! `kubectl describe` parser.
//!
//! # SAFETY INVARIANT
//! NEVER inject `-o json` for `kubectl describe` — `describe` does not support it.
//!
//! Three-tier degradation:
//! - **Tier 1 (N/A)**: describe doesn't support JSON output
//! - **Tier 2 (Degraded)**: Regex on describe text output, strip Annotations/Managed Fields
//! - **Tier 3 (Passthrough)**: Raw output

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;

/// Matches key-value lines in describe output.
static RE_DESCRIBE_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\w[\w\s]*?):(\s*)(.*)$").unwrap());

/// Section headers that should be skipped/stripped from output.
const SKIP_SECTIONS: &[&str] = &["Annotations", "Managed Fields"];

/// No-op: `kubectl describe` does NOT support `-o json`.
///
/// # Safety invariant
/// `kubectl describe` only outputs human-readable text. Injecting `-o json`
/// would cause a fatal error. This function MUST remain a no-op.
pub(crate) fn prepare_args(_args: &mut Vec<String>) {
    // Intentionally empty: no format injection for describe.
}

/// Three-tier parse function for `kubectl describe` output.
pub(crate) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    // Tier 2: regex on describe text (no Tier 1 — describe doesn't support JSON)
    if let Some(result) = try_parse_describe(text) {
        return ParseResult::Degraded(
            result,
            vec!["kubectl describe: using text parser".to_string()],
        );
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_describe(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();
    let mut in_skip_section = false;
    let mut name: Option<String> = None;
    let mut namespace: Option<String> = None;
    let mut has_content = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Check if we're entering a skipped section (Annotations, Managed Fields)
        if SKIP_SECTIONS
            .iter()
            .any(|s| trimmed.starts_with(s) && trimmed.ends_with(':'))
        {
            in_skip_section = true;
            continue;
        }

        // Check if we've left the skip section (non-indented line)
        if in_skip_section && !line.starts_with(' ') && !line.starts_with('\t') {
            in_skip_section = false;
        }

        if in_skip_section {
            continue;
        }

        // Key: Value lines
        if let Some(caps) = RE_DESCRIBE_FIELD.captures(trimmed) {
            let key = caps[1].trim().to_string();
            let value = caps[3].trim().to_string();

            // Track name and namespace for summary.
            // Only set has_content=true when we find definitive describe fields
            // (Name or Namespace) to avoid false-positives on error messages.
            if key == "Name" {
                has_content = true;
                name = Some(value.clone());
            } else if key == "Namespace" {
                has_content = true;
                namespace = Some(value.clone());
            }

            // Include key fields
            let interesting = matches!(
                key.as_str(),
                "Name"
                    | "Namespace"
                    | "Status"
                    | "Phase"
                    | "IP"
                    | "Node"
                    | "Image"
                    | "State"
                    | "Restart Count"
                    | "Ready"
            );
            if interesting && !value.is_empty() {
                items.push(InfraItem { label: key, value });
            }
        }
    }

    if !has_content {
        return None;
    }

    let resource_name = name.unwrap_or_else(|| "unknown".to_string());
    let ns = namespace.unwrap_or_else(|| "default".to_string());
    let summary = format!("{ns}/{resource_name}");

    Some(InfraResult::new(
        "kubectl".to_string(),
        "describe".to_string(),
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
    use crate::cmd::test_support::*;

    #[test]
    fn test_tier2_describe_degraded() {
        let fixture = load_fixture("infra", "kubectl_describe_pod.txt");
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded, got {result:?}"
        );
    }

    #[test]
    fn test_describe_extracts_name_and_namespace() {
        let fixture = load_fixture("infra", "kubectl_describe_pod.txt");
        let output = make_output(&fixture);
        if let ParseResult::Degraded(r, _) = parse_impl(&output) {
            let display = r.to_string();
            assert!(
                display.contains("web-deployment"),
                "should contain pod name"
            );
            assert!(display.contains("default"), "should contain namespace");
        }
    }

    #[test]
    fn test_describe_strips_annotations_section() {
        let fixture = load_fixture("infra", "kubectl_describe_pod.txt");
        let output = make_output(&fixture);
        if let ParseResult::Degraded(r, _) = parse_impl(&output) {
            let display = r.to_string();
            // Annotations section should be stripped
            assert!(
                !display.contains("kubectl.kubernetes.io/last-applied-configuration"),
                "should strip annotation details"
            );
        }
    }

    #[test]
    fn test_empty_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_tier3_garbage_passthrough() {
        let output = make_output("Error: pods 'nonexistent' not found");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    /// Safety invariant: prepare_args MUST NOT inject -o json for kubectl describe.
    #[test]
    fn test_prepare_args_is_noop() {
        let mut args = vec![
            "describe".to_string(),
            "pod".to_string(),
            "my-pod".to_string(),
        ];
        let original = args.clone();
        prepare_args(&mut args);
        assert_eq!(
            args, original,
            "prepare_args MUST NOT modify args for kubectl describe"
        );
    }
}
