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

// ============================================================================
// Injected `--json` field lists
// ============================================================================
//
// Named constants rather than inline literals so the field list has exactly
// one definition: `prepare_args` injects it and
// `test_json_field_list_no_dropped_fields` walks it, which makes that lint
// cover a newly added field automatically instead of only the fields someone
// remembered to copy into the test.

/// `--json` fields injected for `gh pr list`.
const PR_LIST_FIELDS: &str = "number,title,state,author";

/// `--json` fields injected for `gh issue list`.
const ISSUE_LIST_FIELDS: &str = "number,title,state,labels";

/// `--json` fields injected for `gh run list`.
///
/// `databaseId`/`displayTitle`/`status`/`conclusion` say *what happened*;
/// `workflowName`/`headBranch`/`event` say *which run this is*, and
/// `startedAt`/`updatedAt` give it a duration.  Without the second group an
/// agent cannot tell one run from another — which is the whole reason to read
/// `gh run list`.
///
/// The set is deliberately NOT trimmed to keep the compressed view under the
/// ADR-001 net-savings guard.  `gh run list`'s own tabular output carries every
/// one of these columns, so if the structured view costs more than raw the
/// guard serving raw is the *correct* outcome and loses nothing.  Dropping
/// fields to win the size comparison would produce a view that is both smaller
/// and less informative than raw — strictly worse than either branch.
const RUN_LIST_FIELDS: &str =
    "databaseId,displayTitle,status,conclusion,workflowName,headBranch,event,startedAt,updatedAt";

/// Inject `--json` fields for list commands if not already present.
///
/// Only injects for known list subcommands (`pr list`, `issue list`, `run list`).
/// All other commands are left unchanged so that arbitrary `gh` subcommands
/// (e.g., `gh release upload`) are not broken by unexpected flags.
///
/// Every arm added here must also be added to `gh::route_rerunnable`, which
/// arms the ADR-001 raw-fallback re-run for exactly the routes that inject
/// (PF-024).
pub(super) fn prepare_args(cmd_args: &mut Vec<String>) {
    if user_has_flag(cmd_args, &["--json"]) {
        return;
    }

    let subcmd = cmd_args.first().map(|s| s.as_str()).unwrap_or("");
    let action = cmd_args.get(1).map(|s| s.as_str()).unwrap_or("");

    let fields = match (subcmd, action) {
        ("pr", "list") => PR_LIST_FIELDS,
        ("issue", "list") => ISSUE_LIST_FIELDS,
        ("run", "list") => RUN_LIST_FIELDS,
        // release list and other commands: no injection
        _ => return,
    };

    cmd_args.push("--json".to_string());
    cmd_args.push(fields.to_string());
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
// Elapsed time from RFC-3339 timestamps (no date dependency)
// ============================================================================
//
// `gh` reports run timing as two RFC-3339 instants and skim has no date crate
// (and is not adding one to a security-audited binary for a subtraction).
// These four functions are the whole of it: a total parser, a civil-date
// conversion, a formatter, and the combinator the renderer calls.
//
// Every one is pure and allocation-free except the two that build the output
// String, and every scan is bounded by a fixed field width or by
// `MAX_FRACTION_DIGITS`.

/// Maximum fractional-second digits accepted in a timestamp.
///
/// Bounds the only variable-length scan in [`rfc3339_to_unix_secs`]. A
/// timestamp with more precision than this is rejected rather than truncated:
/// misreading an instant is worse than declining to read it.
const MAX_FRACTION_DIGITS: usize = 9;

/// Days from 1970-01-01 to the given proleptic-Gregorian civil date.
///
/// Howard Hinnant's `days_from_civil`. It is exact for every date in range and
/// uses only truncating integer division — the `y - 399` adjustment is what
/// makes Rust's toward-zero division agree with the floor division the
/// algorithm is derived for. No allocation, no branching on input length, and
/// no overflow: `rfc3339_to_unix_secs` bounds the year at four digits, so the
/// result cannot leave `i64`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse exactly `len` ASCII digits at `start`.
///
/// Returns `None` if the slice is short or holds a non-digit. The loop is
/// bounded by `len`, which every caller passes as a fixed field width.
fn digits(bytes: &[u8], start: usize, len: usize) -> Option<i64> {
    let end = start.checked_add(len)?;
    let slice = bytes.get(start..end)?;
    let mut acc: i64 = 0;
    for &byte in slice {
        if !byte.is_ascii_digit() {
            return None;
        }
        acc = acc * 10 + i64::from(byte - b'0');
    }
    Some(acc)
}

/// Parse an RFC-3339 timestamp into seconds since the Unix epoch.
///
/// Accepts `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`; fractional seconds are
/// truncated, not rounded.
///
/// # Untrusted input
///
/// These timestamps come off the network from the GitHub API, so the function
/// is total: it returns `None` for anything it does not fully understand and
/// never panics, never unwraps, and never indexes out of bounds — the fixed
/// prefix is guarded by a single length check and every other access goes
/// through `get`. It has no side effects and reads no clock, so it is
/// deterministic and directly unit-testable against malformed input.
///
/// # Lax day-of-month validation, BY DESIGN
///
/// The calendar guard is `1..=12` for the month and `1..=31` for the day in
/// EVERY month, with no per-month table and no February/leap-year case. So
/// `2026-02-31` and `2026-04-31` are accepted and resolve to a real instant a
/// few days past the end of that month rather than being rejected. Totality
/// and boundedness are preserved either way — `days_from_civil` is defined for
/// any `(y, m, d)` triple — so the laxness costs correctness about WHICH DATES
/// EXIST, and nothing else.
///
/// That is safe here and only here. The single consumer renders the result
/// through [`format_elapsed`] as a display-only elapsed label next to a GitHub
/// run, where a value a few days out is a slightly wrong `3d` that no logic
/// branches on. A per-month table would be cost this caller cannot spend.
///
/// **Any reuse outside that display path must add real calendar validation
/// first** — scheduling, expiry, retention windows, ordering against another
/// clock, or anything a user or a branch acts on. Read this function as total
/// and bounded, never as authoritative about the civil calendar.
fn rfc3339_to_unix_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    // Shortest accepted form is the 20-byte `YYYY-MM-DDTHH:MM:SSZ`. This one
    // check makes every fixed-offset index below provably in bounds.
    if b.len() < 20 {
        return None;
    }
    if b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    // RFC 3339 §5.6 permits a lowercase `t`; a space is the widely used
    // alternative separator.
    if !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }

    let year = digits(b, 0, 4)?;
    let month = digits(b, 5, 2)?;
    let day = digits(b, 8, 2)?;
    let hour = digits(b, 11, 2)?;
    let minute = digits(b, 14, 2)?;
    // 60 is a valid leap second.
    let second = digits(b, 17, 2)?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        // Bounded: at most MAX_FRACTION_DIGITS iterations. A longer fraction
        // leaves a digit where the zone designator must be, so the match below
        // rejects the whole timestamp.
        while i < b.len() && b[i].is_ascii_digit() && i - start < MAX_FRACTION_DIGITS {
            i += 1;
        }
        if i == start {
            return None; // `.` with no digits
        }
    }

    let offset_secs = match b.get(i).copied() {
        // `Z` must be the final byte, so trailing junk is rejected.
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) => {
            // Exactly `±HH:MM`, and nothing after it.
            if b.len() != i + 6 || b[i + 3] != b':' {
                return None;
            }
            let offset_hours = digits(b, i + 1, 2)?;
            let offset_minutes = digits(b, i + 4, 2)?;
            if offset_hours > 23 || offset_minutes > 59 {
                return None;
            }
            let magnitude = offset_hours * 3_600 + offset_minutes * 60;
            if sign == b'-' { -magnitude } else { magnitude }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs)
}

/// Render a non-negative second count the way `gh` renders elapsed time.
fn format_elapsed(secs: i64) -> String {
    let hours = secs / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Elapsed wall time between two RFC-3339 instants, as a display string.
///
/// `finished == false` means the run has started but not finished, so the span
/// between `startedAt` and `updatedAt` is a LOWER BOUND on the run's final
/// duration, not the duration. Those values are suffixed with `+` so the
/// reader is never handed a provisional number that looks final.
///
/// Returns `None` — an omitted field, never an invented one — when:
/// - either timestamp is absent or malformed;
/// - either instant predates the epoch, which is how `gh` renders a run that
///   never started (`0001-01-01T00:00:00Z`); or
/// - the end precedes the start (clock skew or bad data).
fn elapsed_label(started: Option<&str>, updated: Option<&str>, finished: bool) -> Option<String> {
    let start = rfc3339_to_unix_secs(started?)?;
    let end = rfc3339_to_unix_secs(updated?)?;
    if start < 0 || end < 0 || end < start {
        return None;
    }
    let mut out = format_elapsed(end - start);
    if !finished {
        out.push('+');
    }
    Some(out)
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
/// - Workflow / branch / event / elapsed: run list only, appended in that
///   fixed order as ` · `-separated trailing segments (mirroring the column
///   order of `gh run list`'s own table). Each is omitted when absent or
///   empty, so `pr list` and `issue list` render exactly as before.
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

    // Run list: the fields that identify WHICH run a row is.  `status` alone
    // cannot distinguish two runs of different workflows on different branches,
    // which is the question `gh run list` is read to answer.
    let workflow = entry
        .get("workflowName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let branch = entry
        .get("headBranch")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let event = entry
        .get("event")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    // A run is finished once it reports `completed` or carries a conclusion;
    // until then `updatedAt` is the last heartbeat, not the end, so the span
    // it yields is a lower bound (rendered with a trailing `+`).
    let finished = state == "completed" || conclusion.is_some();
    let elapsed = elapsed_label(
        entry.get("startedAt").and_then(|v| v.as_str()),
        entry.get("updatedAt").and_then(|v| v.as_str()),
        finished,
    );

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
    // Fixed order, each segment skipped when absent — bounded at four.
    for segment in [workflow, branch, event, elapsed.as_deref()]
        .into_iter()
        .flatten()
    {
        value.push_str(" · ");
        value.push_str(segment);
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

        // Walk the constants `prepare_args` actually injects rather than a
        // hand-copied list, so a field added to one of them is covered here
        // without anyone remembering to update this test.
        for list in [PR_LIST_FIELDS, ISSUE_LIST_FIELDS, RUN_LIST_FIELDS] {
            for field in list.split(',') {
                assert!(
                    !field.is_empty(),
                    "empty entry in injected field list {list:?}"
                );
                let pattern = format!(".get(\"{field}\")");
                assert!(
                    source.contains(&pattern),
                    "Field '{field}' is injected into --json but never read via {pattern} in \
                     list.rs; this causes silent data loss (E3 / avoids PF-025)"
                );
            }
        }
    }

    /// The identifying fields must be present in the injected list, not merely
    /// readable. Deleting one from `RUN_LIST_FIELDS` to shrink the view past
    /// the ADR-001 guard is the failure mode this pins (avoids PF-027).
    #[test]
    fn test_run_list_requests_the_identifying_fields() {
        let fields: Vec<&str> = RUN_LIST_FIELDS.split(',').collect();
        for required in [
            "databaseId",
            "displayTitle",
            "status",
            "conclusion",
            "workflowName",
            "headBranch",
            "event",
            "startedAt",
            "updatedAt",
        ] {
            assert!(
                fields.contains(&required),
                "run list must request '{required}': {RUN_LIST_FIELDS}"
            );
        }
    }

    #[test]
    fn test_prepare_args_injects_run_list_fields() {
        let mut args: Vec<String> = ["run", "list"].iter().map(|s| s.to_string()).collect();
        prepare_args(&mut args);
        assert_eq!(args, vec!["run", "list", "--json", RUN_LIST_FIELDS]);
    }

    #[test]
    fn test_prepare_args_respects_user_supplied_json() {
        let mut args: Vec<String> = ["run", "list", "--json", "databaseId"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let before = args.clone();
        prepare_args(&mut args);
        assert_eq!(args, before, "user --json must not be overridden");
    }

    // ========================================================================
    // Run list: workflow / branch / event / elapsed
    // ========================================================================

    #[test]
    fn test_run_list_surfaces_workflow_branch_event_and_elapsed() {
        let json = r#"[
            {"databaseId": 42, "displayTitle": "Fix login fails on mobile",
             "status": "completed", "conclusion": "success",
             "workflowName": "CI", "headBranch": "main", "event": "push",
             "startedAt": "2026-01-15T14:30:00Z", "updatedAt": "2026-01-15T14:31:20Z"}
        ]"#;
        let result = try_parse_json_list(json).expect("run list must parse");
        let value = &result.items[0].value;

        for needle in ["CI", "main", "push", "1m20s"] {
            assert!(value.contains(needle), "run row dropped {needle}: {value}");
        }
        assert!(
            !value.contains("1m20s+"),
            "a finished run's elapsed time is exact, not a lower bound: {value}"
        );
    }

    /// A run that has started but not finished has an elapsed time that is a
    /// LOWER BOUND, and must be marked as one.
    #[test]
    fn test_run_list_in_progress_elapsed_is_marked_as_a_lower_bound() {
        let json = r#"[
            {"databaseId": 43, "displayTitle": "Deploy to staging",
             "status": "in_progress", "conclusion": "",
             "workflowName": "Deploy", "headBranch": "main",
             "event": "workflow_dispatch",
             "startedAt": "2026-01-15T14:30:00Z", "updatedAt": "2026-01-15T14:30:45Z"}
        ]"#;
        let result = try_parse_json_list(json).expect("run list must parse");
        let value = &result.items[0].value;
        assert!(
            value.contains("45s+"),
            "unfinished run must mark elapsed as a lower bound: {value}"
        );
    }

    /// The new fields are additive: an entry without them renders exactly as
    /// it did before, with no dangling separators.
    #[test]
    fn test_entries_without_run_fields_are_unchanged() {
        let json = r#"[{"number": 42, "title": "Fix login", "state": "OPEN"}]"#;
        let result = try_parse_json_list(json).expect("must parse");
        assert_eq!(result.items[0].value, "Fix login (open)");
    }

    // ========================================================================
    // ADR-001 disjunction: the served view always carries the fields
    // ========================================================================

    /// Either the compressed run-list view is smaller than raw and carries the
    /// identifying fields, or the ADR-001 guard elects raw — which carries
    /// every field by construction. Both outcomes are acceptable; the one that
    /// is not is a compressed view that wins on size *because* it dropped
    /// fields, leaving the agent with less information than either branch.
    ///
    /// The test asserts the invariant that covers both: whatever skim serves,
    /// the workflow, branch, event and elapsed time reach the reader. It calls
    /// the real `fidelity::decide` rather than re-implementing the comparison,
    /// so it tracks the guard if the guard changes.
    #[test]
    fn test_served_run_list_view_always_carries_identifying_fields() {
        use crate::output::fidelity::{FidelityDecision, decide};

        // What `gh run list` prints WITHOUT --json: the user's literal command,
        // which is the ADR-001 baseline for this route (PF-024, and the
        // `route_rerunnable` opt-in in gh/mod.rs). Columns are
        // STATUS, CONCLUSION, TITLE, WORKFLOW, BRANCH, EVENT, ID, ELAPSED, AGE.
        let raw = "completed\tsuccess\tFix login fails on mobile\tCI\tmain\tpush\t42\t1m20s\t2h\n\
                   completed\tfailure\tAdd dark mode toggle\tCI\tfeat/dark\tpull_request\t43\t2m5s\t3h\n\
                   in_progress\t\tDeploy to staging\tDeploy\tmain\tworkflow_dispatch\t44\t45s\t45s\n";

        let json = r#"[
            {"databaseId": 42, "displayTitle": "Fix login fails on mobile",
             "status": "completed", "conclusion": "success",
             "workflowName": "CI", "headBranch": "main", "event": "push",
             "startedAt": "2026-01-15T14:30:00Z", "updatedAt": "2026-01-15T14:31:20Z"},
            {"databaseId": 43, "displayTitle": "Add dark mode toggle",
             "status": "completed", "conclusion": "failure",
             "workflowName": "CI", "headBranch": "feat/dark", "event": "pull_request",
             "startedAt": "2026-01-15T13:00:00Z", "updatedAt": "2026-01-15T13:02:05Z"},
            {"databaseId": 44, "displayTitle": "Deploy to staging",
             "status": "in_progress", "conclusion": "",
             "workflowName": "Deploy", "headBranch": "main", "event": "workflow_dispatch",
             "startedAt": "2026-01-15T14:30:00Z", "updatedAt": "2026-01-15T14:30:45Z"}
        ]"#;

        let compressed = try_parse_json_list(json).expect("run list must parse");
        let view: &str = compressed.as_ref();

        let (branch, served) = match decide(raw, view) {
            FidelityDecision::Keep => ("compressed", view),
            FidelityDecision::Passthrough => ("raw", raw),
        };

        for needle in ["CI", "Deploy", "main", "feat/dark", "push", "1m20s", "45s"] {
            assert!(
                served.contains(needle),
                "the {branch} view skim serves dropped {needle}:\n{served}"
            );
        }
    }

    // ========================================================================
    // RFC-3339 helper
    // ========================================================================

    #[test]
    fn test_rfc3339_parses_well_formed_timestamps() {
        assert_eq!(rfc3339_to_unix_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            rfc3339_to_unix_secs("2026-01-15T14:30:00Z"),
            Some(1_768_487_400)
        );
        // The same instant written three other legal ways.
        let canonical = rfc3339_to_unix_secs("2026-01-15T14:30:00Z");
        assert_eq!(rfc3339_to_unix_secs("2026-01-15T19:30:00+05:00"), canonical);
        assert_eq!(rfc3339_to_unix_secs("2026-01-15T09:30:00-05:00"), canonical);
        assert_eq!(
            rfc3339_to_unix_secs("2026-01-15T14:30:00.123456Z"),
            canonical
        );
    }

    /// Timestamps arrive from a network service, so the parser is total: every
    /// malformed shape returns `None` instead of panicking, unwrapping, or
    /// slicing out of bounds.
    #[test]
    fn test_rfc3339_rejects_malformed_input_without_panicking() {
        for bad in [
            "",                                // empty
            "not-a-timestamp",                 // nothing like a date
            "2026-01-15",                      // date only
            "2026-01-15T14:30",                // no seconds
            "20x6-01-15T14:30:00Z",            // non-digit in year
            "2026-13-15T14:30:00Z",            // month 13
            "2026-01-32T14:30:00Z",            // day 32
            "2026-01-15T25:30:00Z",            // hour 25
            "2026-01-15T14:60:00Z",            // minute 60
            "2026-01-15X14:30:00Z",            // wrong date/time separator
            "2026-01-15T14:30:00",             // no zone designator
            "2026-01-15T14:30:00+05",          // truncated offset
            "2026-01-15T14:30:00+0530",        // offset missing ':'
            "2026-01-15T14:30:00+99:00",       // offset hour out of range
            "2026-01-15T14:30:00.Z",           // '.' with no digits
            "2026-01-15T14:30:00.1234567890Z", // more precision than accepted
            "2026-01-15T14:30:00Z ",           // trailing junk after 'Z'
            "2026-01-15T14:30:00ZZ",           // doubled designator
            // 28 bytes of multi-byte text: long enough to clear the length
            // gate, so the field separators are what reject it.
            "\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}\u{1F600}",
        ] {
            assert!(
                rfc3339_to_unix_secs(bad).is_none(),
                "must reject malformed timestamp {bad:?}"
            );
        }
    }

    #[test]
    fn test_elapsed_label_omits_rather_than_invents() {
        // `gh` renders a never-started run with the zero timestamp; a workflow
        // run cannot predate the epoch.
        assert_eq!(
            elapsed_label(
                Some("0001-01-01T00:00:00Z"),
                Some("2026-01-15T14:30:00Z"),
                true
            ),
            None
        );
        // End before start: clock skew or bad data, not a negative duration.
        assert_eq!(
            elapsed_label(
                Some("2026-01-15T14:31:00Z"),
                Some("2026-01-15T14:30:00Z"),
                true
            ),
            None
        );
        // Either side missing or malformed.
        assert_eq!(
            elapsed_label(None, Some("2026-01-15T14:30:00Z"), true),
            None
        );
        assert_eq!(
            elapsed_label(Some("2026-01-15T14:30:00Z"), None, true),
            None
        );
        assert_eq!(
            elapsed_label(Some(""), Some("2026-01-15T14:30:00Z"), true),
            None
        );
        assert_eq!(
            elapsed_label(Some("garbage"), Some("2026-01-15T14:30:00Z"), true),
            None
        );
    }

    #[test]
    fn test_elapsed_label_formats_and_marks_lower_bounds() {
        let start = Some("2026-01-15T14:30:00Z");
        assert_eq!(
            elapsed_label(start, Some("2026-01-15T14:30:45Z"), true).as_deref(),
            Some("45s")
        );
        assert_eq!(
            elapsed_label(start, Some("2026-01-15T14:31:20Z"), true).as_deref(),
            Some("1m20s")
        );
        assert_eq!(
            elapsed_label(start, Some("2026-01-15T16:32:05Z"), true).as_deref(),
            Some("2h2m5s")
        );
        // Zero-length span is legal and renders without a unit gap.
        assert_eq!(elapsed_label(start, start, true).as_deref(), Some("0s"));
        // Unfinished → lower bound.
        assert_eq!(
            elapsed_label(start, Some("2026-01-15T14:31:20Z"), false).as_deref(),
            Some("1m20s+")
        );
    }

    /// Cross-checks `days_from_civil` at the boundaries the algorithm is most
    /// likely to get wrong: the epoch itself, a leap day, the day after a leap
    /// day, and a century that is not a leap year.
    #[test]
    fn test_days_from_civil_boundaries() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(1971, 1, 1), 365);
        assert_eq!(days_from_civil(1972, 1, 1), 730);
        // 1972 was a leap year: Feb 29 exists and Mar 1 follows it.
        assert_eq!(
            days_from_civil(1972, 3, 1) - days_from_civil(1972, 2, 29),
            1
        );
        // 1900 was NOT a leap year (divisible by 100, not by 400).
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1
        );
        // 2000 WAS a leap year (divisible by 400).
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
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
