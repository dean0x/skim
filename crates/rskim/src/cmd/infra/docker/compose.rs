//! `docker compose ps` and `docker compose logs` parsers.
//!
//! - `compose ps` reuses the same NDJSON pattern as `docker ps`.
//! - `compose logs` delegates to the shared log compression pipeline.

use serde_json::Value;

use crate::cmd::log::{compress_log, LogFlags};
use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, log_result_to_infra};

/// Three-tier parse function for `docker compose ps` output.
///
/// Reuses the same NDJSON pattern as `docker ps`.
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

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_compose_ps_ndjson(text: &str) -> Option<InfraResult> {
    let first_json = text.lines().find(|l| l.trim().starts_with('{'))?;
    serde_json::from_str::<Value>(first_json.trim()).ok()?;

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
        ParseResult::Degraded(log_result, warnings) => {
            ParseResult::Degraded(log_result_to_infra(log_result, "docker", "compose logs"), warnings)
        }
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
