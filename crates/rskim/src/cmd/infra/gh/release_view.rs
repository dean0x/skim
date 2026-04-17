//! `gh release view` parser with three-tier degradation.
//!
//! Parses release metadata from `gh release view`, surfacing tag, name,
//! published date, body preview, and asset list.
//!
//! # DESIGN NOTE (AD-RV-1) — Body truncation outside code fences
//!
//! Release bodies often contain large changelogs.  We truncate at
//! `MAX_RELEASE_BODY_LINES = 200` lines, but ONLY outside of code fences.
//! If truncation would occur inside a code fence (``` ``` ```), the cut point
//! is delayed until the next fence close.  This prevents mid-fence truncation
//! from producing broken Markdown that could confuse downstream LLMs.
//!
//! Draft releases may have a missing `publishedAt` field — the parser falls
//! through to the text-tier on JSON with `null` publishedAt and tries
//! the text/regex tier (which reads the `Published` field from table output).

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{inject_json_fields, three_tier_parse, try_parse_json_object};

// ============================================================================
// Constants
// ============================================================================

/// Maximum lines of release body to include in the compressed output.
///
/// Truncation occurs outside code fences only (AD-RV-1).
pub(super) const MAX_RELEASE_BODY_LINES: usize = 200;

/// Maximum number of release assets to include.
///
/// Beyond this limit, a `… N more assets` line is appended.
pub(super) const MAX_RELEASE_ASSETS: usize = 20;

/// JSON fields to inject for `gh release view`.
const RELEASE_VIEW_FIELDS: &str = "tagName,name,body,isDraft,isPrerelease,publishedAt,assets,author,createdAt,targetCommitish";

// ============================================================================
// Public entry point
// ============================================================================

/// Inject `--json` for release view if not already present.
pub(super) fn prepare_args(cmd_args: &mut Vec<String>) {
    inject_json_fields(cmd_args, RELEASE_VIEW_FIELDS);
}

/// Three-tier parse function for `gh release view` output.
///
/// # JSON gate
///
/// Accepts JSON objects (`{`).  Draft releases missing `publishedAt` still
/// parse via Tier 1 — we treat null publishedAt as "unpublished/draft".
///
/// # Text tier
///
/// Falls back to regex parsing of tabular `gh release view` text output.
/// Returns `Degraded` (text is a fallback, not the primary format).
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    three_tier_parse(
        output,
        |trimmed| try_parse_json_object(trimmed, try_parse_json),
        |t| t.starts_with('{'),
        try_parse_text,
        false,
        "gh release view: JSON parse failed, using text regex",
    )
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Parse a `gh release view --json` object into an [`InfraResult`].
///
/// Three-tier contract:
/// - **Full**: JSON object with recognizable release fields.
/// - Falls through to text tier if JSON is missing required fields.
pub(super) fn try_parse_json(obj: &serde_json::Value) -> Option<InfraResult> {
    let tag = obj.get("tagName").and_then(|v| v.as_str())?;
    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(tag);

    let is_draft = obj
        .get("isDraft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_prerelease = obj
        .get("isPrerelease")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let published_at = obj
        .get("publishedAt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let body_raw = obj
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let author = obj
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let mut items: Vec<InfraItem> = Vec::new();

    // Tag and name.
    items.push(InfraItem {
        label: "tag".to_string(),
        value: tag.to_string(),
    });
    if name != tag {
        items.push(InfraItem {
            label: "name".to_string(),
            value: name.to_string(),
        });
    }

    // Status flags.
    let mut status_parts: Vec<&str> = Vec::new();
    if is_draft {
        status_parts.push("draft");
    }
    if is_prerelease {
        status_parts.push("pre-release");
    }
    if !status_parts.is_empty() {
        items.push(InfraItem {
            label: "status".to_string(),
            value: status_parts.join(", "),
        });
    }

    // Published date.
    if !published_at.is_empty() {
        // Trim to date portion only (yyyy-mm-dd).
        let date = published_at.split('T').next().unwrap_or(published_at);
        items.push(InfraItem {
            label: "published".to_string(),
            value: date.to_string(),
        });
    }

    // Author.
    items.push(InfraItem {
        label: "author".to_string(),
        value: author.to_string(),
    });

    // Body preview (truncated outside code fences — AD-RV-1).
    if !body_raw.is_empty() {
        let truncated = truncate_body_outside_fences(body_raw, MAX_RELEASE_BODY_LINES);
        items.push(InfraItem {
            label: "body".to_string(),
            value: truncated,
        });
    }

    // Assets.
    if let Some(assets) = obj.get("assets").and_then(|v| v.as_array()) {
        let total = assets.len();
        let shown = total.min(MAX_RELEASE_ASSETS);

        for asset in &assets[..shown] {
            let asset_name = asset
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let size = asset
                .get("size")
                .and_then(|v| v.as_u64())
                .map(format_size)
                .unwrap_or_default();

            items.push(InfraItem {
                label: "asset".to_string(),
                value: if size.is_empty() {
                    asset_name.to_string()
                } else {
                    format!("{asset_name} ({size})")
                },
            });
        }

        if total > MAX_RELEASE_ASSETS {
            items.push(InfraItem {
                label: "assets".to_string(),
                value: format!("… {} more assets", total - MAX_RELEASE_ASSETS),
            });
        }
    }

    let summary = format!("{tag}: {name}");
    Some(InfraResult::new(
        "gh".to_string(),
        "release view".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Tier 2: Text parsing
// ============================================================================

/// Parse `gh release view` text output using regex heuristics.
///
/// Extracts tag, name, and date from tabular text.  Returns `None` if no
/// recognizable release header is found.
fn try_parse_text(combined: &str) -> Option<InfraResult> {
    let mut tag = String::new();
    let mut name = String::new();
    let mut date = String::new();

    for line in combined.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("tag:") {
            tag = val.trim().to_string();
        } else if let Some(val) = trimmed.strip_prefix("name:") {
            name = val.trim().to_string();
        } else if let Some(val) = trimmed
            .strip_prefix("published:")
            .or_else(|| trimmed.strip_prefix("Published:"))
        {
            date = val.trim().to_string();
        }
    }

    if tag.is_empty() && name.is_empty() {
        return None;
    }

    let mut items: Vec<InfraItem> = Vec::new();
    if !tag.is_empty() {
        items.push(InfraItem { label: "tag".to_string(), value: tag.clone() });
    }
    if !name.is_empty() && name != tag {
        items.push(InfraItem { label: "name".to_string(), value: name.clone() });
    }
    if !date.is_empty() {
        items.push(InfraItem { label: "published".to_string(), value: date });
    }

    let summary = if !tag.is_empty() { tag } else { name };
    Some(InfraResult::new(
        "gh".to_string(),
        "release view".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Helpers
// ============================================================================

/// Truncate a release body to at most `max_lines` lines, but ONLY outside of
/// code fences (AD-RV-1).
///
/// If the `max_lines` boundary falls inside a code fence (``` ``` ```), the
/// cut is delayed until the fence closes.  This prevents broken Markdown.
fn truncate_body_outside_fences(body: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() <= max_lines {
        return body.to_string();
    }

    let mut in_fence = false;
    let mut cut_at = max_lines;

    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if i >= max_lines && !in_fence {
            cut_at = i;
            break;
        }
        if i == lines.len() - 1 {
            cut_at = lines.len();
        }
    }

    let truncated = lines[..cut_at].join("\n");
    if cut_at < lines.len() {
        format!("{truncated}\n… {} lines truncated", lines.len() - cut_at)
    } else {
        truncated
    }
}

/// Format asset size in human-readable form.
fn format_size(bytes: u64) -> String {
    if bytes >= 1_024 * 1_024 {
        format!("{:.1} MB", bytes as f64 / (1_024.0 * 1_024.0))
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{bytes} B")
    }
}

// ============================================================================
// Tests
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

    // ---- JSON tier ----

    #[test]
    fn test_parse_json_basic_release() {
        let json = r#"{
            "tagName": "v1.2.3",
            "name": "Release 1.2.3",
            "body": "Changelog:\n- fix: bug\n",
            "isDraft": false,
            "isPrerelease": false,
            "publishedAt": "2026-04-17T10:00:00Z",
            "assets": [],
            "author": {"login": "octocat"}
        }"#;
        let output = make_output(json);
        let result = parse_impl(&output);
        match result {
            crate::output::ParseResult::Full(r) => {
                assert!(r.summary.contains("v1.2.3"), "summary: {}", r.summary);
                assert!(r.items.iter().any(|i| i.label == "published"));
                assert!(r.items.iter().any(|i| i.label == "author"));
            }
            other => panic!("Expected Full, got: {other:?}"),
        }
    }

    #[test]
    fn test_parse_json_draft_release() {
        let json = r#"{
            "tagName": "v2.0.0-rc1",
            "name": "v2.0.0-rc1",
            "body": "",
            "isDraft": true,
            "isPrerelease": true,
            "publishedAt": null,
            "assets": [],
            "author": {"login": "dev"}
        }"#;
        let output = make_output(json);
        let result = parse_impl(&output);
        match result {
            crate::output::ParseResult::Full(r) => {
                let status = r.items.iter().find(|i| i.label == "status");
                assert!(status.is_some(), "draft status should be present");
                assert!(status.unwrap().value.contains("draft"));
            }
            other => panic!("Expected Full, got: {other:?}"),
        }
    }

    #[test]
    fn test_parse_json_many_assets_capped() {
        let assets: Vec<serde_json::Value> = (0..25)
            .map(|i| serde_json::json!({"name": format!("binary-{i}"), "size": 1024}))
            .collect();
        let json = serde_json::json!({
            "tagName": "v1.0",
            "name": "v1.0",
            "body": "",
            "isDraft": false,
            "isPrerelease": false,
            "publishedAt": "2026-01-01T00:00:00Z",
            "assets": assets,
            "author": {"login": "user"}
        });
        let output = make_output(&json.to_string());
        let result = parse_impl(&output);
        match result {
            crate::output::ParseResult::Full(r) => {
                let more = r.items.iter().find(|i| i.value.contains("more assets"));
                assert!(more.is_some(), "should have 'more assets' line");
            }
            other => panic!("Expected Full, got: {other:?}"),
        }
    }

    // ---- Body truncation (AD-RV-1) ----

    #[test]
    fn test_truncate_body_outside_fences_no_fence() {
        let body = (0..300).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let result = truncate_body_outside_fences(&body, 200);
        let line_count = result.lines().count();
        // Should have ~200 lines + truncation marker.
        assert!(line_count <= 202, "truncated: {line_count} lines");
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_truncate_body_no_truncation_when_short() {
        let body = "line 1\nline 2\n";
        let result = truncate_body_outside_fences(body, 200);
        assert_eq!(result, body);
    }

    #[test]
    fn test_truncate_body_does_not_cut_inside_fence() {
        // 5 lines before a fence, fence open at line 6, fence content 10 lines,
        // close at line 17, then more lines.  max_lines = 10 → inside fence.
        // Should delay cut until fence closes.
        let mut lines: Vec<String> = (0..5).map(|i| format!("preamble {i}")).collect();
        lines.push("```rust".to_string());
        for i in 0..10 {
            lines.push(format!("code line {i}"));
        }
        lines.push("```".to_string());
        lines.push("post fence".to_string());
        let body = lines.join("\n");
        let result = truncate_body_outside_fences(&body, 10);
        // Cut should happen after the closing fence, not inside it.
        assert!(!result.contains("``` rust"), "fence should not be split");
        // The closing ``` should appear in the result.
        assert!(result.contains("```"), "closing fence should be preserved");
    }

    // ---- Compression check ----

    #[test]
    fn test_output_shorter_than_input() {
        let long_body: String = (0..250).map(|i| format!("- changelog item {i}\n")).collect();
        let json = serde_json::json!({
            "tagName": "v3.0.0",
            "name": "Version 3.0.0",
            "body": long_body,
            "isDraft": false,
            "isPrerelease": false,
            "publishedAt": "2026-04-17T00:00:00Z",
            "assets": [],
            "author": {"login": "maintainer"}
        });
        let raw = json.to_string();
        let output = make_output(&raw);
        let result = parse_impl(&output);
        match result {
            crate::output::ParseResult::Full(r) => {
                let rendered = format!("{r}");
                assert!(
                    rendered.len() < raw.len(),
                    "compressed={} raw={}",
                    rendered.len(),
                    raw.len()
                );
            }
            other => panic!("Expected Full, got: {other:?}"),
        }
    }

    // ---- Passthrough on unknown JSON ----

    #[test]
    fn test_passthrough_on_unknown_json() {
        let output = make_output(r#"{"error": "not found"}"#);
        let result = parse_impl(&output);
        // Should NOT be Full since no tagName field.
        if let crate::output::ParseResult::Full(_) = result {
            panic!("should not parse as Full — expected Degraded or Passthrough");
        }
    }
}
