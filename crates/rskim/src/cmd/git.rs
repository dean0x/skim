//! Git output compression subcommand (#50)
//!
//! Executes git commands and compresses output for LLM context windows.
//! Supports `status`, `diff`, and `log` subcommands with flag-aware
//! passthrough: when the user already specifies a compact format flag,
//! output is passed through unmodified.

use std::process::ExitCode;

use regex::Regex;

use crate::output::canonical::GitResult;
use crate::runner::CommandRunner;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `git` subcommand.
///
/// Dispatches to `status`, `diff`, or `log` parsers, or prints help.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle --help / -h at the git level
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (global_flags, rest) = split_global_flags(args);

    let Some(subcmd) = rest.first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let subcmd_args = &rest[1..];

    match subcmd.as_str() {
        "status" => run_status(&global_flags, subcmd_args),
        "diff" => run_diff(&global_flags, subcmd_args),
        "log" => run_log(&global_flags, subcmd_args),
        other => {
            anyhow::bail!(
                "unknown git subcommand: '{other}'\n\n\
                 Supported: status, diff, log\n\
                 Run 'skim git --help' for usage"
            );
        }
    }
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    println!("skim git <status|diff|log> [args...]");
    println!();
    println!("  Compress git command output for LLM context windows.");
    println!();
    println!("Subcommands:");
    println!("  status    Show compressed working tree status");
    println!("  diff      Show compressed diff statistics");
    println!("  log       Show compressed commit log");
    println!();
    println!("Global git flags (before subcommand):");
    println!("  -C <path>    Run as if git was started in <path>");
    println!("  --git-dir    Set the path to the repository");
    println!("  --work-tree  Set the path to the working tree");
    println!();
    println!("Examples:");
    println!("  skim git status");
    println!("  skim git -C /path/to/repo status");
    println!("  skim git diff --cached");
    println!("  skim git log -n 5");
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
        if arg == "--no-pager" || arg == "--bare" || arg == "--no-replace-objects" {
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

/// Check whether any of `flags` appears in `args`.
fn user_has_flag(args: &[String], flags: &[&str]) -> bool {
    args.iter().any(|a| flags.contains(&a.as_str()))
}

/// Convert an optional exit code to an ExitCode.
fn exit_code_to_process(code: Option<i32>) -> ExitCode {
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
) -> anyhow::Result<ExitCode> {
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push(subcmd.to_string());
    full_args.extend_from_slice(args);

    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();
    let output = runner.run("git", &arg_refs)?;

    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    Ok(exit_code_to_process(output.exit_code))
}

/// Run a git command and parse its output with the given parser function.
fn run_parsed_command<F>(
    global_flags: &[String],
    subcmd_args: &[String],
    parser: F,
) -> anyhow::Result<ExitCode>
where
    F: FnOnce(&str) -> GitResult,
{
    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = subcmd_args.iter().map(|s| s.as_str()).collect();
    let output = runner.run("git", &arg_refs)?;

    if output.exit_code != Some(0) {
        // On failure, pass through stderr
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        return Ok(exit_code_to_process(output.exit_code));
    }

    let _ = global_flags; // consumed during arg building by callers
    let result = parser(&output.stdout);
    println!("{result}");

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Status
// ============================================================================

/// Run `git status` with compression.
///
/// Flag-aware passthrough: if user has `--porcelain`, `--short`, or `-s`,
/// output is already compact — pass through unmodified.
fn run_status(global_flags: &[String], args: &[String]) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--porcelain", "--short", "-s"]) {
        return run_passthrough(global_flags, "status", args);
    }

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend([
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--branch".to_string(),
    ]);
    full_args.extend_from_slice(args);

    run_parsed_command(global_flags, &full_args, parse_status)
}

/// Parse porcelain v2 status output into a compressed GitResult.
fn parse_status(output: &str) -> GitResult {
    let mut branch = String::new();
    let mut staged: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();
    let mut renamed: Vec<String> = Vec::new();
    let mut unmerged: Vec<String> = Vec::new();

    for line in output.lines() {
        if line.starts_with("# branch.head ") {
            branch = line
                .strip_prefix("# branch.head ")
                .unwrap_or("")
                .to_string();
            continue;
        }

        if line.starts_with('#') {
            continue;
        }

        if line.starts_with('?') {
            // Untracked: "? <path>"
            let path = line.get(2..).unwrap_or("").to_string();
            untracked.push(path);
            continue;
        }

        if line.starts_with('u') {
            // Unmerged: "u <xy> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>"
            let path = extract_last_path(line);
            unmerged.push(path);
            continue;
        }

        if line.starts_with('2') {
            // Renamed: "2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X_score> <path>\t<origPath>"
            let path = extract_renamed_path(line);
            renamed.push(path);
            continue;
        }

        if line.starts_with('1') {
            // Tracked changed: "1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>"
            // XY: index change (X) and worktree change (Y)
            let xy = extract_xy(line);
            let path = extract_last_path(line);

            let x = xy.chars().next().unwrap_or('.');
            let y = xy.chars().nth(1).unwrap_or('.');

            if x != '.' {
                // Staged change (added, modified, deleted in index)
                staged.push(format!("{}{}", stage_prefix(x), path));
            }
            if y != '.' {
                // Worktree change
                modified.push(format!("{}{}", worktree_prefix(y), path));
            }
            continue;
        }
    }

    // Build details: show ALL files (no cap per GRANITE #618 lesson)
    let mut details: Vec<String> = Vec::new();

    if !branch.is_empty() {
        details.push(format!("branch: {branch}"));
    }

    for f in &staged {
        details.push(format!("staged: {f}"));
    }
    for f in &modified {
        details.push(format!("modified: {f}"));
    }
    for f in &untracked {
        details.push(format!("untracked: {f}"));
    }
    for f in &renamed {
        details.push(format!("renamed: {f}"));
    }
    for f in &unmerged {
        details.push(format!("unmerged: {f}"));
    }

    let total_changes =
        staged.len() + modified.len() + untracked.len() + renamed.len() + unmerged.len();

    let summary = if total_changes == 0 {
        "clean".to_string()
    } else {
        let mut parts: Vec<String> = Vec::new();
        if !staged.is_empty() {
            parts.push(format!("{} staged", staged.len()));
        }
        if !modified.is_empty() {
            parts.push(format!("{} modified", modified.len()));
        }
        if !untracked.is_empty() {
            parts.push(format!("{} untracked", untracked.len()));
        }
        if !renamed.is_empty() {
            parts.push(format!("{} renamed", renamed.len()));
        }
        if !unmerged.is_empty() {
            parts.push(format!("{} unmerged", unmerged.len()));
        }
        parts.join(", ")
    };

    GitResult::new("status".to_string(), summary, details)
}

/// Extract XY field from porcelain v2 tracked entry line.
/// Format: "1 <XY> <rest...>"
fn extract_xy(line: &str) -> String {
    line.split_whitespace().nth(1).unwrap_or("..").to_string()
}

/// Extract the last path component from a porcelain v2 line.
/// For type 1 entries: "1 XY sub mH mI mW hH hI <path>"
/// For unmerged: "u XY sub m1 m2 m3 mW h1 h2 h3 <path>"
fn extract_last_path(line: &str) -> String {
    // The path is the last whitespace-separated field
    line.split_whitespace().last().unwrap_or("").to_string()
}

/// Extract the renamed path from a porcelain v2 type 2 entry.
/// Format: "2 XY sub mH mI mW hH hI X_score <path>\t<origPath>"
fn extract_renamed_path(line: &str) -> String {
    // Tab-separated: before tab is the main entry, path is embedded
    if let Some(tab_pos) = line.find('\t') {
        let before_tab = &line[..tab_pos];
        let after_tab = &line[tab_pos + 1..];
        let new_path = before_tab.split_whitespace().last().unwrap_or("");
        format!("{after_tab} -> {new_path}")
    } else {
        line.split_whitespace().last().unwrap_or("").to_string()
    }
}

/// Map index status character to a display prefix.
fn stage_prefix(c: char) -> &'static str {
    match c {
        'M' => "M ",
        'A' => "A ",
        'D' => "D ",
        'R' => "R ",
        'C' => "C ",
        _ => "",
    }
}

/// Map worktree status character to a display prefix.
fn worktree_prefix(c: char) -> &'static str {
    match c {
        'M' => "M ",
        'D' => "D ",
        _ => "",
    }
}

// ============================================================================
// Diff
// ============================================================================

/// Run `git diff` with compression.
///
/// Flag-aware passthrough: if user has `--stat`, `--name-only`, or
/// `--name-status`, output is already compact — pass through unmodified.
fn run_diff(global_flags: &[String], args: &[String]) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--stat", "--name-only", "--name-status"]) {
        return run_passthrough(global_flags, "diff", args);
    }

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["diff".to_string(), "--stat".to_string()]);
    full_args.extend_from_slice(args);

    run_parsed_command(global_flags, &full_args, parse_diff_stat)
}

/// Parse `git diff --stat` output into a compressed GitResult.
fn parse_diff_stat(output: &str) -> GitResult {
    let mut file_stats: Vec<String> = Vec::new();
    let mut summary_line = String::new();

    let stat_re = Regex::new(r"^\s*(.+?)\s+\|\s+(\d+)\s+([+-]+)").unwrap();
    let summary_re = Regex::new(r"(\d+)\s+files?\s+changed").unwrap();

    for line in output.lines() {
        if let Some(caps) = stat_re.captures(line) {
            let file = caps.get(1).map_or("", |m| m.as_str()).trim();
            let count = caps.get(2).map_or("", |m| m.as_str());
            let changes = caps.get(3).map_or("", |m| m.as_str());
            file_stats.push(format!("{file} | {count} {changes}"));
            continue;
        }

        if summary_re.is_match(line) {
            summary_line = line.trim().to_string();
        }
    }

    if summary_line.is_empty() && file_stats.is_empty() {
        return GitResult::new("diff".to_string(), "no changes".to_string(), vec![]);
    }

    if summary_line.is_empty() {
        summary_line = format!("{} files changed", file_stats.len());
    }

    GitResult::new("diff".to_string(), summary_line, file_stats)
}

// ============================================================================
// Log
// ============================================================================

/// Run `git log` with compression.
///
/// Flag-aware passthrough: if user has `--format`, `--pretty`, or `--oneline`,
/// output is already compact — pass through unmodified.
fn run_log(global_flags: &[String], args: &[String]) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--format", "--pretty", "--oneline"]) {
        return run_passthrough(global_flags, "log", args);
    }

    // Check if user already has -n or --max-count
    let has_limit = args.iter().any(|a| {
        a == "-n" || a.starts_with("-n") || a == "--max-count" || a.starts_with("--max-count=")
    });

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["log".to_string(), "--format=%h %s (%cr) <%an>".to_string()]);

    if !has_limit {
        full_args.extend(["-n".to_string(), "20".to_string()]);
    }

    full_args.extend_from_slice(args);

    run_parsed_command(global_flags, &full_args, parse_log)
}

/// Parse formatted `git log` output into a compressed GitResult.
fn parse_log(output: &str) -> GitResult {
    let lines: Vec<String> = output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    let count = lines.len();
    let summary = if count == 1 {
        "1 commit".to_string()
    } else {
        format!("{count} commits")
    };

    GitResult::new("log".to_string(), summary, lines)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
    // parse_status tests
    // ========================================================================

    #[test]
    fn test_parse_status_clean() {
        let output = "# branch.oid abc123\n# branch.head main\n";
        let result = parse_status(output);
        assert_eq!(result.summary, "clean");
        // Details should contain branch info only
        assert!(result.details.iter().any(|d| d.contains("branch: main")));
    }

    #[test]
    fn test_parse_status_dirty() {
        let output = include_str!("../../tests/fixtures/cmd/git/status_dirty.txt");
        let result = parse_status(output);

        // Verify summary contains counts
        assert!(
            result.summary.contains("staged"),
            "expected 'staged' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("modified"),
            "expected 'modified' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("untracked"),
            "expected 'untracked' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("renamed"),
            "expected 'renamed' in summary, got: {}",
            result.summary
        );
    }

    #[test]
    fn test_parse_status_shows_all_files() {
        // Generate 25 untracked files — ensure no cap
        let mut output = String::from("# branch.head main\n");
        for i in 0..25 {
            output.push_str(&format!("? file_{i}.txt\n"));
        }

        let result = parse_status(&output);

        // All 25 should appear in details (no 5/5/3 cap like GRANITE #618)
        let untracked_count = result
            .details
            .iter()
            .filter(|d| d.starts_with("untracked:"))
            .count();
        assert_eq!(
            untracked_count, 25,
            "expected all 25 untracked files, got {untracked_count}"
        );
    }

    // ========================================================================
    // parse_diff_stat tests
    // ========================================================================

    #[test]
    fn test_parse_diff_stat() {
        let output = include_str!("../../tests/fixtures/cmd/git/diff_stat.txt");
        let result = parse_diff_stat(output);

        assert!(
            result.summary.contains("3 files changed"),
            "expected '3 files changed' in summary, got: {}",
            result.summary
        );
        assert_eq!(result.details.len(), 3, "expected 3 file stat entries");
    }

    #[test]
    fn test_parse_diff_stat_empty() {
        let result = parse_diff_stat("");
        assert_eq!(result.summary, "no changes");
        assert!(result.details.is_empty());
    }

    // ========================================================================
    // parse_log tests
    // ========================================================================

    #[test]
    fn test_parse_log_format() {
        let output = include_str!("../../tests/fixtures/cmd/git/log_format.txt");
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
        assert_eq!(result.summary, "0 commits");
        assert!(result.details.is_empty());
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
    // user_has_flag / exit_code_to_process helpers
    // ========================================================================

    #[test]
    fn test_user_has_flag_empty_args() {
        assert!(!user_has_flag(&[], &["--flag"]));
    }

    #[test]
    fn test_exit_code_to_process_success() {
        let code = exit_code_to_process(Some(0));
        // ExitCode doesn't impl PartialEq, so compare via Debug
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }

    #[test]
    fn test_exit_code_to_process_failure() {
        let code = exit_code_to_process(Some(1));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn test_exit_code_to_process_none() {
        let code = exit_code_to_process(None);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    // ========================================================================
    // has_limit detection for log
    // ========================================================================

    #[test]
    fn test_log_detects_n_flag() {
        let args: Vec<String> = vec!["-n".into(), "10".into()];
        let has_limit = args.iter().any(|a| {
            a == "-n" || a.starts_with("-n") || a == "--max-count" || a.starts_with("--max-count=")
        });
        assert!(has_limit);
    }

    #[test]
    fn test_log_detects_max_count() {
        let args: Vec<String> = vec!["--max-count=5".into()];
        let has_limit = args.iter().any(|a| {
            a == "-n" || a.starts_with("-n") || a == "--max-count" || a.starts_with("--max-count=")
        });
        assert!(has_limit);
    }

    #[test]
    fn test_log_no_limit_flag() {
        let args: Vec<String> = vec!["--all".into()];
        let has_limit = args.iter().any(|a| {
            a == "-n" || a.starts_with("-n") || a == "--max-count" || a.starts_with("--max-count=")
        });
        assert!(!has_limit);
    }
}
