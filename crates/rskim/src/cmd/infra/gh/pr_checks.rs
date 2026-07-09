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
//!
//! # AD-INFRA-15 (2026-04-11) — URL surfacing for failing checks
//!
//! URLs are included in the item value **only for failing checks**
//! (`fail`/`failure` status). Passing checks do not need a URL because
//! there is nothing to investigate; including URLs for all checks would add
//! noise without benefit. Failing checks benefit most from a direct link
//! to the log output.
//!
//! Format: `"{status} ({duration}) — {url}"` for failing checks with a URL.
//! For checks without a URL (JSON tier), the URL field is omitted.

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::{ParseResult, strip_ansi};
use crate::runner::CommandOutput;

use super::{MAX_JSON_BYTES, RE_GH_CHECK_SYMBOL, RE_GH_CHECK_TAB, three_tier_parse};

/// A single parsed check entry, shared across text and JSON parse paths.
///
/// Named fields prevent silent positional swaps when the tuple had the shape
/// `(String, String, Option<String>, Option<String>)` — particularly important
/// because both `duration` and `url` are `Option<String>`.
struct ParsedCheck {
    name: String,
    status: String,
    duration: Option<String>,
    url: Option<String>,
}

/// Extract a non-empty string from an optional regex capture match.
///
/// Returns `None` when the capture group is absent or its trimmed text is empty,
/// collapsing the two-step `is_some` / `is_empty` guard into a single combinator
/// chain and eliminating 2 nesting levels at each call site.
fn non_empty_capture(m: Option<regex::Match<'_>>) -> Option<String> {
    m.map(|m| m.as_str().trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

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

/// Parse a pre-trimmed `gh pr checks --json` string into an [`InfraResult`].
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
/// Both the `[` (array) and `{` (wrapping-object) gates are retained as
/// defense-in-depth. Some `gh` versions emit `{"checkRuns": [...]}` instead of
/// a bare array, so both shapes must be handled. The gates guard against
/// accidental misuse with non-JSON input even when call sites guarantee a valid
/// prefix, keeping this function self-contained and reusable.
pub(super) fn try_parse_checks_json(trimmed: &str) -> Option<InfraResult> {
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
            .map(|c| {
                let name = c
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let status = c
                    .get("state")
                    .or_else(|| c.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_lowercase();
                let url = c
                    .get("detailsUrl")
                    .or_else(|| c.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                ParsedCheck {
                    name,
                    status,
                    duration: None,
                    url,
                }
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
/// URLs are captured and included in the item value **only for failing checks**
/// to help agents navigate directly to the failing CI log.
pub(super) fn try_parse_checks_text(text: &str) -> Option<InfraResult> {
    let mut parsed: Vec<ParsedCheck> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try tab format: name\tstatus\tduration\turl
        if let Some(caps) = RE_GH_CHECK_TAB.captures(line) {
            parsed.push(ParsedCheck {
                // strip_ansi is safe on the already-split name field: the tab
                // delimiter has already been consumed by the regex, so the name
                // value itself cannot contain a meaningful tab — no PF-006 risk.
                name: strip_ansi(caps[1].trim()),
                status: caps[2].trim().to_lowercase(),
                duration: non_empty_capture(caps.get(3)),
                url: non_empty_capture(caps.get(4)),
            });
            continue;
        }

        // Try symbol format: ✓/X/- name  duration  url
        if let Some(caps) = RE_GH_CHECK_SYMBOL.captures(line) {
            let symbol = caps[1].trim();
            let status = match symbol {
                "✓" => "pass",
                "X" | "✗" => "fail",
                "-" | "*" => "pending",
                _ => "unknown",
            };
            parsed.push(ParsedCheck {
                // strip_ansi is safe on the already-split name field: check
                // names don't contain tabs (tab is the column delimiter).
                name: strip_ansi(caps[2].trim()),
                status: status.to_string(),
                duration: non_empty_capture(caps.get(3)),
                url: non_empty_capture(caps.get(4)),
            });
        }
    }

    if parsed.is_empty() {
        return None;
    }

    build_checks_result(parsed)
}

/// Build a [`InfraResult`] from parsed check entries.
///
/// Computes pass/fail/pending counts for the summary and formats each check as
/// `"name: status (duration)"`. For failing checks, appends `" — {url}"` when
/// a URL is available so agents can navigate directly to the CI log output.
fn build_checks_result(checks: Vec<ParsedCheck>) -> Option<InfraResult> {
    if checks.is_empty() {
        return None;
    }

    let total = checks.len();
    let pass = checks
        .iter()
        .filter(|c| c.status == "pass" || c.status == "success")
        .count();
    let fail = checks
        .iter()
        .filter(|c| c.status == "fail" || c.status == "failure")
        .count();
    let pending = checks
        .iter()
        .filter(|c| c.status == "pending" || c.status == "in_progress")
        .count();
    // Catch-all for cancelled, neutral, skipped, timed_out, and any future states
    let other = total - pass - fail - pending;

    let other_suffix = if other > 0 {
        format!(", {other} other")
    } else {
        String::new()
    };
    let summary = format!(
        "{total} check{}: {pass} pass, {fail} fail, {pending} pending{other_suffix}",
        if total == 1 { "" } else { "s" }
    );

    let items: Vec<InfraItem> = checks
        .into_iter()
        .map(|c| {
            let is_failing = c.status == "fail" || c.status == "failure";
            let value = match (c.duration, c.url.filter(|_| is_failing)) {
                (Some(dur), Some(u)) => format!("{} ({dur}) — {u}", c.status),
                (Some(dur), None) => format!("{} ({dur})", c.status),
                (None, Some(u)) => format!("{} — {u}", c.status),
                (None, None) => c.status,
            };
            InfraItem {
                label: c.name,
                value,
            }
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
    use super::super::load_gh_fixture as load_fixture;
    use super::*;
    use crate::cmd::test_utils::make_output;

    #[test]
    fn test_tier1_tab_text() {
        let input = load_fixture("gh_pr_checks_text.txt");
        let result = try_parse_checks_text(&input);
        assert!(result.is_some(), "Expected tab text parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("gh "), "got: {}", result.as_ref());
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
        assert!(result.as_ref().contains("gh "), "got: {}", result.as_ref());
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
        assert!(
            summary.contains("5 checks"),
            "Expected '5 checks' in: {summary}"
        );
        assert!(summary.contains("fail"), "Expected 'fail' in: {summary}");
        assert!(
            summary.contains("pending"),
            "Expected 'pending' in: {summary}"
        );
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

    #[test]
    fn test_tier1_json_object_wrapper_check_runs() {
        // Some gh versions wrap results in {"checkRuns": [...]} instead of a
        // bare array. Verify try_parse_checks_json handles this shape.
        let json = r#"{"checkRuns": [{"name": "CI", "state": "SUCCESS"}]}"#;
        let result = try_parse_checks_json(json);
        assert!(
            result.is_some(),
            "Expected JSON parse to succeed for checkRuns wrapper, got None"
        );
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("1 check"),
            "got: {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_other_states_appear_in_summary() {
        // cancelled/skipped/neutral states must not be lumped into "pending"
        let checks = vec![
            ParsedCheck {
                name: "CI / build".to_string(),
                status: "pass".to_string(),
                duration: None,
                url: None,
            },
            ParsedCheck {
                name: "CI / cancel".to_string(),
                status: "cancelled".to_string(),
                duration: None,
                url: None,
            },
            ParsedCheck {
                name: "CI / skip".to_string(),
                status: "skipped".to_string(),
                duration: None,
                url: None,
            },
        ];
        let result = build_checks_result(checks).unwrap();
        assert!(
            result.summary.contains("other"),
            "Expected 'other' in summary for cancelled/skipped states, got: {}",
            result.summary
        );
        assert!(
            !result.summary.contains("2 pending"),
            "Should not label cancelled/skipped as pending, got: {}",
            result.summary
        );
    }

    /// Failing checks must include URL in item value; passing checks must not.
    #[test]
    fn test_failing_check_includes_url() {
        let input = load_fixture("gh_pr_checks_text.txt");
        let result = try_parse_checks_text(&input).expect("must parse");
        // The fixture has 1 failing check (CI / lint) with a URL.
        let lint_item = result
            .items
            .iter()
            .find(|i| i.label == "CI / lint")
            .expect("CI / lint must be present");
        assert!(
            lint_item.value.contains("https://"),
            "Failing check must include URL: {}",
            lint_item.value
        );
        // Passing check must NOT include URL.
        let build_item = result
            .items
            .iter()
            .find(|i| i.label == "CI / build")
            .expect("CI / build must be present");
        assert!(
            !build_item.value.contains("https://"),
            "Passing check must not include URL: {}",
            build_item.value
        );
    }

    /// ANSI escape sequences in a tab-format check name must be stripped from
    /// the re-emitted label.  With `skip_ansi_strip:true` on gh's CONFIG, ANSI
    /// bytes reach the parser raw; this test pins the defense-in-depth behaviour
    /// on the already-split `name` field (avoids PF-006 — strip_ansi is safe here
    /// because the tab delimiter has already been consumed by `RE_GH_CHECK_TAB`).
    #[test]
    fn test_ansi_in_check_name_is_stripped() {
        let input = "\x1b[32mCI / build\x1b[0m\tpass\t2m30s\thttps://example.com/build";
        let result = try_parse_checks_text(input);
        assert!(
            result.is_some(),
            "must parse tab-format line with ANSI in name"
        );
        let rendered = result.unwrap().as_ref().to_string();
        assert!(
            rendered.contains("CI / build"),
            "check name must appear without ESC codes; got: {rendered}"
        );
        assert!(
            !rendered.contains('\x1b'),
            "rendered output must not contain ESC bytes; got: {rendered}"
        );
    }

    /// Verifies that adding ANSI defence does not regress the PR's core fix:
    /// a genuine tab-separated line still parses correctly (the tab delimiter
    /// must NOT be destroyed by the per-field strip_ansi call).
    #[test]
    fn test_tab_delimiter_survives_strip_ansi_on_name_field() {
        // Two entries: one with ANSI in the name, one clean.
        // Both need a non-empty URL field: the regex requires 4 tab-separated
        // fields and `.trim()` would drop a trailing bare tab.
        let input = "\x1b[32mCI / lint\x1b[0m\tfail\t1m05s\thttps://example.com/lint\n\
                     CI / build\tpass\t2m30s\thttps://example.com/build";
        let result = try_parse_checks_text(input);
        assert!(result.is_some(), "both entries must parse");
        let result = result.unwrap();
        assert_eq!(
            result.items.len(),
            2,
            "both tab-separated entries must be captured"
        );
        assert_eq!(
            result.items[0].label, "CI / lint",
            "ANSI-laden name stripped correctly"
        );
        assert_eq!(result.items[1].label, "CI / build", "clean name unchanged");
    }

    /// Symbol-format failing checks must also include URL.
    #[test]
    fn test_failing_check_symbol_includes_url() {
        let input = load_fixture("gh_pr_checks_symbol.txt");
        let result = try_parse_checks_text(&input).expect("must parse");
        let lint_item = result
            .items
            .iter()
            .find(|i| i.label == "CI / lint")
            .expect("CI / lint must be present");
        assert!(
            lint_item.value.contains("https://"),
            "Symbol-format failing check must include URL: {}",
            lint_item.value
        );
    }
}
