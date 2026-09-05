//! `gh` list command parser (pr list, issue list, run list).
//!
//! Handles `gh pr list`, `gh issue list`, `gh run list` by injecting `--json`
//! fields when the user has not already supplied them, then parsing the JSON
//! array response.
//!
//! Three tiers (via [`shared::three_tier_parse`](super::shared::three_tier_parse)):
//! - **Tier 1 (Full)**: JSON array gate (`starts_with('[')`) → structured items
//!   via [`try_parse_json_list`]. Text is not the primary format here, so a
//!   successful Tier 1 parse returns [`ParseResult::Full`].
//! - **Tier 2 (Degraded)**: Tab-separated text (`#N\t...`) → label/value pairs
//!   via [`try_parse_regex`]. Returns [`ParseResult::Degraded`] because text is
//!   a fallback, not the primary format.
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation.

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::{MAX_JSON_BYTES, RE_GH_TAB_ROW, three_tier_parse};

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
/// # Design decision
///
/// Adopts `shared::three_tier_parse` for consistency with the view parsers
/// (batch-C). Prior to batch-C this function hand-rolled the three-tier flow;
/// it now delegates to the shared scaffolding, passing:
/// - `try_parse_json_list` as the Tier 1 JSON parser
/// - `starts_with('[')` as the JSON gate (list responses are JSON arrays)
/// - `try_parse_regex` as the Tier 2 text parser
/// - `text_is_full: false` (text regex matches are a fallback, JSON is primary)
/// - `"gh: JSON parse failed, using regex"` as the degraded reason (preserved
///   verbatim from the pre-batch-C string to avoid breaking any consumer that
///   might match on it, though none are currently known).
///
/// Called by `parse_impl_with_auto_detect` in `gh/mod.rs` as the final
/// text fallback after JSON auto-detection fails. Also exercised by unit tests.
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    three_tier_parse(
        output,
        try_parse_json_list,
        |t| t.starts_with('['),
        try_parse_regex,
        false,
        "gh: JSON parse failed, using regex",
    )
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
/// - Conclusion: `conclusion` (run list only; appended to status as `status/conclusion`)
/// - Author: `author.login` (PR list; appended as `@login`)
/// - Labels: `labels[].name` (issue list; appended as `[label1, label2]`)
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

    // E3: read `conclusion` for run list — failed runs showed "(completed)" without it.
    let conclusion = entry
        .get("conclusion")
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty());

    let state_display = match (state.as_str(), conclusion.as_deref()) {
        ("", _) => String::new(),
        (s, Some(c)) => format!("{s}/{c}"),
        (s, None) => s.to_string(),
    };

    // E3: read `author.login` for PR list.
    let author_login = entry
        .get("author")
        .and_then(|v| v.get("login"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    // E3: read `labels[].name` for issue list.
    let label_names: Vec<&str> = entry
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let mut value = title;
    if !state_display.is_empty() {
        value = format!("{value} ({state_display})");
    }
    if let Some(login) = author_login {
        value = format!("{value} @{login}");
    }
    if !label_names.is_empty() {
        value = format!("{value} [{}]", label_names.join(", "));
    }

    Some(InfraItem { label, value })
}

/// Parse a pre-trimmed gh JSON array string into an [`InfraResult`].
///
/// # Preconditions
///
/// Callers are expected to pass pre-trimmed input. Two call paths exist:
/// - [`parse_impl`] delegates to [`three_tier_parse`], which trims stdout
///   before invoking this function.
/// - [`super::parse_impl_with_auto_detect`] passes the pre-computed `trimmed`
///   slice directly (batch-C, see `mod.rs`).
///
/// # Design decision
///
/// Retains the `starts_with('[')` and `MAX_JSON_BYTES` gates as defense-in-depth
/// even though both call paths guarantee a pre-trimmed, `[`-prefixed string by
/// the time this function is reached. The gates prevent accidental misuse if this
/// function is called directly (e.g., from tests or future callers) with untrimmed
/// or non-array input, without requiring callers to know internal preconditions.
///
/// Returns `None` if the input is not a JSON array, is larger than
/// [`MAX_JSON_BYTES`], or fails to deserialize.
pub(super) fn try_parse_json_list(trimmed: &str) -> Option<InfraResult> {
    if !trimmed.starts_with('[') || trimmed.len() > MAX_JSON_BYTES {
        return None;
    }

    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;

    // Every entry is emitted (#317): the user's --limit controls list size,
    // and the input is already bounded by MAX_JSON_BYTES.
    let items: Vec<InfraItem> = arr.iter().filter_map(json_entry_to_infra_item).collect();

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
// Tier 2: Tab-separated text fallback
// ============================================================================

/// Parse tab-separated gh text output.
///
/// Falls back to regex matching `<number>\t<rest>` lines when JSON is not
/// available. Returns `None` if no such lines are found.
pub(super) fn try_parse_regex(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();

    for line in text.lines() {
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
    use super::super::load_gh_fixture as load_fixture;
    use super::*;
    use crate::cmd::test_utils::make_output;

    #[test]
    fn test_tier1_gh_pass() {
        // `try_parse_json_list` requires pre-trimmed input (batch-C contract).
        // `load_fixture` returns the raw file contents which may have a trailing
        // newline, so we trim before calling — matching what `three_tier_parse`
        // and `parse_impl_with_auto_detect` do in production.
        let input = load_fixture("gh_pr_list.json");
        let result = try_parse_json_list(input.trim());
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("gh "));
        assert_eq!(result.items.len(), 3);
    }

    #[test]
    fn test_tier1_gh_fail_non_json() {
        // After batch-C, `try_parse_json_list` takes pre-trimmed input. This test
        // still passes `"not json"` directly (already trimmed) and expects None,
        // which is returned by the internal `starts_with('[')` defense-in-depth
        // gate before serde_json is invoked.
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

    // ========================================================================
    // E3: gh field-drop fixes — conclusion, author, labels
    // ========================================================================

    /// E3: run list must show conclusion alongside status.
    /// Failed runs must render `(completed/failure)` not `(completed)`.
    #[test]
    fn test_run_list_conclusion_shown() {
        let input = load_fixture("gh_run_list.json");
        let result = try_parse_json_list(input.trim()).expect("run list must parse");
        let values: Vec<&str> = result.items.iter().map(|i| i.value.as_str()).collect();

        // Failed run: must include conclusion
        assert!(
            values.iter().any(|v| v.contains("completed/failure")),
            "failed run must show (completed/failure), got: {:?}",
            values
        );
        // Successful run: show conclusion too
        assert!(
            values.iter().any(|v| v.contains("completed/success")),
            "successful run must show (completed/success), got: {:?}",
            values
        );
        // In-progress run: no conclusion (empty string in fixture)
        assert!(
            values
                .iter()
                .any(|v| v.contains("in_progress") && !v.contains('/')),
            "in-progress run has no conclusion, got: {:?}",
            values
        );
    }

    /// E3: PR list must include author login.
    #[test]
    fn test_pr_list_author_shown() {
        let input = load_fixture("gh_pr_list.json");
        let result = try_parse_json_list(input.trim()).expect("pr list must parse");
        let values: Vec<&str> = result.items.iter().map(|i| i.value.as_str()).collect();

        assert!(
            values.iter().any(|v| v.contains("@alice")),
            "PR list must include author login (@alice), got: {:?}",
            values
        );
    }

    /// E3: issue list with labels must include label names.
    #[test]
    fn test_issue_list_labels_shown() {
        let json = r#"[
            {"number": 42, "title": "Login fails on mobile", "state": "OPEN",
             "labels": [{"name": "bug"}, {"name": "mobile"}]}
        ]"#;
        let result = try_parse_json_list(json).expect("issue list must parse");
        let value = &result.items[0].value;
        assert!(
            value.contains("[bug, mobile]"),
            "issue labels must be shown, got: {value}"
        );
    }

    /// E3 static lint: every field injected into `--json` must be read in the parser.
    ///
    /// Prevents the category of bug where a field appears in the `--json` field list
    /// but is never accessed in `json_entry_to_infra_item`, silently dropping data
    /// (e.g., `conclusion` was requested but not read, so a failed run showed
    /// "(completed)" instead of "(completed/failure)").
    ///
    /// This test reads the list.rs source via `include_str!` and asserts that every
    /// field name from the injected `--json` strings appears as `.get("field")` in
    /// the same file. A one-line grep equivalent in test form.
    #[test]
    fn test_json_field_list_no_dropped_fields() {
        let source = include_str!("list.rs");

        // All --json fields injected by prepare_args for the three list commands.
        let all_fields = [
            // pr list
            "number",
            "title",
            "state",
            "author",
            // issue list (number/title/state shared)
            "labels",
            // run list
            "databaseId",
            "displayTitle",
            "status",
            "conclusion",
        ];

        for field in &all_fields {
            let pattern = format!(".get(\"{field}\")");
            assert!(
                source.contains(&pattern),
                "Field '{field}' is injected into --json but never read via {pattern} in list.rs; \
                 this causes silent data loss (E3 / avoids PF-025)"
            );
        }
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
