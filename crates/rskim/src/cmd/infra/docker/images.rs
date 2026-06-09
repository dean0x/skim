//! `docker images` parser.
//!
//! Three-tier degradation:
//! - **Tier 1 (Full)**: NDJSON (`--format json`) — one JSON object per line
//! - **Tier 2 (Degraded)**: Regex on tabular `REPOSITORY … SIZE` output
//! - **Tier 3 (Passthrough)**: Raw output

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, inject_format_json};

/// Matches the `docker images` tabular header line.
static RE_IMAGES_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"REPOSITORY\s+TAG").unwrap());

/// Inject `--format json` unless the user already specified a format.
pub(crate) fn prepare_args(args: &mut Vec<String>) {
    inject_format_json(args);
}

/// Three-tier parse function for `docker images` output.
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
            vec!["docker images: no JSON output, using text parser".to_string()],
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

        let repo = obj["Repository"].as_str().unwrap_or("<none>");
        let tag = obj["Tag"].as_str().unwrap_or("<none>");
        let id = obj["ID"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect::<String>();
        let size = obj["Size"].as_str().unwrap_or("");

        let label = format!("{repo}:{tag}");
        let value = format!("{id} ({size})");
        items.push(InfraItem { label, value });
    }

    if count == 0 {
        return None;
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "images".to_string(),
        format!("{count} images"),
        items,
    ))
}

fn try_parse_tabular(text: &str) -> Option<InfraResult> {
    let header_line = text.lines().find(|l| RE_IMAGES_HEADER.is_match(l))?;

    let tag_end = header_line.find("IMAGE ID").unwrap_or(30);
    let id_end = header_line.find("CREATED").unwrap_or(50);
    let size_start = header_line
        .find("SIZE")
        .unwrap_or(header_line.len().saturating_sub(10));

    let mut items: Vec<InfraItem> = Vec::new();
    let mut count = 0usize;

    for line in text.lines() {
        if line.trim().is_empty() || RE_IMAGES_HEADER.is_match(line) {
            continue;
        }
        if line.len() < tag_end {
            continue;
        }
        count += 1;

        let repo_tag = line[..tag_end.min(line.len())].trim().to_string();
        let id = if id_end <= line.len() {
            line[tag_end.min(line.len())..id_end.min(line.len())]
                .trim()
                .chars()
                .take(12)
                .collect::<String>()
        } else {
            String::new()
        };
        let size = if size_start < line.len() {
            line[size_start..].trim().to_string()
        } else {
            String::new()
        };

        let value = format!("{id} ({size})");
        items.push(InfraItem {
            label: repo_tag,
            value,
        });
    }

    if items.is_empty() {
        return None;
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "images".to_string(),
        format!("{count} images"),
        items,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::{load_fixture, make_output};

    fn ndjson_fixture() -> String {
        load_fixture("infra", "docker_images.json")
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
            assert!(r.to_string().contains("6 images"));
            assert!(r.to_string().contains("nginx:latest"));
        }
    }

    #[test]
    fn test_tier3_passthrough_on_garbage() {
        let output = make_output("Error: Cannot connect to Docker daemon");
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
        let mut args = vec!["images".to_string()];
        prepare_args(&mut args);
        assert!(args.contains(&"--format".to_string()));
        assert!(args.contains(&"json".to_string()));
    }

    #[test]
    fn test_prepare_args_skip_when_format_present() {
        let mut args = vec![
            "images".to_string(),
            "--format".to_string(),
            "table".to_string(),
        ];
        let original_len = args.len();
        prepare_args(&mut args);
        assert_eq!(args.len(), original_len);
    }
}
