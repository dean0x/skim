//! Cargo build/check/clippy/fmt output compression (#51)
//!
//! Handlers for four `cargo` subcommands:
//!
//! - **`cargo build` / `cargo check` / `cargo clippy`:** Three-tier NDJSON parser.
//!   - **Tier 1 (JSON):** Parse `--message-format=json` NDJSON from stdout.
//!     Track warnings/errors from `compiler-message` events, detect success
//!     from `build-finished` event.
//!   - **Tier 2 (regex):** Fall back to regex matching on stderr for
//!     `error[E\d+]` patterns when JSON parsing is unavailable.
//!   - **Tier 3 (passthrough):** Return raw output when nothing can be parsed.
//!
//! - **`cargo fmt`:** Passthrough-or-success parser. Empty combined output
//!   signals success; any non-empty output is passed through unchanged.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use super::run_parsed_command;
use crate::cmd::{combine_output, inject_flag_before_separator, user_has_flag};
use crate::output::ParseResult;
use crate::output::canonical::{BuildResult, WarningChannel};
use crate::runner::CommandOutput;

// ============================================================================
// Compiled regex patterns (compiled once via LazyLock)
// ============================================================================

static CARGO_ERROR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"error\[E\d+\]").expect("valid regex"));

static CARGO_WARNING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^warning:").expect("valid regex"));

static CARGO_ERROR_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(error\[E\d+\]:.+)").expect("valid regex"));

// ============================================================================
// Public entry points
// ============================================================================

/// Run `cargo build` with output compression.
///
/// Injects `--message-format=json` if not already set by the user, then
/// parses the NDJSON output through the three-tier parser.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_with_json_format("build", args, show_stats, rec)
}

/// Run `cargo check` with output compression.
///
/// Injects `--message-format=json` if not already set by the user, then
/// parses the NDJSON output through the same three-tier parser as cargo build.
/// `cargo check` verifies types and borrow rules without producing an artifact,
/// so its JSON schema is identical to `cargo build`'s.
pub(crate) fn run_check(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_with_json_format("check", args, show_stats, rec)
}

/// Run `cargo fmt` with output compression.
///
/// `cargo fmt` reformats source in-place and emits output only on error.
/// An empty combined output is treated as success. Non-empty output (e.g.
/// diff output from `--check` mode falling through, or rustfmt errors)
/// is passed through unchanged.
///
/// Note: `cargo fmt --check` is ACKed at the engine level (AD-RW-11) and
/// never reaches this handler. This handler covers bare `cargo fmt` and
/// `cargo fmt -- [rustfmt args]` (apply mode).
///
/// # Module placement
///
/// `cargo fmt` is categorized as a LINT operation by the rewrite engine
/// (alongside `biome`, `eslint`, `rustfmt`, etc.), but its handler lives here
/// in the build module rather than in `cmd/lint/`. This is intentional:
/// `cargo fmt` shares the `cargo` executable, the `run_parsed_command` helper,
/// and all cargo-specific plumbing (argument injection, env vars, install hints)
/// with `cargo build`, `cargo check`, and `cargo clippy`. Splitting it into
/// the lint module would require duplicating or re-exporting that infrastructure.
/// The rewrite engine's categorization and the handler's module location are
/// therefore deliberately decoupled.
pub(crate) fn run_fmt(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let mut full_args = vec!["fmt".to_string()];
    full_args.extend_from_slice(args);

    run_parsed_command(
        "cargo",
        &full_args,
        &[("CARGO_TERM_COLOR", "never")],
        "install Rust from https://rustup.rs",
        show_stats,
        rec,
        parse_fmt,
    )
}

/// Run `cargo clippy` with output compression.
///
/// Same JSON injection and parsing as cargo build, but with clippy-specific
/// grouping of warnings by lint rule code.
pub(crate) fn run_clippy(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_with_json_format("clippy", args, show_stats, rec)
}

/// Shared implementation for `run`, `run_check`, and `run_clippy`.
///
/// All three subcommands inject `--message-format=json` and use the same
/// three-tier NDJSON parser. Only the subcommand token differs.
fn run_with_json_format(
    subcmd: &str,
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let mut full_args = vec![subcmd.to_string()];
    full_args.extend_from_slice(args);

    if !user_has_flag(&full_args, &["--message-format"]) {
        inject_flag_before_separator(&mut full_args, "--message-format=json");
    }

    run_parsed_command(
        "cargo",
        &full_args,
        &[("CARGO_TERM_COLOR", "never")],
        "install Rust from https://rustup.rs",
        show_stats,
        rec,
        parse,
    )
}

// ============================================================================
// Parsers
// ============================================================================

/// Parse `cargo fmt` output.
///
/// `cargo fmt` writes to combined stdout+stderr only when it encounters
/// errors (e.g. `rustfmt` not installed, unformatted files in `--check` mode
/// that bypass the ACK path).
///
/// When combined output is empty, the exit code determines success:
/// `exit_code == Some(0)` → success; any other code (non-zero or signal-killed
/// via `None`) → failure. This prevents a signal-killed or panicking `cargo fmt`
/// from being reported as success just because it produced no output.
///
/// Any non-empty output is passed through unchanged.
fn parse_fmt(output: &CommandOutput) -> ParseResult<BuildResult> {
    let combined = combine_output(output);
    let trimmed = combined.trim();
    if trimmed.is_empty() {
        let success = output.exit_code == Some(0);
        ParseResult::Full(BuildResult::new(success, 0, 0, None, vec![]))
    } else {
        ParseResult::Passthrough(trimmed.to_string())
    }
}

/// Parse cargo build/clippy output through three degradation tiers.
fn parse(output: &CommandOutput) -> ParseResult<BuildResult> {
    // Tier 1: JSON parse of stdout NDJSON
    if let Some(result) = try_tier1_json(&output.stdout) {
        return result;
    }

    // Tier 2: Regex on stderr
    if let Some(result) = try_tier2_regex(&output.stderr) {
        return result;
    }

    // Tier 3: Passthrough
    let combined = if output.stderr.is_empty() {
        output.stdout.clone()
    } else if output.stdout.is_empty() {
        output.stderr.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };

    ParseResult::Passthrough(combined)
}

/// Select the span rustc marked as the diagnostic's own error site.
///
/// A rustc diagnostic carries one or more spans and marks exactly one of them
/// `"is_primary": true` — the location its rendered header points at
/// (`--> file:line:col`). **The array is not ordered primary-first.** Measured
/// on rustc 1.96.0 against real `--message-format=json` output:
///
/// ```text
/// E0499 "cannot borrow `s` as mutable more than once at a time"
///   spans[0] is_primary=false src/main.rs:4  "first mutable borrow occurs here"
///   spans[1] is_primary=true  src/main.rs:5  "second mutable borrow occurs here"
///   spans[2] is_primary=false src/main.rs:6  "first borrow later used here"
/// ```
///
/// Taking `spans.first()` there reports line 4 — the *other* borrow — while
/// rustc's own header says `src/main.rs:5`. E0382 (`borrow of moved value`)
/// has the same shape, pointing at the move instead of the use-after-move.
///
/// Fallback: a diagnostic with no primary span is possible in principle — the
/// JSON schema does not forbid it — and reporting *some* location beats
/// reporting none, so an all-secondary list falls back to the first element.
/// An empty list yields `None` and the caller renders the message locationless.
fn primary_span(spans: &[serde_json::Value]) -> Option<&serde_json::Value> {
    spans
        .iter()
        .find(|span| {
            span.get("is_primary")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .or_else(|| spans.first())
}

/// Render a span as `file:line`, substituting placeholders for absent keys.
fn span_location(span: &serde_json::Value) -> String {
    let file = span
        .get("file_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let line = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
    format!("{file}:{line}")
}

/// Render one compiler diagnostic into its single-line form.
///
/// This is the ONLY spelling of that rendering: the error path and the warning
/// path both call it, so the two cannot drift. Two hand-maintained spellings of
/// one rendering is the defect class that produced this repo's earlier
/// double-header bugs — the `level` token is a parameter precisely so a second
/// copy is never needed.
///
/// Shapes (`level` is rustc's own token — `error` or `warning`):
///
/// ```text
/// error[E0308]: mismatched types in src/main.rs:42
/// error[E0308]: mismatched types
/// error: internal compiler error in src/main.rs:42
/// error: internal compiler error
/// ```
fn format_diagnostic(level: &str, code: &str, msg_text: &str, location: &str) -> String {
    match (code.is_empty(), location.is_empty()) {
        (false, false) => format!("{level}[{code}]: {msg_text} in {location}"),
        (false, true) => format!("{level}[{code}]: {msg_text}"),
        (true, false) => format!("{level}: {msg_text} in {location}"),
        (true, true) => format!("{level}: {msg_text}"),
    }
}

/// Maximum number of warnings rendered as individual diagnostics.
///
/// Above this the build's warnings are rendered as the by-lint-code roll-up
/// instead — never both (see [`summarise_warnings`]).
///
/// 50 is not a new number. It is the crate's existing bound for how many
/// warning items a build-family parser enumerates before it stops:
/// `cmd::infra::docker::build::MAX_WARNINGS`, which caps a docker build's
/// `WARNING:` lines at the same figure for the same reason (`pip`/`npm`/`apt`
/// can emit hundreds). Reusing it keeps one answer to one question rather than
/// two build handlers disagreeing about when a warning list stops being a list
/// a reader reads and becomes a wall they skim for the distribution.
///
/// # The FIGURE is shared with docker's `MAX_WARNINGS`; the SEMANTICS are not
///
/// Borrow the number, never the disclosure behaviour. Above their common bound
/// the two parsers do opposite things to the reader, and each owes a different
/// obligation:
///
/// - **This bound AGGREGATES.** `warning_messages` becomes the by-lint-code
///   roll-up, whose buckets sum to the build's total warning count by
///   construction (see [`UNCODED_WARNING_KEY`]). No warning leaves the
///   accounting, so nothing is elided in the #317 sense and no
///   `output::elision_marker` is owed. What the roll-up *does* drop is the
///   per-warning message text and `file:line` location, which is why
///   [`summarise_warnings`] reports the switch back to its caller — the
///   ADR-011 class-1 marker has to name that, or it describes a smaller loss
///   than the one it fired for.
/// - **`docker::build::MAX_WARNINGS` TRUNCATES.** Its collector is
///   `if warnings.len() < MAX_WARNINGS { warnings.push(msg) }` — the 51st line
///   onward is dropped on the floor with no count kept and nothing in the
///   output saying it existed. That is a silent #317 loss and owes an
///   unconditional elision marker with exact counts.
///
/// Aggregation with complete counts and truncation are not the same act. A
/// third parser borrowing "50" has to decide which of the two it is building
/// before it copies either one's disclosure behaviour.
const WARNING_DETAIL_MAX: usize = 50;

/// Bucket label for warnings rustc emitted without a lint code.
///
/// The roll-up's counts MUST sum to the build's total warning count, or the
/// wholesale switch in [`summarise_warnings`] would drop warnings the reader is
/// never told about and would owe an ADR-011 class-1 elision marker. A codeless
/// warning has no lint key to group under, so it gets an explicit bucket rather
/// than silently vanishing from the totals.
const UNCODED_WARNING_KEY: &str = "(no lint code)";

/// What [`summarise_warnings`] chose, and the one fact its caller cannot
/// re-derive afterwards.
struct WarningSummary {
    /// The ONE representation served — per-warning detail or the roll-up —
    /// wrapped in the variant that says which. Never both, never a mix.
    channel: WarningChannel,
    /// Warnings the parser saw, captured BEFORE any roll-up replaced their
    /// detail.
    ///
    /// Not recoverable from `channel` afterwards: a roll-up's bucket count is
    /// the number of distinct lint codes, not the number of warnings, so
    /// `messages.len()` would under-report the moment the switch fires. This is
    /// the figure the `warnings: N` header and the marker both quote.
    total: usize,
}

/// Choose the ONE representation of this build's warnings.
///
/// Returns either the per-warning diagnostics or the by-lint-code roll-up.
/// Never both, never a mix: one return value, one branch, so "rendered twice"
/// is not a state this function can produce. That is the whole point. Before it
/// existed the roll-up was pushed into `error_messages` while the per-warning
/// detail went to `warning_messages`, and the two were gated independently — so
/// a failing clippy run printed every warning twice, in two spellings.
///
/// # Why a wholesale switch and not a truncated list
///
/// Truncating the detailed list at the bound would drop warnings the reader is
/// never told about — a #317 violation unless it carries an ADR-011 class-1
/// marker with exact counts. The roll-up drops no warning: each one is counted
/// in exactly one bucket, the buckets sum to the total (see
/// [`UNCODED_WARNING_KEY`]), and the `warnings: N` header states that total
/// independently. Aggregation with complete counts is not elision, so this path
/// owes no *elision* marker.
///
/// It is also the more useful form at scale: a 200-warning run tells a reader
/// its distribution in a handful of lines and its locations in none, which is
/// what a reader at that volume is actually asking.
///
/// # What the switch DOES owe: [`WarningChannel::RollUp`]
///
/// "Its locations in none" is the part a reader has to be told. The counts
/// survive the roll-up; the per-warning message text and `file:line` do not,
/// and those are the actionable half. A served line reading
/// `warning[dead_code]: unused variable: v17 in src/lib.rs:17` becomes
/// `dead_code: 51 occurrence(s)`.
///
/// The class-1 marker that fires on the same path
/// (`output::diagnostics_summary_marker`, from `cmd::build::run_parsed_command`'s
/// `Keep` arm) is what tells the reader. Its base clause names cargo's *fixed*
/// discard set — source snippets, help/note lines, explain hints — and above
/// this bound it appends the roll-up in as many words: `; N warnings rolled up
/// to lint-code counts, per-warning messages and locations dropped`.
///
/// It needed a `rolled_up_warnings` argument to say that. Without one it named
/// the fixed set alone: the reader was told snippets went, and nothing told
/// them the warning text and locations went with them. ADR-011's 2026-09-24
/// amendment ranks that failure *below* saying nothing — a marker naming the
/// wrong class sends the reader looking for content they were not served —
/// which is why the bound discloses itself rather than relying on the fixed
/// clause to cover it.
///
/// So the CHOICE is returned, not just the result: the roll-up arrives wrapped
/// in [`WarningChannel::RollUp`], which sets `BuildResult::warnings_rolled_up`
/// and lets the marker name what this bound actually dropped.
fn summarise_warnings(
    detailed: Vec<String>,
    warning_codes: &BTreeMap<String, usize>,
) -> WarningSummary {
    let total = detailed.len();
    if total <= WARNING_DETAIL_MAX {
        return WarningSummary {
            channel: WarningChannel::Detail(detailed),
            total,
        };
    }
    WarningSummary {
        channel: WarningChannel::RollUp(roll_up_warning_codes(warning_codes, total)),
        total,
    }
}

/// By-lint-code roll-up whose counts sum to `total` by construction.
fn roll_up_warning_codes(warning_codes: &BTreeMap<String, usize>, total: usize) -> Vec<String> {
    let coded: usize = warning_codes.values().sum();
    let mut rolled: Vec<String> = warning_codes
        .iter()
        .map(|(code, count)| format!("{code}: {count} occurrence(s)"))
        .collect();
    let uncoded = total.saturating_sub(coded);
    if uncoded > 0 {
        rolled.push(format!("{UNCODED_WARNING_KEY}: {uncoded} occurrence(s)"));
    }
    rolled
}

/// Extract counts, formatted messages, and warning codes from a single
/// `{"reason":"compiler-message",...}` JSON object, accumulating results into
/// the caller's mutable accumulators.
///
/// Returns `false` when the `message` key is absent (malformed event), so the
/// caller can skip the line without allocating any per-message heap objects.
///
/// Accepts `&mut` references to the caller's pre-allocated accumulators instead
/// of returning a freshly heap-allocated struct per message. For large builds
/// with hundreds of `compiler-message` events this eliminates all per-iteration
/// allocation for `Vec<String>` and `BTreeMap<String, usize>`.
///
/// # There are no separate counters, by design
///
/// Every `error` level pushes exactly one line to `error_messages` and every
/// `warning` level exactly one to `warning_messages`, from the same match arm,
/// so the vectors' lengths ARE the counts and the caller reads `.len()`.
///
/// Do not add `errors: &mut usize` / `warnings: &mut usize` parameters back.
/// A counter incremented beside each push is a second source for a fact the
/// vectors already carry, and the only thing that can hold the two in step is
/// an assertion — which, as a `debug_assert!`, is compiled out of exactly the
/// release builds users run. Deriving the counts keeps the invariant true by
/// construction instead, with nothing to defend it.
fn process_compiler_message(
    json: &serde_json::Value,
    error_messages: &mut Vec<String>,
    warning_messages: &mut Vec<String>,
    warning_codes: &mut BTreeMap<String, usize>,
) -> bool {
    let Some(message) = json.get("message") else {
        return false;
    };
    let level = message.get("level").and_then(|v| v.as_str()).unwrap_or("");
    let msg_text = message
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let code = message
        .get("code")
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Report the span rustc marked primary, not whichever span happens to be
    // first — see `primary_span` for the measured E0499 / E0382 counterexamples.
    let location = message
        .get("spans")
        .and_then(|v| v.as_array())
        .and_then(|spans| primary_span(spans.as_slice()))
        .map(span_location)
        .unwrap_or_default();

    match level {
        "error" => {
            error_messages.push(format_diagnostic("error", code, msg_text, &location));
        }
        "warning" => {
            warning_messages.push(format_diagnostic("warning", code, msg_text, &location));
            if !code.is_empty() {
                *warning_codes.entry(code.to_string()).or_insert(0) += 1;
            }
        }
        _ => {}
    }

    true
}

/// Tier 1: Parse NDJSON lines from cargo's `--message-format=json` output.
///
/// Looks for:
/// - `{"reason":"compiler-message",...}` entries to count warnings/errors
/// - `{"reason":"build-finished","success":true/false}` for final status
fn try_tier1_json(stdout: &str) -> Option<ParseResult<BuildResult>> {
    let mut error_messages: Vec<String> = Vec::new();
    let mut warning_messages: Vec<String> = Vec::new();
    let mut warning_codes: BTreeMap<String, usize> = BTreeMap::new();
    let mut found_build_finished = false;
    let mut success = false;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match json.get("reason").and_then(|v| v.as_str()) {
            Some("compiler-message") => {
                process_compiler_message(
                    &json,
                    &mut error_messages,
                    &mut warning_messages,
                    &mut warning_codes,
                );
            }
            Some("build-finished") => {
                found_build_finished = true;
                success = json
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
            _ => {}
        }
    }

    // Require the build-finished event for a Full result
    if !found_build_finished {
        return None;
    }

    // Counts ARE the vectors' lengths: every "error"/"warning" level pushes
    // exactly one diagnostic line (see `process_compiler_message`), so each
    // figure has exactly one source and cannot disagree with itself.
    let errors = error_messages.len();

    // ONE representation of the warnings — per-warning detail or the
    // by-lint-code roll-up, chosen in a single place so both can never render.
    // `error_messages` carries errors only; it no longer doubles as a warning
    // channel.
    //
    // `summarise_warnings` captures the true warning total before it may
    // replace the detail, which is why the count is read off its return value
    // and not off `warning_messages` afterwards: above the bound that vector
    // holds one entry per lint CODE.
    let warnings = summarise_warnings(warning_messages, &warning_codes);

    let duration_ms = None; // Cargo doesn't report build duration in JSON
    Some(ParseResult::Full(
        BuildResult::new(success, warnings.total, errors, duration_ms, error_messages)
            .with_warnings(warnings.channel),
    ))
}

/// Tier 2: Regex-based fallback parsing on stderr.
///
/// Matches `error[E\d+]` and `warning:` patterns to approximate counts.
fn try_tier2_regex(stderr: &str) -> Option<ParseResult<BuildResult>> {
    if stderr.trim().is_empty() {
        return None;
    }

    let error_count = CARGO_ERROR_RE.find_iter(stderr).count();
    let warning_count = CARGO_WARNING_RE.find_iter(stderr).count();

    if error_count == 0 && warning_count == 0 {
        return None;
    }

    // Extract error messages from lines matching the pattern
    let error_messages: Vec<String> = CARGO_ERROR_LINE_RE
        .captures_iter(stderr)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect();

    let success = error_count == 0;
    let result = BuildResult::new(success, warning_count, error_count, None, error_messages);

    Some(ParseResult::Degraded(
        result,
        vec!["cargo build: structured parse failed, using regex".to_string()],
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::{load_fixture, make_output_full};

    // ========================================================================
    // Tier 1: JSON parsing
    // ========================================================================

    #[test]
    fn test_tier1_build_success() {
        let stdout = load_fixture("build", "cargo_build_ok.json");
        let output = make_output_full(&stdout, "", Some(0));
        let result = parse(&output);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(build_result.success, "expected success");
            assert_eq!(build_result.errors, 0);
        }
    }

    #[test]
    fn test_tier1_build_failure() {
        let stdout = load_fixture("build", "cargo_build_fail.json");
        let output = make_output_full(&stdout, "", Some(101));
        let result = parse(&output);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(!build_result.success, "expected failure");
            assert!(build_result.errors > 0, "expected errors > 0");
        }
    }

    #[test]
    fn test_tier1_clippy_warnings() {
        let stdout = load_fixture("build", "clippy_warnings.json");
        let output = make_output_full(&stdout, "", Some(0));
        let result = parse(&output);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert_eq!(build_result.warnings, 2, "expected 2 warnings");
            assert!(build_result.success, "expected success");
        }
    }

    /// Synthesise a cargo NDJSON stream carrying `n` `dead_code` warnings plus
    /// the `build-finished` line tier 1 requires.
    ///
    /// Built inline rather than as a fixture file. The roll-up's domain starts
    /// ABOVE `WARNING_DETAIL_MAX`, and `tests/fixtures/cmd/build/clippy_warnings.json`
    /// deliberately carries two warnings — it pins the DETAIL domain and must
    /// keep doing so, so it is the wrong file to grow.
    fn clippy_ndjson_with_warnings(n: usize) -> String {
        use serde_json::json;
        let mut out = String::new();
        for i in 0..n {
            let msg = json!({
                "reason": "compiler-message",
                "message": {
                    "level": "warning",
                    "message": format!("unused variable: `v{i}`"),
                    "code": {"code": "dead_code"},
                    "spans": [
                        {"file_name": "src/lib.rs", "line_start": i, "is_primary": true}
                    ]
                }
            });
            out.push_str(&msg.to_string());
            out.push('\n');
        }
        out.push_str(&json!({"reason": "build-finished", "success": true}).to_string());
        out.push('\n');
        out
    }

    /// The by-lint-code roll-up still exists. What changed is that it now has a
    /// DEFINED DOMAIN — it is what `warning_messages` carries above
    /// `WARNING_DETAIL_MAX`, *in place of* the per-warning lines rather than
    /// alongside them, and it no longer travels in `error_messages`.
    ///
    /// Re-aimed rather than deleted: the roll-up is still the subject, and the
    /// roll-up is what changed.
    #[test]
    fn test_tier1_clippy_warning_codes_grouped_above_the_detail_bound() {
        let n = WARNING_DETAIL_MAX + 1;
        let stdout = clippy_ndjson_with_warnings(n);
        let output = make_output_full(&stdout, "", Some(0));
        let result = parse(&output);

        let ParseResult::Full(build_result) = &result else {
            panic!("expected Full, got {:?}", result.tier_name());
        };
        assert_eq!(build_result.warnings, n);
        assert_eq!(
            build_result.warning_messages,
            vec![format!("dead_code: {n} occurrence(s)")],
            "above the bound the roll-up REPLACES the per-warning lines"
        );
        assert!(
            build_result.error_messages.is_empty(),
            "the roll-up no longer travels in error_messages: {:?}",
            build_result.error_messages
        );
    }

    /// The defect this ruling targets: both representations rendering for the
    /// same warnings. Enforced structurally inside `summarise_warnings` (one
    /// return value, one branch); pinned here from the outside on both sides of
    /// the bound, since a structural guarantee is only as good as the caller
    /// that honours it.
    #[test]
    fn test_warning_representations_are_mutually_exclusive() {
        for n in [WARNING_DETAIL_MAX, WARNING_DETAIL_MAX + 1] {
            let output = make_output_full(&clippy_ndjson_with_warnings(n), "", Some(0));
            let result = parse(&output);
            let ParseResult::Full(build_result) = &result else {
                panic!("expected Full for n={n}, got {:?}", result.tier_name());
            };
            let rendered = format!("{build_result}");
            let has_detail = rendered.contains("warning[dead_code]: unused variable:");
            let has_rollup = rendered.contains("occurrence(s)");
            assert!(
                has_detail ^ has_rollup,
                "exactly one representation may render (n={n}, detail={has_detail}, \
                 rollup={has_rollup}): {rendered:?}"
            );
        }
    }

    /// Synthesise a cargo NDJSON stream carrying `warnings` warnings AND
    /// `errors` errors, plus the `build-finished` line tier 1 requires.
    fn ndjson_with(warnings: usize, errors: usize) -> String {
        use serde_json::json;
        let mut out = String::new();
        for i in 0..warnings {
            out.push_str(
                &json!({
                    "reason": "compiler-message",
                    "message": {
                        "level": "warning",
                        "message": format!("unused variable: `v{i}`"),
                        "code": {"code": "dead_code"},
                        "spans": [
                            {"file_name": "src/lib.rs", "line_start": i, "is_primary": true}
                        ]
                    }
                })
                .to_string(),
            );
            out.push('\n');
        }
        for i in 0..errors {
            out.push_str(
                &json!({
                    "reason": "compiler-message",
                    "message": {
                        "level": "error",
                        "message": "mismatched types",
                        "code": {"code": "E0308"},
                        "spans": [
                            {"file_name": "src/main.rs", "line_start": i, "is_primary": true}
                        ]
                    }
                })
                .to_string(),
            );
            out.push('\n');
        }
        out.push_str(&json!({"reason": "build-finished", "success": errors == 0}).to_string());
        out.push('\n');
        out
    }

    /// `error_messages` is ERRORS ONLY, on both sides of the bound.
    ///
    /// Declaring an end state, not describing an accident. On main this field
    /// unconditionally carried one `"<code>: N occurrence(s)"` entry per lint
    /// code, so a `skim cargo clippy --json` consumer could read the warning
    /// distribution out of it. It cannot any more: warnings travel on
    /// `warning_messages`, and BELOW the bound the roll-up is never built at
    /// all, so `error_messages` on a green clippy run is empty where it used to
    /// have entries.
    ///
    /// The above-bound half of that is already pinned by
    /// `test_tier1_clippy_warning_codes_grouped_above_the_detail_bound`. The
    /// below-bound half — the common case, every run under 51 warnings — was
    /// not, which is the drift this test closes. A roll-up leaking back into
    /// `error_messages` must fail here, not in a consumer.
    #[test]
    fn test_error_messages_carries_errors_only() {
        for (warnings, errors) in [(3usize, 0usize), (3, 2), (WARNING_DETAIL_MAX + 1, 2)] {
            let output = make_output_full(&ndjson_with(warnings, errors), "", Some(0));
            let result = parse(&output);
            let ParseResult::Full(build_result) = &result else {
                panic!(
                    "expected Full for ({warnings}, {errors}), got {:?}",
                    result.tier_name()
                );
            };

            assert_eq!(
                build_result.error_messages.len(),
                errors,
                "error_messages must hold exactly the errors: {:?}",
                build_result.error_messages
            );
            assert!(
                build_result
                    .error_messages
                    .iter()
                    .all(|m| m.starts_with("error")),
                "every error_messages entry must be an error diagnostic: {:?}",
                build_result.error_messages
            );
            assert!(
                !build_result
                    .error_messages
                    .iter()
                    .any(|m| m.contains("occurrence(s)")),
                "the by-lint-code roll-up must never travel in error_messages: {:?}",
                build_result.error_messages
            );
            assert_eq!(
                build_result.warnings, warnings,
                "the warning TOTAL is reported whichever representation was chosen"
            );
        }
    }

    /// The roll-up announces itself, so the ADR-011 class-1 marker can name
    /// what this bound dropped.
    ///
    /// Above the bound the reader loses per-warning message text and
    /// `file:line` locations. `diagnostics_summary_marker` otherwise names only
    /// cargo's fixed discard set (source snippets, help/note lines, explain
    /// hints) and would leave that unsaid — the misstatement ADR-011's
    /// 2026-09-24 amendment ranks below omission.
    #[test]
    fn test_rollup_is_disclosed_on_the_build_result() {
        let below = make_output_full(&ndjson_with(WARNING_DETAIL_MAX, 0), "", Some(0));
        let ParseResult::Full(below) = parse(&below) else {
            panic!("expected Full below the bound");
        };
        assert!(
            !below.warnings_rolled_up,
            "at the bound the per-warning detail is served, so nothing is rolled up"
        );

        let above = make_output_full(&ndjson_with(WARNING_DETAIL_MAX + 1, 0), "", Some(0));
        let ParseResult::Full(above) = parse(&above) else {
            panic!("expected Full above the bound");
        };
        assert!(
            above.warnings_rolled_up,
            "above the bound the roll-up replaced the detail and must say so"
        );
        assert_eq!(
            above.warnings,
            WARNING_DETAIL_MAX + 1,
            "the marker quotes this total, so it must survive the roll-up"
        );
    }

    /// The wholesale switch owes no ADR-011 class-1 marker only because the
    /// roll-up accounts for every warning. A codeless warning has no lint key
    /// to group under, so it gets an explicit bucket instead of vanishing from
    /// the totals — without which "nothing is elided" would be false.
    #[test]
    fn test_rollup_counts_account_for_every_warning() {
        let total = WARNING_DETAIL_MAX + 3;
        let mut codes = BTreeMap::new();
        codes.insert("dead_code".to_string(), total - 2);

        let rolled = roll_up_warning_codes(&codes, total);

        let summed: usize = rolled
            .iter()
            .filter_map(|line| line.split(": ").nth(1))
            .filter_map(|tail| tail.split(' ').next())
            .filter_map(|n| n.parse::<usize>().ok())
            .sum();
        assert_eq!(
            summed, total,
            "roll-up buckets must sum to the warning total: {rolled:?}"
        );
        assert!(
            rolled
                .iter()
                .any(|l| l.starts_with("(no lint code): 2 occurrence(s)")),
            "codeless warnings need their own bucket: {rolled:?}"
        );
    }

    #[test]
    fn test_rollup_omits_uncoded_bucket_when_every_warning_has_a_code() {
        let mut codes = BTreeMap::new();
        codes.insert("dead_code".to_string(), 51usize);

        let rolled = roll_up_warning_codes(&codes, 51);

        assert_eq!(rolled, vec!["dead_code: 51 occurrence(s)".to_string()]);
    }

    // ========================================================================
    // process_compiler_message unit tests
    // ========================================================================

    /// Helper: build a minimal compiler-message JSON value.
    fn make_compiler_message(
        level: &str,
        message: &str,
        code: Option<&str>,
        file: Option<&str>,
        line_start: Option<u64>,
    ) -> serde_json::Value {
        use serde_json::json;
        let code_obj = code
            .map(|c| json!({"code": c}))
            .unwrap_or(serde_json::Value::Null);
        let spans = match (file, line_start) {
            (Some(f), Some(l)) => json!([{"file_name": f, "line_start": l, "is_primary": true}]),
            _ => json!([]),
        };
        json!({
            "reason": "compiler-message",
            "message": {
                "level": level,
                "message": message,
                "code": code_obj,
                "spans": spans
            }
        })
    }

    #[test]
    fn test_process_compiler_message_missing_message_key() {
        // A JSON object with no "message" field must return false and leave
        // all accumulators untouched.
        let json = serde_json::json!({"reason": "compiler-message"});
        let mut msgs: Vec<String> = Vec::new();
        let mut warn_msgs: Vec<String> = Vec::new();
        let mut codes: BTreeMap<String, usize> = BTreeMap::new();

        let ok = process_compiler_message(&json, &mut msgs, &mut warn_msgs, &mut codes);

        assert!(!ok, "should return false for missing message key");
        assert!(msgs.is_empty());
        assert!(warn_msgs.is_empty());
        assert!(codes.is_empty());
    }

    #[test]
    fn test_process_compiler_message_error_with_code_and_location() {
        // An error event with both a rustc error code and a span should produce
        // a fully qualified "error[E####]: ... in file:line" message.
        let json = make_compiler_message(
            "error",
            "mismatched types",
            Some("E0308"),
            Some("src/main.rs"),
            Some(42),
        );
        let mut msgs: Vec<String> = Vec::new();
        let mut warn_msgs: Vec<String> = Vec::new();
        let mut codes: BTreeMap<String, usize> = BTreeMap::new();

        let ok = process_compiler_message(&json, &mut msgs, &mut warn_msgs, &mut codes);

        assert!(ok, "should return true for valid compiler-message");
        assert_eq!(msgs.len(), 1);
        assert!(warn_msgs.is_empty(), "an error is not a warning");
        assert_eq!(msgs[0], "error[E0308]: mismatched types in src/main.rs:42");
        assert!(
            codes.is_empty(),
            "no warning codes expected for error level"
        );
    }

    #[test]
    fn test_process_compiler_message_error_without_code_or_location() {
        // An error with neither a code nor a span should fall back to the
        // plain "error: <message>" format.
        let json = make_compiler_message("error", "internal compiler error", None, None, None);
        let mut msgs: Vec<String> = Vec::new();
        let mut warn_msgs: Vec<String> = Vec::new();
        let mut codes: BTreeMap<String, usize> = BTreeMap::new();

        let ok = process_compiler_message(&json, &mut msgs, &mut warn_msgs, &mut codes);

        assert!(ok);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "error: internal compiler error");
    }

    #[test]
    fn test_process_compiler_message_warning_with_code() {
        // A warning event with a lint code should push exactly one warning
        // diagnostic and record the code in the warning_codes map, but add
        // nothing to error_messages.
        let json = make_compiler_message(
            "warning",
            "unused variable: `x`",
            Some("dead_code"),
            Some("src/lib.rs"),
            Some(10),
        );
        let mut msgs: Vec<String> = Vec::new();
        let mut warn_msgs: Vec<String> = Vec::new();
        let mut codes: BTreeMap<String, usize> = BTreeMap::new();

        let ok = process_compiler_message(&json, &mut msgs, &mut warn_msgs, &mut codes);

        assert!(ok);
        assert!(msgs.is_empty(), "warnings should not add to error_messages");
        assert_eq!(codes.get("dead_code"), Some(&1));
        // E-4: the warning is rendered by the SAME function as an error, into
        // its own channel — same `[code]: msg in file:line` shape, `warning`
        // where an error says `error`.
        assert_eq!(
            warn_msgs,
            vec!["warning[dead_code]: unused variable: `x` in src/lib.rs:10".to_string()],
            "warning must be rendered through format_diagnostic"
        );
    }

    #[test]
    fn test_process_compiler_message_warning_without_code() {
        // A warning with no lint code still pushes exactly one warning
        // diagnostic but leaves warning_codes empty.
        let json = make_compiler_message("warning", "unused import", None, None, None);
        let mut msgs: Vec<String> = Vec::new();
        let mut warn_msgs: Vec<String> = Vec::new();
        let mut codes: BTreeMap<String, usize> = BTreeMap::new();

        let ok = process_compiler_message(&json, &mut msgs, &mut warn_msgs, &mut codes);

        assert!(ok);
        assert!(codes.is_empty());
        assert!(msgs.is_empty());
        assert_eq!(
            warn_msgs,
            vec!["warning: unused import".to_string()],
            "a codeless, spanless warning still renders through format_diagnostic"
        );
    }

    // ========================================================================
    // E-2: primary span selection
    // ========================================================================

    /// Real rustc payload shape, captured from `cargo build --message-format=json`
    /// on rustc 1.96.0 for:
    ///
    /// ```ignore
    /// let a = &mut s;   // line 4 — secondary, "first mutable borrow occurs here"
    /// let b = &mut s;   // line 5 — PRIMARY,   "second mutable borrow occurs here"
    /// a.push('x');      // line 6 — secondary, "first borrow later used here"
    /// ```
    ///
    /// rustc's own header reads `--> src/main.rs:5:13`. `spans.first()` reports
    /// line 4 — the other borrow, not the error site.
    fn e0499_multi_span_message() -> serde_json::Value {
        use serde_json::json;
        json!({
            "reason": "compiler-message",
            "message": {
                "level": "error",
                "message": "cannot borrow `s` as mutable more than once at a time",
                "code": {"code": "E0499"},
                "spans": [
                    {"file_name": "src/main.rs", "line_start": 4, "is_primary": false,
                     "label": "first mutable borrow occurs here"},
                    {"file_name": "src/main.rs", "line_start": 5, "is_primary": true,
                     "label": "second mutable borrow occurs here"},
                    {"file_name": "src/main.rs", "line_start": 6, "is_primary": false,
                     "label": "first borrow later used here"}
                ]
            }
        })
    }

    #[test]
    fn test_primary_span_prefers_is_primary_over_first() {
        let msg = e0499_multi_span_message();
        let spans = msg["message"]["spans"].as_array().expect("spans array");

        let chosen = primary_span(spans.as_slice()).expect("a span is selected");
        assert_eq!(
            span_location(chosen),
            "src/main.rs:5",
            "must report the is_primary span (rustc's own `--> src/main.rs:5:13`), not spans[0]"
        );
    }

    #[test]
    fn test_primary_span_falls_back_to_first_when_none_primary() {
        // Nothing in the JSON schema forbids an all-secondary span list.
        // Reporting *some* location beats reporting none.
        let spans = vec![
            serde_json::json!({"file_name": "a.rs", "line_start": 7, "is_primary": false}),
            serde_json::json!({"file_name": "b.rs", "line_start": 9, "is_primary": false}),
        ];
        let chosen = primary_span(&spans).expect("fallback selects the first span");
        assert_eq!(span_location(chosen), "a.rs:7");
    }

    #[test]
    fn test_primary_span_missing_is_primary_key_falls_back() {
        // Absent key is not "primary" — it must not be read as true.
        let spans = vec![
            serde_json::json!({"file_name": "a.rs", "line_start": 7}),
            serde_json::json!({"file_name": "b.rs", "line_start": 9, "is_primary": true}),
        ];
        let chosen = primary_span(&spans).expect("selects the explicit primary");
        assert_eq!(span_location(chosen), "b.rs:9");
    }

    #[test]
    fn test_primary_span_empty_list_is_none() {
        assert!(primary_span(&[]).is_none());
    }

    #[test]
    fn test_process_compiler_message_reports_primary_span_location() {
        // Regression for E-2: before the fix this rendered `… in src/main.rs:4`.
        let json = e0499_multi_span_message();
        let mut msgs: Vec<String> = Vec::new();
        let mut warn_msgs: Vec<String> = Vec::new();
        let mut codes: BTreeMap<String, usize> = BTreeMap::new();

        let ok = process_compiler_message(&json, &mut msgs, &mut warn_msgs, &mut codes);

        assert!(ok);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0],
            "error[E0499]: cannot borrow `s` as mutable more than once at a time in src/main.rs:5",
            "the reported location must be the primary span, not spans[0]"
        );
    }

    // ========================================================================
    // E-4: one rendering shared by the error and warning paths
    // ========================================================================

    #[test]
    fn test_format_diagnostic_covers_all_four_shapes() {
        assert_eq!(
            format_diagnostic("error", "E0308", "mismatched types", "src/main.rs:42"),
            "error[E0308]: mismatched types in src/main.rs:42"
        );
        assert_eq!(
            format_diagnostic("error", "E0308", "mismatched types", ""),
            "error[E0308]: mismatched types"
        );
        assert_eq!(
            format_diagnostic("error", "", "internal compiler error", "src/main.rs:42"),
            "error: internal compiler error in src/main.rs:42"
        );
        assert_eq!(
            format_diagnostic("error", "", "internal compiler error", ""),
            "error: internal compiler error"
        );
    }

    #[test]
    fn test_format_diagnostic_error_and_warning_differ_only_in_level() {
        // The anti-drift property: one rendering, two levels. If a second
        // spelling is ever introduced this assertion is what breaks.
        let err = format_diagnostic("error", "E0499", "cannot borrow", "src/main.rs:5");
        let warn = format_diagnostic("warning", "E0499", "cannot borrow", "src/main.rs:5");
        assert_eq!(
            err.strip_prefix("error"),
            warn.strip_prefix("warning"),
            "error and warning renderings must differ only in the level token"
        );
    }

    #[test]
    fn test_tier1_warning_messages_rendered_on_successful_build() {
        // A green build's warnings are its only diagnostics. Before E-4 they were
        // dropped entirely: `error_messages` carried a grouped code count that
        // `BuildResult::render` suppresses when `success`.
        let stdout = load_fixture("build", "clippy_warnings.json");
        let output = make_output_full(&stdout, "", Some(0));
        let result = parse(&output);

        let ParseResult::Full(build_result) = &result else {
            panic!("expected Full result, got {:?}", result.tier_name());
        };
        assert!(build_result.success, "fixture is a successful clippy run");
        assert_eq!(
            build_result.warning_messages.len(),
            2,
            "both warnings must be carried: {:?}",
            build_result.warning_messages
        );
        assert!(
            build_result
                .warning_messages
                .iter()
                .all(|m| m.starts_with("warning")),
            "warning diagnostics must carry the `warning` level token: {:?}",
            build_result.warning_messages
        );
        let rendered = format!("{build_result}");
        for msg in &build_result.warning_messages {
            assert!(
                rendered.contains(msg.as_str()),
                "warning {msg:?} must appear in the rendered output, got: {rendered:?}"
            );
        }
        // The DETAIL domain: two warnings is far below `WARNING_DETAIL_MAX`, so
        // the by-lint-code roll-up must be absent. This fixture is the pin for
        // this side of the bound.
        assert!(
            !rendered.contains("occurrence(s)"),
            "the roll-up must not render alongside the per-warning lines: {rendered:?}"
        );
    }

    #[test]
    fn test_tier2_regex_carries_no_warning_messages() {
        // Tier 2 has no structured warning payload to format, so it stays on the
        // 5-argument constructor — one of the 15 builders whose serialized shape
        // is unchanged.
        let stderr = "warning: unused variable\nerror[E0308]: mismatched types\n";
        let output = make_output_full("", stderr, Some(101));
        let result = parse(&output);

        let ParseResult::Degraded(build_result, _) = &result else {
            panic!("expected Degraded, got {:?}", result.tier_name());
        };
        assert!(build_result.warning_messages.is_empty());
        let json = serde_json::to_string(build_result).expect("serializes");
        assert!(
            !json.contains("warning_messages"),
            "absent field must not appear in the JSON envelope: {json}"
        );
    }

    #[test]
    fn test_flag_injection_skipped() {
        // If user already has --message-format=json2, we should not inject our own
        let args = vec!["--message-format=json2".to_string()];
        assert!(
            user_has_flag(&args, &["--message-format"]),
            "should detect existing --message-format flag"
        );
    }

    #[test]
    fn test_user_message_format_skips_injection_and_falls_through() {
        // When user provides --message-format=short, we skip JSON injection.
        // Cargo then emits human-readable text instead of JSON, so tier 1
        // (JSON) fails and the output falls through to tier 2 or tier 3.
        //
        // Simulate: cargo outputs human text to stderr (no JSON on stdout).
        let stderr = "error[E0308]: mismatched types\n  --> src/main.rs:10:5\n";
        let output = make_output_full("", stderr, Some(101));

        // Verify flag detection prevents injection
        let user_args = vec!["build".to_string(), "--message-format=short".to_string()];
        assert!(
            user_has_flag(&user_args, &["--message-format"]),
            "should detect user's --message-format flag"
        );

        // Verify parser still works via tier 2 regex fallback
        let result = parse(&output);
        assert!(
            result.is_degraded(),
            "expected Degraded (tier 2) when JSON unavailable, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Degraded(build_result, _) = &result {
            assert_eq!(build_result.errors, 1, "expected 1 error from regex tier");
            assert!(!build_result.success, "expected failure");
        }
    }

    // ========================================================================
    // Tier 2: Regex fallback
    // ========================================================================

    #[test]
    fn test_tier2_regex_errors() {
        let stderr = "error[E0308]: mismatched types\n  --> src/main.rs:10:5\nerror[E0425]: cannot find value\n";
        let output = make_output_full("", stderr, Some(101));
        let result = parse(&output);

        assert!(
            result.is_degraded(),
            "expected Degraded, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Degraded(build_result, markers) = &result {
            assert_eq!(build_result.errors, 2, "expected 2 errors from regex");
            assert!(!build_result.success, "expected failure");
            assert!(
                markers.contains(&"cargo build: structured parse failed, using regex".to_string())
            );
        }
    }

    // ========================================================================
    // Tier 3: Passthrough
    // ========================================================================

    #[test]
    fn test_tier3_passthrough() {
        let output = make_output_full("some random output", "", Some(0));
        let result = parse(&output);

        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {:?}",
            result.tier_name()
        );
    }

    // ========================================================================
    // cargo fmt parser
    // ========================================================================

    #[test]
    fn test_parse_fmt_empty_output_is_success() {
        let output = make_output_full("", "", Some(0));
        let result = parse_fmt(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(build_result.success, "expected success for empty output");
            assert_eq!(build_result.errors, 0);
            assert_eq!(build_result.warnings, 0);
        }
    }

    #[test]
    fn test_parse_fmt_whitespace_only_is_success() {
        let output = make_output_full("  \n\n", " \t\n", Some(0));
        let result = parse_fmt(&output);
        assert!(result.is_full(), "expected Full for whitespace-only output");
        if let ParseResult::Full(build_result) = &result {
            assert!(build_result.success);
        }
    }

    #[test]
    fn test_parse_fmt_empty_output_nonzero_exit_is_failure() {
        // A signal-killed or internally-panicking `cargo fmt` may exit non-zero
        // with no output at all. This must be reported as failure, not success.
        let output = make_output_full("", "", Some(1));
        let result = parse_fmt(&output);
        assert!(
            result.is_full(),
            "expected Full for empty output, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(
                !build_result.success,
                "empty output + non-zero exit must be reported as failure"
            );
        }
    }

    #[test]
    fn test_parse_fmt_empty_output_signal_killed_is_failure() {
        // exit_code == None means the process was killed by a signal (Unix).
        // This must be reported as failure, not success.
        let output = make_output_full("", "", None);
        let result = parse_fmt(&output);
        assert!(result.is_full(), "expected Full for empty output");
        if let ParseResult::Full(build_result) = &result {
            assert!(
                !build_result.success,
                "empty output + signal-killed (exit_code None) must be reported as failure"
            );
        }
    }

    #[test]
    fn test_parse_fmt_error_output_is_passthrough() {
        let stderr = "error: rustfmt not installed\n";
        let output = make_output_full("", stderr, Some(1));
        let result = parse_fmt(&output);
        assert!(
            result.is_passthrough(),
            "expected Passthrough for error output, got {:?}",
            result.tier_name()
        );
        assert!(result.content().contains("rustfmt not installed"));
    }

    #[test]
    fn test_parse_fmt_stdout_and_stderr_separated_by_newline() {
        // When both stdout and stderr have content, they must be joined with a
        // newline separator so the last line of stdout and first line of stderr
        // are not merged into a single line. Regression test for the
        // format!("{}{}") → combine_output fix.
        let stdout = "stdout line";
        let stderr = "stderr line";
        let output = make_output_full(stdout, stderr, Some(1));
        let result = parse_fmt(&output);
        assert!(
            result.is_passthrough(),
            "expected Passthrough for non-empty output, got {:?}",
            result.tier_name()
        );
        let content = result.content();
        // Lines must be separated by a newline, not concatenated: "stdout linestderr line"
        assert!(
            content.contains("stdout line\nstderr line"),
            "stdout and stderr must be newline-separated in combined output: {content:?}"
        );
    }

    // ========================================================================
    // Helper tests
    // ========================================================================

    #[test]
    fn test_inject_flag_before_separator() {
        let mut args = vec![
            "build".to_string(),
            "--release".to_string(),
            "--".to_string(),
            "-W".to_string(),
            "clippy::pedantic".to_string(),
        ];
        inject_flag_before_separator(&mut args, "--message-format=json");
        assert_eq!(args[2], "--message-format=json");
        assert_eq!(args[3], "--");
    }

    #[test]
    fn test_inject_flag_no_separator() {
        let mut args = vec!["build".to_string(), "--release".to_string()];
        inject_flag_before_separator(&mut args, "--message-format=json");
        assert_eq!(args.last().unwrap(), "--message-format=json");
    }

    #[test]
    fn test_user_has_flag_present() {
        let args = vec!["build".to_string(), "--message-format=json2".to_string()];
        assert!(user_has_flag(&args, &["--message-format"]));
    }

    #[test]
    fn test_user_has_flag_absent() {
        let args = vec!["build".to_string(), "--release".to_string()];
        assert!(!user_has_flag(&args, &["--message-format"]));
    }
}
