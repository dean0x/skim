//! Git output compression subcommand (#50, #103)
//!
//! Executes git commands and compresses output for LLM context windows.
//! Supports `status`, `diff`, and `log` subcommands with flag-aware
//! passthrough: when the user already specifies a compact format flag,
//! output is passed through unmodified.
//!
//! The `diff` subcommand uses an AST-aware pipeline (#103): it parses
//! unified diff output, overlays changed line ranges on tree-sitter ASTs,
//! and renders changed nodes with full function boundaries and standard
//! `+`/`-` markers.

// Private: only accessed via run() dispatch in this module
mod diff;
mod fetch;
mod log;
mod show;
mod status;

use std::process::ExitCode;

use crate::cmd::OutputFormat;
use crate::output::canonical::GitResult;
use crate::runner::CommandRunner;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `git` subcommand.
///
/// Dispatches to `status`, `diff`, `log`, `show`, etc., or prints help.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle --help / -h at the `skim git` level: only when the first
    // non-global-flag token is the help flag (e.g., `skim git --help`),
    // not when it appears deeper inside a subcommand (`skim git show --help`).
    if args.is_empty()
        || args
            .first()
            .is_some_and(|a| matches!(a.as_str(), "--help" | "-h"))
    {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);

    let (global_flags, rest) = split_global_flags(&filtered_args);

    let Some(subcmd) = rest.first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let subcmd_args = &rest[1..];

    match subcmd.as_str() {
        "status" => status::run_status(&global_flags, subcmd_args, show_stats),
        "diff" => diff::run_diff(&global_flags, subcmd_args, show_stats),
        "fetch" => fetch::run_fetch(&global_flags, subcmd_args, show_stats),
        "log" => log::run_log(&global_flags, subcmd_args, show_stats),
        "show" => show::run_show(&global_flags, subcmd_args, show_stats),
        other => {
            let safe_other = crate::cmd::sanitize_for_display(other);
            anyhow::bail!(
                "unknown git subcommand: '{safe_other}'\n\n\
                 Supported: status, diff, fetch, log, show\n\
                 Run 'skim git --help' for usage"
            );
        }
    }
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    println!("skim git <status|diff|fetch|log|show> [args...]");
    println!();
    println!("  Compress git command output for LLM context windows.");
    println!();
    println!("Subcommands:");
    println!("  status    Show compressed working tree status");
    println!("  diff      AST-aware diff with full function boundaries");
    println!("  fetch     Show compressed fetch summary (new branches, tags, pruned)");
    println!("  log       Show compressed commit log");
    println!("  show      Show compressed commit or file content at a ref");
    println!();
    println!("Global git flags (before subcommand):");
    println!("  -C <path>    Run as if git was started in <path>");
    println!("  --git-dir    Set the path to the repository");
    println!("  --work-tree  Set the path to the working tree");
    println!();
    println!("Flags (all subcommands):");
    println!("  --json           Machine-readable JSON output");
    println!("  --show-stats     Show token savings statistics");
    println!();
    println!("Examples:");
    println!("  skim git status");
    println!("  skim git status --json");
    println!("  skim git diff --cached");
    println!("  skim git diff --mode structure");
    println!("  skim git diff main..feature --json");
    println!("  skim git fetch");
    println!("  skim git fetch --prune");
    println!("  skim git log -n 5");
    println!("  skim git show HEAD");
    println!("  skim git show HEAD:src/main.rs");
    println!("  skim git diff --help                   Diff-specific options");
    println!("  skim git show --help                   Show-specific options");
}

// ============================================================================
// Global flag splitting
// ============================================================================

/// Split leading git global flags (e.g., `-C <path>`, `--git-dir=...`)
/// from the subcommand and its arguments.
///
/// Git global flags appear before the subcommand:
///   `git -C /path --no-pager status --short`
///         ^^^^^^^^^^^^^^^^^^ global  ^^^^^^ subcommand args
///
/// Returns `(global_flags, rest)` where `rest[0]` is the subcommand name.
fn split_global_flags(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut global_flags = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // Flags that consume a following value
        if matches!(arg.as_str(), "-C" | "--git-dir" | "--work-tree" | "-c") {
            global_flags.push(arg.clone());
            if i + 1 < args.len() {
                global_flags.push(args[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        // Flags with embedded value (--git-dir=..., --work-tree=...)
        if arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("-c=")
        {
            global_flags.push(arg.clone());
            i += 1;
            continue;
        }

        // Boolean global flags
        if matches!(
            arg.as_str(),
            "--no-pager" | "--bare" | "--no-replace-objects" | "--no-optional-locks"
        ) {
            global_flags.push(arg.clone());
            i += 1;
            continue;
        }

        // Not a global flag — this is the subcommand (or subcommand arg)
        break;
    }

    let rest = args[i..].to_vec();
    (global_flags, rest)
}

// ============================================================================
// Helpers
// ============================================================================

/// Check whether the user has specified a limit flag (`-n`, `--max-count`).
fn has_limit_flag(args: &[String]) -> bool {
    args.iter()
        .any(|a| a.starts_with("-n") || a == "--max-count" || a.starts_with("--max-count="))
}

/// Build the analytics label string for a git subcommand invocation.
///
/// Returns `"skim git {subcmd} {args}"` when either `--show-stats` or analytics
/// recording is active, and an empty `String` otherwise.  This avoids an
/// unconditional `format!` allocation on the hot path when both flags are off.
///
/// All six parsed-command handlers (`show` ×2, `diff`, `status`, `log`, `fetch`)
/// share this exact guard logic; centralising it here eliminates the repeated
/// five-line block at each call site.
pub(super) fn build_analytics_label(subcmd: &str, args: &[String], show_stats: bool) -> String {
    if show_stats || crate::analytics::is_analytics_enabled() {
        format!("skim git {subcmd} {}", args.join(" "))
    } else {
        String::new()
    }
}

/// Record token stats and fire-and-forget analytics for any git handler.
///
/// Centralises the analytics + stats tail that previously appeared inline in
/// `run_passthrough`, `run_parsed_command`, and the deleted `record_show_result`.
///
/// Three variants are provided to minimise allocations:
///
/// - [`finalize_git_output`] — borrowed variant; for the rare case where
///   `raw != output` but the caller still holds `&str` references.  Performs
///   one `.to_string()` clone per argument when analytics are enabled.
///   **Not used in production call sites currently — prefer the owned or
///   passthrough variants.**
///
/// - [`finalize_git_output_owned`] — owned variant; callers that already own
///   the `String` and raw ≠ output (e.g. `run_parsed_command`, `run_diff`,
///   `emit_show_commit`) move the values directly into the analytics call —
///   zero extra allocations on the analytics path, and still zero allocations
///   when analytics are off.
///
/// - [`finalize_git_output_passthrough`] — passthrough variant; for all call
///   sites where raw == output (no compression occurred).  Clones once instead
///   of twice when analytics are enabled (PF-018).
///
/// # Parameters (shared by all variants)
/// - `raw`          — Original git output before any compression.
/// - `output`       — Compressed output (may equal `raw` for passthrough).
/// - `label`        — Command label stored in the analytics DB.
/// - `show_stats`   — Whether to print token-savings stats to stderr.
/// - `command_type` — Analytics command-type tag (e.g., `CommandType::Git`).
/// - `duration`     — Wall-clock duration of the underlying git command.
/// - `parse_tier`   — Optional tier label set by the parser (AD-12).
// Allow dead_code: used in tests (see `test_finalize_git_output_accepts_empty_strings`)
// and available as a fallback for future handlers where raw != output but no
// owned String is available.
#[allow(dead_code)]
pub(super) fn finalize_git_output(
    raw: &str,
    output: &str,
    label: String,
    show_stats: bool,
    command_type: crate::analytics::CommandType,
    duration: std::time::Duration,
    parse_tier: Option<&'static str>,
) {
    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(raw, output);
        crate::process::report_token_stats(orig, comp, "");
    }
    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            raw.to_string(),
            output.to_string(),
            label,
            command_type,
            duration,
            parse_tier,
        );
    }
}

/// Owned variant of [`finalize_git_output`].
///
/// Takes ownership of `raw` and `output`, moving them directly into the
/// analytics call when analytics are enabled — eliminating the extra
/// `.to_string()` clone that the borrowed variant must perform.  When
/// analytics are disabled neither string is heap-touched after the stats
/// computation (which only needs `&str`).
///
/// Use this variant in handlers that already own their output strings
/// (i.e. the string would be dropped immediately after the call anyway).
///
/// `parse_tier` is forwarded to the analytics record (AD-12).
pub(super) fn finalize_git_output_owned(
    raw: String,
    output: String,
    label: String,
    show_stats: bool,
    command_type: crate::analytics::CommandType,
    duration: std::time::Duration,
    parse_tier: Option<&'static str>,
) {
    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&raw, &output);
        crate::process::report_token_stats(orig, comp, "");
    }
    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            raw,
            output,
            label,
            command_type,
            duration,
            parse_tier,
        );
    }
}

/// Passthrough variant of [`finalize_git_output`].
///
/// Use this when `raw` and `output` are **the same string** (passthrough
/// semantics: no compression occurred).  Clones the buffer **once** when
/// analytics are enabled instead of the **twice** the borrowed variant would
/// perform when called with `raw == output` (PF-018 resolution).
///
/// Call sites: `run_passthrough`, `run_parsed_command` non-zero exit,
/// `run_diff` non-zero exit / empty diff / empty-after-parse, and the
/// equivalent failure paths in `show.rs`.
pub(super) fn finalize_git_output_passthrough(
    raw: &str,
    label: String,
    show_stats: bool,
    command_type: crate::analytics::CommandType,
    duration: std::time::Duration,
    parse_tier: Option<&'static str>,
) {
    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(raw, raw);
        crate::process::report_token_stats(orig, comp, "");
    }
    if crate::analytics::is_analytics_enabled() {
        // Clone once: both `raw_text` and `compressed_text` are the same
        // string, so we clone once and reuse.
        let s = raw.to_string();
        crate::analytics::try_record_command(
            s.clone(),
            s,
            label,
            command_type,
            duration,
            parse_tier,
        );
    }
}

/// Convert an optional exit code to an ExitCode.
fn map_exit_code(code: Option<i32>) -> ExitCode {
    match code {
        Some(0) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// Run a git command with passthrough (no parsing).
fn run_passthrough(
    global_flags: &[String],
    subcmd: &str,
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push(subcmd.to_string());
    full_args.extend_from_slice(args);

    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
    let output = runner.run("git", &arg_refs)?;

    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    // Passthrough: raw == compressed. Use the single-clone passthrough variant
    // to avoid cloning the same buffer twice (PF-018).  Label is built lazily
    // via build_analytics_label so the format! is skipped when both
    // show_stats and analytics are disabled (PF-021).
    finalize_git_output_passthrough(
        &output.stdout,
        build_analytics_label(subcmd, args, show_stats),
        show_stats,
        crate::analytics::CommandType::Git,
        output.duration,
        Some("passthrough"),
    );

    Ok(map_exit_code(output.exit_code))
}

/// Run a git command and parse its output with the given parser function.
///
/// Callers are responsible for baking global flags into `subcmd_args` before
/// calling this function.
///
/// `label` is the analytics label string built by the caller from the user's
/// **original** (pre-rewrite) args via [`build_analytics_label`].
///
/// When `combine_stderr` is `true`, the parser receives `stderr + stdout`
/// combined. Git fetch writes its output to stderr, so fetch uses `true`;
/// all other subcommands use `false` (stdout only).
pub(super) fn run_parsed_command<F>(
    subcmd_args: &[String],
    show_stats: bool,
    output_format: OutputFormat,
    combine_stderr: bool,
    label: String,
    parser: F,
) -> anyhow::Result<ExitCode>
where
    F: FnOnce(&str) -> GitResult,
{
    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = subcmd_args.iter().map(String::as_str).collect();
    let output = runner.run("git", &arg_refs)?;

    if output.exit_code != Some(0) {
        // On failure, pass through stderr
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        // Record analytics even on non-zero exit so the DB reflects failed
        // invocations. raw == compressed here; use single-clone passthrough
        // variant to avoid cloning the same buffer twice (PF-018).
        finalize_git_output_passthrough(
            &output.stdout,
            label,
            show_stats,
            crate::analytics::CommandType::Git,
            output.duration,
            Some("passthrough"),
        );
        return Ok(map_exit_code(output.exit_code));
    }

    // Git fetch writes to stderr; other subcommands write to stdout.
    let raw: String = if combine_stderr {
        format!("{}\n{}", output.stderr, output.stdout)
    } else {
        output.stdout
    };

    let result = parser(&raw);
    // Capture parse_tier before result is consumed by rendering.
    let parse_tier = result.parse_tier;
    let result_str = match output_format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize result: {e}"))?;
            println!("{json}");
            json
        }
        OutputFormat::Text => {
            let s = result.to_string();
            println!("{s}");
            s
        }
    };

    // Both `raw` and `result_str` are owned here and consumed at end-of-function;
    // use the owned variant to move them directly rather than cloning.
    // `label` is supplied by the caller from the user's original (pre-rewrite) args
    // so the analytics DB records the invocation as the user typed it.
    // `parse_tier` propagates the parser's tier annotation to the analytics DB (AD-12).
    finalize_git_output_owned(
        raw,
        result_str,
        label,
        show_stats,
        crate::analytics::CommandType::Git,
        output.duration,
        parse_tier,
    );

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::user_has_flag;

    // ========================================================================
    // split_global_flags tests
    // ========================================================================

    #[test]
    fn test_split_no_global_flags() {
        let args: Vec<String> = vec!["status".into(), "--short".into()];
        let (global, rest) = split_global_flags(&args);
        assert!(global.is_empty());
        assert_eq!(rest, vec!["status", "--short"]);
    }

    #[test]
    fn test_split_with_c_flag() {
        let args: Vec<String> = vec!["-C".into(), "/tmp".into(), "status".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["-C", "/tmp"]);
        assert_eq!(rest, vec!["status"]);
    }

    #[test]
    fn test_split_with_git_dir_equals() {
        let args: Vec<String> = vec!["--git-dir=/repo/.git".into(), "log".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["--git-dir=/repo/.git"]);
        assert_eq!(rest, vec!["log"]);
    }

    #[test]
    fn test_split_with_no_pager() {
        let args: Vec<String> = vec!["--no-pager".into(), "diff".into(), "--cached".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["--no-pager"]);
        assert_eq!(rest, vec!["diff", "--cached"]);
    }

    #[test]
    fn test_split_multiple_global_flags() {
        let args: Vec<String> = vec![
            "-C".into(),
            "/tmp".into(),
            "--no-pager".into(),
            "status".into(),
        ];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["-C", "/tmp", "--no-pager"]);
        assert_eq!(rest, vec!["status"]);
    }

    // ========================================================================
    // --no-optional-locks global flag
    // ========================================================================

    #[test]
    fn test_split_with_no_optional_locks() {
        let args: Vec<String> = vec!["--no-optional-locks".into(), "status".into()];
        let (global, rest) = split_global_flags(&args);
        assert_eq!(global, vec!["--no-optional-locks"]);
        assert_eq!(rest, vec!["status"]);
    }

    // ========================================================================
    // Passthrough flag detection tests
    // ========================================================================

    #[test]
    fn test_status_passthrough_with_porcelain() {
        assert!(user_has_flag(
            &["--porcelain".to_string()],
            &["--porcelain", "--short", "-s"]
        ));
    }

    #[test]
    fn test_status_passthrough_with_short() {
        assert!(user_has_flag(
            &["-s".to_string()],
            &["--porcelain", "--short", "-s"]
        ));
    }

    #[test]
    fn test_diff_passthrough_with_name_only() {
        assert!(user_has_flag(
            &["--name-only".to_string()],
            &["--stat", "--name-only", "--name-status"]
        ));
    }

    #[test]
    fn test_diff_no_passthrough_without_flag() {
        assert!(!user_has_flag(
            &["--cached".to_string()],
            &["--stat", "--name-only", "--name-status"]
        ));
    }

    #[test]
    fn test_log_passthrough_with_oneline() {
        assert!(user_has_flag(
            &["--oneline".to_string()],
            &["--format", "--pretty", "--oneline"]
        ));
    }

    #[test]
    fn test_log_passthrough_with_format() {
        assert!(user_has_flag(
            &["--format".to_string()],
            &["--format", "--pretty", "--oneline"]
        ));
    }

    // ========================================================================
    // user_has_flag / map_exit_code helpers
    // ========================================================================

    #[test]
    fn test_user_has_flag_empty_args() {
        assert!(!user_has_flag(&[], &["--flag"]));
    }

    #[test]
    fn test_map_exit_code_success() {
        let code = map_exit_code(Some(0));
        // ExitCode doesn't impl PartialEq, so compare via Debug
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }

    #[test]
    fn test_map_exit_code_failure() {
        let code = map_exit_code(Some(1));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn test_map_exit_code_none() {
        let code = map_exit_code(None);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    // ========================================================================
    // has_limit detection for log
    // ========================================================================

    #[test]
    fn test_log_detects_n_flag() {
        let args: Vec<String> = vec!["-n".into(), "10".into()];
        assert!(has_limit_flag(&args));
    }

    #[test]
    fn test_log_detects_max_count() {
        let args: Vec<String> = vec!["--max-count=5".into()];
        assert!(has_limit_flag(&args));
    }

    #[test]
    fn test_log_no_limit_flag() {
        let args: Vec<String> = vec!["--all".into()];
        assert!(!has_limit_flag(&args));
    }

    // ========================================================================
    // Prefix-match passthrough (--format=%H, --porcelain=v2)
    // ========================================================================

    #[test]
    fn test_log_passthrough_with_format_equals() {
        assert!(user_has_flag(
            &["--format=%H".to_string()],
            &["--format", "--pretty", "--oneline"]
        ));
    }

    #[test]
    fn test_status_passthrough_with_porcelain_v2() {
        assert!(user_has_flag(
            &["--porcelain=v2".to_string()],
            &["--porcelain", "--short", "-s"]
        ));
    }

    // ========================================================================
    // --check passthrough for diff
    // ========================================================================

    #[test]
    fn test_diff_passthrough_with_check() {
        assert!(user_has_flag(
            &["--check".to_string()],
            &["--stat", "--name-only", "--name-status", "--check"]
        ));
    }

    // ========================================================================
    // --shortstat and --numstat passthrough for diff
    // ========================================================================

    #[test]
    fn test_diff_passthrough_with_shortstat() {
        assert!(user_has_flag(
            &["--shortstat".to_string()],
            &[
                "--stat",
                "--shortstat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--check"
            ]
        ));
    }

    #[test]
    fn test_diff_passthrough_with_numstat() {
        assert!(user_has_flag(
            &["--numstat".to_string()],
            &[
                "--stat",
                "--shortstat",
                "--numstat",
                "--name-only",
                "--name-status",
                "--check"
            ]
        ));
    }

    // ========================================================================
    // Non-zero exit analytics documentation
    // ========================================================================

    /// Documents that `run_parsed_command` records analytics on non-zero exit.
    ///
    /// Previously, a non-zero exit returned early without recording, causing
    /// failed invocations (e.g., `git log` on a bare repo) to be invisible in
    /// the analytics DB. The fix calls `finalize_git_output` on the error path
    /// with raw==compressed (passthrough semantics) so the DB is consistent.
    ///
    /// This test validates `finalize_git_output` itself is callable with
    /// empty strings (the non-zero path uses empty stdout on most failures).
    #[test]
    fn test_finalize_git_output_accepts_empty_strings() {
        // Analytics is fire-and-forget; we just ensure the call path compiles
        // and does not panic. SKIM_DISABLE_ANALYTICS=1 suppresses DB writes.
        std::env::set_var("SKIM_DISABLE_ANALYTICS", "1");
        finalize_git_output(
            "",
            "",
            "skim git log".to_string(),
            false,
            crate::analytics::CommandType::Git,
            std::time::Duration::ZERO,
            None,
        );
        std::env::remove_var("SKIM_DISABLE_ANALYTICS");
    }
}
