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
//! When `skim gh` receives piped stdin with no arguments, the dispatcher
//! uses strong discriminators to route JSON objects to the correct parser:
//! - `"jobs"` field → run view
//! - `"headRefName"` field → PR view
//! - `"number"` + `"state"` + body/labels/assignees → issue view
//! - JSON array → list parser (may fall through to checks JSON)
//!
//! # Adding a new sub-parser
//!
//! New `gh` subcommand parsers should follow the established pattern:
//! 1. Use [`shared::three_tier_parse`] as the `parse_impl` scaffold.
//! 2. Use [`shared::try_parse_json_object`] to compose the Tier 1 JSON closure
//!    (avoids the duplicated `serde_json::from_str(...).and_then(...)`
//!    plumbing).
//! 3. Document the JSON gate choice (`{`, `[`, or both), the
//!    `text_is_full` flag, and the degraded-reason string as a design
//!    decision in `parse_impl`'s rustdoc.
//! 4. Accept pre-trimmed input in any Tier 1 JSON function that might also
//!    be called from [`parse_impl_with_auto_detect`] — pass the `trimmed`
//!    slice from there rather than `&output.stdout`.

pub(crate) mod api;
pub(crate) mod issue_view;
pub(crate) mod list;
pub(crate) mod pr_checks;
pub(crate) mod pr_view;
pub(crate) mod release_view;
pub(crate) mod run_view;
pub(crate) mod run_watch;
pub(super) mod shared;
pub(super) mod streaming;

// Re-export everything from `shared` so that submodule `use super::…` imports
// continue to resolve without any changes to the sub-parser files.
pub(super) use shared::{
    MAX_JSON_BYTES, RE_GH_CHECK_SYMBOL, RE_GH_CHECK_TAB, RE_GH_RUN_HEADER, RE_GH_RUN_JOB,
    RE_GH_TAB_ROW, RE_GH_VIEW_FIELD, extract_comments, inject_json_fields, parse_view_text,
    three_tier_parse, try_parse_json_object,
};

use crate::output::ParseResult;
use crate::output::canonical::InfraResult;
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "gh",
    env_overrides: &[],
    install_hint: "Install gh: https://cli.github.com/",
    family: "infra",
    skip_ansi_strip: false,
    command_type: CommandType::Infra,
    expected_exit_codes: &[],
    forward_stderr: false,
};

// ============================================================================
// Run entry point
// ============================================================================

/// Run `skim gh [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // Transparency gate: user steered output → pass gh through (UTF-8, capped —
    // see #317 for the streaming/non-UTF-8 follow-up).  Reuses
    // run_raw_passthrough (separate stdout/stderr, inherited stdin, exit code
    // preserved). Covers wrapper/argv0 + direct `skim gh`; the hook path is
    // handled by the rules.rs skip-list. See the "compress, never truncate"
    // tracking issue (#317).
    if shared::user_steers_output(args) {
        return crate::cmd::run_raw_passthrough(CONFIG.program, args, CONFIG.env_overrides);
    }

    let subcmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match (subcmd, action) {
        ("issue", "view") => run_tool(
            CONFIG,
            args,
            ctx,
            issue_view::prepare_args,
            issue_view::parse_impl,
        ),
        ("pr", "view") => run_tool(
            CONFIG,
            args,
            ctx,
            pr_view::prepare_args,
            pr_view::parse_impl,
        ),
        ("pr", "checks") => run_tool(
            CONFIG,
            args,
            ctx,
            pr_checks::prepare_args,
            pr_checks::parse_impl,
        ),
        ("run", "view") => run_tool(
            CONFIG,
            args,
            ctx,
            run_view::prepare_args,
            run_view::parse_impl,
        ),
        ("run", "watch") => {
            // Streaming handler — does not use run_tool.
            // Passes remaining args (after "run watch") to run_watch.
            let watch_args = if args.len() > 2 { &args[2..] } else { &[] };
            run_watch::run_watch(watch_args, ctx)
        }
        ("release", "view") => run_tool(
            CONFIG,
            args,
            ctx,
            release_view::prepare_args,
            release_view::parse_impl,
        ),
        ("api", _) => {
            // Strip the leading "api" token so run_tool sees the
            // remaining args only.  This lets use_stdin detection fire when
            // the user pipes `gh api ... | skim gh api` with no
            // endpoint arg — args[1..] is empty → stdin is read.
            // api::prepare_args re-inserts "api" before the spawn so the
            // child process still receives `gh api [endpoint...]`.
            let api_args = if args.is_empty() { &[][..] } else { &args[1..] };
            run_tool(CONFIG, api_args, ctx, api::prepare_args, api::parse_impl)
        }
        _ => run_tool(
            CONFIG,
            args,
            ctx,
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
/// When the user pipes `gh ... | skim gh` without explicit subcommand
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
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed)
            && let Some(result) = try_parse_view_json_auto(&obj)
        {
            return ParseResult::Full(result);
        }
        // Unknown JSON object shape → passthrough (e.g., gh api responses)
        let combined = combine_stdout_stderr(output);
        return ParseResult::Passthrough(combined.into_owned());
    }

    // JSON array — try list first, then checks JSON.
    // NOTE: pass the pre-computed `trimmed` slice, not `&output.stdout`.
    // Both `try_parse_json_list` and `try_parse_checks_json` now require
    // pre-trimmed input as a documented precondition (batch-C).
    if trimmed.starts_with('[') {
        if let Some(result) = list::try_parse_json_list(trimmed) {
            return ParseResult::Full(result);
        }
        if let Some(result) = pr_checks::try_parse_checks_json(trimmed) {
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
    let has_issue_fields =
        obj.get("body").is_some() || obj.get("labels").is_some() || obj.get("assignees").is_some();

    if has_number && has_state && has_issue_fields {
        return issue_view::try_parse_json(obj);
    }

    None
}

// ============================================================================
// Shared test helpers
// ============================================================================

/// Shared fixture loader for gh submodule tests.
///
/// Delegates to `test_utils::load_fixture` with the `"infra"` subdir
/// pre-applied. Sub-modules import it as
/// `use super::super::load_gh_fixture as load_fixture`.
#[cfg(test)]
pub(super) fn load_gh_fixture(name: &str) -> String {
    crate::cmd::test_utils::load_fixture("infra", name)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::load_gh_fixture as load_fixture;
    use super::*;
    use crate::cmd::test_utils::{make_output, make_output_full};

    // --- extract_comments ---

    #[test]
    fn test_extract_comments_empty() {
        let result = extract_comments(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_extract_comments_strips_quotes() {
        let comments = vec![serde_json::json!({
            "author": {"login": "alice"},
            "body": "> quoted text\n\nActual reply here"
        })];
        let result = extract_comments(&comments);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].contains("Actual reply here"),
            "got: {}",
            result[0]
        );
        assert!(!result[0].contains("quoted text"));
    }

    #[test]
    fn test_extract_comments_emits_all_in_full() {
        // #317: every comment, full text — no count cap, no preview cap.
        let long_body = format!("First line\nSecond line with detail: {}", "x".repeat(300));
        let mut comments: Vec<serde_json::Value> = (0..10)
            .map(|i| {
                serde_json::json!({
                    "author": {"login": format!("user{i}")},
                    "body": format!("Comment {i}")
                })
            })
            .collect();
        comments.push(serde_json::json!({
            "author": {"login": "verbose"},
            "body": long_body
        }));
        let result = extract_comments(&comments);
        assert_eq!(result.len(), 11, "all comments must be emitted");
        assert!(result.iter().any(|s| s.contains("user0")));
        assert!(result.iter().any(|s| s.contains("user9")));
        let verbose = result.iter().find(|s| s.contains("@verbose")).unwrap();
        assert!(
            verbose.contains("Second line with detail"),
            "full multi-line text must survive: {verbose:.80}"
        );
        assert!(
            verbose.contains(&"x".repeat(300)),
            "no 120-char preview cap"
        );
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
        let output = make_output_full(
            "Not Found (HTTP 404)",
            "gh: 404 - Not Found\nhttps://github.com",
            Some(1),
        );
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for 404, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_auth_error_passthrough() {
        let output = make_output_full(
            "",
            "To get started with GitHub CLI, please run:  gh auth login",
            Some(4),
        );
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

    #[test]
    fn test_freeform_release_text_passthrough() {
        // Freeform text that is neither checks format nor list format should
        // pass through unchanged via auto-detect. This text matches no tab
        // pattern (no `<number>\t<rest>`) and no checks regex.
        let input = "Release v2.0.0\nPublished by @dean\nSee CHANGELOG.md for details.";
        let output = make_output(input);
        let result = parse_impl_with_auto_detect(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for freeform release text, got {}",
            result.tier_name()
        );
    }
}
