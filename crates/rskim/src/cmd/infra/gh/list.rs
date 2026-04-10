//! `gh` list command parser (pr list, issue list, run list).
//!
//! Handles `gh pr list`, `gh issue list`, `gh run list` by injecting `--json`
//! fields when the user has not already supplied them, then parsing the JSON
//! array response.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON array (`[...]`) → structured items
//! - **Tier 2 (Degraded)**: Tab-separated text (`#N\t...`) → label/value pairs
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use crate::cmd::user_has_flag;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;
use super::{MAX_ITEMS, MAX_JSON_BYTES, RE_GH_TAB_ROW};

/// Inject `--json` fields for list commands if not already present.
///
/// Only injects for known list subcommands (`pr list`, `issue list`, `run list`).
/// All other commands are left unchanged so that arbitrary `gh` subcommands
/// (e.g., `gh release upload`) are not broken by unexpected flags.
pub(super) fn prepare_args(cmd_args: &mut Vec<String>) {
    if user_has_flag(cmd_args, &["--json"]) {
        return;
    }

    let subcmd = cmd_args.first().map(|s| s.as_str()).unwrap_or("");
    let action = cmd_args.get(1).map(|s| s.as_str()).unwrap_or("");

    match (subcmd, action) {
        ("pr", "list") => {
            cmd_args.push("--json".to_string());
            cmd_args.push("number,title,state,author".to_string());
        }
        ("issue", "list") => {
            cmd_args.push("--json".to_string());
            cmd_args.push("number,title,state,labels".to_string());
        }
        ("run", "list") => {
            cmd_args.push("--json".to_string());
            cmd_args.push("databaseId,displayTitle,status,conclusion".to_string());
        }
        // release list and other commands: no injection
        _ => {}
    }
}

/// Three-tier parse function for gh list output.
///
/// Called by `parse_impl_with_auto_detect` in `gh/mod.rs` as the final text
/// fallback after JSON auto-detection fails. Also exercised by unit tests.
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    if let Some(result) = try_parse_json_list(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["gh: JSON parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON array parsing
// ============================================================================

/// Convert a single JSON entry from a `gh` list response into an [`InfraItem`].
///
/// Handles field name alternatives used by different `gh` subcommands:
/// - Label: `number` (issues/PRs) or `databaseId` (runs)
/// - Title: `title` (issues/PRs) or `displayTitle` (runs)
/// - State: `state` (issues/PRs) or `status` (runs)
///
/// Returns `None` if neither label alternative is present.
fn json_entry_to_infra_item(entry: &serde_json::Value) -> Option<InfraItem> {
    let label = entry
        .get("number")
        .and_then(|v| v.as_u64())
        .or_else(|| entry.get("databaseId").and_then(|v| v.as_u64()))
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| "item".to_string());

    let title = entry
        .get("title")
        .and_then(|v| v.as_str())
        .or_else(|| entry.get("displayTitle").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    let state = entry
        .get("state")
        .and_then(|v| v.as_str())
        .or_else(|| entry.get("status").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_lowercase();

    let value = if state.is_empty() {
        title
    } else {
        format!("{title} ({state})")
    };

    Some(InfraItem { label, value })
}

/// Parse gh JSON array output.
///
/// Returns `None` if the input is not a JSON array, is larger than
/// [`MAX_JSON_BYTES`], or fails to deserialize.
pub(super) fn try_parse_json_list(stdout: &str) -> Option<InfraResult> {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('[') || trimmed.len() > MAX_JSON_BYTES {
        return None;
    }

    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;
    let total = arr.len();
    let truncated = total > MAX_ITEMS;

    let items: Vec<InfraItem> = arr
        .into_iter()
        .take(MAX_ITEMS)
        .filter_map(|entry| json_entry_to_infra_item(&entry))
        .collect();

    let count = items.len();
    let summary = if truncated {
        format!("showing first {MAX_ITEMS} of {total} items")
    } else {
        format!("{count} item{}", if count == 1 { "" } else { "s" })
    };
    Some(InfraResult::new(
        "gh".to_string(),
        "list".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Tier 2: Tab-separated text fallback
// ============================================================================

/// Parse tab-separated gh text output.
///
/// Falls back to regex matching `<number>\t<rest>` lines when JSON is not
/// available. Returns `None` if no such lines are found.
pub(super) fn try_parse_regex(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();

    for line in text.lines() {
        if items.len() >= MAX_ITEMS {
            break;
        }
        if let Some(caps) = RE_GH_TAB_ROW.captures(line) {
            let num = caps[1].to_string();
            let rest = caps[2].trim().to_string();
            items.push(InfraItem {
                label: format!("#{num}"),
                value: rest,
            });
        }
    }

    if items.is_empty() {
        return None;
    }

    let count = items.len();
    let summary = format!("{count} item{}", if count == 1 { "" } else { "s" });
    Some(InfraResult::new(
        "gh".to_string(),
        "list".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{load_fixture, make_output};
    use super::*;

    #[test]
    fn test_tier1_gh_pass() {
        let input = load_fixture("gh_pr_list.json");
        let result = try_parse_json_list(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("INFRA: gh list"));
        assert_eq!(result.items.len(), 3);
    }

    #[test]
    fn test_tier1_gh_fail_non_json() {
        let result = try_parse_json_list("not json");
        assert!(result.is_none());
    }

    #[test]
    fn test_tier2_gh_regex() {
        let input = load_fixture("gh_pr_list_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.items.len(), 3);
        assert!(result.items.iter().any(|i| i.label == "#42"));
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("gh_pr_list.json");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_produces_passthrough() {
        let output = make_output("completely unparseable output\nno json, no regex match");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        // Tier 2 input: tab-separated tabular text output (not JSON) that matches
        // the `^\d+\t.+` regex. This is what `gh pr list` emits without `--json`.
        let output = make_output("42\tFix login bug\tOPEN\n57\tAdd dark mode\tOPEN\n");
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded parse result, got {}",
            result.tier_name()
        );
    }
}
