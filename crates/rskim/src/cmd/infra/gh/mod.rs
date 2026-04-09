//! GitHub CLI (`gh`) parser with three-tier degradation (#131).
//!
//! Executes `gh` and parses the output into structured `InfraResult`.
//!
//! Dispatches to sub-parsers based on `(subcmd, action)`:
//! - `gh issue view` → [`issue_view`]
//! - `gh pr view`    → [`pr_view`]
//! - `gh pr checks`  → [`pr_checks`]
//! - `gh run view`   → [`run_view`]
//! - All other (list, release, …) → [`list`] with auto-detect fallback
//!
//! Three tiers per parser:
//! - **Tier 1 (Full)**: JSON parsing (inject `--json` for supported commands)
//! - **Tier 2 (Degraded)**: Regex on tabular/text output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! # Auto-detection (stdin / piped usage)
//!
//! When `skim infra gh` receives piped stdin with no arguments, the dispatcher
//! uses strong discriminators to route JSON objects to the correct parser:
//! - `"jobs"` field → run view
//! - `"headRefName"` field → PR view
//! - `"number"` + `"state"` + body/labels/assignees → issue view
//! - JSON array → list parser (may fall through to checks JSON)

pub(crate) mod issue_view;
pub(crate) mod list;
pub(crate) mod pr_checks;
pub(crate) mod pr_view;
pub(crate) mod run_view;
pub(super) mod shared;

// Re-export everything from `shared` so that submodule `use super::…` imports
// continue to resolve without any changes to the sub-parser files.
pub(super) use shared::{
    extract_comments, inject_json_fields, parse_view_text, three_tier_parse, truncate_body,
    MAX_BODY_LINES, MAX_COMMENTS, MAX_ITEMS, MAX_JSON_BYTES, MAX_STEP_DETAIL, RE_GH_CHECK_SYMBOL,
    RE_GH_CHECK_TAB, RE_GH_RUN_HEADER, RE_GH_RUN_JOB, RE_GH_VIEW_FIELD,
};

use crate::output::canonical::InfraResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, run_infra_tool, InfraToolConfig};

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "gh",
    env_overrides: &[],
    install_hint: "Install gh: https://cli.github.com/",
};

// ============================================================================
// Run entry point
// ============================================================================

/// Run `skim infra gh [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<std::process::ExitCode> {
    let subcmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match (subcmd, action) {
        ("issue", "view") => run_infra_tool(
            CONFIG,
            args,
            show_stats,
            json_output,
            issue_view::prepare_args,
            issue_view::parse_impl,
        ),
        ("pr", "view") => run_infra_tool(
            CONFIG,
            args,
            show_stats,
            json_output,
            pr_view::prepare_args,
            pr_view::parse_impl,
        ),
        ("pr", "checks") => run_infra_tool(
            CONFIG,
            args,
            show_stats,
            json_output,
            pr_checks::prepare_args,
            pr_checks::parse_impl,
        ),
        ("run", "view") => run_infra_tool(
            CONFIG,
            args,
            show_stats,
            json_output,
            run_view::prepare_args,
            run_view::parse_impl,
        ),
        _ => run_infra_tool(
            CONFIG,
            args,
            show_stats,
            json_output,
            list::prepare_args,
            parse_impl_with_auto_detect,
        ),
    }
}

// ============================================================================
// Auto-detect dispatcher (stdin / piped usage with no specific route)
// ============================================================================

/// Parse function used for list-routed commands that may receive piped JSON of
/// any gh type via stdin.
///
/// When the user pipes `gh ... | skim infra gh` without explicit subcommand
/// arguments, this function auto-detects the JSON shape and routes accordingly:
/// - JSON object with `"jobs"` → run view
/// - JSON object with `"headRefName"` → PR view
/// - JSON object with `"number"` + issue discriminators → issue view
/// - JSON array → list parser, then checks JSON fallback
/// - Text → checks text regex, then list regex
///
/// Error outputs (404, auth failures, malformed JSON) pass through unchanged.
pub(crate) fn parse_impl_with_auto_detect(output: &CommandOutput) -> ParseResult<InfraResult> {
    let trimmed = output.stdout.trim();

    // JSON object — auto-detect by discriminating fields
    if trimmed.starts_with('{') && trimmed.len() <= MAX_JSON_BYTES {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(result) = try_parse_view_json_auto(&obj) {
                return ParseResult::Full(result);
            }
        }
        // Unknown JSON object shape → passthrough (e.g., gh api responses)
        let combined = combine_stdout_stderr(output);
        return ParseResult::Passthrough(combined.into_owned());
    }

    // JSON array — try list first, then checks JSON
    if trimmed.starts_with('[') {
        if let Some(result) = list::try_parse_json_list(&output.stdout) {
            return ParseResult::Full(result);
        }
        if let Some(result) = pr_checks::try_parse_checks_json(&output.stdout) {
            return ParseResult::Full(result);
        }
        // Unknown JSON array (e.g., gh api) → passthrough
        let combined = combine_stdout_stderr(output);
        return ParseResult::Passthrough(combined.into_owned());
    }

    // Text — try checks text format first, then list three-tier fallback.
    // Delegate to list::parse_impl so text passthrough follows the same path as
    // direct list commands (regex Tier 2 → Passthrough Tier 3).
    if let Some(result) = pr_checks::try_parse_checks_text(&output.stdout) {
        return ParseResult::Full(result);
    }

    list::parse_impl(output)
}

/// Discriminate a JSON object by its fields to select the correct view parser.
///
/// Uses strong discriminators to avoid false positives:
/// - `"jobs"` is only present in run view responses
/// - `"headRefName"` is only present in PR responses
/// - `"number"` + `"state"` + (`"body"` or `"labels"` or `"assignees"`) →
///   issue view (also matches PR view, but PR has `headRefName` and is caught first)
fn try_parse_view_json_auto(obj: &serde_json::Value) -> Option<InfraResult> {
    // Run view: only run JSON has a "jobs" array
    if obj.get("jobs").is_some() {
        return run_view::try_parse_json(obj);
    }

    // PR view: "headRefName" is a PR-only field
    if obj.get("headRefName").is_some() {
        return pr_view::try_parse_json(obj);
    }

    // Issue view: "number" + "state" + issue-specific field
    let has_number = obj.get("number").is_some();
    let has_state = obj.get("state").is_some();
    let has_issue_fields = obj.get("body").is_some()
        || obj.get("labels").is_some()
        || obj.get("assignees").is_some();

    if has_number && has_state && has_issue_fields {
        return issue_view::try_parse_json(obj);
    }

    None
}

// ============================================================================
// Shared test helpers
// ============================================================================

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::runner::CommandOutput;

    pub(super) fn make_output(stdout: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    }

    pub(super) fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/infra");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_helpers::{load_fixture, make_output};

    // --- truncate_body ---

    #[test]
    fn test_truncate_body_fits() {
        let body = "line1\nline2\nline3";
        assert_eq!(truncate_body(body, 10), body);
    }

    #[test]
    fn test_truncate_body_truncates() {
        let body = "a\nb\nc\nd\ne";
        let result = truncate_body(body, 3);
        assert!(result.contains("... (2 more lines)"));
        assert!(result.starts_with("a\nb\nc"));
    }

    // --- extract_comments ---

    #[test]
    fn test_extract_comments_empty() {
        let result = extract_comments(&[], 3);
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_comments_strips_quotes() {
        let comments = vec![serde_json::json!({
            "author": {"login": "alice"},
            "body": "> quoted text\n\nActual reply here"
        })];
        let result = extract_comments(&comments, 3);
        assert_eq!(result.len(), 1);
        assert!(result[0].contains("Actual reply here"), "got: {}", result[0]);
        assert!(!result[0].contains("quoted text"));
    }

    #[test]
    fn test_extract_comments_limits_to_last_n() {
        let comments: Vec<serde_json::Value> = (0..10)
            .map(|i| {
                serde_json::json!({
                    "author": {"login": format!("user{i}")},
                    "body": format!("Comment {i}")
                })
            })
            .collect();
        let result = extract_comments(&comments, 3);
        assert_eq!(result.len(), 3);
        // Should contain the last 3 (7, 8, 9)
        assert!(result.iter().any(|s| s.contains("user7")));
        assert!(result.iter().any(|s| s.contains("user9")));
    }

    // --- auto-detect ---

    #[test]
    fn test_auto_detect_issue_view() {
        let input = load_fixture("gh_issue_view.json");
        let output = make_output(&input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_full(),
            "Expected Full for issue view, got {}",
            result.tier_name()
        );
        let s = match &result {
            ParseResult::Full(r) => r.as_ref().to_string(),
            _ => unreachable!(),
        };
        assert!(s.contains("issue view"), "Expected 'issue view' in: {s}");
    }

    #[test]
    fn test_auto_detect_pr_view() {
        let input = load_fixture("gh_pr_view.json");
        let output = make_output(&input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_full(),
            "Expected Full for pr view, got {}",
            result.tier_name()
        );
        let s = match &result {
            ParseResult::Full(r) => r.as_ref().to_string(),
            _ => unreachable!(),
        };
        assert!(s.contains("pr view"), "Expected 'pr view' in: {s}");
    }

    #[test]
    fn test_auto_detect_run_view() {
        let input = load_fixture("gh_run_view.json");
        let output = make_output(&input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_full(),
            "Expected Full for run view, got {}",
            result.tier_name()
        );
        let s = match &result {
            ParseResult::Full(r) => r.as_ref().to_string(),
            _ => unreachable!(),
        };
        assert!(s.contains("run view"), "Expected 'run view' in: {s}");
    }

    #[test]
    fn test_auto_detect_checks_text() {
        let input = load_fixture("gh_pr_checks_text.txt");
        let output = make_output(&input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_full(),
            "Expected Full for checks text, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_auto_detect_list_json() {
        let input = load_fixture("gh_pr_list.json");
        let output = make_output(&input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_full(),
            "Expected Full for list JSON, got {}",
            result.tier_name()
        );
        let s = match &result {
            ParseResult::Full(r) => r.as_ref().to_string(),
            _ => unreachable!(),
        };
        assert!(s.contains("gh list"), "Expected 'gh list' in: {s}");
    }

    #[test]
    fn test_auto_detect_unknown_json_object_passthrough() {
        // A JSON object that doesn't match any known shape should pass through
        let input = r#"{"some": "unknown", "response": true}"#;
        let output = make_output(input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for unknown JSON object, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_auto_detect_gh_api_no_false_positive() {
        // gh api responses with arbitrary fields should not be misidentified
        let input = r#"{"id": 123, "node_id": "abc", "url": "https://api.github.com/repos/foo"}"#;
        let output = make_output(input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for gh api response, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_404_error_passthrough() {
        let input = "Not Found (HTTP 404)";
        let output = CommandOutput {
            stdout: input.to_string(),
            stderr: "gh: 404 - Not Found\nhttps://github.com".to_string(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for 404, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_auth_error_passthrough() {
        let input = "";
        let output = CommandOutput {
            stdout: input.to_string(),
            stderr: "To get started with GitHub CLI, please run:  gh auth login".to_string(),
            exit_code: Some(4),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for auth error, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_malformed_json_passthrough() {
        let input = "{ not valid json }";
        let output = make_output(input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for malformed JSON, got {}",
            result.tier_name()
        );
    }
}
