//! Git log compression — commit log formatting.

use std::process::ExitCode;

use crate::cmd::{OutputFormat, extract_output_format, user_has_flag};
use crate::output::canonical::GitResult;
use crate::runner::CommandRunner;

use super::run_passthrough;

/// Run `git log` with compression.
///
/// Flag-aware passthrough: if user has `--format` or `--pretty` (custom
/// format strings that cannot be parsed generically), pass through unmodified.
/// `--oneline` is handled by stripping it and injecting the handler's own
/// `--format` flag instead.
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
    if user_has_flag(args, &["--format", "--pretty"]) {
        return run_passthrough(global_flags, "log", args, show_stats, rec);
    }

    // Strip --oneline — handler injects its own --format flag.
    let stripped_args: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--oneline")
        .cloned()
        .collect();

    let (filtered_args, output_format) = extract_output_format(&stripped_args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["log".to_string(), "--format=%h %s (%cr) <%an>".to_string()]);
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
        if !scrubbed_stderr.is_empty() {
            eprintln!("{scrubbed_stderr}");
        }
        let scrubbed_stdout = super::shared::scrub_lines(&output.stdout);
        if !scrubbed_stdout.is_empty() {
            println!("{scrubbed_stdout}");
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

    let (result_str, effective_tier) = match output_format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize result: {e}"))?;
            println!("{json}");
            if let Some(ref marker) = elision {
                println!("{marker}");
            }
            (json, parse_tier)
        }
        OutputFormat::Text => {
            let s = result.to_string();
            let tier_str: Option<&'static str> = if parse_tier.is_some_and(|t| t == "passthrough") {
                // Already passthrough tier — skip guard, print as-is.
                println!("{s}");
                if let Some(ref marker) = elision {
                    println!("{marker}");
                }
                parse_tier
            } else {
                match crate::cmd::execution::savings_decision(&raw, &s) {
                    crate::cmd::execution::SavingsDecision::Keep => {
                        println!("{s}");
                        if let Some(ref marker) = elision {
                            println!("{marker}");
                        }
                        parse_tier
                    }
                    crate::cmd::execution::SavingsDecision::Passthrough => {
                        // Even when passthrough wins, if we truncated stdout
                        // then the raw itself is incomplete — still emit the
                        // elision marker so the caller knows.
                        let tier = crate::cmd::execution::emit_raw_passthrough(&raw)?;
                        if let Some(ref marker) = elision {
                            println!("{marker}");
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

/// Return `true` when `line` matches the `%h`-format commit-header shape:
/// a non-empty lowercase hex prefix followed by a space.
///
/// `git log --format=%h %s (%cr) <%an>` emits commit lines with a 7-char
/// abbreviated SHA1 (e.g. `abc1234 feat: ...`).  Patch lines from `-p` start
/// with `diff`, `index`, `@`, `+`, `-`, etc. — none of which are all-hex.
/// This filter prevents patch-body lines from inflating the commit count
/// (reliability-09).
fn is_commit_line(line: &str) -> bool {
    line.split_once(' ')
        .map(|(hash, _)| !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or(false)
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
