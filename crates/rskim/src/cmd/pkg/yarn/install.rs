use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::runner::CommandOutput;

use super::combine_output;

// ============================================================================
// Static regex patterns
// ============================================================================

static RE_YARN_SAVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"success Saved lockfile").expect("valid regex"));
static RE_YARN_WARNING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^warning\s+(.+)").expect("valid regex"));
static RE_YARN_INFO_RESOLVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^info\s+Resolved\s+(\d+)\s+packages?").expect("valid regex"));
static RE_YARN_DONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Done in \d+").expect("valid regex"));

pub(super) fn run_install(
    args: &[String],
    show_stats: bool,
    _json_output: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "yarn",
            subcommand: "install",
            expected_exit_codes: &[1],
            forward_stderr: false,
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Yarn: npm install -g yarn",
        },
        args,
        show_stats,
        rec,
        |_cmd_args| {
            // yarn install has no useful --json for install output in v1 classic
        },
        parse_install,
    )
}

fn parse_install(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: NDJSON (yarn --json per-line)
    if let Some(result) = try_parse_ndjson(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex on text
    let combined = combine_output(output);
    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["yarn install: structured parse failed, using regex".to_string()],
        );
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

/// Parse yarn NDJSON output (one JSON object per line).
/// yarn v1 `--json` produces lines like: `{"type":"step","data":{"message":"Resolving packages","current":1,"total":4}}`
fn try_parse_ndjson(stdout: &str) -> Option<PkgResult> {
    let mut any_json = false;
    let mut saved_lockfile = false;
    let mut warnings = 0usize;
    let mut added = 0usize;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        any_json = true;

        let type_str = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match type_str {
            "success" => {
                if v.get("data")
                    .and_then(|d| d.as_str())
                    .map(|s| s.contains("Saved lockfile"))
                    .unwrap_or(false)
                {
                    saved_lockfile = true;
                }
                // Parse package count from success messages like "Saved 42 new packages."
                if let Some(data) = v.get("data").and_then(|d| d.as_str())
                    && let Some(caps) = LazyLock::force(&RE_YARN_INFO_RESOLVED).captures(data)
                {
                    added = caps[1].parse().unwrap_or(0);
                }
            }
            "warning" => {
                warnings += 1;
            }
            "info" => {
                if let Some(data) = v.get("data").and_then(|d| d.as_str())
                    && let Some(caps) = LazyLock::force(&RE_YARN_INFO_RESOLVED).captures(data)
                {
                    added = caps[1].parse().unwrap_or(added);
                }
            }
            _ => {}
        }
    }

    if !any_json {
        return None;
    }

    Some(PkgResult::new(
        "yarn".to_string(),
        PkgOperation::Install {
            added,
            removed: 0,
            changed: 0,
            warnings,
        },
        saved_lockfile || warnings == 0,
        vec![],
    ))
}

fn try_parse_regex(text: &str) -> Option<PkgResult> {
    // Detect yarn output by looking for "Done in N" or "success Saved"
    if !RE_YARN_DONE.is_match(text) && !RE_YARN_SAVED.is_match(text) {
        return None;
    }

    let saved_lockfile = RE_YARN_SAVED.is_match(text);
    let warnings = RE_YARN_WARNING.find_iter(text).count();

    Some(PkgResult::new(
        "yarn".to_string(),
        PkgOperation::Install {
            added: 0,
            removed: 0,
            changed: 0,
            warnings,
        },
        saved_lockfile || warnings == 0,
        vec![],
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const YARN_INSTALL_NDJSON: &str = r#"{"type":"step","data":{"message":"Resolving packages","current":1,"total":4}}
{"type":"step","data":{"message":"Fetching packages","current":2,"total":4}}
{"type":"step","data":{"message":"Linking dependencies","current":3,"total":4}}
{"type":"info","data":"Resolved 42 packages"}
{"type":"warning","data":"unmet peer dependency react@>=16"}
{"type":"success","data":"Saved lockfile"}
{"type":"success","data":"Saved 42 new packages."}"#;

    const YARN_INSTALL_TEXT: &str = "yarn install v1.22.19\n[1/4] Resolving packages...\n[2/4] Fetching packages...\n[3/4] Linking dependencies...\n[4/4] Building fresh packages...\nsuccess Saved lockfile.\nDone in 3.45s.\n";

    #[test]
    fn test_yarn_install_tier1_ndjson() {
        let result = try_parse_ndjson(YARN_INSTALL_NDJSON);
        assert!(result.is_some(), "Expected NDJSON parse to succeed");
        let r = result.unwrap();
        let s = format!("{r}");
        assert!(s.contains("yarn install"), "Display: {s}");
    }

    #[test]
    fn test_yarn_install_tier2_regex() {
        let result = try_parse_regex(YARN_INSTALL_TEXT);
        assert!(result.is_some(), "Expected regex parse to succeed");
        let r = result.unwrap();
        let s = format!("{r}");
        assert!(s.contains("yarn install"), "Display: {s}");
    }

    #[test]
    fn test_yarn_install_tier3_passthrough() {
        let output = CommandOutput {
            stdout: "completely random output".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_install(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
