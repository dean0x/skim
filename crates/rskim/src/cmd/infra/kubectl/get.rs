//! `kubectl get` parser.
//!
//! Three-tier degradation:
//! - **Tier 1 (Full)**: JSON (`-o json`) — Pod, PodList, Deployment, etc.
//! - **Tier 2 (Degraded)**: Regex on tabular `NAME READY STATUS` output
//! - **Tier 3 (Passthrough)**: Raw output
//!
//! # SAFETY INVARIANT
//! Inject `-o json` only if the user has NOT specified `-o`/`--output` and
//! is NOT using `-w`/`--watch` (watch mode streams indefinitely).

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;

/// Matches tabular `kubectl get` output header line.
static RE_GET_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"NAME\s+READY\s+STATUS").unwrap());

/// Inject `-o json` unless the user has already specified output format or watch mode.
pub(crate) fn prepare_args(args: &mut Vec<String>) {
    let has_output = args.iter().any(|a| {
        a == "-o" || a == "--output" || a.starts_with("-o=") || a.starts_with("--output=")
    });
    let has_watch = args.iter().any(|a| a == "-w" || a == "--watch");

    if !has_output && !has_watch {
        args.push("-o".to_string());
        args.push("json".to_string());
    }
}

/// Three-tier parse function for `kubectl get` output.
pub(crate) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    // Tier 1: JSON object/list
    if let Some(result) = try_parse_json(text) {
        return ParseResult::Full(result);
    }

    // Tier 2: tabular text
    if let Some(result) = try_parse_tabular(text) {
        return ParseResult::Degraded(
            result,
            vec!["kubectl get: no JSON output, using text parser".to_string()],
        );
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_json(text: &str) -> Option<InfraResult> {
    if !text.starts_with('{') {
        return None;
    }
    let obj: Value = serde_json::from_str(text).ok()?;

    let kind = obj["kind"].as_str().unwrap_or("Unknown");

    // List resource
    if let Some(item_kind) = kind.strip_suffix("List") {
        let items = obj["items"].as_array()?;
        let count = items.len();

        let result_items: Vec<InfraItem> = items.iter().map(extract_resource_item).collect();

        return Some(InfraResult::new(
            "kubectl".to_string(),
            "get".to_string(),
            format!("{count} {item_kind}s"),
            result_items,
        ));
    }

    // Single resource
    let item = extract_resource_item(&obj);
    Some(InfraResult::new(
        "kubectl".to_string(),
        "get".to_string(),
        format!("1 {kind}"),
        vec![item],
    ))
}

fn extract_resource_item(obj: &Value) -> InfraItem {
    let name = obj["metadata"]["name"].as_str().unwrap_or("unknown");
    let namespace = obj["metadata"]["namespace"].as_str().unwrap_or("default");
    let phase = obj["status"]["phase"].as_str().unwrap_or("Unknown");

    let label = format!("{namespace}/{name}");
    let value = format!("[{phase}]");
    InfraItem { label, value }
}

fn try_parse_tabular(text: &str) -> Option<InfraResult> {
    let header_line = text.lines().find(|l| RE_GET_HEADER.is_match(l))?;

    let status_start = header_line.find("STATUS").unwrap_or(60);
    let name_end = header_line.find("READY").unwrap_or(50);

    let mut items: Vec<InfraItem> = Vec::new();
    let mut count = 0usize;

    for line in text.lines() {
        if line.trim().is_empty() || RE_GET_HEADER.is_match(line) {
            continue;
        }
        if line.len() < name_end {
            continue;
        }
        count += 1;

        let name = line[..name_end.min(line.len())].trim().to_string();
        let status = if status_start < line.len() {
            // Status column until next column (RESTARTS)
            let end = line.len();
            let s = line[status_start.min(line.len())..end]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            s
        } else {
            String::new()
        };

        items.push(InfraItem {
            label: name,
            value: format!("[{status}]"),
        });
    }

    if items.is_empty() {
        return None;
    }

    Some(InfraResult::new(
        "kubectl".to_string(),
        "get".to_string(),
        format!("{count} resources"),
        items,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CommandOutput;

    fn make_output(stdout: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    }

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/infra");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    #[test]
    fn test_tier1_json_podlist_full_result() {
        let fixture = load_fixture("kubectl_get_pods.json");
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            let display = r.to_string();
            assert!(display.contains("5 Pods"));
            assert!(display.contains("web-deployment"));
        }
    }

    #[test]
    fn test_tier2_tabular_degraded() {
        let fixture = load_fixture("kubectl_get_pods_text.txt");
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded, got {result:?}"
        );
        if let ParseResult::Degraded(r, warnings) = result {
            assert!(r.to_string().contains("resources"));
            assert!(!warnings.is_empty());
        }
    }

    #[test]
    fn test_tier3_passthrough_on_garbage() {
        let output = make_output("Error: connection refused");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_empty_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_prepare_args_inject_json() {
        let mut args = vec!["get".to_string(), "pods".to_string()];
        prepare_args(&mut args);
        assert!(args.contains(&"-o".to_string()));
        assert!(args.contains(&"json".to_string()));
    }

    #[test]
    fn test_prepare_args_skip_when_output_present() {
        let mut args = vec![
            "get".to_string(),
            "pods".to_string(),
            "-o".to_string(),
            "yaml".to_string(),
        ];
        let original_len = args.len();
        prepare_args(&mut args);
        assert_eq!(args.len(), original_len);
    }

    #[test]
    fn test_prepare_args_skip_when_watch_present() {
        let mut args = vec!["get".to_string(), "pods".to_string(), "-w".to_string()];
        let original_len = args.len();
        prepare_args(&mut args);
        // Must NOT inject -o json when -w is present
        assert_eq!(
            args.len(),
            original_len,
            "must not inject when -w is present"
        );
    }
}
