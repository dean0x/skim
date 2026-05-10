//! `docker inspect` parser.
//!
//! SAFETY INVARIANT: Do NOT inject `--format` — `docker inspect` outputs JSON by default.
//!
//! Three-tier degradation:
//! - **Tier 1 (Full)**: JSON array of container objects
//! - **Tier 2 (N/A)**: inspect always outputs JSON
//! - **Tier 3 (Passthrough)**: Raw output on parse failure

use serde_json::Value;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;

/// No-op: `docker inspect` already outputs JSON — never inject `--format`.
///
/// # Safety invariant
/// `docker inspect` does not support `--format json`; it natively emits a JSON
/// array. Injecting any `--format` flag would corrupt the output.
pub(crate) fn prepare_args(_args: &mut Vec<String>) {
    // Intentionally empty: no format injection for inspect.
}

/// Three-tier parse function for `docker inspect` output.
pub(crate) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    // Tier 1: JSON array
    if let Some(result) = try_parse_json_array(text) {
        return ParseResult::Full(result);
    }

    // Tier 3: passthrough (no Tier 2 — inspect always outputs JSON)
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_json_array(text: &str) -> Option<InfraResult> {
    if !text.starts_with('[') {
        return None;
    }
    let arr: Vec<Value> = serde_json::from_str(text).ok()?;

    let mut items: Vec<InfraItem> = Vec::new();
    let count = arr.len();

    for container in &arr {
        let id = container["Id"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect::<String>();
        let status = container["State"]["Status"].as_str().unwrap_or("unknown");
        let image = container["Config"]["Image"].as_str().unwrap_or("");
        let ip = container["NetworkSettings"]["IPAddress"]
            .as_str()
            .unwrap_or("");

        // Collect mount destinations
        let mounts: Vec<&str> = container["Mounts"]
            .as_array()
            .map(|m| m.iter().filter_map(|v| v["Destination"].as_str()).collect())
            .unwrap_or_default();

        let label = format!("{id} [{status}]");
        let mut value = image.to_string();
        if !ip.is_empty() {
            value.push_str(&format!(" ip={ip}"));
        }
        if !mounts.is_empty() {
            let mount_list = mounts.join(", ");
            value.push_str(&format!(" mounts=[{mount_list}]"));
        }

        items.push(InfraItem { label, value });
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "inspect".to_string(),
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

    fn json_fixture() -> String {
        load_fixture("docker_inspect.json")
    }

    #[test]
    fn test_tier1_json_array_full_result() {
        let fixture = json_fixture();
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            let display = r.to_string();
            assert!(display.contains("1 containers"));
            assert!(display.contains("running"));
            assert!(display.contains("nginx:latest"));
        }
    }

    #[test]
    fn test_tier1_extracts_mount_destination() {
        let fixture = json_fixture();
        let output = make_output(&fixture);
        if let ParseResult::Full(r) = parse_impl(&output) {
            assert!(r.to_string().contains("/etc/nginx/nginx.conf"));
        }
    }

    #[test]
    fn test_tier3_passthrough_on_non_json() {
        let output = make_output("Error: No such container: nonexistent");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_empty_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    /// Safety invariant: prepare_args must never inject --format for `docker inspect`.
    #[test]
    fn test_prepare_args_is_noop() {
        let mut args = vec!["inspect".to_string(), "my-container".to_string()];
        let original = args.clone();
        prepare_args(&mut args);
        assert_eq!(
            args, original,
            "prepare_args must not modify args for docker inspect"
        );
    }
}
