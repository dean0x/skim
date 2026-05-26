//! `docker ps` / `docker container ls` parser.
//!
//! Three-tier degradation:
//! - **Tier 1 (Full)**: NDJSON (`--format json`) — one JSON object per line
//! - **Tier 2 (Degraded)**: Regex on tabular `CONTAINER ID … NAMES` output
//! - **Tier 3 (Passthrough)**: Raw output

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, inject_format_json};

/// Matches the `docker ps` tabular header line.
static RE_PS_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"CONTAINER ID\s+IMAGE").unwrap());

/// Inject `--format json` unless the user already specified a format.
pub(crate) fn prepare_args(args: &mut Vec<String>) {
    inject_format_json(args);
}

/// Three-tier parse function for `docker ps` output.
pub(crate) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    // Tier 1: NDJSON
    if let Some(result) = try_parse_ndjson(text) {
        return ParseResult::Full(result);
    }

    // Tier 2: tabular text
    if let Some(result) = try_parse_tabular(text) {
        return ParseResult::Degraded(
            result,
            vec!["docker ps: no JSON output, using text parser".to_string()],
        );
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_ndjson(text: &str) -> Option<InfraResult> {
    // Quick format check: bail early if no line looks like a JSON object.
    // The actual parse happens in the main loop below.
    if !text.lines().any(|l| l.trim().starts_with('{')) {
        return None;
    }

    let mut items: Vec<InfraItem> = Vec::new();
    let mut count = 0usize;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        count += 1;

        let id = obj["ID"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect::<String>();
        let image = obj["Image"].as_str().unwrap_or("");
        let status = obj["Status"].as_str().unwrap_or("");
        let ports = obj["Ports"].as_str().unwrap_or("");

        let value = if ports.is_empty() {
            format!("{image} [{status}]")
        } else {
            format!("{image} [{status}] {ports}")
        };
        items.push(InfraItem { label: id, value });
    }

    if count == 0 {
        return None;
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "ps".to_string(),
        format!("{count} containers"),
        items,
    ))
}

fn try_parse_tabular(text: &str) -> Option<InfraResult> {
    // Must find the CONTAINER ID header
    let header_line = text.lines().find(|l| RE_PS_HEADER.is_match(l))?;

    // Determine column boundaries from header
    let id_end = header_line.find("IMAGE").unwrap_or(15);
    let image_end = header_line.find("COMMAND").unwrap_or(30);
    let status_start = header_line.find("STATUS").unwrap_or(60);
    let names_start = header_line
        .rfind("NAMES")
        .unwrap_or(header_line.len().saturating_sub(20));

    let mut items: Vec<InfraItem> = Vec::new();
    let mut count = 0usize;

    for line in text.lines() {
        // Skip header and empty lines
        if line.trim().is_empty() || RE_PS_HEADER.is_match(line) {
            continue;
        }
        if line.len() < id_end {
            continue;
        }
        count += 1;

        let id = line[..id_end.min(line.len())]
            .trim()
            .chars()
            .take(12)
            .collect::<String>();
        let image = if image_end <= line.len() {
            line[id_end.min(line.len())..image_end.min(line.len())]
                .trim()
                .to_string()
        } else {
            String::new()
        };
        let status = if status_start < line.len() {
            let end = names_start.min(line.len());
            line[status_start.min(line.len())..end].trim().to_string()
        } else {
            String::new()
        };
        let name = if names_start < line.len() {
            line[names_start..].trim().to_string()
        } else {
            String::new()
        };

        let value = format!("{image} [{status}] {name}");
        items.push(InfraItem { label: id, value });
    }

    if items.is_empty() {
        return None;
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "ps".to_string(),
        format!("{count} containers"),
        items,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture, make_output};

    fn ndjson_fixture() -> String {
        load_fixture("infra", "docker_ps.json")
    }

    fn tabular_fixture() -> String {
        load_fixture("infra", "docker_ps_text.txt")
    }

    #[test]
    fn test_tier1_ndjson_full_result() {
        let fixture = ndjson_fixture();
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            assert!(r.to_string().contains("5 containers"));
            assert!(r.to_string().contains("nginx:latest"));
        }
    }

    #[test]
    fn test_tier1_ndjson_container_ids_truncated_to_12() {
        let fixture = ndjson_fixture();
        let output = make_output(&fixture);
        if let ParseResult::Full(r) = parse_impl(&output) {
            let display = r.to_string();
            assert!(display.contains("a1b2c3d4e5f6"));
        }
    }

    #[test]
    fn test_tier2_tabular_degraded() {
        let fixture = tabular_fixture();
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded, got {result:?}"
        );
        if let ParseResult::Degraded(r, warnings) = result {
            assert!(r.to_string().contains("containers"));
            assert!(!warnings.is_empty());
        }
    }

    #[test]
    fn test_tier3_passthrough_on_garbage() {
        let output = make_output("Error: Cannot connect to the Docker daemon");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_empty_input_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_prepare_args_inject_format() {
        let mut args = vec!["ps".to_string()];
        prepare_args(&mut args);
        assert!(args.contains(&"--format".to_string()));
        assert!(args.contains(&"json".to_string()));
    }

    #[test]
    fn test_prepare_args_skip_when_format_present() {
        let mut args = vec![
            "ps".to_string(),
            "--format".to_string(),
            "table".to_string(),
        ];
        let original_len = args.len();
        prepare_args(&mut args);
        assert_eq!(
            args.len(),
            original_len,
            "should not inject when --format already present"
        );
    }

    #[test]
    fn test_prepare_args_skip_when_format_equals_present() {
        let mut args = vec!["ps".to_string(), "--format=table".to_string()];
        let original_len = args.len();
        prepare_args(&mut args);
        assert_eq!(args.len(), original_len);
    }
}
