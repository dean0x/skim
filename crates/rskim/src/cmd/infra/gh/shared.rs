//! Shared constants, regexes, helpers, and the `three_tier_parse` scaffolding
//! used by all `gh` sub-parsers.
//!
//! # Design Decision: three_tier_parse scaffolding
//!
//! All four `parse_impl` functions in the `gh` sub-parsers follow identical
//! three-tier scaffolding:
//! 1. Trim stdout; if it looks like a JSON object within the size limit, try
//!    the JSON parser.
//! 2. Combine stdout + stderr; try the text/regex parser.
//! 3. Return passthrough.
//!
//! The only variation between parsers is:
//! - The text tier returns `Degraded` for view commands (text is a fallback)
//!   but `Full` for `pr checks` (text is the primary format).
//! - `pr checks` has a slightly wider JSON gate (`[` or `{`) vs. `{` only for
//!   view commands.
//!
//! [`three_tier_parse`] captures the common skeleton while allowing callers to
//! provide the JSON parser, the text parser, and a flag controlling whether a
//! successful text parse is `Full` or `Degraded`.

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;

// ============================================================================
// Shared constants
// ============================================================================

/// Maximum body lines included in issue/PR view output.
///
/// Bodies are truncated to this many lines to prevent excessive context
/// consumption when an issue has a multi-page description.
pub const MAX_BODY_LINES: usize = 10;

/// Maximum number of comments to include in issue/PR view output.
///
/// Only the most recent N comments are shown to surface actionable context.
pub const MAX_COMMENTS: usize = 3;

/// Maximum step details shown per failed job in run view.
pub const MAX_STEP_DETAIL: usize = 5;

/// Maximum items in list output.
pub const MAX_ITEMS: usize = 100;

/// Maximum byte length of JSON input accepted for Tier 1 parsing.
///
/// Inputs larger than this are skipped and fall through to the regex tier,
/// preventing unbounded allocation on pathological or adversarial responses.
pub const MAX_JSON_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

// ============================================================================
// Shared regexes
// ============================================================================

/// Matches tab-separated pr checks output: `name\tstatus\tduration\turl`.
pub static RE_GH_CHECK_TAB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+)\t(pass|fail|pending|skipped|cancelled|neutral)\t([^\t]*)\t(.*)$").unwrap()
});

/// Matches symbol-format pr checks output: `✓  name  duration  url`.
///
/// Newer `gh` versions prefix each line with `✓` (pass), `X` (fail), or
/// `-` (pending/skipped). We match the first non-whitespace token and treat
/// the rest as name.
pub static RE_GH_CHECK_SYMBOL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([✓✗X\-*])\s{1,3}(.+?)\s{2,}(\d+[ms][^\s]*|\d+:\d+)\s+(\S+)\s*$").unwrap()
});

/// Matches gh issue/pr view text header: `<title> #<number>`.
pub static RE_GH_VIEW_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+)\s+#(\d+)$").unwrap());

/// Matches `key:\tvalue` fields in gh issue/pr view text output.
pub static RE_GH_VIEW_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\w[\w ]*?):\s+(.+)$").unwrap());

/// Matches gh run view text header line.
pub static RE_GH_RUN_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+)\s+run\s+#(\d+)").unwrap());

/// Matches job lines in gh run view text output.
///
/// Format: `✓  name    status  duration`
/// Uses `\s{2,}` as a column delimiter to separate name from status word,
/// since names may contain embedded single spaces (e.g., `CI / build`).
pub static RE_GH_RUN_JOB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[✓✗X\-*]\s+(.+?)\s{2,}(\w+)\s+\S+\s*$").unwrap());

// ============================================================================
// Shared helpers
// ============================================================================

/// Inject `--json <fields>` into a command argument list if the user has not
/// already supplied `--json`.
///
/// Called by `prepare_args` in each view sub-parser. Callers that have version
/// compatibility constraints (e.g., `pr_checks`) should not call this and
/// should instead implement their own `prepare_args`.
pub fn inject_json_fields(cmd_args: &mut Vec<String>, fields: &str) {
    if !user_has_flag(cmd_args, &["--json"]) {
        cmd_args.push("--json".to_string());
        cmd_args.push(fields.to_string());
    }
}

/// Truncate a body string to at most `max_lines` lines.
///
/// If the body fits within the limit it is returned as-is.
/// Otherwise the first `max_lines` lines are kept and a suffix of the form
/// `... (M more lines)` is appended.
///
/// Uses an iterator-based approach to avoid materializing all lines before
/// truncating: only the kept lines are collected.
pub fn truncate_body(body: &str, max_lines: usize) -> String {
    let mut lines_iter = body.lines();
    let kept: Vec<&str> = lines_iter.by_ref().take(max_lines).collect();
    let remaining = lines_iter.count();
    if remaining == 0 {
        return body.to_string();
    }
    let mut result = kept.join("\n");
    result.push_str(&format!("\n... ({remaining} more lines)"));
    result
}

/// Parse `gh issue view` / `gh pr view` text output using regex.
///
/// Both commands emit the same human-readable header + key-value format. The
/// caller passes `operation` (`"issue view"` or `"pr view"`) to label the result.
///
/// Returns `None` if the text contains no recognizable header or fields.
pub fn parse_view_text(text: &str, operation: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();
    let mut summary = String::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if summary.is_empty() {
            if let Some(caps) = RE_GH_VIEW_HEADER.captures(line) {
                summary = format!("#{}: {}", &caps[2], &caps[1]);
                continue;
            }
        }
        if let Some(caps) = RE_GH_VIEW_FIELD.captures(line) {
            items.push(InfraItem {
                label: caps[1].to_lowercase(),
                value: caps[2].to_string(),
            });
        }
    }

    if summary.is_empty() && items.is_empty() {
        return None;
    }

    if summary.is_empty() {
        summary = operation.to_string();
    }

    Some(InfraResult::new(
        "gh".to_string(),
        operation.to_string(),
        summary,
        items,
    ))
}

/// Parse a pre-trimmed JSON string into a [`serde_json::Value`] and dispatch to `f`.
///
/// # Design decision
///
/// Exists to remove closure duplication across view sub-parsers. Before this
/// helper, each of `issue_view`, `pr_view`, and `run_view` embedded the same
/// `serde_json::from_str(...).ok().and_then(|obj| try_parse_json(&obj))`
/// plumbing inside its [`three_tier_parse`] JSON closure. Centralizing that
/// two-step dance here keeps the call sites focused on "which parser handles
/// this object" rather than "how do I hand serde_json a slice."
///
/// # Preconditions
///
/// The caller is expected to pass pre-trimmed input. [`three_tier_parse`]
/// already trims before invoking the JSON closure, so callers on that path
/// get this for free.
///
/// Returns `None` if the input is not valid JSON or if `f` returns `None`.
#[allow(dead_code)]
pub fn try_parse_json_object<F>(trimmed: &str, f: F) -> Option<InfraResult>
where
    F: FnOnce(&serde_json::Value) -> Option<InfraResult>,
{
    let obj: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    f(&obj)
}

/// Extract the last `max` comments, stripping quoted-reply (`>`) lines.
///
/// Returns one entry per comment in the format `"@author: first_line..."`.
/// Leading `>` lines (quoted replies in Markdown) are removed before
/// extracting the first meaningful line so that only the new text is shown.
pub fn extract_comments(comments: &[serde_json::Value], max: usize) -> Vec<String> {
    let start = comments.len().saturating_sub(max);
    comments[start..]
        .iter()
        .filter_map(|c| {
            let author = c
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .or_else(|| c.get("login").and_then(|l| l.as_str()))
                .unwrap_or("unknown");
            let body = c.get("body").and_then(|b| b.as_str()).unwrap_or("");
            // Strip quoted lines (starting with `>`)
            let first_line = body
                .lines()
                .find(|l| !l.trim_start().starts_with('>') && !l.trim().is_empty())?;
            // Use char_indices for byte-offset slicing to avoid heap allocation
            // per comment: find the byte index of the 121st char boundary, if any.
            let preview = match first_line.char_indices().nth(120) {
                Some((idx, _)) => &first_line[..idx],
                None => first_line,
            };
            Some(format!("@{author}: {preview}"))
        })
        .collect()
}

// ============================================================================
// Three-tier parse scaffolding
// ============================================================================

/// Execute the standard JSON → text → passthrough three-tier parse flow.
///
/// # Parameters
///
/// - `output` — raw command output (stdout + stderr + exit code)
/// - `try_json` — closure that receives the trimmed stdout and returns
///   `Some(result)` on a successful JSON parse, `None` to fall through.
///   Only called when `is_json_input` returns `true` and the trimmed length
///   is within `MAX_JSON_BYTES`.
/// - `is_json_input` — predicate on the trimmed stdout determining whether
///   to attempt Tier 1 JSON parsing. For view commands this is
///   `|t| t.starts_with('{')`. For `pr checks` it is
///   `|t| t.starts_with('[') || t.starts_with('{')`.
/// - `try_text` — closure that receives the combined stdout+stderr string and
///   returns `Some(result)` on success, `None` to fall through.
/// - `text_is_full` — when `true`, a successful text parse returns
///   [`ParseResult::Full`]; when `false`, it returns
///   [`ParseResult::Degraded`] with `degraded_reason`.
/// - `degraded_reason` — message included in `Degraded` when
///   `text_is_full` is `false`.
pub fn three_tier_parse<FJ, FI, FT>(
    output: &CommandOutput,
    try_json: FJ,
    is_json_input: FI,
    try_text: FT,
    text_is_full: bool,
    degraded_reason: &str,
) -> ParseResult<InfraResult>
where
    FJ: FnOnce(&str) -> Option<InfraResult>,
    FI: Fn(&str) -> bool,
    FT: FnOnce(&str) -> Option<InfraResult>,
{
    let trimmed = output.stdout.trim();

    // Tier 1: JSON
    if is_json_input(trimmed) && trimmed.len() <= MAX_JSON_BYTES {
        if let Some(result) = try_json(trimmed) {
            return ParseResult::Full(result);
        }
    }

    let combined = combine_stdout_stderr(output);

    // Tier 2: text / regex
    if let Some(result) = try_text(&combined) {
        return if text_is_full {
            ParseResult::Full(result)
        } else {
            ParseResult::Degraded(result, vec![degraded_reason.to_string()])
        };
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}
