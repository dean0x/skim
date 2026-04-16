//! `gh pr view` parser with three-tier degradation.
//!
//! Extends the issue view parser with PR-specific overlay fields:
//! branch information (`headRefName → baseRefName`), diff statistics
//! (`additions`, `deletions`, `changedFiles`), and blocking signals
//! (`isDraft`, `mergeable`, `statusCheckRollup`).
//!
//! # Design Decision: Reuse issue_view core
//!
//! PR view shares all issue fields (`number`, `title`, `state`, `body`,
//! `labels`, `assignees`, `author`, `milestone`, `comments`) and adds
//! PR-only fields. We call [`issue_view::try_parse_json`] for the common
//! items, then overlay the PR-specific items rather than duplicating code.
//!
//! # AD-INFRA-9 (2026-04-11) — Always-render draft/mergeable/CI
//!
//! `draft`, `mergeable`, and `ci` items are rendered **always**, not only
//! when blocking, so agents can observe the full merge-readiness signal set
//! even when the PR is clean. This is important because "clean" is itself a
//! signal: an agent that sees `ci: passing` knows it does not need to wait.
//!
//! CI aggregation hierarchy (deterministic): any FAILURE|CANCELLED|TIMED_OUT
//! → `failing`; else any PENDING|QUEUED|IN_PROGRESS → `pending`; else
//! `passing`; `none` if statusCheckRollup is null, missing, or empty.

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{
    inject_json_fields, issue_view, parse_view_text, three_tier_parse, try_parse_json_object,
};

/// JSON fields to inject for `gh pr view`.
///
/// Superset of issue view fields with PR-specific additions.
/// Includes `isDraft`, `mergeable`, and `statusCheckRollup` for AD-INFRA-9 always-render.
const PR_VIEW_FIELDS: &str =
    "number,title,state,body,labels,assignees,author,headRefName,baseRefName,additions,deletions,changedFiles,comments,isDraft,mergeable,statusCheckRollup";

/// Inject `--json` for PR view if not already present.
pub(super) fn prepare_args(cmd_args: &mut Vec<String>) {
    inject_json_fields(cmd_args, PR_VIEW_FIELDS);
}

/// Three-tier parse function for `gh pr view` output.
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    three_tier_parse(
        output,
        |trimmed| try_parse_json_object(trimmed, try_parse_json),
        |t| t.starts_with('{'),
        try_parse_text,
        false,
        "gh pr view: JSON parse failed, using text regex",
    )
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse a `gh pr view --json` object into an [`InfraResult`].
///
/// Delegates to [`issue_view::try_parse_json`] for common fields, then
/// re-applies PR-specific fields and re-renders as "pr view" operation.
///
/// Accepts a pre-parsed JSON `Value` so this function can also be called
/// from the auto-detect dispatcher.
pub(super) fn try_parse_json(obj: &serde_json::Value) -> Option<InfraResult> {
    // Get the common issue items via issue_view parser
    let issue_result = issue_view::try_parse_json(obj)?;

    // NOTE: number/title/state are re-extracted here because issue_view returns
    // a rendered InfraResult with its summary already baked in as "issue view".
    // We need the raw fields to re-render the summary as "pr view". The three
    // field lookups are cheap (no allocation on the hot path) and avoidable only
    // by threading raw fields back through issue_view's return type — not worth
    // the added coupling for three string reads.
    let number = obj.get("number").and_then(|v| v.as_u64())?;
    let title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("(no title)");
    let state = obj
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();

    // AD-INFRA-9: Prefix summary with [DRAFT] when isDraft is true.
    let is_draft = obj
        .get("isDraft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let draft_prefix = if is_draft { "[DRAFT] " } else { "" };
    let summary = format!("{draft_prefix}#{number}: {title} ({state})");

    // Start with the issue items
    let mut items: Vec<InfraItem> = issue_result.items;

    // PR-specific overlay: branch
    let head = obj
        .get("headRefName")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let base = obj
        .get("baseRefName")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    items.push(InfraItem {
        label: "branch".to_string(),
        value: format!("{head} → {base}"),
    });

    // PR-specific overlay: diff stats
    let additions = obj.get("additions").and_then(|v| v.as_u64()).unwrap_or(0);
    let deletions = obj.get("deletions").and_then(|v| v.as_u64()).unwrap_or(0);
    let changed_files = obj
        .get("changedFiles")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    items.push(InfraItem {
        label: "changes".to_string(),
        value: format!("+{additions} -{deletions} ({changed_files} files)"),
    });

    // AD-INFRA-9: Always-render draft, mergeable, and CI status items.

    // draft: always true or false
    items.push(InfraItem {
        label: "draft".to_string(),
        value: is_draft.to_string(),
    });

    // mergeable: MERGEABLE → clean, CONFLICTING → conflicting, otherwise unknown
    let mergeable_raw = obj
        .get("mergeable")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    let mergeable_display = match mergeable_raw {
        "MERGEABLE" => "clean",
        "CONFLICTING" => "conflicting",
        _ => "unknown",
    };
    items.push(InfraItem {
        label: "mergeable".to_string(),
        value: mergeable_display.to_string(),
    });

    // ci: aggregate statusCheckRollup conclusions
    // Hierarchy: any FAILURE|CANCELLED|TIMED_OUT → failing;
    //            else any PENDING|QUEUED|IN_PROGRESS → pending;
    //            else passing; none if null/empty.
    let ci_display = aggregate_ci_status(obj);
    items.push(InfraItem {
        label: "ci".to_string(),
        value: ci_display,
    });

    Some(InfraResult::new(
        "gh".to_string(),
        "pr view".to_string(),
        summary,
        items,
    ))
}

/// Aggregate `statusCheckRollup` into a single CI status string.
///
/// # AD-INFRA-9 (2026-04-11) — CI aggregation hierarchy
/// Deterministic: FAILURE|CANCELLED|TIMED_OUT wins over PENDING|QUEUED|IN_PROGRESS
/// wins over SUCCESS. Returns `"none"` when the field is null, absent, or empty.
fn aggregate_ci_status(obj: &serde_json::Value) -> String {
    let checks = match obj.get("statusCheckRollup").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return "none".to_string(),
    };

    let mut has_pending = false;

    for check in checks {
        let conclusion = check
            .get("conclusion")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match conclusion {
            "FAILURE" | "CANCELLED" | "TIMED_OUT" => return "failing".to_string(),
            "PENDING" | "QUEUED" | "IN_PROGRESS" | "" => {
                has_pending = true;
            }
            _ => {}
        }
    }

    if has_pending {
        "pending".to_string()
    } else {
        "passing".to_string()
    }
}

// ============================================================================
// Tier 2: text regex fallback
// ============================================================================

/// Parse `gh pr view` text output using regex.
fn try_parse_text(text: &str) -> Option<InfraResult> {
    parse_view_text(text, "pr view")
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{load_fixture, make_output};
    use super::*;

    #[test]
    fn test_tier1_json() {
        let input = load_fixture("gh_pr_view.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("INFRA: gh pr view"),
            "got: {}",
            result.as_ref()
        );
        assert!(result.as_ref().contains("#15"), "got: {}", result.as_ref());
        // Should have branch info
        let branch_item = result.items.iter().find(|i| i.label == "branch");
        assert!(branch_item.is_some(), "Expected branch item");
        assert!(
            branch_item.unwrap().value.contains("→"),
            "got: {}",
            branch_item.unwrap().value
        );
        // Should have changes
        let changes_item = result.items.iter().find(|i| i.label == "changes");
        assert!(changes_item.is_some(), "Expected changes item");
        assert!(
            changes_item.unwrap().value.contains("+150"),
            "got: {}",
            changes_item.unwrap().value
        );
    }

    #[test]
    fn test_tier1_user_json_fields_not_overridden() {
        let mut args = vec![
            "pr".to_string(),
            "view".to_string(),
            "15".to_string(),
            "--json".to_string(),
            "title".to_string(),
        ];
        let original_len = args.len();
        prepare_args(&mut args);
        assert_eq!(
            args.len(),
            original_len,
            "Should not inject when --json present"
        );
    }

    #[test]
    fn test_tier2_text() {
        let text = "Add dark mode #15\nState: open\nAuthor: feature-dev\nBase: main\n";
        let result = try_parse_text(text);
        assert!(result.is_some(), "Expected Tier 2 text parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("gh pr view"));
    }

    #[test]
    fn test_passthrough_garbage() {
        let output = make_output("not a PR response at all");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for garbage, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("gh_pr_view.json");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    // ========================================================================
    // AD-INFRA-9: draft/mergeable/CI always-render tests
    // ========================================================================

    #[test]
    fn test_gh_pr_view_draft_conflict_renders_all_three() {
        let input = load_fixture("gh_pr_view_draft_conflict.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).expect("must parse");
        let display = result.as_ref();
        assert!(display.contains("draft"), "must render draft: {display}");
        assert!(
            display.contains("mergeable"),
            "must render mergeable: {display}"
        );
        assert!(display.contains("ci"), "must render ci: {display}");
        // Values
        assert!(display.contains("true"), "draft must be true: {display}");
        assert!(
            display.contains("conflicting"),
            "mergeable must be conflicting: {display}"
        );
        assert!(display.contains("failing"), "ci must be failing: {display}");
        // Summary must be prefixed with [DRAFT]
        assert!(
            display.contains("[DRAFT]"),
            "summary must have [DRAFT] prefix: {display}"
        );
    }

    #[test]
    fn test_gh_pr_view_clean_still_renders_items() {
        // Always-render contract: even on clean PRs, all three items appear.
        let input = load_fixture("gh_pr_view_clean.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).expect("must parse");
        let display = result.as_ref();
        assert!(
            display.contains("draft"),
            "must render draft even when false: {display}"
        );
        assert!(
            display.contains("mergeable"),
            "must render mergeable even when clean: {display}"
        );
        assert!(
            display.contains("ci"),
            "must render ci even when passing: {display}"
        );
        assert!(display.contains("false"), "draft must be false: {display}");
        assert!(
            display.contains("clean"),
            "mergeable must be clean: {display}"
        );
        assert!(display.contains("passing"), "ci must be passing: {display}");
    }

    #[test]
    fn test_gh_pr_view_no_ci_renders_none() {
        let input = load_fixture("gh_pr_view_no_ci.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).expect("must parse");
        let display = result.as_ref();
        assert!(display.contains("ci"), "must render ci item: {display}");
        assert!(
            display.contains("none"),
            "ci must be none when statusCheckRollup is null: {display}"
        );
    }

    #[test]
    fn test_gh_pr_view_draft_summary_prefix() {
        let input = load_fixture("gh_pr_view_draft_conflict.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).expect("must parse");
        // [DRAFT] must appear before the # character in the summary.
        let display = result.as_ref();
        let draft_pos = display.find("[DRAFT]").expect("[DRAFT] must be present");
        let hash_pos = display.find('#').expect("# must be present");
        assert!(draft_pos < hash_pos, "[DRAFT] must precede #: {display}");
    }
}
