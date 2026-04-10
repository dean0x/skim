//! `gh issue view` parser with three-tier degradation.
//!
//! Injects `--json` to get structured issue data and renders a compact summary
//! including title, state, labels, assignees, milestone, body preview, and
//! recent comments.
//!
//! # Design Decision: `--json` field injection
//!
//! We inject a fixed set of fields (`number,title,state,body,labels,assignees,
//! author,milestone,comments`) rather than using the raw default output because:
//! 1. The default text output format is not stable across `gh` versions.
//! 2. JSON gives us structured data for clean truncation and formatting.
//! 3. The user can override by passing `--json` themselves — we check before injecting.
//!
//! # Tier 2 (Degraded)
//!
//! Falls back to regex-based parsing of `gh issue view` text output when JSON
//! is unavailable. Extracts title, state, and visible fields.

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{
    extract_comments, inject_json_fields, parse_view_text, three_tier_parse, truncate_body,
    try_parse_json_object, MAX_BODY_LINES, MAX_COMMENTS,
};

/// JSON fields to inject for `gh issue view`.
const ISSUE_VIEW_FIELDS: &str =
    "number,title,state,body,labels,assignees,author,milestone,comments";

// ============================================================================
// Field extraction helpers
// ============================================================================

/// Push an array field by joining extracted sub-field strings with `", "`.
///
/// Each array element has `sub_key` extracted as a string and the results are
/// joined. If the result is empty, `empty_fallback` is used.
fn push_array_field(
    items: &mut Vec<InfraItem>,
    label: &str,
    obj: &serde_json::Value,
    key: &str,
    sub_key: &str,
    empty_fallback: &str,
) {
    let joined = obj
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|el| el.get(sub_key).and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let value = if joined.is_empty() {
        empty_fallback.to_string()
    } else {
        joined
    };
    items.push(InfraItem {
        label: label.to_string(),
        value,
    });
}

/// Inject `--json` for issue view if not already present.
pub(super) fn prepare_args(cmd_args: &mut Vec<String>) {
    inject_json_fields(cmd_args, ISSUE_VIEW_FIELDS);
}

/// Three-tier parse function for `gh issue view` output.
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    three_tier_parse(
        output,
        |trimmed| try_parse_json_object(trimmed, try_parse_json),
        |t| t.starts_with('{'),
        try_parse_text,
        false,
        "gh issue view: JSON parse failed, using text regex",
    )
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse a `gh issue view --json` object into an [`InfraResult`].
///
/// Accepts a pre-parsed JSON `Value` so this function can also be called
/// from the auto-detect dispatcher without re-parsing.
pub(super) fn try_parse_json(obj: &serde_json::Value) -> Option<InfraResult> {
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

    let summary = format!("#{number}: {title} ({state})");

    let mut items: Vec<InfraItem> = Vec::new();

    // Author — nested object: author.login
    let author = obj
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("unknown");
    items.push(InfraItem {
        label: "author".to_string(),
        value: author.to_string(),
    });

    // Labels — array of {name: string}
    push_array_field(&mut items, "labels", obj, "labels", "name", "(none)");

    // Assignees — array of {login: string}
    push_array_field(&mut items, "assignees", obj, "assignees", "login", "(none)");

    // Milestone — optional nested object: milestone.title
    let milestone = obj
        .get("milestone")
        .and_then(|m| {
            if m.is_null() {
                None
            } else {
                m.get("title").and_then(|t| t.as_str())
            }
        })
        .unwrap_or("(none)");
    items.push(InfraItem {
        label: "milestone".to_string(),
        value: milestone.to_string(),
    });

    // Body — truncated to MAX_BODY_LINES
    let body = obj
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let body_value = if body.is_empty() {
        "(empty)".to_string()
    } else {
        truncate_body(body, MAX_BODY_LINES)
    };
    items.push(InfraItem {
        label: "body".to_string(),
        value: body_value,
    });

    // Comments count + last MAX_COMMENTS previews
    let comments_arr = obj
        .get("comments")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let count = comments_arr.len();
    items.push(InfraItem {
        label: "comments".to_string(),
        value: format!("{count} total"),
    });
    for c in extract_comments(comments_arr, MAX_COMMENTS) {
        items.push(InfraItem {
            label: "comment".to_string(),
            value: c,
        });
    }

    Some(InfraResult::new(
        "gh".to_string(),
        "issue view".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Tier 2: text regex fallback
// ============================================================================

/// Parse `gh issue view` text output using regex.
fn try_parse_text(text: &str) -> Option<InfraResult> {
    parse_view_text(text, "issue view")
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
        let input = load_fixture("gh_issue_view.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("INFRA: gh issue view"),
            "got: {}",
            result.as_ref()
        );
        assert!(result.as_ref().contains("#42"), "got: {}", result.as_ref());
        assert!(
            result.as_ref().contains("Fix login bug"),
            "got: {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_tier1_body_truncation() {
        let input = load_fixture("gh_issue_view.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).unwrap();
        // Body has more than MAX_BODY_LINES lines — should be truncated
        let body_item = result.items.iter().find(|i| i.label == "body").unwrap();
        assert!(
            body_item.value.contains("more lines"),
            "Expected body truncation marker, got: {}",
            body_item.value
        );
    }

    #[test]
    fn test_tier1_comment_limit() {
        let input = load_fixture("gh_issue_view.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).unwrap();
        let comment_items: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.label == "comment")
            .collect();
        // Fixture has 5 comments, MAX_COMMENTS is 3 → should show 3
        assert_eq!(
            comment_items.len(),
            MAX_COMMENTS,
            "Expected {MAX_COMMENTS} comment items, got {}",
            comment_items.len()
        );
    }

    #[test]
    fn test_tier1_minimal() {
        let input = load_fixture("gh_issue_view_minimal.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).unwrap();
        let body_item = result.items.iter().find(|i| i.label == "body").unwrap();
        assert_eq!(body_item.value, "(empty)");
        let labels_item = result.items.iter().find(|i| i.label == "labels").unwrap();
        assert_eq!(labels_item.value, "(none)");
        let assignees_item = result
            .items
            .iter()
            .find(|i| i.label == "assignees")
            .unwrap();
        assert_eq!(assignees_item.value, "(none)");
        let milestone_item = result
            .items
            .iter()
            .find(|i| i.label == "milestone")
            .unwrap();
        assert_eq!(milestone_item.value, "(none)");
    }

    #[test]
    fn test_tier1_user_json_fields_not_overridden() {
        // When user already passed --json, we should not inject again
        let mut args = vec![
            "issue".to_string(),
            "view".to_string(),
            "42".to_string(),
            "--json".to_string(),
            "title,state".to_string(),
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
        let text = "Fix login bug #42\nState: open\nAuthor: alice\nLabels: bug, auth\n";
        let result = try_parse_text(text);
        assert!(result.is_some(), "Expected Tier 2 text parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("gh issue view"));
    }

    #[test]
    fn test_passthrough_garbage() {
        let output = make_output("HTTP 404 Not Found\nNo issue found");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for garbage, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("gh_issue_view.json");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_oversized_json_does_not_parse_as_full() {
        // Input larger than MAX_JSON_BYTES must skip Tier 1 and not return Full
        // from the JSON path. It should fall through to Tier 2 (Degraded) or
        // Tier 3 (Passthrough) — either is acceptable; Full from JSON is not.
        use super::super::MAX_JSON_BYTES;
        // Build a valid-looking JSON object prefix padded to exceed the limit.
        // We use a large string field so serde_json would succeed if the gate
        // weren't there — confirming the gate rejects it before deserialization.
        let padding = "x".repeat(MAX_JSON_BYTES + 1);
        let oversized = format!(r#"{{"number":1,"title":"T","state":"open","_pad":"{padding}"}}"#);
        assert!(oversized.len() > MAX_JSON_BYTES);
        let output = make_output(&oversized);
        let result = parse_impl(&output);
        assert!(
            !result.is_full(),
            "Expected non-Full for oversized JSON input (got Full — MAX_JSON_BYTES gate not applied)"
        );
    }
}
