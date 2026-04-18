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
//! [`shared::scrub_git_url`] is called on every line before it is included in
//! any output, ensuring tokens are never forwarded to the caller's terminal or
//! analytics database.
//!
//! # DESIGN NOTE (AD-GP-2) — `--porcelain` auto-injection
//!
//! `git push --porcelain` emits machine-readable per-ref lines:
//! `= refs/heads/main:refs/heads/main [up to date]`
//! `* refs/heads/feat:refs/heads/feat [new branch]`
//! `+ refs/heads/force:refs/heads/force [forced update]`
//! `! refs/heads/bad:refs/heads/bad [rejected]`
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

use super::shared::scrub_git_url;
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
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--help"]) {
        return run_passthrough(global_flags, "push", args, show_stats, analytics_enabled);
    }

    let (filtered_args, output_format) = extract_output_format(args);

    // Auto-inject --porcelain for stable parsing (AD-GP-2).
    let mut effective_args = filtered_args.clone();
    let needs_porcelain = !user_has_flag(
        &filtered_args,
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

    let label = super::build_analytics_label("push", args, show_stats, analytics_enabled);

    run_parsed_command(
        &full_args,
        show_stats,
        analytics_enabled,
        output_format,
        true, // combine_stderr: push writes to stderr
        label,
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
/// Credential URLs are scrubbed from all lines via [`scrub_git_url`] (AD-GP-1).
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
        .map(|l| scrub_git_url(l).into_owned());
    let summary = iter.next().unwrap_or_else(|| "pushed".to_string());
    let details: Vec<String> = iter.collect();
    GitResult::new("push".to_string(), summary, details).with_tier("passthrough")
}

// ============================================================================
// Tier 1: Porcelain parsing
// ============================================================================

/// Parse `git push --porcelain` output.
///
/// Porcelain format per-ref lines:
/// - ` = refs/heads/main:refs/heads/main [up to date]`
/// - ` * refs/heads/feat:refs/heads/feat [new branch]`
/// - ` + refs/heads/force:refs/heads/force [forced update]`
/// - ` ! refs/heads/bad:refs/heads/bad [rejected]`
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
        let line = scrub_git_url(raw_line.trim());
        let line = line.as_ref();

        if line == "Done" {
            found_porcelain = true;
            continue;
        }

        // Porcelain status lines start with a flag char, then a tab.
        // Format: `<flag>\t<src>:<dst>\t<summary>`
        // Older git: ` <flag> <refs>`  (leading space, flag, space)
        let (flag, rest) = if let Some(after_tab) = line.strip_prefix('\t') {
            // Some git versions emit a leading tab before the flag.
            let first = after_tab.chars().next().unwrap_or(' ');
            if matches!(first, '=' | '*' | '+' | '!' | '-') {
                let flag = &after_tab[..1];
                let rest = after_tab[1..].trim_start_matches('\t');
                (flag, rest)
            } else {
                continue;
            }
        } else if !line.is_empty() {
            let flag_char = line.chars().next().unwrap_or(' ');
            if matches!(flag_char, '=' | '*' | '+' | '!' | '-') {
                let after_flag = &line[1..];
                // Strip optional tab (strip_ansi_escapes removes tab bytes, so the tab
                // may already be absent after preprocessing).
                let rest = after_flag.trim_start_matches('\t');
                // Require that the ref content starts with `refs/` or contains `:`
                // (src:dst ref notation).  Lines like `! [remote rejected]` start with
                // a space or bracket and are informational text, not ref-status lines.
                // This guards against false-triggering on `! [remote rejected]` while
                // preserving real porcelain lines like `!refs/heads/bad:refs/heads/bad`.
                // SEE: AD-GP-2.
                if !rest.starts_with("refs/") && !rest.contains(':') {
                    if line.starts_with("remote:") || line.starts_with("To ") {
                        remote_lines.push(line.to_string());
                    }
                    continue;
                }
                let flag = &line[..1];
                (flag, rest)
            } else {
                // remote: lines and other informational text.
                if line.starts_with("remote:") || line.starts_with("To ") {
                    remote_lines.push(line.to_string());
                }
                continue;
            }
        } else {
            continue;
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

    let summary = if parts.is_empty() {
        "push complete".to_string()
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
        let line = scrub_git_url(raw_line.trim());
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
}
