//! `docker compose ps` and `docker compose logs` parsers.
//!
//! - `compose ps` uses three-tier degradation:
//!   Tier 1: NDJSON (`docker compose ps --format json`, modern docker)
//!   Tier 2: Tabular text (`NAME  IMAGE  ...` header + rows, legacy compose)
//!   Tier 3: Passthrough (unrecognised format)
//! - `compose logs` delegates to the shared log compression pipeline.
//!
//! **Known limitation:** Only `docker compose` (v2, plugin form) is supported.
//! The standalone `docker-compose` binary (v1) was deprecated in July 2023 and
//! reached end-of-life. Users should migrate to `docker compose` (space-separated).

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::cmd::log::{compress_log, LogFlags};
use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, log_result_to_infra};

/// Regex that detects the header line of tabular `docker compose ps` output.
///
/// Matches lines like `NAME   IMAGE   COMMAND   SERVICE   CREATED   STATUS   PORTS`
/// (case-insensitive, whitespace-separated columns).  Presence of `NAME` and
/// `IMAGE` together on a line is sufficient to identify the tabular format.
static COMPOSE_PS_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*NAME\s+IMAGE\s+").expect("compose ps header regex is valid")
});

/// Three-tier parse function for `docker compose ps` output.
///
/// - Tier 1: NDJSON (modern `docker compose ps --format json`)
/// - Tier 2: Tabular text fallback (legacy compose or older docker versions)
/// - Tier 3: Passthrough (unrecognised format)
pub(crate) fn parse_ps(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    // Tier 1: NDJSON
    if let Some(result) = try_parse_compose_ps_ndjson(text) {
        return ParseResult::Full(result);
    }

    // Tier 2: Tabular text
    if let Some(result) = try_parse_compose_ps_tabular(text) {
        return ParseResult::Degraded(result, vec!["tabular fallback (no NDJSON)".to_string()]);
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_compose_ps_ndjson(text: &str) -> Option<InfraResult> {
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
        let service = obj["Service"].as_str().unwrap_or("");
        let image = obj["Image"].as_str().unwrap_or("");
        let state = obj["State"].as_str().unwrap_or("");
        let status = obj["Status"].as_str().unwrap_or("");

        let label = format!("{service} ({id})");
        let value = format!("{image} [{state}/{status}]");
        items.push(InfraItem { label, value });
    }

    if count == 0 {
        return None;
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "compose ps".to_string(),
        format!("{count} services"),
        items,
    ))
}

/// Tier 2: Parse tabular `docker compose ps` output.
///
/// Handles output produced by older docker compose versions that emit a
/// plain-text table instead of NDJSON:
///
/// ```text
/// NAME        IMAGE       COMMAND   SERVICE   CREATED       STATUS    PORTS
/// web_1       nginx:alpine ...       web       2 hours ago   Up        0.0.0.0:80->80/tcp
/// db_1        postgres:15  ...       db        2 hours ago   Up
/// ```
///
/// Returns `None` when the output does not match the expected tabular format.
fn try_parse_compose_ps_tabular(text: &str) -> Option<InfraResult> {
    let mut lines = text.lines();

    // Find the header line.
    let header_line = lines.find(|l| COMPOSE_PS_HEADER_RE.is_match(l))?;

    // Locate column offsets by scanning for column name starting positions.
    // docker compose ps always starts with NAME then IMAGE; we detect their
    // character offsets to extract values from data rows.
    let name_col = header_line
        .find("NAME")
        .or_else(|| header_line.to_uppercase().find("NAME"))?;
    let image_col = header_line
        .find("IMAGE")
        .or_else(|| header_line.to_uppercase().find("IMAGE"))?;

    // Find STATUS column (optional — may not be present in all versions).
    // The header is ASCII so byte offsets from the uppercased version match.
    let status_col = header_line.to_uppercase().find("STATUS");

    let mut items: Vec<InfraItem> = Vec::new();
    let mut count = 0usize;

    for line in text.lines() {
        // Skip empty lines and the header line itself.
        if line.trim().is_empty() || COMPOSE_PS_HEADER_RE.is_match(line) {
            continue;
        }

        // Skip separator lines (all dashes/hyphens).
        if line
            .trim()
            .chars()
            .all(|c| c == '-' || c == '+' || c == ' ')
        {
            continue;
        }

        // Extract NAME (from name_col to image_col, trimmed).
        let name = if line.len() > name_col {
            let end = image_col.min(line.len());
            line[name_col..end].trim()
        } else {
            continue;
        };

        if name.is_empty() {
            continue;
        }

        // Extract IMAGE (from image_col to next column or end, trimmed).
        let image = if line.len() > image_col {
            // Take up to the next column boundary (40 chars heuristic) or end.
            let end = (image_col + 40).min(line.len());
            let raw = &line[image_col..end];
            // Trim trailing whitespace but keep the image name/tag.
            raw.split_whitespace().next().unwrap_or("").to_string()
        } else {
            String::new()
        };

        // Extract STATUS if column is known.
        let status = if let Some(sc) = status_col {
            if line.len() > sc {
                let raw = &line[sc..];
                raw.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        count += 1;
        let label = name.to_string();
        let value = if status.is_empty() {
            image
        } else {
            format!("{image} [{status}]")
        };
        items.push(InfraItem { label, value });
    }

    if count == 0 {
        return None;
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "compose ps".to_string(),
        format!("{count} services"),
        items,
    ))
}

/// Parse function for `docker compose logs` output.
///
/// Delegates to the shared log compression pipeline.
pub(crate) fn parse_logs(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    let flags = LogFlags::default();
    match compress_log(text, &flags) {
        ParseResult::Full(log_result) => {
            ParseResult::Full(log_result_to_infra(log_result, "docker", "compose logs"))
        }
        ParseResult::Degraded(log_result, warnings) => ParseResult::Degraded(
            log_result_to_infra(log_result, "docker", "compose logs"),
            warnings,
        ),
        ParseResult::Passthrough(raw) => ParseResult::Passthrough(raw),
    }
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

    fn compose_ps_fixture() -> String {
        load_fixture("docker_compose_ps.json")
    }

    fn logs_fixture() -> String {
        load_fixture("docker_logs.txt")
    }

    fn tabular_fixture() -> String {
        load_fixture("docker_compose_ps_tabular.txt")
    }

    #[test]
    fn test_compose_ps_tier1_ndjson() {
        let fixture = compose_ps_fixture();
        let output = make_output(&fixture);
        let result = parse_ps(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            let display = r.to_string();
            assert!(display.contains("3 services"));
            assert!(display.contains("web"));
        }
    }

    #[test]
    fn test_compose_ps_tier2_tabular() {
        let fixture = tabular_fixture();
        let output = make_output(&fixture);
        let result = parse_ps(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "tabular output should be Degraded (Tier 2), got {result:?}"
        );
        if let ParseResult::Degraded(r, _) = result {
            let display = r.to_string();
            assert!(
                display.contains("3 services"),
                "should have 3 services: {display}"
            );
            assert!(
                display.contains("web"),
                "should contain web service: {display}"
            );
            assert!(
                display.contains("db"),
                "should contain db service: {display}"
            );
            assert!(
                display.contains("redis"),
                "should contain redis service: {display}"
            );
        }
    }

    #[test]
    fn test_compose_ps_empty_passthrough() {
        let output = make_output("");
        let result = parse_ps(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_compose_ps_garbage_passthrough() {
        let output = make_output("Error: compose not found");
        let result = parse_ps(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_compose_logs_structured() {
        let fixture = logs_fixture();
        let output = make_output(&fixture);
        let result = parse_logs(&output);
        assert!(
            !matches!(result, ParseResult::Passthrough(_)),
            "expected structured result for log content"
        );
    }

    #[test]
    fn test_compose_logs_empty_passthrough() {
        let output = make_output("");
        let result = parse_logs(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }
}
