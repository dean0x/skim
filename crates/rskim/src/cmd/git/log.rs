//! Git log compression — commit log formatting.

use std::process::ExitCode;

use crate::cmd::execution as exec;
use crate::cmd::{OutputFormat, extract_output_format};
use crate::output::canonical::GitResult;
use crate::output::fidelity::Completeness;
use crate::runner::CommandRunner;

/// Run `git log` with compression.
///
/// Flag-aware passthrough lives one level up: `--format`, `--pretty`, `--stat`,
/// `--numstat`, `--name-only`, `--name-status`, `--shortstat`, `--raw` and
/// `--graph` are entries in `super::MACHINE_CONTRACT_FLAGS` — the flags whose
/// output this handler used to discard silently (ADR-022).
///
/// **That gate is disarmed by `--json`, so which of them reach this function
/// depends on the caller's view flag, and the two halves differ.**  With
/// `--json`, the flags that reshape the commit line
/// ([`COMMIT_SHAPE_BREAKING_FLAGS`] — `--format`, `--pretty`, `--graph`, `-z` /
/// `--null`) keep the gate armed and still never arrive, because the envelope
/// this handler would build for them is false rather than merely lossy.  The
/// stat family does arrive, and `parse_log` drops its blocks under a disclosed
/// ADR-011 class-1 count.  Without `--json` nothing in the set arrives at all.
///
/// `--oneline` is *not* in that set: it is handled here by stripping it and
/// injecting the equivalent `--format` flag — see `injected_log_format`.
///
/// Large-output degrade (ADR-002 / reliability-01 / #317): when `git log`
/// output exceeds the 64 MiB pipe cap, this function emits the bytes read so
/// far followed by an unconditional elision marker (ADR-011) rather than
/// returning an error.  The child process exits via SIGPIPE after the pipe
/// read-end is dropped.
pub(super) fn run_log(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    // Strip --oneline — handler injects the equivalent --format flag.
    let stripped_args: Vec<String> = args
        .iter()
        .filter(|a| !is_oneline_flag(a.as_str()))
        .cloned()
        .collect();

    let (filtered_args, output_format) = extract_output_format(&stripped_args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["log".to_string(), injected_log_format(args).to_string()]);
    full_args.extend_from_slice(&filtered_args);

    let label = super::build_analytics_label("log", args, show_stats, rec.enabled);

    // Use run_stdout_degrade so that output exceeding MAX_OUTPUT_BYTES yields
    // partial data + elision marker instead of a hard error.  Stderr keeps
    // the hard cap: git log stderr is normally empty or short, and a flooded
    // stderr is a real problem worth surfacing.
    let runner = CommandRunner::new();
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
    let (output, stdout_truncated) = runner.run_stdout_degrade("git", &arg_refs)?;

    // Forward real git errors (non-zero exit when WE did not truncate stdout).
    // When stdout was truncated, the child exited via SIGPIPE (our doing) and
    // exit_code reflects the signal, not a git-level error.
    if !stdout_truncated && output.exit_code != Some(0) {
        // Scrub credential URLs before forwarding (PF-024).
        let scrubbed_stderr = super::shared::scrub_lines(&output.stderr);
        if !scrubbed_stderr.is_empty()
            && exec::write_line_to_stderr(&scrubbed_stderr)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        let scrubbed_stdout = super::shared::scrub_lines(&output.stdout);
        if !scrubbed_stdout.is_empty()
            && exec::write_line_to_stdout(&scrubbed_stdout)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        let exit_code = output.exit_code;
        super::finalize_git_output_passthrough(
            scrubbed_stdout,
            label,
            show_stats,
            rec.with_tier("passthrough"),
            output.duration,
        );
        return Ok(match exit_code {
            Some(0) => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        });
    }

    let raw = output.stdout;
    let duration = output.duration;

    let result = parse_log(&raw);
    let parse_tier = result.parse_tier;

    // Loss-bearing elision marker: unconditional per ADR-011 (the caller has
    // less data than git produced — the truncation is real).
    let elision = if stdout_truncated {
        Some(crate::output::elision_marker_unbounded(
            "first 64 MiB",
            "commits",
        ))
    } else {
        None
    };

    // Emit the elision marker (when one is due) on stdout, exactly where the
    // former `println!("{marker}")` sat.  It is ADR-010 / ADR-011 class 1 —
    // loss-bearing and unconditional — but it is still stdout, so a departed
    // reader must stop the run rather than panic the process.
    let emit_elision = |elision: Option<&String>| -> anyhow::Result<exec::StdoutStatus> {
        match elision {
            Some(marker) => exec::write_line_to_stdout(marker),
            None => Ok(exec::StdoutStatus::Written),
        }
    };

    let (result_str, effective_tier) = match output_format {
        OutputFormat::Json => {
            // ADR-011 / D1 declaration — `Lossy`.
            //
            // Two independent drops, neither recoverable from the envelope:
            //   1. The handler injects `--format=%h %s (%cr) <%an>`, so every
            //      commit *body* below the subject line is gone before parsing.
            //   2. `parse_log` keeps only `is_commit_line` matches, so a
            //      `git log -p` patch body is filtered out entirely.
            //
            // Drop (2) is what the count measures: `details.len()` commit lines
            // kept out of `raw.lines().count()` lines git produced.  When the
            // two are equal (`git log` without `-p`) the marker falls back to
            // the countless wording, which still discloses drop (1).
            //
            // When stdout was truncated (`stdout_truncated`), the elision info
            // is folded into the JSON body as `stdout_elision` rather than
            // appended as plain text after the JSON document. Appending plain
            // text after the JSON would make stdout invalid JSON (breaking `jq`
            // and other consumers). The sink's unconditional stderr marker from
            // `Completeness::Lossy` is a double-disclosure in that case, but
            // keeping stdout a valid JSON document takes priority.
            let kept = result.details.len();
            let total = raw.lines().count();
            let mut json_val = serde_json::to_value(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize result: {e}"))?;
            if let Some(marker) = elision.as_ref() {
                json_val["stdout_elision"] = serde_json::Value::String(marker.clone());
            }
            let json = serde_json::to_string_pretty(&json_val)
                .map_err(|e| anyhow::anyhow!("failed to format result: {e}"))?;
            let elided = Some(exec::ElidedCount {
                kept,
                total,
                unit: "lines",
            });
            if exec::emit_json_envelope(
                &json,
                Completeness::Lossy,
                "git",
                elided,
                exec::LineTermination::Newline,
            )? == exec::StdoutStatus::PipeClosed
            {
                return Ok(exec::pipe_closed_exit());
            }
            // `emit_elision` is deliberately NOT called for JSON output: the
            // elision text is folded into the JSON body above (`stdout_elision`)
            // and the sink's stderr marker already discloses the data loss.
            (json, parse_tier)
        }
        OutputFormat::Text => {
            let s = result.to_string();
            let tier_str: Option<&'static str> = if parse_tier.is_some_and(|t| t == "passthrough") {
                // Already passthrough tier — skip guard, print as-is.
                if exec::write_line_to_stdout(&s)? == exec::StdoutStatus::PipeClosed
                    || emit_elision(elision.as_ref())? == exec::StdoutStatus::PipeClosed
                {
                    return Ok(exec::pipe_closed_exit());
                }
                parse_tier
            } else {
                match exec::savings_decision(&raw, &s) {
                    exec::SavingsDecision::Keep => {
                        if exec::write_line_to_stdout(&s)? == exec::StdoutStatus::PipeClosed
                            || emit_elision(elision.as_ref())? == exec::StdoutStatus::PipeClosed
                        {
                            return Ok(exec::pipe_closed_exit());
                        }
                        parse_tier
                    }
                    exec::SavingsDecision::Passthrough => {
                        // Even when passthrough wins, if we truncated stdout
                        // then the raw itself is incomplete — still emit the
                        // elision marker so the caller knows.
                        let (tier, status) = exec::emit_raw_passthrough(&raw)?;
                        if status == exec::StdoutStatus::PipeClosed
                            || emit_elision(elision.as_ref())? == exec::StdoutStatus::PipeClosed
                        {
                            return Ok(exec::pipe_closed_exit());
                        }
                        Some(tier)
                    }
                }
            };
            (s, tier_str)
        }
    };

    // Scrub credentials before analytics recording (PF-024).
    let analytics_raw = super::shared::scrub_lines(&raw);
    super::finalize_git_output_owned(
        analytics_raw,
        result_str,
        label,
        show_stats,
        rec.with_tier_opt(effective_tier),
        duration,
    );

    Ok(ExitCode::SUCCESS)
}

/// `--format` injected when the user asked for no particular density.
///
/// One line per commit: abbreviated hash, subject, relative date, author.
const LOG_FORMAT_DEFAULT: &str = "--format=%h %s (%cr) <%an>";

/// `--format` injected when the user asked for `--oneline`.
///
/// `git log --oneline` is `--pretty=oneline --abbrev-commit`, i.e. the
/// abbreviated hash, a space, the subject, and nothing else.  This string
/// reproduces those bytes exactly.
const LOG_FORMAT_ONELINE: &str = "--format=%h %s";

/// Recognise `--oneline` as the user's own token.
///
/// The strip filter and the injected-format choice MUST read argv through this
/// one predicate.  Stripping a density flag and then ignoring the density it
/// asked for is exactly the defect `injected_log_format` exists to prevent,
/// and two independent scans are how such a pair drifts apart.
fn is_oneline_flag(arg: &str) -> bool {
    arg == "--oneline"
}

/// Choose the `--format` string injected in place of the user's argv flags.
///
/// The handler replaces the user's formatting flags with one of its own so the
/// output has a parseable shape.  When the user asked for `--oneline`, injecting
/// the richer default makes skim's view **denser than the one the user
/// requested**, and the ADR-001 net-savings guard cannot catch it: that guard
/// baselines against the *injected* command's output, so it compares skim's
/// render to skim's own inflated raw (PF-024).  Measured on this repository,
/// `skim git log --oneline -6` emitted 670 bytes where `git log --oneline -6`
/// emits 496 — a 35% expansion by a wrapper whose purpose is compression — with
/// the guard present and correctly electing raw.
///
/// Honouring the requested density makes the injected command's output
/// byte-identical to the user's own, so the raw-fallback body *is* the user's
/// command output and the view can no longer exceed what it compresses.
///
/// This is deliberately not a second `git log` invocation (the `raw_override`
/// treatment `git status` gets): `git log` is unbounded by history, so a second
/// invocation can be arbitrarily expensive.
fn injected_log_format(args: &[String]) -> &'static str {
    if args.iter().any(|a| is_oneline_flag(a.as_str())) {
        LOG_FORMAT_ONELINE
    } else {
        LOG_FORMAT_DEFAULT
    }
}

/// Return `true` when `line` matches the `%h`-format commit-header shape:
/// a non-empty lowercase hex prefix followed by a space.
///
/// `git log --format=%h %s (%cr) <%an>` emits commit lines with a 7-char
/// abbreviated SHA1 (e.g. `abc1234 feat: ...`).  Patch lines from `-p` start
/// with `diff`, `index`, `@`, `+`, `-`, etc. — none of which are all-hex.
/// This filter prevents patch-body lines from inflating the commit count
/// (reliability-09).
///
/// # It reads a *prefix*, so any prefix git prepends defeats it
///
/// `split_once(' ')` takes whatever precedes the first space as the candidate
/// hash, so a rail character in front of the SHA is enough: under `--graph`
/// every commit line begins `* <hash>`, the candidate becomes `"*"`, no line
/// matches, and `parse_log` reports `no commits` for a non-empty range —
/// measured at `c2b4378` as 15 bytes of stdout, exit 0, empty stderr, for a
/// 3-commit range.  That is not a lossy summary but an *inverted answer*: a
/// reader records absence as positive evidence (the PF-021 shape).
///
/// `--graph` is therefore kept away from this function by **two** gates, and
/// both are load-bearing: it is in `super::MACHINE_CONTRACT_FLAGS`, *and* it is
/// in [`COMMIT_SHAPE_BREAKING_FLAGS`] so that `--json` does not stand the first
/// gate down.  The set membership alone was not enough — measured at `69d7d57`,
/// `skim git log --graph --json -n 2` reported `no commits` over a two-commit
/// range, because `--json` disarmed the gate and routed the rails straight back
/// here.
///
/// Widening this predicate instead was considered and rejected: it would produce
/// a reflowed graph whose rails no longer align, whereas the gate serves git's
/// own bytes.  Any *new* decorating flag needs the same treatment — an entry in
/// **both** sets, not a looser prefix rule here.
fn is_commit_line(line: &str) -> bool {
    line.split_once(' ')
        .map(|(hash, _)| !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or(false)
}

/// Contract flags that break the **commit-line shape** [`is_commit_line`]
/// matches, so `parse_log` can model nothing at all and the `--json` envelope
/// it would produce is FALSE rather than merely lossy.
///
/// # Membership rule: replaces / re-separates / prefixes — not "appends beside"
///
/// `parse_log` reads exactly one shape: the `%h`-prefixed line [`run_log`]
/// *itself* injects via [`injected_log_format`].  Three mechanisms destroy it,
/// and each member is here for exactly one of them:
///
/// - **replaces the format** — `--format`, `--pretty` carry a *user-supplied*
///   format string that git resolves last-wins, so it overrides the injected
///   one and skim cannot know what the result encodes.  Measured: a bare `%H`
///   emits a 40-hex SHA with no space, `is_commit_line` needs one, so every
///   line is filtered out and the envelope asserts `no commits` over a
///   two-commit range.  `--format=%H %s` would have parsed *by luck*; that the
///   answer depends on a string skim never reads is the whole argument for
///   serving raw.
/// - **replaces the record separator** — `-z` / `--null` terminate each commit
///   with NUL, so `output.lines()` sees the entire range as ONE line.  Measured
///   the worst of the four: the envelope claims `1 commit` for two *and*
///   smuggles a raw NUL into a JSON string value (`"…<t>\u0000a044f4e…"`), and
///   the class-1 marker degrades to the countless wording — the mechanism that
///   broke the count also broke the counter.
/// - **prefixes the line** — `--graph` writes `* <hash>`, so
///   `split_once(' ')` takes `"*"` as the candidate hash.  See
///   [`is_commit_line`] for the measurement.
///
/// The stat family (`--stat`, `--shortstat`, `--numstat`, `--name-only`,
/// `--name-status`, `--raw`) is deliberately **absent**.  Those append a block
/// *beside* the commit line rather than reshaping it, so the count stays true
/// and the dropped block is disclosed with an exact ADR-011 class-1 count
/// (measured: `13 lines omitted (2 of 15 shown)` for `--stat`).  That is the
/// disclosed-lossy route `cli_git_contract_flags.rs`'s
/// `json_with_a_contract_flag_still_serves_skims_envelope` and
/// `lossy_json_route_still_fires_its_class_one_marker` bless; arming the gate
/// for them is a separate decision, not this one.
///
/// # Why the list lives here and not beside `MACHINE_CONTRACT_FLAGS`
///
/// The two sets answer different questions with different owners.
/// `MACHINE_CONTRACT_FLAGS` asks "did the caller name a machine contract?" —
/// a question about *intent*, global to every git subcommand, which is why
/// `--format` / `--pretty` were hoisted out of this file into it.  This set
/// asks "can `parse_log` read it?" — a question about *this parser's* limits,
/// answerable only where `is_commit_line` is in view.  Every member must also
/// be a machine-contract flag or the gate never sees the argv; that subset
/// relation is pinned by
/// `super::tests::commit_shape_breaking_flags_are_a_subset_of_the_contract_set`.
const COMMIT_SHAPE_BREAKING_FLAGS: &[&str] = &["--format", "--pretty", "--graph", "--null"];

/// Short option characters that break the commit-line shape when they appear
/// **anywhere inside** a single-dash cluster.
///
/// `z` is the only member, for the record-separator reason given in
/// [`COMMIT_SHAPE_BREAKING_FLAGS`].  Cluster-aware for the same reason
/// `super::CONTRACT_SHORT_OPTS` is: an exact-token matcher here would be
/// *weaker* than the gate it narrows, which is a false negative manufactured by
/// the fix.
const COMMIT_SHAPE_BREAKING_SHORTS: &[char] = &['z'];

/// Whether `args` carry a flag whose payload `parse_log` provably cannot read.
///
/// Consulted by `super::json_envelope_would_misreport`, which is where the
/// decision this predicate feeds is documented.
pub(super) fn commit_shape_is_broken_by(args: &[String]) -> bool {
    super::args_match_flag_set(
        args,
        COMMIT_SHAPE_BREAKING_FLAGS,
        COMMIT_SHAPE_BREAKING_SHORTS,
    )
}

/// Parse formatted `git log` output into a compressed GitResult.
///
/// Only lines matching the `%h`-prefixed commit-header shape are counted as
/// commits; patch body lines (from `git log -p`) are excluded from both the
/// count and the details list (reliability-09).
fn parse_log(output: &str) -> GitResult {
    let lines: Vec<String> = output
        .lines()
        .filter(|l| is_commit_line(l))
        .map(str::to_string)
        .collect();

    let summary = match lines.len() {
        0 => "no commits".to_string(),
        1 => "1 commit".to_string(),
        n => format!("{n} commits"),
    };

    GitResult::new("log".to_string(), summary, lines).with_tier("full")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // injected format density tests
    // ========================================================================

    /// Regression: `skim git log --oneline` must not serve a denser view than
    /// `--oneline` asked for.  The ADR-001 guard cannot catch this — it
    /// baselines against the injected command's own output (PF-024) — so the
    /// injected format is where the density has to be honoured.
    #[test]
    fn injected_log_format_honours_oneline_density() {
        let args = vec!["--oneline".to_string(), "-6".to_string()];
        assert_eq!(injected_log_format(&args), LOG_FORMAT_ONELINE);
    }

    #[test]
    fn injected_log_format_defaults_when_oneline_absent() {
        let args = vec!["-6".to_string()];
        assert_eq!(injected_log_format(&args), LOG_FORMAT_DEFAULT);
    }

    /// Pin the density claim itself, not just the string: `git log --oneline`
    /// emits an abbreviated hash and a subject, so the injected equivalent must
    /// carry no additional placeholder.
    #[test]
    fn oneline_format_adds_no_field_beyond_hash_and_subject() {
        assert_eq!(LOG_FORMAT_ONELINE, "--format=%h %s");
        for placeholder in ["%cr", "%an", "%ae", "%cd", "%d", "%b"] {
            assert!(
                !LOG_FORMAT_ONELINE.contains(placeholder),
                "--oneline must not be enriched with {placeholder}"
            );
        }
    }

    #[test]
    fn is_oneline_flag_matches_the_bare_token_only() {
        assert!(is_oneline_flag("--oneline"));
        assert!(!is_oneline_flag("--oneline=1"));
        assert!(!is_oneline_flag("--online"));
        assert!(!is_oneline_flag("-o"));
    }

    /// The strip filter and the format choice must agree on what `--oneline`
    /// is: a token removed from argv whose requested density is then ignored is
    /// precisely the defect.
    #[test]
    fn oneline_is_both_stripped_and_honoured() {
        let args = vec!["--oneline".to_string(), "-6".to_string()];
        let stripped: Vec<&str> = args
            .iter()
            .filter(|a| !is_oneline_flag(a.as_str()))
            .map(String::as_str)
            .collect();
        assert_eq!(stripped, vec!["-6"], "--oneline is stripped from argv");
        assert_eq!(
            injected_log_format(&args),
            LOG_FORMAT_ONELINE,
            "and the format it asked for is what replaces it"
        );
    }

    // ========================================================================
    // is_commit_line tests
    // ========================================================================

    #[test]
    fn is_commit_line_accepts_valid_hex_prefix() {
        assert!(is_commit_line("abc1234 feat: add parser"));
        assert!(is_commit_line("def5678 fix: edge case"));
        assert!(is_commit_line("0123456 chore: bump deps"));
    }

    #[test]
    fn is_commit_line_rejects_non_hex_chars() {
        // 'g', 'h', 'i', etc. are not hex digits.
        assert!(!is_commit_line("012efgh docs: bad hash"));
        assert!(!is_commit_line("345ijkl chore: bad hash"));
    }

    #[test]
    fn is_commit_line_rejects_patch_body_lines() {
        assert!(!is_commit_line("diff --git a/src/lib.rs b/src/lib.rs"));
        assert!(!is_commit_line("index abc1234..def5678 100644"));
        assert!(!is_commit_line("@@ -1,3 +1,4 @@"));
        assert!(!is_commit_line("+added line"));
        assert!(!is_commit_line("-removed line"));
        assert!(!is_commit_line(" context line"));
    }

    #[test]
    fn is_commit_line_rejects_empty_and_no_space() {
        assert!(!is_commit_line(""));
        assert!(!is_commit_line("abc1234"));
    }

    // ========================================================================
    // parse_log tests
    // ========================================================================

    #[test]
    fn test_parse_log_format() {
        let output = include_str!("../../../tests/fixtures/cmd/git/log_format.txt");
        let result = parse_log(output);

        assert!(
            result.summary.contains("5 commits"),
            "expected '5 commits' in summary, got: {}",
            result.summary
        );
        assert_eq!(result.details.len(), 5, "expected 5 commit lines");
    }

    #[test]
    fn test_parse_log_single_commit() {
        let output = "abc1234 feat: initial commit (1 day ago) <Author>\n";
        let result = parse_log(output);
        assert_eq!(result.summary, "1 commit");
        assert_eq!(result.details.len(), 1);
    }

    #[test]
    fn test_parse_log_empty() {
        let result = parse_log("");
        assert_eq!(result.summary, "no commits");
        assert!(result.details.is_empty());
    }

    /// AD-GIT-12: parse_tier must be propagated so analytics can bucket git log
    /// invocations by tier. The log parser always succeeds (no fallback tiers),
    /// so every result is tagged `"full"`.
    #[test]
    fn test_parse_log_parse_tier_is_full() {
        let result = parse_log("abc1234 feat: init (1 day ago) <Author>\n");
        assert_eq!(
            result.parse_tier,
            Some("full"),
            "git log parser must tag parse_tier as 'full' (AD-GIT-12)"
        );
    }

    /// Regression: parse_log must count only commit-header lines, not patch
    /// body lines produced by `git log -p` (reliability-09).
    #[test]
    fn parse_log_with_patch_body_counts_only_commit_headers() {
        // Simulated `git log -p` output: 2 commits each with a patch hunk.
        let output = "\
abc1234 feat: add parser (1 day ago) <Alice>
diff --git a/src/lib.rs b/src/lib.rs
index 000000..abc1234 100644
--- /dev/null
+++ b/src/lib.rs
@@ -0,0 +1,3 @@
+pub fn parse() {}
def5678 fix: edge case (2 days ago) <Bob>
diff --git a/src/lib.rs b/src/lib.rs
index abc1234..def5678 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
+// comment
 pub fn parse() {}
";
        let result = parse_log(output);
        assert_eq!(
            result.summary, "2 commits",
            "patch body lines must not inflate commit count"
        );
        assert_eq!(
            result.details.len(),
            2,
            "details must contain only commit-header lines"
        );
    }

    /// Regression: lines whose hash prefix contains non-hex characters must be
    /// excluded from the commit count (reliability-09).
    #[test]
    fn parse_log_excludes_non_hex_prefix_lines() {
        let output = "\
abc1234 valid commit (1 day ago) <Alice>
012efgh invalid hash (2 days ago) <Bob>
345ijkl another invalid (3 days ago) <Charlie>
";
        let result = parse_log(output);
        assert_eq!(
            result.summary, "1 commit",
            "only the valid hex-prefix line should be counted"
        );
    }
}
