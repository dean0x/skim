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
        // ADR-011 / D1 declaration — `Lossy`.  `parse_push` renders the
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
        // Render BOTH sides of the refspec: `feature -> main` when they differ,
        // a single short name when they do not.  See [`format_ref_pair`].
        let ref_pair = format_ref_pair(rest.trim());

        match flag {
            "=" => updated.push(format!("= {ref_pair} [up to date]")),
            "*" => pushed.push(format!("* {ref_pair} [new]")),
            "+" => pushed.push(format!("+ {ref_pair} [forced]")),
            "!" => rejected.push(format!("! {ref_pair} [rejected]")),
            "-" => deleted.push(format!("- {ref_pair} [deleted]")),
            // Space flag: a successfully pushed fast-forward.  Its porcelain
            // summary column is the ref range (`e6bab99..13b30c2`) rather than
            // a bracketed label, and that range is the only per-ref detail the
            // line carries, so it is reported alongside the ref name.
            " " => {
                let detail = match porcelain_summary_field(rest) {
                    Some(range) => format!("  {ref_pair} [fast-forward] {range}"),
                    None => format!("  {ref_pair} [fast-forward]"),
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

/// The two sides of a porcelain `<from>:<to>` ref field, shortened — the
/// classification [`format_ref_pair`] renders.
///
/// Every shape git can put in that field is a *variant* here rather than a
/// special case noticed at a call site.  [`RefPair::Destination`] is the one
/// that matters most: git's real deleted-ref line has an **empty source side**
/// (`-\t:refs/heads/old\t[deleted]` — recorded in this module's header and
/// measured against git 2.50.1), so "no source" is a state the classifier
/// names, not a blank string a renderer has to guard against.
#[derive(Debug, PartialEq, Eq)]
enum RefPair {
    /// Both sides name the same ref: `refs/heads/main:refs/heads/main`, or a
    /// bare `main` whose destination is implicitly its source.
    Same(String),
    /// The sides differ: `refs/heads/feature:refs/heads/main`.  The only shape
    /// that earns the arrow form.
    Distinct { src: String, dst: String },
    /// No source side — git's deleted-ref shape, `:refs/heads/old`.
    Destination(String),
    /// No destination side.  git does not emit this; a hand-written refspec can.
    Source(String),
    /// Neither side names a ref (a lone `:`, or an empty field).  Reported
    /// verbatim so the reader sees the token skim could not interpret rather
    /// than nothing at all (#317).
    Unnamed(String),
}

/// Classify the ref field of a porcelain push status line.
///
/// `s` is the content *after* the flag column — the same input
/// [`extract_short_ref`] takes — so the `<from>:<to>` pair is the first
/// TAB/space-delimited field and anything after it is the summary column.
fn classify_ref_pair(s: &str) -> RefPair {
    // Field 0 of `<from>:<to>\t<summary>`.  A ref name can contain neither a
    // space, a TAB, nor a colon (`git check-ref-format`), so neither this split
    // nor the `split_once(':')` below can cut a name in half.
    let field = s.split(['\t', ' ']).next().unwrap_or(s);

    // A leading `+` is legal refspec syntax for a force push, but git's
    // porcelain `<from>` field never carries it: measured against git 2.50.1,
    // `git push --porcelain +side:refs/heads/dst` emits
    // `+\trefs/heads/side:refs/heads/dst\t<range> (forced update)`, i.e. git's
    // refspec parser consumes the `+` and force-ness moves to the FLAG column,
    // which the caller already renders as `[forced]`.  This strip is therefore
    // defensive — for a caller that hands this renderer a user-typed refspec —
    // and it discards no information skim does not already show.
    let field = field.strip_prefix('+').unwrap_or(field);

    let (src, dst) = match field.split_once(':') {
        Some(pair) => pair,
        // No colon: a bare refspec pushes a ref to the same name on the remote.
        None => (field, field),
    };

    // [`extract_short_ref`] is reused below as the per-side prefix shortener.
    // On a single isolated side its own `:` and whitespace splits are no-ops,
    // so it reduces to the `refs/heads/` | `refs/tags/` | `refs/` strip — and
    // keeping that rule in exactly one place is why this classifier does not
    // re-implement it.
    match (src.is_empty(), dst.is_empty()) {
        (true, true) => RefPair::Unnamed(field.to_string()),
        (true, false) => RefPair::Destination(extract_short_ref(dst)),
        (false, true) => RefPair::Source(extract_short_ref(src)),
        // Equality is decided on the FULL sides, BEFORE shortening, so a pair
        // that differs only in its namespace is never collapsed into one name.
        (false, false) if src == dst => RefPair::Same(extract_short_ref(src)),
        (false, false) => {
            let short_src = extract_short_ref(src);
            let short_dst = extract_short_ref(dst);
            if short_src == short_dst {
                // Shortening collided two genuinely different refs
                // (`refs/heads/x:refs/tags/x`).  Show the full names: the arrow
                // form exists precisely to state that the sides differ, and
                // `x -> x` would deny it.
                RefPair::Distinct {
                    src: src.to_string(),
                    dst: dst.to_string(),
                }
            } else {
                RefPair::Distinct {
                    src: short_src,
                    dst: short_dst,
                }
            }
        }
    }
}

/// Render a porcelain ref field for the reader, naming **both** sides of an
/// asymmetric refspec.
///
/// `git push origin feature:main` writes `main` on the remote, and the
/// destination is the half that says what actually changed there.  Measured at
/// `c2b4378`, skim rendered `* f2 [new]` for the refspec
/// `f2:refs/heads/dst-two` while git itself printed
/// ` * [new branch]      f3 -> dst-three`: the reader could not tell where the
/// branch had landed, on the one refspec shape that exists *because* the two
/// sides differ.
///
/// The arrow form follows git's own prose spelling (`<src> -> <dst>`) so a
/// reader can line skim's summary up against git's output.
///
/// **Equal sides collapse to a single name.**  `src == dst` is the
/// overwhelmingly common case (`git push origin main`), `main -> main` states
/// one fact twice, and this render is graded by the ADR-001 net-savings guard —
/// inflating every ordinary push would spend bytes to say nothing and push the
/// guard toward serving raw on exactly the invocations skim compresses today.
///
/// Shapes handled, all measured against `git push --porcelain` (git 2.50.1):
///
/// | Field | Render |
/// |---|---|
/// | `refs/heads/feature:refs/heads/main` | `feature -> main` |
/// | `refs/heads/main:refs/heads/main` | `main` |
/// | `main` | `main` |
/// | `HEAD:refs/heads/feature` | `HEAD -> feature` |
/// | `:refs/heads/old` (deletion) | `old` |
/// | `+feature:main` | `feature -> main` |
fn format_ref_pair(s: &str) -> String {
    match classify_ref_pair(s) {
        RefPair::Same(name)
        | RefPair::Destination(name)
        | RefPair::Source(name)
        | RefPair::Unnamed(name) => name,
        RefPair::Distinct { src, dst } => format!("{src} -> {dst}"),
    }
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

    // ---- Ref-pair rendering (F7) ----
    //
    // Every input below is a shape measured out of real `git push --porcelain`
    // output (git 2.50.1) against a local bare repo, except where a case is
    // marked as reachable only through a hand-written refspec.
    //
    // These assert the RENDERED string, never `RefPair`, so they cannot be
    // satisfied by a classifier that is right about the structure and wrong
    // about the bytes the reader sees.

    /// F7 regression: an asymmetric refspec must name its **destination**.
    ///
    /// `git push origin feature:main` writes `main` on the remote, and
    /// `src:dst` exists precisely because the two sides differ — so the
    /// destination is the half that says what actually changed there.
    ///
    /// RED at the parent commit: `format_ref_pair` did not exist and the call
    /// site used `extract_short_ref`, which discards everything after the
    /// colon.  Measured at `c2b4378` against a hermetic bare remote, the
    /// refspec `fa:refs/heads/dst-aa` rendered as `* fa [new]` — the
    /// destination absent — while git itself printed
    /// ` * [new branch]      fa -> dst-aa`.
    ///
    /// The negative assertion is what makes this discriminating: a renderer
    /// that simply echoed the refspec verbatim would satisfy "the destination
    /// appears somewhere" while still failing to state the relationship.
    #[test]
    fn test_format_ref_pair_renders_asymmetric_refspec() {
        assert_eq!(
            format_ref_pair("refs/heads/feature:refs/heads/main"),
            "feature -> main"
        );
        assert_eq!(
            format_ref_pair("refs/heads/feature:refs/heads/main\t[new branch]"),
            "feature -> main",
            "the summary column must not leak into the ref pair"
        );
    }

    /// End-to-end through the porcelain parser: the rendered detail line, in
    /// full.  Pinning the whole line rather than a substring is deliberate — a
    /// `contains` assertion on the destination name is satisfied by the pre-fix
    /// output whenever the source side happens to contain it.
    #[test]
    fn test_parse_porcelain_asymmetric_refspec_names_both_sides() {
        let input = "*\trefs/heads/feature:refs/heads/main\t[new branch]\nDone\n";
        let result = parse_push(input);
        assert_eq!(result.summary, "1 pushed", "summary: {}", result.summary);
        assert_eq!(
            result.details,
            vec!["* feature -> main [new]".to_string()],
            "the destination ref must be rendered: {:?}",
            result.details
        );
        let rendered = format!("{result}");
        assert!(
            !rendered.contains("refs/heads/feature:refs/heads/main"),
            "the refspec must be rendered, not echoed verbatim: {rendered}"
        );
    }

    /// Equal sides collapse to a single name.  `src == dst` is the ordinary
    /// push, `main -> main` states one fact twice, and this render is graded by
    /// the ADR-001 net-savings guard — so inflating the common case would spend
    /// bytes to say nothing.
    ///
    /// Not a free green: it is the assertion that stops the fix from rewriting
    /// every ordinary push line.  A renderer that always emitted the arrow form
    /// would pass `test_format_ref_pair_renders_asymmetric_refspec` and fail
    /// here.
    #[test]
    fn test_format_ref_pair_equal_sides_collapse_to_one_name() {
        assert_eq!(
            format_ref_pair("refs/heads/main:refs/heads/main"),
            "main",
            "a symmetric refspec must not render an arrow"
        );
        assert_eq!(format_ref_pair("main:main"), "main");
        assert_eq!(
            format_ref_pair("refs/tags/v1.0:refs/tags/v1.0"),
            "v1.0",
            "tags shorten on both sides"
        );
        // Bare refspec: the destination is implicitly the source.  Reachable
        // through a hand-written refspec only — `extract_flag_and_rest`
        // requires a `refs/` prefix or a colon before a line reaches here.
        assert_eq!(format_ref_pair("main"), "main");
        assert_eq!(format_ref_pair("refs/heads/main"), "main");
    }

    /// A fully-qualified asymmetric pair across namespaces, plus the tag form.
    #[test]
    fn test_format_ref_pair_fully_qualified_refs_shorten_both_sides() {
        assert_eq!(
            format_ref_pair("refs/tags/v1.0:refs/tags/v2.0"),
            "v1.0 -> v2.0"
        );
        assert_eq!(
            format_ref_pair("refs/heads/topic:refs/remotes/upstream/topic"),
            "topic -> remotes/upstream/topic",
            "an unrecognised namespace keeps its remaining path (the `refs/` strip)"
        );
        // git's own field for `git push <remote> HEAD:refs/heads/x` — measured;
        // the source side is the literal `HEAD`, not a `refs/` path.
        assert_eq!(
            format_ref_pair("HEAD:refs/heads/feature"),
            "HEAD -> feature"
        );
    }

    /// A leading `+` (force) refspec renders as the pair, with the `+`
    /// consumed.
    ///
    /// Measured against git 2.50.1: git's porcelain `<from>` field never
    /// carries the `+` — `git push --porcelain +side:refs/heads/dst` emits
    /// `+\trefs/heads/side:refs/heads/dst\t<range> (forced update)`, moving
    /// force-ness into the FLAG column, which `try_parse_porcelain` already
    /// renders as `[forced]`.  So this case is defensive against a caller that
    /// hands the renderer a user-typed refspec, and dropping the `+` loses
    /// nothing: the flag column still says the push was forced.
    #[test]
    fn test_format_ref_pair_force_refspec_drops_the_plus() {
        assert_eq!(
            format_ref_pair("+refs/heads/feature:refs/heads/main"),
            "feature -> main"
        );
        assert_eq!(format_ref_pair("+feature:main"), "feature -> main");
        assert_eq!(
            format_ref_pair("+refs/heads/main:refs/heads/main"),
            "main",
            "a forced symmetric refspec still collapses"
        );
        // The genuine `+` FLAG line, end to end: the flag column is what
        // carries force-ness, and the ref pair rides alongside it.
        let input = "+\trefs/heads/side:refs/heads/dst\tba90096...4de3509 (forced update)\nDone\n";
        let result = parse_push(input);
        assert_eq!(
            result.details,
            vec!["+ side -> dst [forced]".to_string()],
            "details: {:?}",
            result.details
        );
    }

    /// An **empty source side** is git's real deleted-ref shape
    /// (`-\t:refs/heads/old\t[deleted]`, measured) and must render the
    /// destination, never a blank.
    ///
    /// This pins the renderer half of that contract only, in isolation.  The
    /// parse-through-render half — that the delete LINE git actually emits
    /// reaches this renderer and comes out naming the ref — is
    /// `test_format_ref_pair_empty_source_is_a_deletion` below, and the
    /// deleted-ref happy path is `test_deleted_ref_porcelain_happy_path`.
    #[test]
    fn test_format_ref_pair_empty_source_names_the_destination() {
        assert_eq!(format_ref_pair(":refs/heads/old"), "old");
        assert_eq!(
            format_ref_pair(":refs/heads/old\t[deleted]"),
            "old",
            "the summary column must not leak into the ref pair"
        );
        assert_eq!(format_ref_pair(":refs/tags/v1.0"), "v1.0");
        assert_ne!(
            format_ref_pair(":refs/heads/old"),
            "",
            "a deletion must never render a blank ref name"
        );
    }

    /// F8 regression: git's real deleted-ref **line**, parsed end to end.
    ///
    /// F7 fixed the render — `format_ref_pair` classifies an empty source side
    /// as `RefPair::Destination` and names it — and
    /// `test_format_ref_pair_empty_source_names_the_destination` pins that
    /// renderer in isolation.  What neither closed is the link between them:
    /// that the line `git push --porcelain --delete` actually emits reaches
    /// that renderer and comes out naming the ref.  This test is that link, so
    /// F8 changed **no production line** — it is a test-correctness commit.
    ///
    /// The input is measured, not assumed.  Against a local bare remote
    /// (git 2.50.1, ambient config nulled), `git push --porcelain --delete
    /// throwaway dst-ddd` emitted `-\t:refs/heads/dst-ddd\t[deleted]` — the
    /// source side **empty**, exactly the shape this module's header has
    /// recorded since AD-GP-2 was written.
    ///
    /// RED against the parent commit's renderer: `extract_short_ref` took the
    /// source side, so this same line rendered ` -  [deleted]` — a double
    /// space where the ref name belongs (measured at `c2b4378`:
    /// `20 2d 20 20 5b 64 65 6c 65 74 65 64 5d`, stdout 152 B, stderr 0 B,
    /// exit 0).  The reader was told something was deleted, and not what.
    ///
    /// The classifier arm is asserted through the `RefPair` variant on
    /// purpose: an empty source is a **named state**, not a blank string a
    /// call site guards with an `is_empty()` check, and pinning the variant is
    /// what stops the delete path from being re-special-cased later.
    #[test]
    fn test_format_ref_pair_empty_source_is_a_deletion() {
        // The classifier names the state.  A reclassification to `Unnamed("")`
        // or `Same("")` — either of which renders blank again — fails here
        // rather than silently, three layers downstream.
        match classify_ref_pair(":refs/heads/dst-ddd\t[deleted]") {
            RefPair::Destination(name) => assert_eq!(
                name, "dst-ddd",
                "the empty-source state must carry the destination's short name"
            ),
            other => panic!(
                "git's measured delete shape must classify as RefPair::Destination, \
                 got {other:?} — an empty source is a named state, not a blank \
                 string a renderer has to guard against"
            ),
        }

        // Parse through render, on the measured line, byte for byte.
        let input = "-\t:refs/heads/dst-ddd\t[deleted]\nDone\n";
        let result = parse_push(input);
        assert_eq!(result.summary, "1 deleted", "summary: {}", result.summary);
        assert_eq!(
            result.details,
            vec!["- dst-ddd [deleted]".to_string()],
            "the deleted ref must be named: {:?}",
            result.details
        );

        // The two discriminating negatives.  Each names a renderer that would
        // satisfy a `contains("deleted")` assertion and still fail the reader.
        let rendered = format!("{result}");
        assert!(
            !rendered.contains("-  [deleted]"),
            "the blank-name shape measured at `c2b4378` must not return: {rendered}"
        );
        assert!(
            !rendered.contains(":refs/heads/dst-ddd"),
            "the refspec must be rendered, not echoed verbatim: {rendered}"
        );
    }

    /// Equality is decided on the FULL sides, before shortening, so a pair that
    /// differs only in its namespace is never collapsed.  Shortening would make
    /// both sides read `x`, which would report a branch-to-tag push as an
    /// ordinary one — so the full names are shown instead.
    #[test]
    fn test_format_ref_pair_namespace_collision_shows_full_refs() {
        assert_eq!(
            format_ref_pair("refs/heads/x:refs/tags/x"),
            "refs/heads/x -> refs/tags/x"
        );
    }

    /// A field with no ref on either side is reported verbatim rather than as
    /// nothing (#317): the reader sees the token skim could not interpret.
    #[test]
    fn test_format_ref_pair_unnamed_field_is_reported_verbatim() {
        assert_eq!(format_ref_pair(":"), ":");
        assert_eq!(format_ref_pair(""), "");
    }

    /// The fast-forward (space-flag) line carries the ref pair too, alongside
    /// the range in its summary column.
    #[test]
    fn test_fast_forward_asymmetric_refspec_names_both_sides() {
        let input = " \trefs/heads/feature:refs/heads/main\te6bab99..13b30c2\nDone\n";
        let result = parse_push(input);
        assert_eq!(
            result.details,
            vec!["  feature -> main [fast-forward] e6bab99..13b30c2".to_string()],
            "details: {:?}",
            result.details
        );
    }

    /// The rejected and up-to-date flags render the pair on the same rule.
    /// `!` is the shape a reader most needs both halves of: a rejection names
    /// the remote ref that refused the write.
    #[test]
    fn test_rejected_and_up_to_date_render_the_ref_pair() {
        let rejected =
            parse_push("!\trefs/heads/div2:refs/heads/dst\t[rejected] (non-fast-forward)\nDone\n");
        assert_eq!(
            rejected.details,
            vec!["! div2 -> dst [rejected]".to_string()],
            "details: {:?}",
            rejected.details
        );
        let up_to_date = parse_push("=\trefs/heads/divergent:refs/heads/dst\t[up to date]\nDone\n");
        assert_eq!(
            up_to_date.details,
            vec!["= divergent -> dst [up to date]".to_string()],
            "details: {:?}",
            up_to_date.details
        );
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

    /// Happy-path: a real deleted-ref porcelain line produces a deleted-ref
    /// summary **that names the ref**.
    ///
    /// F8 corrected this test's fixture and its assertions; neither was sound.
    ///
    /// The fixture fed `-\trefs/heads/old:refs/heads/old\t[deleted]`, a
    /// **symmetric** refspec git never emits for a delete — contradicting this
    /// module's own header two hundred lines above, which has recorded the
    /// real shape since AD-GP-2 was written.  A real delete has an **empty
    /// source side**: measured against a local bare remote (git 2.50.1),
    /// `git push --porcelain --delete throwaway dst-ddd` emitted
    /// `-\t:refs/heads/dst-ddd\t[deleted]`.  The fixture below is that shape.
    ///
    /// The assertions were the half that mattered, and both were vacuous:
    /// `contains("deleted")` holds whatever the renderer does, because the
    /// literal comes from the summary column, and `contains("old")` was
    /// satisfied by the *source* side of the symmetric fixture this test
    /// should never have had.  Together they passed against a renderer that
    /// printed no ref name at all — measured at `c2b4378` as ` -  [deleted]`.
    /// A test that cannot fail retires the concern it was written for
    /// (PF-025), so the assertion now pins the whole rendered detail line.
    #[test]
    fn test_deleted_ref_porcelain_happy_path() {
        let input = "-\t:refs/heads/old\t[deleted]\nDone\n";
        let result = try_parse_porcelain(input);
        assert!(
            result.is_some(),
            "Deleted-ref porcelain line must be parsed"
        );
        let output = result.unwrap();
        assert_eq!(output.summary, "1 deleted", "summary: {}", output.summary);
        assert_eq!(
            output.details,
            vec!["- old [deleted]".to_string()],
            "the deleted ref must be named, not blank: {:?}",
            output.details
        );
        let rendered = format!("{output}");
        assert!(
            !rendered.contains("-  [deleted]"),
            "a blank ref name is the defect this fixture exists to catch: {rendered}"
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
