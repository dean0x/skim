//! `git push` output compression.
//!
//! Parses the combined stdout+stderr from `git push` into a compact
//! [`GitResult`] surfacing pushed refs, errors, and branch tracking updates.
//!
//! # DESIGN NOTE (AD-GP-1) — credential URL scrubbing
//!
//! `git push` output can contain credential-embedded remote URLs when callers
//! use `https://<token>@github.com/org/repo`.  These appear verbatim on stderr
//! in lines such as `To https://ghp_token@github.com/org/repo.git`.
//! [`shared::scrub_credential_url`] is called on every line before it is included in
//! any output, ensuring tokens are never forwarded to the caller's terminal or
//! analytics database.
//!
//! # DESIGN NOTE (AD-GP-2) — `--porcelain` auto-injection
//!
//! `git push --porcelain` emits one machine-readable line per ref, in the shape
//! `<flag>\t<from>:<to>\t<summary>` — the flag is a single character occupying
//! column 0, and it is `' '` (a space) for a successfully pushed fast-forward:
//! `=\trefs/heads/main:refs/heads/main\t[up to date]`
//! `*\trefs/heads/feat:refs/heads/feat\t[new branch]`
//! `+\trefs/heads/force:refs/heads/force\t[forced update]`
//! `!\trefs/heads/bad:refs/heads/bad\t[rejected] (fetch first)`
//! `-\t:refs/heads/old\t[deleted]`
//! ` \trefs/heads/main:refs/heads/main\te6bab99..13b30c2`
//! `Done`
//!
//! We auto-inject `--porcelain` unless the user already supplied
//! `--porcelain`, `--no-porcelain`, `--quiet`, or `-q`, giving us a stable
//! parsing surface independent of `git push` prose output variations.
//!
//! # Combine stderr
//!
//! Git push writes remote responses, progress, and ref updates to stderr.
//! We set `combine_stderr: true` so the parser receives the full output
//! (identical to what `git push 2>&1` would produce).

use std::process::ExitCode;

use crate::cmd::{extract_output_format, user_has_flag};
use crate::output::canonical::GitResult;
use crate::output::strip_ansi;

use super::shared::scrub_credential_url;
use super::{run_parsed_command, run_passthrough};

// ============================================================================
// Public entry point
// ============================================================================

/// Run `git push` with output compression.
///
/// Flag-aware passthrough:
/// - `--help` passes through unmodified.
///
/// `--porcelain` is auto-injected unless the user supplied `--porcelain`,
/// `--no-porcelain`, `--quiet`, or `-q`.  See AD-GP-2.
pub(super) fn run_push(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--help"]) {
        return run_passthrough(global_flags, "push", args, show_stats, rec);
    }

    let (mut effective_args, output_format) = extract_output_format(args);

    // Auto-inject --porcelain for stable parsing (AD-GP-2).
    let needs_porcelain = !user_has_flag(
        &effective_args,
        &["--porcelain", "--no-porcelain", "--quiet", "-q"],
    );
    if needs_porcelain {
        // Insert --porcelain as the first flag (after any remote/refspec args
        // are left in place).  Prepending ensures git sees it before positionals.
        effective_args.insert(0, "--porcelain".to_string());
    }

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push("push".to_string());
    full_args.extend_from_slice(&effective_args);

    let label = super::build_analytics_label("push", args, show_stats, rec.enabled);

    run_parsed_command(
        &full_args,
        show_stats,
        rec,
        output_format,
        label,
        // ADR-015 / D1 declaration — `Lossy`.  `parse_push` renders the
        // injected `--porcelain` per-ref lines into a summary and drops git's
        // remaining output; countless, so `elided` = None.
        super::ParsedCommandOptions::combined(crate::output::fidelity::Completeness::Lossy),
        parse_push,
    )
}

// ============================================================================
// Parser
// ============================================================================

/// Parse `git push` output into a compact [`GitResult`].
///
/// Three-tier contract:
/// - **Full**: Porcelain output parsed into per-ref summary lines.
/// - **Full**: Text output parsed for "up-to-date", "rejected", or "Done".
/// - **Passthrough**: Empty or unrecognized output.
///
/// Credential URLs are scrubbed from all lines via [`scrub_credential_url`] (AD-GP-1).
pub(super) fn parse_push(input: &str) -> GitResult {
    let clean = strip_ansi(input);
    let text: &str = clean.as_ref();

    if text.trim().is_empty() {
        return GitResult::new("push".to_string(), "no output".to_string(), Vec::new())
            .with_tier("passthrough");
    }

    // Try porcelain parse first.
    if let Some(result) = try_parse_porcelain(text) {
        return result;
    }

    // Fallback: text-tier parse.
    if let Some(result) = try_parse_text(text) {
        return result;
    }

    // Ultimate fallback: scrub credentials and passthrough.
    // Use iterator destructuring so the non-empty invariant is encoded
    // structurally: `.next()` yields the summary and the rest become details,
    // with no implicit reliance on `scrubbed.len() >= 1` from an early guard.
    let mut iter = text
        .lines()
        .filter(|l: &&str| !l.trim().is_empty())
        .map(|l| scrub_credential_url(l).into_owned());
    let summary = iter.next().unwrap_or_else(|| "pushed".to_string());
    let details: Vec<String> = iter.collect();
    GitResult::new("push".to_string(), summary, details).with_tier("passthrough")
}

// ============================================================================
// Tier 1: Porcelain parsing
// ============================================================================

/// Extract porcelain flag character and ref content from a line.
///
/// Handles two formats:
/// - Tab-prefixed: `\t<flag>\t<refs>` (some git versions)
/// - Bare flag: `<flag>\t<refs>` (standard porcelain)
///
/// `line` must be trailing-trimmed only: the flag occupies column 0 and is a
/// space for a successfully pushed fast-forward, so leading-trimming the line
/// deletes the column this function reads.
///
/// Returns `None` for informational lines (`remote:`, `To`, `Done`),
/// non-flag-char lines, and lines where the flag char is not followed
/// by ref content (`refs/` prefix or `:` notation).
fn extract_flag_and_rest(line: &str) -> Option<(&str, &str)> {
    if let Some(after_tab) = line.strip_prefix('\t') {
        // Some git versions emit a leading tab before the flag.
        let first = after_tab.chars().next().unwrap_or(' ');
        if matches!(first, '=' | '*' | '+' | '!' | '-') {
            let flag = &after_tab[..1];
            let rest = after_tab[1..].trim_start_matches('\t');
            // Same ref-content guard as the bare-flag branch (AD-GP-2):
            // reject lines where the content after the flag is informational
            // text rather than a ref spec.
            if !rest.starts_with("refs/") && !rest.contains(':') {
                return None;
            }
            Some((flag, rest))
        } else {
            None
        }
    } else if !line.is_empty() {
        // The `unwrap_or` is unreachable under the non-empty guard above, and
        // its default must NOT be a character the match accepts — a space is a
        // real porcelain flag.
        let flag_char = line.chars().next().unwrap_or('\0');
        if matches!(flag_char, '=' | '*' | '+' | '!' | '-' | ' ') {
            // Every accepted flag is ASCII, so byte index 1 is a char boundary.
            let after_flag = &line[1..];
            // IMMEDIATE-TAB GUARD (space flag only).
            //
            // `' '` is the porcelain flag for a successfully pushed
            // fast-forward, and it is the one flag that collides with git's
            // human-readable prose, which also puts a space in column 0:
            //
            //   ` * [new branch]      feat -> feat`
            //   ` ! [remote rejected] main -> main (GH006: Protected branch ...)`
            //
            // A real ref line puts a TAB immediately after the flag column
            // (`<flag>\t<from>:<to>\t<summary>`); the prose lines put a SPACE
            // there.  The TAB is the only discriminator, because the
            // ref-content check below is satisfied by any prose line carrying a
            // colon — so without this guard a `[remote rejected]` message is
            // parsed as a ref and a FAILED push is reported as a successful one.
            if flag_char == ' ' && !after_flag.starts_with('\t') {
                return None;
            }
            // Strip optional tab — git push porcelain output may include a tab
            // after the flag character; trim it defensively before validating.
            let rest = after_flag.trim_start_matches('\t');
            // Require that the ref content starts with `refs/` or contains `:`
            // (src:dst ref notation).  Lines like `! [remote rejected]` start with
            // a space or bracket and are informational text, not ref-status lines.
            // This guards against false-triggering on `! [remote rejected]` while
            // preserving real porcelain lines like `!refs/heads/bad:refs/heads/bad`.
            // SEE: AD-GP-2.
            if !rest.starts_with("refs/") && !rest.contains(':') {
                return None;
            }
            let flag = &line[..1];
            Some((flag, rest))
        } else {
            None
        }
    } else {
        None
    }
}

/// Parse `git push --porcelain` output.
///
/// Porcelain per-ref lines are `<flag>\t<from>:<to>\t<summary>`:
/// - `=\trefs/heads/main:refs/heads/main\t[up to date]`
/// - `*\trefs/heads/feat:refs/heads/feat\t[new branch]`
/// - `+\trefs/heads/force:refs/heads/force\t[forced update]`
/// - `!\trefs/heads/bad:refs/heads/bad\t[rejected] (fetch first)`
/// - `-\t:refs/heads/old\t[deleted]`
/// - ` \trefs/heads/main:refs/heads/main\te6bab99..13b30c2` (fast-forward)
/// - `Done` (terminal marker)
///
/// Returns `None` if no porcelain lines are found.
fn try_parse_porcelain(text: &str) -> Option<GitResult> {
    let mut pushed: Vec<String> = Vec::new();
    let mut updated: Vec<String> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut remote_lines: Vec<String> = Vec::new();
    let mut found_porcelain = false;

    for raw_line in text.lines() {
        // Trailing trim ONLY.  The porcelain flag occupies column 0 and is a
        // space for a successfully pushed fast-forward, so a leading trim
        // deletes the very column the parser reads — which is why fast-forward
        // pushes were dropped entirely.
        let line = scrub_credential_url(raw_line.trim_end());
        let line = line.as_ref();
        // Informational lines are still matched on the leading-trimmed form, so
        // their handling is unchanged by the switch from `trim` to `trim_end`.
        let info = line.trim_start();

        if info == "Done" {
            found_porcelain = true;
            continue;
        }

        // Porcelain status lines start with a flag char, then a tab.
        // Format: `<flag>\t<src>:<dst>\t<summary>`
        //
        // Informational lines (remote:, To) that don't parse as flag+rest
        // are collected in remote_lines for the details section.
        let (flag, rest) = match extract_flag_and_rest(line) {
            Some(pair) => pair,
            None => {
                if info.starts_with("remote:") || info.starts_with("To ") {
                    remote_lines.push(info.to_string());
                }
                continue;
            }
        };

        found_porcelain = true;
        // Extract short ref name from `refs/heads/foo:refs/heads/foo` or bare `foo`.
        let short_ref = extract_short_ref(rest.trim());

        match flag {
            "=" => updated.push(format!("= {short_ref} [up to date]")),
            "*" => pushed.push(format!("* {short_ref} [new]")),
            "+" => pushed.push(format!("+ {short_ref} [forced]")),
            "!" => rejected.push(format!("! {short_ref} [rejected]")),
            "-" => deleted.push(format!("- {short_ref} [deleted]")),
            // Space flag: a successfully pushed fast-forward.  Its porcelain
            // summary column is the ref range (`e6bab99..13b30c2`) rather than
            // a bracketed label, and that range is the only per-ref detail the
            // line carries, so it is reported alongside the ref name.
            " " => {
                let detail = match porcelain_summary_field(rest) {
                    Some(range) => format!("  {short_ref} [fast-forward] {range}"),
                    None => format!("  {short_ref} [fast-forward]"),
                };
                pushed.push(detail);
            }
            _ => {}
        }
    }

    if !found_porcelain {
        return None;
    }

    // Build summary.
    let mut parts: Vec<String> = Vec::new();
    if !pushed.is_empty() {
        parts.push(format!("{} pushed", pushed.len()));
    }
    if !updated.is_empty() {
        parts.push(format!("{} up to date", updated.len()));
    }
    if !rejected.is_empty() {
        parts.push(format!("{} rejected", rejected.len()));
    }
    if !deleted.is_empty() {
        parts.push(format!("{} deleted", deleted.len()));
    }

    // `GitResult::render` prepends the operation name, so this string must not
    // repeat it — `"push complete"` rendered as `push push complete`.
    let summary = if parts.is_empty() {
        "complete".to_string()
    } else {
        parts.join(", ")
    };

    let mut details: Vec<String> = Vec::new();
    details.extend(pushed);
    details.extend(updated);
    details.extend(rejected);
    details.extend(deleted);
    details.extend(remote_lines);

    Some(GitResult::new("push".to_string(), summary, details).with_tier("full"))
}

// ============================================================================
// Tier 2: Text parsing
// ============================================================================

/// Fallback text-tier parser for non-porcelain push output.
fn try_parse_text(text: &str) -> Option<GitResult> {
    let mut details: Vec<String> = Vec::new();
    let mut has_signal = false;

    for raw_line in text.lines() {
        let line = scrub_credential_url(raw_line.trim());
        let line_s = line.as_ref();

        if line_s.is_empty() {
            continue;
        }

        // "Everything up-to-date" — standalone success indicator.
        if line_s.contains("up-to-date") || line_s.contains("up to date") {
            return Some(
                GitResult::new("push".to_string(), "up to date".to_string(), Vec::new())
                    .with_tier("full"),
            );
        }

        // Non-fast-forward rejection.
        if line_s.contains("[rejected]") || line_s.contains("non-fast-forward") {
            details.push(line_s.to_string());
            has_signal = true;
            continue;
        }

        // Remote: lines with meaningful info, or "To <remote>" lines.
        if (line_s.starts_with("remote:") && !line_s.contains("...")) || line_s.starts_with("To ") {
            details.push(line_s.to_string());
            has_signal = true;
        }
    }

    if has_signal {
        let summary = details
            .first()
            .cloned()
            .unwrap_or_else(|| "push output".to_string());
        let rest = details[1..].to_vec();
        Some(GitResult::new("push".to_string(), summary, rest).with_tier("degraded"))
    } else {
        None
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Extract the porcelain summary column — the third TAB-delimited field of
/// `<flag>\t<from>:<to>\t<summary>`.
///
/// For a fast-forward the summary is the ref range (`e6bab99..13b30c2`); for
/// the bracketed flags it is `[new branch]`, `[up to date]`,
/// `[rejected] (fetch first)`, and so on.  `rest` is the content *after* the
/// flag column, so the `from:to` field is index 0 and the summary is index 1.
///
/// Returns `None` when the column is absent or blank.
fn porcelain_summary_field(rest: &str) -> Option<&str> {
    rest.split('\t')
        .nth(1)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Extract a short ref name from a porcelain ref entry.
///
/// Input examples:
/// - `refs/heads/main:refs/heads/main [up to date]`
/// - `main:main`
/// - `main`
///
/// Returns `main` or the original string if it cannot be shortened.
fn extract_short_ref(s: &str) -> String {
    // Take the source side (before `:` or whitespace/tab).
    let src = s.split(['\t', ' ']).next().unwrap_or(s);
    let src = src.split(':').next().unwrap_or(src);
    // Strip `refs/heads/` or `refs/tags/` prefix.
    src.strip_prefix("refs/heads/")
        .or_else(|| src.strip_prefix("refs/tags/"))
        .or_else(|| src.strip_prefix("refs/"))
        .unwrap_or(src)
        .to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Porcelain parsing ----

    #[test]
    fn test_parse_porcelain_new_branch() {
        // Porcelain: `*\trefs/heads/feat:refs/heads/feat\t[new branch]`
        let input = "*\trefs/heads/feat:refs/heads/feat\t[new branch]\nDone\n";
        let result = parse_push(input);
        assert_eq!(result.operation, "push");
        assert!(
            result.summary.contains("pushed"),
            "summary: {}",
            result.summary
        );
        assert!(result.details.iter().any(|d| d.contains("feat")));
    }

    #[test]
    fn test_parse_porcelain_up_to_date() {
        let input = "=\trefs/heads/main:refs/heads/main\t[up to date]\nDone\n";
        let result = parse_push(input);
        assert!(
            result.summary.contains("up to date"),
            "summary: {}",
            result.summary
        );
    }

    #[test]
    fn test_parse_porcelain_forced_update() {
        let input = "+\trefs/heads/feat:refs/heads/feat\t[forced update]\nDone\n";
        let result = parse_push(input);
        assert!(
            result.summary.contains("pushed"),
            "summary: {}",
            result.summary
        );
        assert!(result.details.iter().any(|d| d.contains("[forced]")));
    }

    #[test]
    fn test_parse_porcelain_rejected() {
        let input = "!\trefs/heads/feat:refs/heads/feat\t[rejected]\nDone\n";
        let result = parse_push(input);
        assert!(
            result.summary.contains("rejected"),
            "summary: {}",
            result.summary
        );
    }

    // ---- Fast-forward (space flag) ----

    /// Regression: a successfully pushed fast-forward uses a **space** as its
    /// porcelain flag.  The parser used to leading-trim that column away and
    /// did not accept `' '` as a flag, so fast-forward pushes — the single most
    /// common push outcome — vanished from the report entirely and the summary
    /// fell back to the flagless "complete" wording.
    ///
    /// Input is the verbatim shape emitted by `git push --porcelain` for a
    /// fast-forward (captured from a real push to a local bare repo).
    ///
    /// RED at the parent commit: summary was `"complete"`.
    #[test]
    fn test_parse_porcelain_fast_forward_is_reported() {
        let input = concat!(
            "To /tmp/remote.git\n",
            " \trefs/heads/main:refs/heads/main\te6bab99..13b30c2\n",
            "Done\n",
        );
        let result = parse_push(input);
        assert_eq!(
            result.summary, "1 pushed",
            "a fast-forward must be counted as a push: {}",
            result.summary
        );
        assert!(
            result.details.iter().any(|d| d.contains("main")),
            "details must name the ref: {:?}",
            result.details
        );
    }

    /// The fast-forward line's porcelain summary column is the ref range, not a
    /// bracketed label — it is the only per-ref detail the line carries, so it
    /// must survive into the report.
    #[test]
    fn test_fast_forward_reports_ref_range() {
        let input = " \trefs/heads/main:refs/heads/main\te6bab99..13b30c2\nDone\n";
        let result = parse_push(input);
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("e6bab99..13b30c2")),
            "the ref range must be reported: {:?}",
            result.details
        );
    }

    /// Regression: pushing two branches where one is new and one is a
    /// fast-forward must report BOTH.  Before the space flag parsed, the
    /// fast-forward was dropped and the summary under-reported as "1 pushed".
    ///
    /// Verbatim `git push --porcelain <remote> main feat` output.
    ///
    /// RED at the parent commit: summary was `"1 pushed"`.
    #[test]
    fn test_two_branches_new_plus_fast_forward_both_counted() {
        let input = concat!(
            "To /tmp/remote.git\n",
            " \trefs/heads/main:refs/heads/main\t13b30c2..ef0c7a7\n",
            "*\trefs/heads/feat:refs/heads/feat\t[new branch]\n",
            "Done\n",
        );
        let result = parse_push(input);
        assert_eq!(
            result.summary, "2 pushed",
            "both refs land in the same `pushed` bucket: {}",
            result.summary
        );
    }

    /// The immediate-TAB guard, stated as its failure mode: git's
    /// human-readable prose also puts a space in column 0, and a
    /// `[remote rejected]` reason can contain a colon — which satisfies the
    /// ref-content check.  Without the guard, that prose line parses as a
    /// space-flagged ref and a FAILED push is reported as a successful one.
    ///
    /// `(GH006: Protected branch update failed for ...)` is GitHub's real
    /// protected-branch rejection text.
    ///
    /// RED two ways, which is why this input is worth its length:
    /// - accepting `' '` WITHOUT the immediate-TAB guard: `"1 pushed,
    ///   1 rejected"` — a failed push reported as partly successful;
    /// - at the parent commit (full leading trim): `"2 rejected"` — the trim
    ///   exposed the prose line's `!` to the flag match, and its colon
    ///   satisfied the ref-content check, double-counting the one rejection.
    #[test]
    fn test_human_form_remote_rejected_with_colon_is_not_a_push() {
        let input = concat!(
            "remote: denied: policy violation on refs/heads/main\n",
            "To https://github.com/org/repo.git\n",
            " ! [remote rejected] main -> main (GH006: Protected branch update",
            " failed for refs/heads/main.)\n",
            "!\trefs/heads/main:refs/heads/main\t[remote rejected] (GH006:",
            " Protected branch update failed for refs/heads/main.)\n",
            "Done\n",
            "error: failed to push some refs to 'https://github.com/org/repo.git'\n",
        );
        let result = parse_push(input);
        assert_eq!(
            result.summary, "1 rejected",
            "a rejected push must not be reported as pushed: {}",
            result.summary
        );
        assert!(
            !result.details.iter().any(|d| d.contains("[fast-forward]")),
            "prose line must not become a fast-forward ref: {:?}",
            result.details
        );
    }

    /// The same guard against the `* [new branch]` prose form, which has a
    /// space in column 0 and a space — never a TAB — after the flag.
    #[test]
    fn test_human_form_new_branch_line_is_not_a_ref_line() {
        let input = " * [new branch]      feat -> feat\nDone\n";
        let result = parse_push(input);
        assert_eq!(
            result.summary, "complete",
            "prose must not be parsed as a ref: {}",
            result.summary
        );
    }

    /// git's non-porcelain fast-forward line is three leading spaces followed
    /// by the range — space in column 0, no TAB after it.  It must not be
    /// mistaken for the porcelain space-flag form.
    #[test]
    fn test_human_form_fast_forward_line_is_not_a_ref_line() {
        let input = "To /tmp/remote.git\n   ef0c7a7..05c71e7  main -> main\nDone\n";
        let result = parse_push(input);
        assert_eq!(
            result.summary, "complete",
            "prose must not be parsed as a ref: {}",
            result.summary
        );
    }

    // ---- Summary wording ----

    /// `GitResult::render` prepends the operation name, so a summary of
    /// "push complete" rendered as "push push complete".
    #[test]
    fn test_summary_does_not_repeat_operation_name() {
        let result = parse_push("Done\n");
        assert_eq!(result.summary, "complete");
        assert_eq!(
            format!("{result}"),
            "push complete",
            "the operation name must appear exactly once"
        );
    }

    // ---- Porcelain summary column ----

    #[test]
    fn test_porcelain_summary_field_extracts_range() {
        assert_eq!(
            porcelain_summary_field("refs/heads/main:refs/heads/main\te6bab99..13b30c2"),
            Some("e6bab99..13b30c2")
        );
    }

    #[test]
    fn test_porcelain_summary_field_absent_or_blank() {
        assert_eq!(
            porcelain_summary_field("refs/heads/main:refs/heads/main"),
            None
        );
        assert_eq!(
            porcelain_summary_field("refs/heads/main:refs/heads/main\t  "),
            None
        );
    }

    // ---- Credential scrubbing ----

    #[test]
    fn test_credential_url_scrubbed() {
        let input = "To https://ghp_supersecrettoken@github.com/org/repo.git\nDone\n";
        let result = parse_push(input);
        let rendered = format!("{result}");
        assert!(
            !rendered.contains("ghp_supersecrettoken"),
            "credential leaked in output"
        );
        assert!(
            rendered.contains("github.com"),
            "URL remainder should be preserved"
        );
    }

    // ---- Text tier ----

    #[test]
    fn test_parse_text_up_to_date() {
        let input = "Everything up-to-date\n";
        let result = parse_push(input);
        assert!(
            result.summary.contains("up to date"),
            "summary: {}",
            result.summary
        );
    }

    // ---- Empty input ----

    #[test]
    fn test_parse_empty_input() {
        let result = parse_push("");
        assert_eq!(result.operation, "push");
        assert_eq!(result.parse_tier, Some("passthrough"));
    }

    // ---- Short ref extraction ----

    #[test]
    fn test_extract_short_ref_heads() {
        assert_eq!(extract_short_ref("refs/heads/main:refs/heads/main"), "main");
    }

    #[test]
    fn test_extract_short_ref_tags() {
        assert_eq!(extract_short_ref("refs/tags/v1.0:refs/tags/v1.0"), "v1.0");
    }

    #[test]
    fn test_extract_short_ref_bare() {
        assert_eq!(extract_short_ref("main"), "main");
    }

    // ---- Compression check ----

    #[test]
    fn test_output_is_shorter_than_porcelain_input() {
        let input = concat!(
            "remote: Resolving deltas: 100% (3/3), completed with 1 local object.\n",
            "remote: \n",
            "remote: Create a pull request for 'feat' on GitHub by visiting:\n",
            "remote:      https://github.com/org/repo/pull/new/feat\n",
            "remote: \n",
            "To https://github.com/org/repo.git\n",
            " * [new branch]      feat -> feat\n",
            "=\trefs/heads/main:refs/heads/main\t[up to date]\n",
            "*\trefs/heads/feat:refs/heads/feat\t[new branch]\n",
            "Done\n",
        );
        let result = parse_push(input);
        let rendered = format!("{result}");
        assert!(
            rendered.len() < input.len(),
            "Compressed should be shorter: compressed={}, raw={}",
            rendered.len(),
            input.len()
        );
    }

    /// Regression (AD-GP-2): `! [remote rejected]` is informational text, not a
    /// porcelain ref-status line.  Without the tab-guard, the parser incorrectly
    /// treats the `!` character as a flag and produces a rejected-ref entry.
    #[test]
    fn test_non_porcelain_exclamation_skipped() {
        let input = "! [remote rejected] main -> main (declined)\nDone\n";
        // try_parse_porcelain still returns Some because "Done" is present, but
        // must NOT produce a rejected ref entry.
        let result = parse_push(input);
        let rendered = format!("{result}");
        assert!(
            !rendered.contains("rejected"),
            "Informational ! line without tab must not produce a rejected ref: {rendered}"
        );
    }

    /// Regression (AD-GP-2): `- Some info text` is informational, not a deleted-ref
    /// porcelain line.
    #[test]
    fn test_non_porcelain_dash_skipped() {
        let input = "- Some info text\nDone\n";
        let result = parse_push(input);
        let rendered = format!("{result}");
        assert!(
            !rendered.contains("deleted"),
            "Informational - line without tab must not produce a deleted ref: {rendered}"
        );
    }

    /// Happy-path: a real deleted-ref porcelain line produces a deleted-ref summary.
    ///
    /// Input uses the standard `git push --porcelain` format for a deleted ref:
    /// `-\trefs/heads/old:refs/heads/old\t[deleted]`.
    #[test]
    fn test_deleted_ref_porcelain_happy_path() {
        let input = "-\trefs/heads/old:refs/heads/old\t[deleted]\nDone\n";
        let result = try_parse_porcelain(input);
        assert!(
            result.is_some(),
            "Deleted-ref porcelain line must be parsed"
        );
        let output = result.unwrap();
        let rendered = format!("{output}");
        assert!(
            rendered.contains("deleted"),
            "Output must mention the deleted ref: {rendered}"
        );
        // Verify the short ref name is extracted correctly.
        assert!(
            rendered.contains("old"),
            "Output must contain the ref name 'old': {rendered}"
        );
    }

    /// Happy-path: a real porcelain line with tab separator must still work.
    #[test]
    fn test_real_porcelain_with_tab_works() {
        let input = "=\trefs/heads/main:refs/heads/main\t[up to date]\nDone\n";
        let result = try_parse_porcelain(input);
        assert!(result.is_some(), "Real porcelain with tab must be parsed");
        let output = result.unwrap();
        let rendered = format!("{output}");
        assert!(
            rendered.contains("up to date") || rendered.contains("main"),
            "Parsed output should contain ref info: {rendered}"
        );
    }

    /// Tab-prefixed informational line must not be mistaken for a porcelain
    /// ref-status line.  The ref-content guard (`refs/` or `:`) must apply
    /// to the tab-prefixed branch too, not just the bare-flag branch.
    #[test]
    fn test_tab_prefixed_informational_line_skipped() {
        // A hypothetical tab-prefixed `! [remote rejected]` line should be
        // treated as informational, not a porcelain ref-status entry.
        let input = "\t! [remote rejected] main -> main (declined)\nDone\n";
        let result = parse_push(input);
        let rendered = format!("{result}");
        assert!(
            !rendered.contains("rejected"),
            "Tab-prefixed informational ! line must not produce a rejected ref: {rendered}"
        );
    }
}
