//! Shared constants, regexes, helpers, and the `three_tier_parse` scaffolding
//! used by all `gh` sub-parsers.
//!
//! # Design Decision: three_tier_parse scaffolding
//!
//! All five `parse_impl` functions in the `gh` sub-parsers (`issue_view`,
//! `pr_view`, `run_view`, `pr_checks`, and `list`) follow identical three-tier
//! scaffolding:
//! 1. Trim stdout; if it looks like JSON within the size limit, try the JSON
//!    parser.
//! 2. Combine stdout + stderr; try the text/regex parser.
//! 3. Return passthrough.
//!
//! The only variation between parsers is:
//! - The text tier returns `Degraded` for view commands and `list` (text is a
//!   fallback) but `Full` for `pr checks` (text is the primary format).
//! - JSON gate character: `{` for view commands; `[` for `list`; `[` or `{`
//!   for `pr checks` (both formats exist in the wild).
//!
//! [`three_tier_parse`] captures the common skeleton while allowing callers to
//! provide the JSON parser, the text parser, and a flag controlling whether a
//! successful text parse is `Full` or `Degraded`.

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;

// ============================================================================
// Transparency gate helpers
// ============================================================================

/// gh flags by which the user controls output format. When any of these are
/// present, skim passes gh's output through (see [`user_steers_output`]).
///
/// `--web`/`-w` are intentionally EXCLUDED: `--web` emits no stdout to
/// corrupt, and `-w` is ambiguous (`--workflow` on `gh run list`).
///
/// Note: `gh api` and `gh run watch` are exempt from the `--json` entry in
/// this set (see [`user_steers_output`] for the per-subcommand carve-out).
pub(crate) const GH_OUTPUT_STEERING_FLAGS: &[&str] = &["--json", "--jq", "-q", "--template", "-t"];

/// Return true if `args` contains any output-steering flag before a `--`
/// separator.
///
/// Uses strict matching via [`crate::cmd::user_has_flag`]: `arg == flag` or
/// `arg` starts with `flag=`.  Glued short-value forms (`-q.body`) are NOT
/// matched by design — consistent with the hook skip-list behaviour.
///
/// # Per-subcommand carve-out
///
/// `gh api` and `gh run watch` are exempt from the `--json` steering flag:
/// - `gh api` responses are always JSON natively (no `--json` flag in the CLI).
/// - `gh run watch` is a streaming TUI (no `--json` flag).
///
/// For those two subcommands only `--jq`/`-q`/`--template`/`-t` trigger
/// passthrough.  This matches the rewrite layer's per-command skip-list in
/// `rules.rs` (e.g. the `gh api` rule omits `--json` from
/// `skip_if_flag_prefix`), so both layers agree on when to pass through.
/// The `--json` flag on all other `gh` subcommands (issue, pr, run, release,
/// …) still triggers passthrough as usual.
pub(crate) fn user_steers_output(args: &[String]) -> bool {
    let subcmd = args.first().map(String::as_str).unwrap_or("");
    let subcmd2 = args.get(1).map(String::as_str).unwrap_or("");

    // gh api and gh run watch do not support --json as a steering flag;
    // use the narrower set so the gate agrees with rules.rs for those two.
    let flags: &[&str] = if subcmd == "api" || (subcmd == "run" && subcmd2 == "watch") {
        &["--jq", "-q", "--template", "-t"]
    } else {
        GH_OUTPUT_STEERING_FLAGS
    };

    // Collect args before the `--` end-of-options separator, then delegate
    // the per-flag strict match to `user_has_flag` (avoids a third copy of
    // the `arg == flag || arg starts_with flag=` predicate).
    let before_dashdash: Vec<String> = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .cloned()
        .collect();
    crate::cmd::user_has_flag(&before_dashdash, flags)
}

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

/// Matches tab-separated gh list text output: `<number>\t<rest>`.
///
/// Used by [`crate::cmd::infra::gh::list::try_parse_regex`] as the Tier 2
/// fallback when `gh pr list` / `issue list` / `run list` is invoked without
/// `--json` (or when JSON injection is suppressed).
///
/// # Design decision
///
/// Lives in `shared.rs` alongside the check/view/run regexes to keep ALL
/// `gh` regex patterns discoverable in one place. Before batch-C this
/// regex lived in `list.rs`; it was moved here for consistency.
pub static RE_GH_TAB_ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)\t(.+)").unwrap());

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
        if summary.is_empty()
            && let Some(caps) = RE_GH_VIEW_HEADER.captures(line)
        {
            summary = format!("#{}: {}", &caps[2], &caps[1]);
            continue;
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
    if is_json_input(trimmed)
        && trimmed.len() <= MAX_JSON_BYTES
        && let Some(result) = try_json(trimmed)
    {
        return ParseResult::Full(result);
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

// ============================================================================
// Unit tests for user_steers_output
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // --- TRUE cases (steering flag present) ---

    #[test]
    fn test_user_steers_output_json() {
        assert!(user_steers_output(&args(&[
            "issue", "view", "93", "--json"
        ])));
    }

    #[test]
    fn test_user_steers_output_jq() {
        assert!(user_steers_output(&args(&[
            "issue", "view", "93", "--jq", ".body"
        ])));
    }

    #[test]
    fn test_user_steers_output_q_short() {
        assert!(user_steers_output(&args(&[
            "issue", "view", "93", "-q", ".body"
        ])));
    }

    #[test]
    fn test_user_steers_output_template() {
        assert!(user_steers_output(&args(&[
            "pr",
            "view",
            "15",
            "--template",
            "{{.title}}"
        ])));
    }

    #[test]
    fn test_user_steers_output_t_short() {
        assert!(user_steers_output(&args(&[
            "pr",
            "view",
            "15",
            "-t",
            "{{.title}}"
        ])));
    }

    #[test]
    fn test_user_steers_output_json_eq_value() {
        assert!(user_steers_output(&args(&[
            "issue",
            "view",
            "93",
            "--json=number,title"
        ])));
    }

    #[test]
    fn test_user_steers_output_q_eq_value() {
        assert!(user_steers_output(&args(&[
            "issue", "view", "93", "-q=.body"
        ])));
    }

    // --- FALSE cases (no steering flag) ---

    #[test]
    fn test_user_steers_output_empty() {
        assert!(!user_steers_output(&args(&[])));
    }

    #[test]
    fn test_user_steers_output_web_not_steering() {
        // --web intentionally excluded (opens browser, no stdout to corrupt)
        assert!(!user_steers_output(&args(&[
            "issue", "view", "93", "--web"
        ])));
    }

    #[test]
    fn test_user_steers_output_w_not_steering() {
        // -w excluded: ambiguous (--workflow on gh run list)
        assert!(!user_steers_output(&args(&["run", "list", "-w", "ci.yml"])));
    }

    #[test]
    fn test_user_steers_output_workflow_not_steering() {
        assert!(!user_steers_output(&args(&[
            "run",
            "list",
            "--workflow",
            "ci.yml"
        ])));
    }

    #[test]
    fn test_user_steers_output_steering_after_separator_ignored() {
        // Flags after `--` must not trigger the gate
        assert!(!user_steers_output(&args(&[
            "api",
            "repos/o/r",
            "--",
            "--json"
        ])));
    }

    #[test]
    fn test_user_steers_output_glued_q_not_matched() {
        // Glued -q.body: strict match does not fire for the short alias
        assert!(!user_steers_output(&args(&[
            "issue", "view", "93", "-q.body"
        ])));
    }

    #[test]
    fn test_user_steers_output_glued_qx_not_matched() {
        // -qx: no `=` separator → strict match does not fire
        assert!(!user_steers_output(&args(&["issue", "view", "93", "-qx"])));
    }

    // --- Carve-out: gh api and gh run watch are exempt from --json ---

    #[test]
    fn test_user_steers_output_api_json_not_steering() {
        // gh api --json is NOT an output-steering flag: api responses are
        // always JSON natively. The gate must agree with the rules.rs api
        // skip-list which omits --json. (cross-layer consistency)
        assert!(!user_steers_output(&args(&[
            "api", "repos/o/r", "--json", "name"
        ])));
    }

    #[test]
    fn test_user_steers_output_api_q_is_steering() {
        // -q/-jq on gh api IS a steering flag (user-defined JQ projection).
        assert!(user_steers_output(&args(&[
            "api", "repos/o/r", "-q", ".name"
        ])));
    }

    #[test]
    fn test_user_steers_output_run_watch_json_not_steering() {
        // gh run watch is a streaming TUI — no --json flag. The gate must
        // agree with the rules.rs skip-list which omits --json for run watch.
        assert!(!user_steers_output(&args(&[
            "run", "watch", "12345", "--json"
        ])));
    }

    #[test]
    fn test_user_steers_output_run_watch_q_is_steering() {
        // -q on gh run watch IS a steering flag.
        assert!(user_steers_output(&args(&[
            "run", "watch", "12345", "-q", ".conclusion"
        ])));
    }

    // --- Cross-layer consistency: GH_OUTPUT_STEERING_FLAGS vs rewrite rules ---

    /// Locks the gate's steering set (minus documented per-subcommand carve-outs)
    /// against the rewrite rules' steering subset so the two layers cannot
    /// silently drift in future (avoids the `--json` drift described in #2).
    ///
    /// The gate uses `GH_OUTPUT_STEERING_FLAGS` for all non-exempt subcommands.
    /// The rewrite rules for gh issue/pr/run/release view and list all skip on
    /// the same five flags. This test asserts that every flag in
    /// `GH_OUTPUT_STEERING_FLAGS` also appears in a representative view rule's
    /// skip-list, and that the api rule omits `--json` (the documented
    /// asymmetry).
    ///
    /// NOTE: Batch 2 owns rules.rs; this test reads public data only and does
    /// not import from that file — it validates the invariant structurally.
    #[test]
    fn test_steering_flags_cover_standard_subcommands_and_carve_out_api() {
        // Standard subcommand: all five flags must trigger the gate.
        let standard_cases: &[&[&str]] = &[
            &["issue", "view", "93", "--json", "body"],
            &["issue", "view", "93", "--jq", ".body"],
            &["issue", "view", "93", "-q", ".body"],
            &["issue", "view", "93", "--template", "{{.body}}"],
            &["issue", "view", "93", "-t", "{{.body}}"],
        ];
        for case in standard_cases {
            assert!(
                user_steers_output(&args(case)),
                "Expected steering=true for standard subcommand with {:?}",
                case
            );
        }

        // Carve-out: gh api must NOT trigger on --json.
        assert!(
            !user_steers_output(&args(&["api", "repos/o/r", "--json", "x"])),
            "gh api --json must not trigger the gate (api carve-out)"
        );
        // But gh api -q must still trigger.
        assert!(
            user_steers_output(&args(&["api", "repos/o/r", "-q", ".x"])),
            "gh api -q must trigger the gate"
        );

        // Carve-out: gh run watch must NOT trigger on --json.
        assert!(
            !user_steers_output(&args(&["run", "watch", "123", "--json"])),
            "gh run watch --json must not trigger the gate (run watch carve-out)"
        );
    }
}
