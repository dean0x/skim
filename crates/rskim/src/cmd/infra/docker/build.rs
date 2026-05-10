//! `docker build` parser.
//!
//! SAFETY INVARIANT: Do NOT inject `--format json` — `docker build` does not
//! support the `--format` flag.
//!
//! Three-tier degradation:
//! - **Tier 1 (N/A)**: No JSON format for build
//! - **Tier 2 (Degraded)**: Regex on legacy (`Step N/M`) or BuildKit (`#N [stage]`) output
//! - **Tier 3 (Passthrough)**: Raw output

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;

/// Legacy builder: `Step N/M : COMMAND`
static RE_BUILD_STEP_LEGACY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Step\s+(\d+)/(\d+)\s*:\s*(.+)$").unwrap());

/// BuildKit: `#N [stage N/M] COMMAND` or `[N/M] COMMAND`
static RE_BUILD_STEP_BUILDKIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:=>|>)\s+\[([^\]]+)\]\s+(.+)$").unwrap());

/// Final image ID from legacy builder.
static RE_BUILD_SUCCESS_LEGACY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Successfully built ([0-9a-f]+)").unwrap());

/// Final image ID/name from BuildKit.
static RE_BUILD_SUCCESS_BUILDKIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"writing image (sha256:[0-9a-f]+)").unwrap());

/// Warning or error lines.
static RE_BUILD_WARN_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:WARN(?:ING)?|ERROR):\s+(.+)$").unwrap());

/// No-op: `docker build` does not support `--format json`.
///
/// # Safety invariant
/// Injecting `--format json` to `docker build` would cause an error.
pub(crate) fn prepare_args(_args: &mut Vec<String>) {
    // Intentionally empty: no format injection for build.
}

/// Three-tier parse function for `docker build` output.
pub(crate) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    // Tier 2: regex on build output (no Tier 1 JSON for build)
    if let Some(result) = try_parse_build(text) {
        return ParseResult::Degraded(
            result,
            vec!["docker build: using build step parser".to_string()],
        );
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_build(text: &str) -> Option<InfraResult> {
    let mut steps: Vec<String> = Vec::new();
    let mut final_image: Option<String> = None;
    let mut warnings: Vec<String> = Vec::new();
    let mut is_legacy = false;
    let mut is_buildkit = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Legacy format
        if let Some(caps) = RE_BUILD_STEP_LEGACY.captures(trimmed) {
            is_legacy = true;
            let step_num = &caps[1];
            let total = &caps[2];
            let cmd = &caps[3];
            steps.push(format!("Step {step_num}/{total}: {cmd}"));
            continue;
        }

        // BuildKit format — match lines like `=> [1/6] FROM ...`
        if let Some(caps) = RE_BUILD_STEP_BUILDKIT.captures(trimmed) {
            is_buildkit = true;
            let stage = &caps[1];
            let cmd = &caps[2];
            // Skip internal/metadata steps
            if !stage.contains("internal")
                && !stage.contains("load")
                && !stage.contains("exporting")
            {
                steps.push(format!("[{stage}] {cmd}"));
            }
            continue;
        }

        // Image ID (legacy)
        if let Some(caps) = RE_BUILD_SUCCESS_LEGACY.captures(trimmed) {
            final_image = Some(caps[1].chars().take(12).collect());
            continue;
        }

        // Image ID (BuildKit)
        if let Some(caps) = RE_BUILD_SUCCESS_BUILDKIT.captures(trimmed) {
            final_image = Some(caps[1].chars().take(19).collect()); // sha256:12chars
            continue;
        }

        // Warnings/errors
        if let Some(caps) = RE_BUILD_WARN_ERROR.captures(trimmed) {
            warnings.push(caps[1].to_string());
        }
    }

    if !is_legacy && !is_buildkit {
        return None;
    }

    let format = if is_buildkit { "BuildKit" } else { "legacy" };
    let step_count = steps.len();
    let summary = format!("{step_count} steps ({format})");

    let mut items: Vec<InfraItem> = steps
        .into_iter()
        .enumerate()
        .map(|(i, s)| InfraItem {
            label: format!("step {}", i + 1),
            value: s,
        })
        .collect();

    if let Some(img) = final_image {
        items.push(InfraItem {
            label: "image".to_string(),
            value: img,
        });
    }

    for warn in warnings {
        items.push(InfraItem {
            label: "warning".to_string(),
            value: warn,
        });
    }

    Some(InfraResult::new(
        "docker".to_string(),
        "build".to_string(),
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

    fn legacy_fixture() -> String {
        load_fixture("docker_build_legacy.txt")
    }

    fn buildkit_fixture() -> String {
        load_fixture("docker_build_buildkit.txt")
    }

    #[test]
    fn test_tier2_legacy_build_degraded() {
        let fixture = legacy_fixture();
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded, got {result:?}"
        );
        if let ParseResult::Degraded(r, _) = result {
            let display = r.to_string();
            assert!(display.contains("step"));
            assert!(display.contains("legacy"));
        }
    }

    #[test]
    fn test_tier2_legacy_build_extracts_steps() {
        let fixture = legacy_fixture();
        let output = make_output(&fixture);
        if let ParseResult::Degraded(r, _) = parse_impl(&output) {
            let display = r.to_string();
            assert!(display.contains("FROM python"), "should extract FROM step");
        }
    }

    #[test]
    fn test_tier2_buildkit_degraded() {
        let fixture = buildkit_fixture();
        let output = make_output(&fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded, got {result:?}"
        );
        if let ParseResult::Degraded(r, _) = result {
            assert!(r.to_string().contains("BuildKit"));
        }
    }

    #[test]
    fn test_tier3_passthrough_on_garbage() {
        let output = make_output("some random unrelated output here");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_empty_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    /// Safety invariant: prepare_args must never inject --format for `docker build`.
    #[test]
    fn test_prepare_args_is_noop() {
        let mut args = vec![
            "build".to_string(),
            "-t".to_string(),
            "myapp".to_string(),
            ".".to_string(),
        ];
        let original = args.clone();
        prepare_args(&mut args);
        assert_eq!(
            args, original,
            "prepare_args must not modify args for docker build"
        );
    }
}
