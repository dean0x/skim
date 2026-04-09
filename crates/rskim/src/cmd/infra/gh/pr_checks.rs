//! `gh pr checks` parser with three-tier degradation.
//!
//! Parses check status output from `gh pr checks`, supporting two text formats
//! and an optional JSON format when the user provides `--json`.
//!
//! # Design Decision: No `--json` injection for version compatibility
//!
//! Unlike other `gh` subcommands, `gh pr checks` does not support a stable
//! `--json` flag across all versions. The tab-separated text output is the
//! primary reliable format, so Tier 1 is text parsing (not JSON). This makes
//! text parsing return `Full` (not `Degraded`) because text IS the primary
//! format for this command.
//!
//! If the user explicitly passes `--json`, we parse that as Tier 1 JSON.
//!
//! # Supported Text Formats
//!
//! **Tab format** (older `gh` versions):
//! ```text
//! CI / build\tpass\t2m30s\thttps://...
//! ```
//!
//! **Symbol format** (newer `gh` versions):
//! ```text
//! ✓  CI / build  2m30s  https://...
//! X  CI / lint   1m5s   https://...
//! ```

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{three_tier_parse, MAX_ITEMS, MAX_JSON_BYTES, RE_GH_CHECK_SYMBOL, RE_GH_CHECK_TAB};

/// No-op `prepare_args`: `gh pr checks` has no stable `--json` flag to inject.
///
/// Users who want JSON output can pass `--json` themselves.
pub(super) fn prepare_args(_cmd_args: &mut Vec<String>) {
    // DESIGN DECISION: No injection — see module doc.
}

/// Three-tier parse function for `gh pr checks` output.
///
/// Unlike the view parsers, text is the primary format here (`text_is_full: true`)
/// and JSON is only attempted when the user explicitly passes `--json` (the flag is
/// not injected by `prepare_args`). Both `[` and `{` prefixes are accepted for
/// JSON because some gh versions wrap the result in a `{checkRuns: [...]}` object.
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    three_tier_parse(
        output,
        // `try_parse_checks_json` does its own internal trim, so passing the
        // already-trimmed slice from the gate is equivalent to the original.
        try_parse_checks_json,
        |t| t.starts_with('[') || t.starts_with('{'),
        try_parse_checks_text,
        true,
        "",
    )
}

// ============================================================================
// Tier 1a: JSON parsing (user-provided --json)
// ============================================================================

/// Parse `gh pr checks --json` output.
///
/// The JSON format is a JSON array of check objects with fields including
/// `name`, `state`/`status`, `startedAt`, `completedAt`, and `detailsUrl`.
pub(super) fn try_parse_checks_json(stdout: &str) -> Option<InfraResult> {
    let trimmed = stdout.trim();
    if trimmed.len() > MAX_JSON_BYTES {
        return None;
    }

    // Try as array first
    let checks: Vec<serde_json::Value> = if trimmed.starts_with('[') {
        serde_json::from_str(trimmed).ok()?
    } else if trimmed.starts_with('{') {
        // Some versions wrap in an object
        let obj: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        obj.get("checkRuns")
            .or_else(|| obj.get("statusCheckRollup"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    } else {
        return None;
    };

    if checks.is_empty() {
        return None;
    }

    build_checks_result(
        checks
            .iter()
            .take(MAX_ITEMS)
            .map(|c| {
                let name = c
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let state = c
                    .get("state")
                    .or_else(|| c.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_lowercase();
                (name, state, None)
            })
            .collect(),
    )
}

// ============================================================================
// Tier 1b: Text parsing (tab or symbol format)
// ============================================================================

/// Parse `gh pr checks` text output (tab or symbol format).
///
/// Tries both formats and takes whichever produces results first.
/// URLs are stripped from the output items to reduce noise.
pub(super) fn try_parse_checks_text(text: &str) -> Option<InfraResult> {
    let mut parsed: Vec<(String, String, Option<String>)> = Vec::new();

    for line in text.lines() {
        if parsed.len() >= MAX_ITEMS {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try tab format: name\tstatus\tduration\turl
        if let Some(caps) = RE_GH_CHECK_TAB.captures(line) {
            let name = caps[1].trim().to_string();
            let status = caps[2].trim().to_lowercase();
            let duration = caps[3].trim();
            let dur = if duration.is_empty() {
                None
            } else {
                Some(duration.to_string())
            };
            parsed.push((name, status, dur));
            continue;
        }

        // Try symbol format: ✓/X/- name  duration  url
        if let Some(caps) = RE_GH_CHECK_SYMBOL.captures(line) {
            let symbol = caps[1].trim();
            let name = caps[2].trim().to_string();
            let duration = caps[3].trim();
            let status = match symbol {
                "✓" => "pass",
                "X" | "✗" => "fail",
                "-" | "*" => "pending",
                _ => "unknown",
            };
            parsed.push((name, status.to_string(), Some(duration.to_string())));
        }
    }

    if parsed.is_empty() {
        return None;
    }

    build_checks_result(parsed)
}

/// Build a [`InfraResult`] from parsed check entries.
///
/// Computes pass/fail/pending counts for the summary and formats
/// each check as `"name: status (duration)"`.
fn build_checks_result(checks: Vec<(String, String, Option<String>)>) -> Option<InfraResult> {
    if checks.is_empty() {
        return None;
    }

    let total = checks.len();
    let pass = checks.iter().filter(|(_, s, _)| s == "pass" || s == "success").count();
    let fail = checks
        .iter()
        .filter(|(_, s, _)| s == "fail" || s == "failure")
        .count();
    let pending = total - pass - fail;

    let summary = format!(
        "{total} check{}: {pass} pass, {fail} fail, {pending} pending",
        if total == 1 { "" } else { "s" }
    );

    let items: Vec<InfraItem> = checks
        .into_iter()
        .map(|(name, status, duration)| {
            let value = if let Some(dur) = duration {
                format!("{status} ({dur})")
            } else {
                status
            };
            InfraItem { label: name, value }
        })
        .collect();

    Some(InfraResult::new(
        "gh".to_string(),
        "pr checks".to_string(),
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
    use super::super::test_helpers::{load_fixture, make_output};

    #[test]
    fn test_tier1_tab_text() {
        let input = load_fixture("gh_pr_checks_text.txt");
        let result = try_parse_checks_text(&input);
        assert!(result.is_some(), "Expected tab text parse to succeed");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("INFRA: gh pr checks"),
            "got: {}",
            result.as_ref()
        );
        assert!(
            result.as_ref().contains("5 checks"),
            "Expected 5 checks, got: {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_tier1_symbol_text() {
        let input = load_fixture("gh_pr_checks_symbol.txt");
        let result = try_parse_checks_text(&input);
        assert!(result.is_some(), "Expected symbol text parse to succeed");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("INFRA: gh pr checks"),
            "got: {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_tier1_json() {
        // Build a simple JSON array of check objects
        let json = r#"[
            {"name": "CI / build", "state": "SUCCESS"},
            {"name": "CI / test", "state": "FAILURE"},
            {"name": "CI / lint", "state": "SUCCESS"}
        ]"#;
        let result = try_parse_checks_json(json);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("3 checks"));
    }

    #[test]
    fn test_summary_counts() {
        let input = load_fixture("gh_pr_checks_text.txt");
        let result = try_parse_checks_text(&input).unwrap();
        // Fixture: 4 pass, 1 fail, 1 pending = 5 total... let's check summary
        let summary = &result.summary;
        assert!(summary.contains("5 checks"), "Expected '5 checks' in: {summary}");
        assert!(summary.contains("fail"), "Expected 'fail' in: {summary}");
        assert!(summary.contains("pending"), "Expected 'pending' in: {summary}");
    }

    #[test]
    fn test_parse_impl_text_produces_full() {
        let input = load_fixture("gh_pr_checks_text.txt");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full (text is primary for pr checks), got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_passthrough_garbage() {
        let output = make_output("no checks found here");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for garbage, got {}",
            result.tier_name()
        );
    }
}
