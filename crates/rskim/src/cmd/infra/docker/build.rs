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

// ============================================================================
// BuildFormat enum
// ============================================================================

/// Detected build output format.
///
/// Replaces the former `is_legacy: bool` / `is_buildkit: bool` pair.
///
/// ## Illegal state elimination
///
/// The old boolean pair had a silent third state — both `false` — which meant
/// "no build output detected" and was handled by the `if !is_legacy && !is_buildkit`
/// guard.  `BuildFormat` makes all three states explicit:
///
/// - `BuildFormat::Legacy`: classic `Step N/M : COMMAND` lines seen
/// - `BuildFormat::BuildKit`: modern `=> [stage N/M] COMMAND` lines seen
/// - `None` (absence of this value): no recognisable build output → `try_parse_build` returns `None`
///
/// A 0-step build (both formats seen, no actual steps extracted) is rejected
/// via an explicit guard to avoid emitting an empty result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildFormat {
    Legacy,
    BuildKit,
}

impl BuildFormat {
    /// Human-readable label used in the build summary line.
    fn label(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::BuildKit => "BuildKit",
        }
    }
}

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
    let mut fmt: Option<BuildFormat> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        // Legacy format
        if let Some(caps) = RE_BUILD_STEP_LEGACY.captures(trimmed) {
            // First-writer-wins: only set format if not yet detected.
            // Prevents a mixed Legacy+BuildKit log from silently flipping to
            // whichever format matched last.
            if fmt.is_none() {
                fmt = Some(BuildFormat::Legacy);
            }
            let step_num = &caps[1];
            let total = &caps[2];
            let cmd = &caps[3];
            steps.push(format!("Step {step_num}/{total}: {cmd}"));
            continue;
        }

        // BuildKit format — match lines like `=> [1/6] FROM ...`
        if let Some(caps) = RE_BUILD_STEP_BUILDKIT.captures(trimmed) {
            // First-writer-wins: only set format if not yet detected.
            if fmt.is_none() {
                fmt = Some(BuildFormat::BuildKit);
            }
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

    // No recognised build output → passthrough
    let fmt = fmt?;

    // A recognised header with zero extracted steps is ambiguous; reject rather
    // than emit an empty result (e.g. a build log where all lines were filtered
    // as internal/load/exporting steps).
    if steps.is_empty() && final_image.is_none() {
        return None;
    }

    let label = fmt.label();
    let step_count = steps.len();
    let summary = format!("{step_count} steps ({label})");

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
    use crate::cmd::test_support::{load_fixture, make_output};

    fn legacy_fixture() -> String {
        load_fixture("infra", "docker_build_legacy.txt")
    }

    fn buildkit_fixture() -> String {
        load_fixture("infra", "docker_build_buildkit.txt")
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

    // ── BuildFormat enum tests ───────────────────────────────────────────────

    #[test]
    fn test_build_format_legacy_label() {
        assert_eq!(BuildFormat::Legacy.label(), "legacy");
    }

    #[test]
    fn test_build_format_buildkit_label() {
        assert_eq!(BuildFormat::BuildKit.label(), "BuildKit");
    }

    /// `WARNING:` lines in build output are captured as `warning` items in the result.
    #[test]
    fn test_try_parse_build_captures_warnings() {
        let input = "\
 => [1/3] FROM docker.io/library/python:3.11\n\
 => [2/3] COPY requirements.txt .\n\
 => [3/3] RUN pip install -r requirements.txt\n\
WARNING: Running pip as the 'root' user can result in broken permissions\n\
WARNING: pip is configured with locations that require TLS/SSL\n";
        let result =
            try_parse_build(input).expect("expected Some for BuildKit output with warnings");
        let display = result.to_string();
        assert!(
            display.contains("Running pip as the 'root' user"),
            "expected first warning text in output, got: {display}"
        );
        assert!(
            display.contains("pip is configured with locations"),
            "expected second warning text in output, got: {display}"
        );
    }

    /// `ERROR:` lines in build output are captured as `warning` items in the result.
    #[test]
    fn test_try_parse_build_captures_errors() {
        let input = "\
 => [1/2] FROM docker.io/library/node:20\n\
 => [2/2] RUN npm install\n\
ERROR: failed to solve: failed to read dockerfile: open Dockerfile: no such file or directory\n";
        let result = try_parse_build(input).expect("expected Some for BuildKit output with errors");
        let display = result.to_string();
        assert!(
            display.contains("failed to solve"),
            "expected error text in output, got: {display}"
        );
    }

    /// Mixed Legacy+BuildKit output must use first-writer-wins for BuildFormat.
    ///
    /// If `Step N/M` lines appear before `=> [stage]` lines the format must be
    /// locked to `Legacy` — not silently overwritten by the later BuildKit match.
    /// Conversely, if BuildKit appears first the format stays `BuildKit`.
    #[test]
    fn test_mixed_output_first_writer_wins_legacy_first() {
        // Legacy lines appear first — format must stay Legacy.
        let input = "Step 1/2 : FROM python:3.11\n\
                     Step 2/2 : RUN pip install flask\n\
                     => [1/2] FROM docker.io/library/python:3.11\n\
                     Successfully built abc123456def\n";
        let result = try_parse_build(input).expect("expected Some for mixed output");
        let display = result.to_string();
        assert!(
            display.contains("legacy"),
            "expected format label 'legacy' when Legacy lines appear first, got: {display}"
        );
        assert!(
            !display.contains("BuildKit"),
            "BuildKit label must not appear when Legacy was detected first, got: {display}"
        );
    }

    /// Mirror of the above: BuildKit first must not be overwritten by a later
    /// Legacy line.
    #[test]
    fn test_mixed_output_first_writer_wins_buildkit_first() {
        // BuildKit lines appear first — format must stay BuildKit.
        let input = " => [1/2] FROM docker.io/library/python:3.11\n\
                     Step 1/1 : RUN pip install flask\n\
                     Successfully built abc123456def\n";
        let result = try_parse_build(input).expect("expected Some for mixed output");
        let display = result.to_string();
        assert!(
            display.contains("BuildKit"),
            "expected format label 'BuildKit' when BuildKit lines appear first, got: {display}"
        );
        assert!(
            !display.contains("legacy"),
            "legacy label must not appear when BuildKit was detected first, got: {display}"
        );
    }

    /// A build log whose steps are all filtered (internal/load/exporting) should
    /// return `None` rather than emitting an empty result.
    #[test]
    fn test_try_parse_build_zero_steps_returns_none() {
        // These lines set `fmt = Some(BuildFormat::BuildKit)` but produce no
        // entries in `steps` because all three are filtered metadata lines.
        let input = "\
 => [internal] load build definition from Dockerfile\n\
 => [internal] load .dockerignore\n\
 => [internal] load metadata for docker.io/library/python:3.11\n";
        let result = try_parse_build(input);
        assert!(
            result.is_none(),
            "expected None for all-filtered BuildKit output, got {result:?}"
        );
    }
}
