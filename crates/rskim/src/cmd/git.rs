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

use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;
use rskim_core::Language;

use crate::cmd::user_has_flag;
use crate::output::canonical::{DiffFileEntry, DiffFileStatus, DiffResult, GitResult};
use crate::runner::CommandRunner;

// ============================================================================
// Compiled regexes (compiled once, reused across calls)
// ============================================================================

/// Matches diff stat lines: " file | 42 +++++---"
#[cfg(test)]
static STAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\|\s+(\d+)\s+([+-]+)").unwrap());

/// Matches diff stat summary lines: "3 files changed, ..."
#[cfg(test)]
static SUMMARY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s+files?\s+changed").unwrap());

/// Matches binary diff stat lines: " file.bin | Bin 0 -> 1234 bytes"
#[cfg(test)]
static BINARY_STAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\|\s+Bin\s+").unwrap());

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

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);

    let (global_flags, rest) = split_global_flags(&filtered_args);

    let Some(subcmd) = rest.first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let subcmd_args = &rest[1..];

    match subcmd.as_str() {
        "status" => run_status(&global_flags, subcmd_args, show_stats),
        "diff" => run_diff(&global_flags, subcmd_args, show_stats),
        "log" => run_log(&global_flags, subcmd_args, show_stats),
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
    println!("  diff      AST-aware diff with full function boundaries");
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
        if arg == "--no-pager"
            || arg == "--bare"
            || arg == "--no-replace-objects"
            || arg == "--no-optional-locks"
        {
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
    let arg_refs: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();
    let output = runner.run("git", &arg_refs)?;

    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    if show_stats {
        // Passthrough: raw == compressed (no savings)
        let raw = &output.stdout;
        let (orig, comp) = crate::process::count_token_pair(raw, raw);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking).
    // Passthrough: raw == compressed (no transformation applied).
    // Guard behind is_analytics_enabled() to avoid cloning large git output
    // (100 KB+) when analytics are disabled.
    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            output.stdout.clone(),
            output.stdout,
            format!("skim git {} {}", subcmd, args.join(" ")),
            crate::analytics::CommandType::Git,
            output.duration,
            None,
        );
    }

    Ok(map_exit_code(output.exit_code))
}

/// Run a git command and parse its output with the given parser function.
///
/// Callers are responsible for baking global flags into `subcmd_args` before
/// calling this function.
fn run_parsed_command<F>(
    subcmd_args: &[String],
    show_stats: bool,
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
        return Ok(map_exit_code(output.exit_code));
    }

    let result = parser(&output.stdout);
    let result_str = result.to_string();
    println!("{result_str}");

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&output.stdout, &result_str);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking).
    // Guard to avoid allocations when analytics are disabled.
    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            output.stdout,
            result_str,
            format!("skim git {}", subcmd_args.join(" ")),
            crate::analytics::CommandType::Git,
            output.duration,
            None,
        );
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Status
// ============================================================================

/// Run `git status` with compression.
///
/// Flag-aware passthrough: if user has `--porcelain`, `--short`, or `-s`,
/// output is already compact — pass through unmodified.
fn run_status(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--porcelain", "--short", "-s"]) {
        return run_passthrough(global_flags, "status", args, show_stats);
    }

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend([
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--branch".to_string(),
    ]);
    full_args.extend_from_slice(args);

    run_parsed_command(&full_args, show_stats, parse_status)
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

/// Extract the path from a porcelain v2 line using fixed field counts.
///
/// Type 1 entries: "1 XY sub mH mI mW hH hI <path>" (8 fields before path)
/// Unmerged entries: "u XY sub m1 m2 m3 mW h1 h2 h3 <path>" (10 fields before path)
///
/// Uses `splitn` with the correct field count so paths with spaces are preserved.
fn extract_last_path(line: &str) -> String {
    let field_count = if line.starts_with('u') {
        // Unmerged: 10 fixed fields + path
        11
    } else {
        // Type 1: 8 fixed fields + path
        9
    };

    let fields: Vec<&str> = line.splitn(field_count, ' ').collect();
    fields.last().unwrap_or(&"").to_string()
}

/// Extract the renamed path from a porcelain v2 type 2 entry.
/// Format: "2 XY sub mH mI mW hH hI X_score <path>\t<origPath>"
fn extract_renamed_path(line: &str) -> String {
    // Porcelain v2 type-2 entries always contain a tab separator:
    // "2 XY sub mH mI mW hH hI X_score <path>\t<origPath>"
    let tab_pos = line
        .find('\t')
        .expect("porcelain v2 type-2 entries always contain a tab");
    let before_tab = &line[..tab_pos];
    let after_tab = &line[tab_pos + 1..];
    // Field 10 (0-indexed 9) is the new path; use splitn to preserve spaces in path
    let new_path = before_tab.splitn(10, ' ').last().unwrap_or("");
    format!("{after_tab} -> {new_path}")
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
// Diff — AST-aware pipeline (#103)
// ============================================================================

/// Maximum file size for AST processing (100 KB). Larger files fall back
/// to raw diff hunks.
const MAX_AST_FILE_SIZE: usize = 100 * 1024;

/// Matches hunk headers: `@@ -N,M +N,M @@ optional context`
static HUNK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@").expect("valid regex")
});

/// A single hunk from a unified diff.
#[derive(Debug, Clone)]
struct DiffHunk {
    /// Start line in the old file (1-indexed).
    /// Used in tests and for hunk-to-node overlap calculations.
    #[allow(dead_code)]
    old_start: usize,
    /// Number of lines removed from old file.
    /// Used in tests for validating hunk parsing.
    #[allow(dead_code)]
    old_count: usize,
    /// Start line in the new file (1-indexed)
    new_start: usize,
    /// Number of lines added in new file
    new_count: usize,
    /// Raw patch lines (including `+`, `-`, and context ` ` prefixes)
    patch_lines: Vec<String>,
}

/// Status of a file in a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Binary,
}

impl From<&FileStatus> for DiffFileStatus {
    fn from(status: &FileStatus) -> Self {
        match status {
            FileStatus::Added => DiffFileStatus::Added,
            FileStatus::Modified => DiffFileStatus::Modified,
            FileStatus::Deleted => DiffFileStatus::Deleted,
            FileStatus::Renamed => DiffFileStatus::Renamed,
            FileStatus::Binary => DiffFileStatus::Binary,
        }
    }
}

/// Parsed representation of a single file in a unified diff.
#[derive(Debug, Clone)]
struct FileDiff {
    /// File path (new path for renames/adds, old path for deletes)
    path: String,
    /// Original path for renames (old name)
    old_path: Option<String>,
    /// File status
    status: FileStatus,
    /// Hunks of changed lines
    hunks: Vec<DiffHunk>,
}

/// Parse a hunk header line: `@@ -N,M +N,M @@`
///
/// Returns `(old_start, old_count, new_start, new_count)` on success.
fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let caps = HUNK_RE.captures(line)?;
    let old_start: usize = caps.get(1)?.as_str().parse().ok()?;
    let old_count: usize = caps.get(2).map_or(1, |m| m.as_str().parse().unwrap_or(1));
    let new_start: usize = caps.get(3)?.as_str().parse().ok()?;
    let new_count: usize = caps.get(4).map_or(1, |m| m.as_str().parse().unwrap_or(1));
    Some((old_start, old_count, new_start, new_count))
}

/// Parse unified diff output into a list of per-file diffs.
///
/// Handles standard `git diff --no-color` output including:
/// - New files (`--- /dev/null`)
/// - Deleted files (`+++ /dev/null`)
/// - Renamed files (`rename from` / `rename to`)
/// - Binary files (`Binary files ... differ`)
fn parse_unified_diff(output: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Look for diff headers
        if !line.starts_with("diff --git ") {
            i += 1;
            continue;
        }

        // Parse diff header: "diff --git a/path b/path"
        let (a_path, b_path) = parse_diff_git_header(line);

        // Scan extended headers and --- / +++ lines
        i += 1;
        let mut old_path: Option<String> = None;
        let mut is_binary = false;
        let mut is_new = false;
        let mut is_deleted = false;
        let mut is_renamed = false;
        let mut rename_from: Option<String> = None;
        let mut file_minus = String::new();
        let mut file_plus = String::new();

        while i < lines.len() && !lines[i].starts_with("diff --git ") {
            let l = lines[i];

            if l.starts_with("new file mode") {
                is_new = true;
            } else if l.starts_with("deleted file mode") {
                is_deleted = true;
            } else if l.starts_with("rename from ") {
                is_renamed = true;
                rename_from = Some(l.strip_prefix("rename from ").unwrap_or("").to_string());
            } else if l.starts_with("rename to ") {
                is_renamed = true;
            } else if l.starts_with("Binary files") && l.contains("differ") {
                is_binary = true;
            } else if l.starts_with("--- ") {
                file_minus = l.strip_prefix("--- ").unwrap_or("").to_string();
            } else if l.starts_with("+++ ") {
                file_plus = l.strip_prefix("+++ ").unwrap_or("").to_string();
            } else if l.starts_with("@@") {
                // Hunk header — we've moved past extended headers, stop scanning
                break;
            }

            i += 1;
        }

        // Determine file status and path
        let status = if is_binary {
            FileStatus::Binary
        } else if is_new || file_minus == "/dev/null" || file_minus == "a//dev/null" {
            FileStatus::Added
        } else if is_deleted || file_plus == "/dev/null" || file_plus == "b//dev/null" {
            FileStatus::Deleted
        } else if is_renamed {
            FileStatus::Renamed
        } else {
            FileStatus::Modified
        };

        let path = if status == FileStatus::Deleted {
            strip_ab_prefix(&a_path)
        } else {
            strip_ab_prefix(&b_path)
        };

        if is_renamed {
            old_path = rename_from.or_else(|| Some(strip_ab_prefix(&a_path)));
        }

        // Parse hunks
        let mut hunks: Vec<DiffHunk> = Vec::new();

        if !is_binary {
            while i < lines.len() && !lines[i].starts_with("diff --git ") {
                let l = lines[i];

                if l.starts_with("@@") {
                    if let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(l)
                    {
                        let mut patch_lines: Vec<String> = Vec::new();
                        i += 1;

                        while i < lines.len() {
                            let pl = lines[i];
                            if pl.starts_with("diff --git ") || pl.starts_with("@@") {
                                break;
                            }
                            // Only keep actual patch lines (+, -, space, or \ no newline)
                            if pl.starts_with('+')
                                || pl.starts_with('-')
                                || pl.starts_with(' ')
                                || pl.starts_with('\\')
                            {
                                patch_lines.push(pl.to_string());
                            }
                            i += 1;
                        }

                        hunks.push(DiffHunk {
                            old_start,
                            old_count,
                            new_start,
                            new_count,
                            patch_lines,
                        });
                        continue;
                    }
                }
                i += 1;
            }
        } else {
            // Skip remaining lines for binary files
            while i < lines.len() && !lines[i].starts_with("diff --git ") {
                i += 1;
            }
        }

        files.push(FileDiff {
            path,
            old_path,
            status,
            hunks,
        });
    }

    files
}

/// Parse the `diff --git a/path b/path` header to extract both paths.
fn parse_diff_git_header(line: &str) -> (String, String) {
    // Format: "diff --git a/path b/path"
    // Handle paths with spaces by splitting on " b/"
    let rest = line.strip_prefix("diff --git ").unwrap_or(line);

    // Find the boundary between a/path and b/path.
    // We look for " b/" as the separator, but need to handle cases where
    // the path itself contains " b/".
    if let Some(pos) = rest.find(" b/") {
        let a_part = &rest[..pos];
        let b_part = &rest[pos + 1..];
        (a_part.to_string(), b_part.to_string())
    } else if let Some(pos) = rest.find(" b\\") {
        let a_part = &rest[..pos];
        let b_part = &rest[pos + 1..];
        (a_part.to_string(), b_part.to_string())
    } else {
        // Fallback: split on last space (handles unusual path formats)
        if let Some(pos) = rest.rfind(' ') {
            let a_part = &rest[..pos];
            let b_part = &rest[pos + 1..];
            (a_part.to_string(), b_part.to_string())
        } else {
            // No separator found — treat entire string as b-path
            (rest.to_string(), rest.to_string())
        }
    }
}

/// Strip the `a/` or `b/` prefix from a diff path.
fn strip_ab_prefix(path: &str) -> String {
    if let Some(stripped) = path.strip_prefix("a/") {
        stripped.to_string()
    } else if let Some(stripped) = path.strip_prefix("b/") {
        stripped.to_string()
    } else {
        path.to_string()
    }
}

/// Run `git show <ref_spec>` and return stdout, or bail on failure.
fn git_show(global_flags: &[String], ref_spec: &str) -> anyhow::Result<String> {
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["show".to_string(), ref_spec.to_string()]);
    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();
    let output = runner.run("git", &arg_refs)?;
    if output.exit_code != Some(0) {
        anyhow::bail!("git show {ref_spec} failed: {}", output.stderr.trim());
    }
    Ok(output.stdout)
}

/// Resolve the file source content for AST parsing.
///
/// - Unstaged (working tree): read from disk (respects `-C` flag)
/// - `--cached` / `--staged`: use `git show :path`
/// - Commit range (`A..B` or `A B`): use `git show B:path`
fn get_file_source(path: &str, global_flags: &[String], args: &[String]) -> anyhow::Result<String> {
    if user_has_flag(args, &["--cached", "--staged"]) {
        return git_show(global_flags, &format!(":{path}"));
    }

    // Check for commit range in args (e.g., "HEAD~2..HEAD")
    let range_commit = args.iter().find_map(|a| {
        let pos = a.find("..")?;
        let right = &a[pos + 2..];
        Some(if right.is_empty() {
            "HEAD".to_string()
        } else {
            right.to_string()
        })
    });

    if let Some(commit) = range_commit {
        return git_show(global_flags, &format!("{commit}:{path}"));
    }

    // Default: read from working tree (disk).
    // When `-C <dir>` is present in global flags, resolve the path relative
    // to that directory since git diff outputs paths relative to the repo root.
    let base_dir = extract_c_flag_dir(global_flags);
    let full_path = match &base_dir {
        Some(dir) => std::path::PathBuf::from(dir).join(path),
        None => std::path::PathBuf::from(path),
    };
    std::fs::read_to_string(&full_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", full_path.display()))
}

/// Extract the directory from a `-C <dir>` global flag, if present.
fn extract_c_flag_dir(global_flags: &[String]) -> Option<String> {
    let mut iter = global_flags.iter();
    while let Some(flag) = iter.next() {
        if flag == "-C" {
            return iter.next().cloned();
        }
    }
    None
}

/// Find which top-level AST nodes overlap with changed line ranges from hunks.
///
/// Returns a list of `(start_line, end_line)` ranges for changed top-level nodes.
/// Lines are 1-indexed to match diff output.
fn find_changed_node_ranges(
    tree: &tree_sitter::Tree,
    hunks: &[DiffHunk],
) -> Vec<(usize, usize)> {
    if hunks.is_empty() {
        return Vec::new();
    }

    // Build a set of changed line numbers (1-indexed, using new-file line numbers)
    let mut changed_lines: HashSet<usize> = HashSet::new();
    for hunk in hunks {
        // Walk through the patch lines to determine exact new-file line numbers
        let mut new_line = hunk.new_start;
        for patch_line in &hunk.patch_lines {
            if patch_line.starts_with('+') {
                changed_lines.insert(new_line);
                new_line += 1;
            } else if patch_line.starts_with('-') {
                // Removed lines exist in old file, mark the current position
                // in the new file as a change boundary
                changed_lines.insert(new_line);
            } else if patch_line.starts_with(' ') {
                new_line += 1;
            }
            // Skip lines starting with '\'
        }
    }

    if changed_lines.is_empty() {
        return Vec::new();
    }

    // Walk top-level AST nodes and find overlaps
    let root = tree.root_node();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        // tree-sitter rows are 0-indexed, convert to 1-indexed
        let node_start = child.start_position().row + 1;
        let node_end = child.end_position().row + 1;

        // Check if any changed line falls within this node's range
        let overlaps = changed_lines
            .iter()
            .any(|&line| line >= node_start && line <= node_end);

        if overlaps {
            ranges.push((node_start, node_end));
        }
    }

    ranges
}

/// Render a single file diff with AST-aware context.
///
/// For supported languages: shows changed AST nodes with full boundaries,
/// preserving `+`/`-` markers from the patch.
///
/// For unsupported languages or parse failures: falls back to raw hunks.
fn render_diff_file(file_diff: &FileDiff, global_flags: &[String], args: &[String]) -> String {
    let mut output = String::new();

    // File header
    let status_label = match file_diff.status {
        FileStatus::Added => "added",
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        FileStatus::Binary => "binary",
    };

    // Renames with a known old path show "old -> new (renamed)"
    if let (FileStatus::Renamed, Some(old)) = (&file_diff.status, &file_diff.old_path) {
        let _ = writeln!(
            output,
            "\u{2500}\u{2500} {} \u{2192} {} ({}) \u{2500}\u{2500}",
            old, file_diff.path, status_label
        );
    } else {
        let _ = writeln!(
            output,
            "\u{2500}\u{2500} {} ({}) \u{2500}\u{2500}",
            file_diff.path, status_label
        );
    }

    // Binary files
    if file_diff.status == FileStatus::Binary {
        let _ = writeln!(output, "Binary file differs");
        return output;
    }

    // No hunks means nothing to show
    if file_diff.hunks.is_empty() {
        return output;
    }

    // Added/deleted files: show all patch lines verbatim (no AST overlay needed)
    if file_diff.status == FileStatus::Deleted || file_diff.status == FileStatus::Added {
        return render_raw_hunks(file_diff, &output);
    }

    // For modified/renamed files, try AST-aware rendering
    let language = Language::from_path(Path::new(&file_diff.path));

    let source = match get_file_source(&file_diff.path, global_flags, args) {
        Ok(s) => s,
        Err(_) => return render_raw_hunks(file_diff, &output),
    };

    // Skip AST for files > 100KB
    if source.len() > MAX_AST_FILE_SIZE {
        return render_raw_hunks(file_diff, &output);
    }

    let Some(lang) = language else {
        return render_raw_hunks(file_diff, &output);
    };

    // Languages that don't use tree-sitter (JSON, YAML, TOML) fall back to raw hunks
    if lang.is_serde_based() {
        return render_raw_hunks(file_diff, &output);
    }

    // Parse with tree-sitter
    let mut parser = match rskim_core::Parser::new(lang) {
        Ok(p) => p,
        Err(_) => return render_raw_hunks(file_diff, &output),
    };

    let tree = match parser.parse(&source) {
        Ok(t) => t,
        Err(_) => return render_raw_hunks(file_diff, &output),
    };

    // Find changed AST node ranges
    let changed_ranges = find_changed_node_ranges(&tree, &file_diff.hunks);

    if changed_ranges.is_empty() {
        // No overlapping AST nodes found — fall back to raw hunks
        return render_raw_hunks(file_diff, &output);
    }

    // Render: for each changed node range, emit the patch lines that fall within it.
    // Group consecutive hunks and extend to node boundaries.
    let source_lines: Vec<&str> = source.lines().collect();

    for (node_start, node_end) in &changed_ranges {
        // Collect relevant hunks for this node
        let relevant_hunks: Vec<&DiffHunk> = file_diff
            .hunks
            .iter()
            .filter(|h| {
                let hunk_start = h.new_start;
                let hunk_end = h.new_start + h.new_count.saturating_sub(1);
                // Check overlap: hunk range overlaps with node range
                hunk_start <= *node_end && hunk_end >= *node_start
            })
            .collect();

        if relevant_hunks.is_empty() {
            continue;
        }

        // Render the node region. We output source lines from node_start to node_end,
        // substituting hunk patch lines where they apply.
        let mut current_new_line = *node_start;

        for hunk in &relevant_hunks {
            // Output unchanged source lines before this hunk's position
            while current_new_line < hunk.new_start && current_new_line <= *node_end {
                if let Some(line) = source_lines.get(current_new_line - 1) {
                    let _ = writeln!(output, " {line}");
                }
                current_new_line += 1;
            }

            // Output the hunk's patch lines
            for patch_line in &hunk.patch_lines {
                if patch_line.starts_with('+') {
                    let _ = writeln!(output, "{patch_line}");
                    current_new_line += 1;
                } else if patch_line.starts_with('-') {
                    let _ = writeln!(output, "{patch_line}");
                    // Deleted lines don't advance the new-file line counter
                } else if patch_line.starts_with(' ') {
                    let _ = writeln!(output, "{patch_line}");
                    current_new_line += 1;
                } else if patch_line.starts_with('\\') {
                    let _ = writeln!(output, "{patch_line}");
                }
            }
        }

        // Output remaining unchanged source lines to end of node
        while current_new_line <= *node_end {
            if let Some(line) = source_lines.get(current_new_line - 1) {
                let _ = writeln!(output, " {line}");
            }
            current_new_line += 1;
        }
    }

    output
}

/// Render raw diff hunks as fallback (no AST awareness).
fn render_raw_hunks(file_diff: &FileDiff, header: &str) -> String {
    let mut output = header.to_string();
    for hunk in &file_diff.hunks {
        for line in &hunk.patch_lines {
            let _ = writeln!(output, "{line}");
        }
    }
    output
}

/// Run `git diff` with AST-aware pipeline (#103).
///
/// Flag-aware passthrough: `--stat`, `--name-only`, `--name-status`, `--check`
/// pass through to git unmodified.
///
/// Default: parses unified diff, overlays changed lines on tree-sitter AST,
/// renders changed nodes with full function boundaries and `+`/`-` markers.
fn run_diff(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--stat", "--name-only", "--name-status", "--check"]) {
        return run_passthrough(global_flags, "diff", args, show_stats);
    }

    // Run `git diff --no-color [user args]` to get unified diff output
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["diff".to_string(), "--no-color".to_string()]);
    full_args.extend_from_slice(args);

    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();
    let output = runner.run("git", &arg_refs)?;

    if output.exit_code != Some(0) {
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        return Ok(map_exit_code(output.exit_code));
    }

    let raw_diff = &output.stdout;

    // Handle empty diff
    if raw_diff.trim().is_empty() {
        eprintln!("No changes");
        return Ok(ExitCode::SUCCESS);
    }

    // Parse unified diff into per-file structures
    let file_diffs = parse_unified_diff(raw_diff);

    if file_diffs.is_empty() {
        eprintln!("No changes");
        return Ok(ExitCode::SUCCESS);
    }

    // Render each file with AST-aware context
    let mut rendered_output = String::new();
    let mut diff_file_entries: Vec<DiffFileEntry> = Vec::new();

    for file_diff in &file_diffs {
        rendered_output.push_str(&render_diff_file(file_diff, global_flags, args));
        diff_file_entries.push(DiffFileEntry {
            path: file_diff.path.clone(),
            status: DiffFileStatus::from(&file_diff.status),
            changed_regions: file_diff.hunks.len(),
        });
    }

    let result = DiffResult::new(diff_file_entries, rendered_output);
    let result_str = result.to_string();
    print!("{result_str}");

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(raw_diff, &result_str);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking).
    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            raw_diff.to_string(),
            result_str,
            format!("skim git diff {}", args.join(" ")),
            crate::analytics::CommandType::Diff,
            output.duration,
            None,
        );
    }

    Ok(ExitCode::SUCCESS)
}

/// Parse `git diff --stat` output into a compressed GitResult.
///
/// Retained for testing and potential future use (e.g., `--mode stat`).
#[cfg(test)]
fn parse_diff_stat(output: &str) -> GitResult {
    let mut file_stats: Vec<String> = Vec::new();
    let mut summary_line = String::new();

    for line in output.lines() {
        if let Some(caps) = STAT_RE.captures(line) {
            let file = caps.get(1).map_or("", |m| m.as_str()).trim();
            let count = caps.get(2).map_or("", |m| m.as_str());
            let changes = caps.get(3).map_or("", |m| m.as_str());
            file_stats.push(format!("{file} | {count} {changes}"));
            continue;
        }

        // Binary files appear as "file.bin | Bin 0 -> 1234 bytes"
        if let Some(caps) = BINARY_STAT_RE.captures(line) {
            let file = caps.get(1).map_or("", |m| m.as_str()).trim();
            file_stats.push(format!("{file} | Bin"));
            continue;
        }

        if SUMMARY_RE.is_match(line) {
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
fn run_log(global_flags: &[String], args: &[String], show_stats: bool) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--format", "--pretty", "--oneline"]) {
        return run_passthrough(global_flags, "log", args, show_stats);
    }

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["log".to_string(), "--format=%h %s (%cr) <%an>".to_string()]);

    if !has_limit_flag(args) {
        full_args.extend(["-n".to_string(), "20".to_string()]);
    }

    full_args.extend_from_slice(args);

    run_parsed_command(&full_args, show_stats, parse_log)
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
    // Paths with spaces in git status
    // ========================================================================

    #[test]
    fn test_extract_last_path_with_spaces() {
        // Type 1 entry with space in path: 8 fixed fields + path
        let line = "1 M. N... 100644 100644 100644 abc1234 def5678 src/my file.rs";
        assert_eq!(extract_last_path(line), "src/my file.rs");
    }

    #[test]
    fn test_parse_status_path_with_spaces() {
        let output = "# branch.head main\n\
                      1 M. N... 100644 100644 100644 abc1234 def5678 src/my file.rs\n";
        let result = parse_status(output);
        assert!(
            result.details.iter().any(|d| d.contains("my file.rs")),
            "expected path with spaces in details, got: {:?}",
            result.details
        );
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
    // Unmerged entries in status
    // ========================================================================

    #[test]
    fn test_parse_status_unmerged_entries() {
        let output = "# branch.head main\n\
                      u UU N... 100644 100644 100644 100644 abc1234 def5678 ghi9012 src/conflict.rs\n";
        let result = parse_status(output);
        assert!(
            result.summary.contains("unmerged"),
            "expected 'unmerged' in summary, got: {}",
            result.summary
        );
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("unmerged:") && d.contains("conflict.rs")),
            "expected unmerged detail for conflict.rs, got: {:?}",
            result.details
        );
    }

    // ========================================================================
    // Binary files in diff stat
    // ========================================================================

    #[test]
    fn test_parse_diff_stat_binary_files() {
        let output = " src/main.rs   | 15 +++++++++------\n\
                       image.png     | Bin 0 -> 1234 bytes\n\
                       2 files changed, 10 insertions(+), 5 deletions(-)\n";
        let result = parse_diff_stat(output);
        assert_eq!(
            result.details.len(),
            2,
            "expected 2 file stat entries (1 text + 1 binary), got: {:?}",
            result.details
        );
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("image.png") && d.contains("Bin")),
            "expected binary file entry, got: {:?}",
            result.details
        );
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
    // Hunk header parsing tests (#103)
    // ========================================================================

    #[test]
    fn test_parse_hunk_header_basic() {
        let result = parse_hunk_header("@@ -1,5 +1,8 @@ function foo() {");
        assert_eq!(result, Some((1, 5, 1, 8)));
    }

    #[test]
    fn test_parse_hunk_header_single_line() {
        let result = parse_hunk_header("@@ -1 +1 @@");
        assert_eq!(result, Some((1, 1, 1, 1)));
    }

    #[test]
    fn test_parse_hunk_header_with_context_label() {
        let result = parse_hunk_header("@@ -10,3 +12,5 @@ impl MyStruct {");
        assert_eq!(result, Some((10, 3, 12, 5)));
    }

    #[test]
    fn test_parse_hunk_header_malformed() {
        assert!(parse_hunk_header("not a hunk header").is_none());
        assert!(parse_hunk_header("@@ invalid @@").is_none());
        assert!(parse_hunk_header("--- a/file.rs").is_none());
    }

    #[test]
    fn test_parse_hunk_header_zero_count() {
        // New file with no old content
        let result = parse_hunk_header("@@ -0,0 +1,12 @@");
        assert_eq!(result, Some((0, 0, 1, 12)));
    }

    #[test]
    fn test_parse_hunk_header_large_line_numbers() {
        let result = parse_hunk_header("@@ -1000,50 +1050,60 @@");
        assert_eq!(result, Some((1000, 50, 1050, 60)));
    }

    // ========================================================================
    // Unified diff parsing tests (#103)
    // ========================================================================

    #[test]
    fn test_parse_unified_diff_single_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/single_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/auth/middleware.ts");
        assert_eq!(files[0].status, FileStatus::Modified);
        assert_eq!(files[0].hunks.len(), 1, "expected 1 hunk");
    }

    #[test]
    fn test_parse_unified_diff_multi_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/multi_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 2, "expected 2 files");
        assert_eq!(files[0].path, "src/api/routes.ts");
        assert_eq!(files[0].status, FileStatus::Modified);
        assert_eq!(files[1].path, "src/api/handlers.ts");
        assert_eq!(files[1].status, FileStatus::Modified);
        assert_eq!(files[1].hunks.len(), 2, "expected 2 hunks for handlers.ts");
    }

    #[test]
    fn test_parse_unified_diff_new_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/new_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/utils/validator.ts");
        assert_eq!(files[0].status, FileStatus::Added);
        assert_eq!(files[0].hunks.len(), 1, "expected 1 hunk");
        // All lines should be additions
        assert!(
            files[0].hunks[0]
                .patch_lines
                .iter()
                .all(|l| l.starts_with('+')),
            "all lines in new file should be additions"
        );
    }

    #[test]
    fn test_parse_unified_diff_deleted_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/deleted_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/legacy/old_auth.ts");
        assert_eq!(files[0].status, FileStatus::Deleted);
        // All lines should be deletions
        assert!(
            files[0].hunks[0]
                .patch_lines
                .iter()
                .all(|l| l.starts_with('-')),
            "all lines in deleted file should be deletions"
        );
    }

    #[test]
    fn test_parse_unified_diff_renamed_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/renamed_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/utils/format.ts");
        assert_eq!(files[0].status, FileStatus::Renamed);
        assert_eq!(
            files[0].old_path.as_deref(),
            Some("src/utils/helpers.ts"),
            "expected old path for rename"
        );
    }

    #[test]
    fn test_parse_unified_diff_binary_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/binary_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "assets/logo.png");
        assert_eq!(files[0].status, FileStatus::Binary);
        assert!(
            files[0].hunks.is_empty(),
            "binary files should have no hunks"
        );
    }

    // ========================================================================
    // File status detection tests (#103)
    // ========================================================================

    #[test]
    fn test_file_status_from_new_file() {
        let diff = "diff --git a/new.ts b/new.ts\nnew file mode 100644\nindex 0000000..abc1234\n--- /dev/null\n+++ b/new.ts\n@@ -0,0 +1,3 @@\n+line 1\n+line 2\n+line 3\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].status, FileStatus::Added);
    }

    #[test]
    fn test_file_status_from_deleted_file() {
        let diff = "diff --git a/old.ts b/old.ts\ndeleted file mode 100644\nindex abc1234..0000000\n--- a/old.ts\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-line 1\n-line 2\n-line 3\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].status, FileStatus::Deleted);
    }

    #[test]
    fn test_file_status_modified() {
        let diff = "diff --git a/mod.ts b/mod.ts\nindex abc..def 100644\n--- a/mod.ts\n+++ b/mod.ts\n@@ -1,3 +1,4 @@\n line 1\n-line 2\n+line 2 modified\n+line 2b\n line 3\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].status, FileStatus::Modified);
    }

    // ========================================================================
    // Hunk content extraction tests (#103)
    // ========================================================================

    #[test]
    fn test_hunk_content_single_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/single_file.diff");
        let files = parse_unified_diff(input);

        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.old_start, 5);
        assert_eq!(hunk.old_count, 7);
        assert_eq!(hunk.new_start, 5);
        assert_eq!(hunk.new_count, 10);

        // Should contain both + and - lines
        let has_additions = hunk.patch_lines.iter().any(|l| l.starts_with('+'));
        let has_deletions = hunk.patch_lines.iter().any(|l| l.starts_with('-'));
        assert!(has_additions, "expected additions in hunk");
        assert!(has_deletions, "expected deletions in hunk");
    }

    #[test]
    fn test_hunk_content_new_file() {
        let input = include_str!("../../tests/fixtures/cmd/diff/new_file.diff");
        let files = parse_unified_diff(input);

        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.old_start, 0);
        assert_eq!(hunk.old_count, 0);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_count, 12);
    }

    // ========================================================================
    // Changed node detection tests (#103)
    // ========================================================================

    #[test]
    fn test_find_changed_nodes_function_overlaps_hunk() {
        let source = "function foo() {\n  return 1;\n}\n\nfunction bar() {\n  return 2;\n}\n";

        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        // Simulate a hunk that changes line 2 (inside foo)
        let hunks = vec![DiffHunk {
            old_start: 2,
            old_count: 1,
            new_start: 2,
            new_count: 2,
            patch_lines: vec![
                "-  return 1;".to_string(),
                "+  return 42;".to_string(),
                "+  console.log(42);".to_string(),
            ],
        }];

        let ranges = find_changed_node_ranges(&tree, &hunks);

        // Should find at least the function containing line 2
        assert!(
            !ranges.is_empty(),
            "expected at least one changed node range"
        );
        // The changed range should cover foo (lines 1-3) but not bar (lines 5-7)
        let (start, end) = ranges[0];
        assert!(start <= 2, "changed range should start at or before line 2");
        assert!(end >= 2, "changed range should end at or after line 2");
    }

    #[test]
    fn test_find_changed_nodes_empty_hunks() {
        let source = "function foo() {}\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        let ranges = find_changed_node_ranges(&tree, &[]);
        assert!(ranges.is_empty(), "no hunks should yield no changed nodes");
    }

    #[test]
    fn test_find_changed_nodes_import_overlaps() {
        let source = "import { foo } from 'bar';\nimport { baz } from 'qux';\n\nfunction main() {\n  foo();\n}\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        // Simulate a hunk that changes line 1 (first import)
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            patch_lines: vec![
                "-import { foo } from 'bar';".to_string(),
                "+import { foo, extra } from 'bar';".to_string(),
            ],
        }];

        let ranges = find_changed_node_ranges(&tree, &hunks);
        assert!(!ranges.is_empty(), "import change should be detected");
    }

    // ========================================================================
    // strip_ab_prefix tests (#103)
    // ========================================================================

    #[test]
    fn test_strip_ab_prefix() {
        assert_eq!(strip_ab_prefix("a/src/main.rs"), "src/main.rs");
        assert_eq!(strip_ab_prefix("b/src/main.rs"), "src/main.rs");
        assert_eq!(strip_ab_prefix("src/main.rs"), "src/main.rs");
        assert_eq!(strip_ab_prefix("/dev/null"), "/dev/null");
    }

    // ========================================================================
    // extract_c_flag_dir tests (#103)
    // ========================================================================

    #[test]
    fn test_extract_c_flag_dir_present() {
        let flags: Vec<String> = vec!["-C".into(), "/tmp/repo".into()];
        assert_eq!(extract_c_flag_dir(&flags), Some("/tmp/repo".to_string()));
    }

    #[test]
    fn test_extract_c_flag_dir_absent() {
        let flags: Vec<String> = vec!["--no-pager".into()];
        assert_eq!(extract_c_flag_dir(&flags), None);
    }

    #[test]
    fn test_extract_c_flag_dir_empty() {
        let flags: Vec<String> = vec![];
        assert_eq!(extract_c_flag_dir(&flags), None);
    }

    #[test]
    fn test_extract_c_flag_dir_with_other_flags() {
        let flags: Vec<String> = vec![
            "--no-pager".into(),
            "-C".into(),
            "/my/repo".into(),
            "--bare".into(),
        ];
        assert_eq!(extract_c_flag_dir(&flags), Some("/my/repo".to_string()));
    }

    // ========================================================================
    // parse_diff_git_header tests (#103)
    // ========================================================================

    #[test]
    fn test_parse_diff_git_header_simple() {
        let (a, b) = parse_diff_git_header("diff --git a/src/main.rs b/src/main.rs");
        assert_eq!(a, "a/src/main.rs");
        assert_eq!(b, "b/src/main.rs");
    }

    #[test]
    fn test_parse_diff_git_header_different_paths() {
        let (a, b) = parse_diff_git_header("diff --git a/old/path.ts b/new/path.ts");
        assert_eq!(a, "a/old/path.ts");
        assert_eq!(b, "b/new/path.ts");
    }

    #[test]
    fn test_parse_diff_git_header_fallback_no_b_prefix() {
        // Unusual format without " b/" — falls back to last-space split
        let (a, b) = parse_diff_git_header("diff --git a-path b-path");
        assert_eq!(a, "a-path");
        assert_eq!(b, "b-path");
    }

    #[test]
    fn test_parse_diff_git_header_no_separator() {
        // Degenerate input with no space after stripping prefix
        let (a, b) = parse_diff_git_header("diff --git noseparator");
        // Both should be the same — no split possible
        assert_eq!(a, "noseparator");
        assert_eq!(b, "noseparator");
    }

    // ========================================================================
    // Render output tests (#103)
    // ========================================================================

    #[test]
    fn test_render_binary_file() {
        let file_diff = FileDiff {
            path: "assets/logo.png".to_string(),
            old_path: None,
            status: FileStatus::Binary,
            hunks: vec![],
        };
        let rendered = render_diff_file(&file_diff, &[], &[]);
        assert!(rendered.contains("logo.png"));
        assert!(rendered.contains("binary"));
        assert!(rendered.contains("Binary file differs"));
    }

    #[test]
    fn test_render_added_file() {
        let file_diff = FileDiff {
            path: "src/new.ts".to_string(),
            old_path: None,
            status: FileStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 2,
                patch_lines: vec!["+const x = 1;".to_string(), "+const y = 2;".to_string()],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[]);
        assert!(rendered.contains("added"), "header should show 'added'");
        assert!(
            rendered.contains("+const x = 1;"),
            "should contain added lines"
        );
    }

    #[test]
    fn test_render_deleted_file() {
        let file_diff = FileDiff {
            path: "src/old.ts".to_string(),
            old_path: None,
            status: FileStatus::Deleted,
            hunks: vec![DiffHunk {
                old_start: 1,
                old_count: 2,
                new_start: 0,
                new_count: 0,
                patch_lines: vec!["-const x = 1;".to_string(), "-const y = 2;".to_string()],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[]);
        assert!(rendered.contains("deleted"), "header should show 'deleted'");
        assert!(
            rendered.contains("-const x = 1;"),
            "should contain deleted lines"
        );
    }

    #[test]
    fn test_render_renamed_file_header() {
        let file_diff = FileDiff {
            path: "src/utils/format.ts".to_string(),
            old_path: Some("src/utils/helpers.ts".to_string()),
            status: FileStatus::Renamed,
            hunks: vec![],
        };
        let rendered = render_diff_file(&file_diff, &[], &[]);
        assert!(rendered.contains("helpers.ts"), "should show old path");
        assert!(rendered.contains("format.ts"), "should show new path");
        assert!(rendered.contains("renamed"), "header should show 'renamed'");
    }

    // ========================================================================
    // DiffResult output type tests (#103)
    // ========================================================================

    #[test]
    fn test_diff_result_display() {
        let entries = vec![
            DiffFileEntry {
                path: "src/main.rs".to_string(),
                status: DiffFileStatus::Modified,
                changed_regions: 2,
            },
            DiffFileEntry {
                path: "src/lib.rs".to_string(),
                status: DiffFileStatus::Added,
                changed_regions: 1,
            },
        ];
        let result = DiffResult::new(entries, "test rendered output".to_string());
        assert_eq!(result.files_changed, 2);
        assert_eq!(result.to_string(), "test rendered output");
    }

    #[test]
    fn test_diff_result_serde_roundtrip() {
        let entries = vec![DiffFileEntry {
            path: "src/main.rs".to_string(),
            status: DiffFileStatus::Modified,
            changed_regions: 1,
        }];
        let original = DiffResult::new(entries, "rendered output".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: DiffResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        // After deserialization+ensure_rendered, it should have some output
        assert!(!deserialized.as_ref().is_empty());
    }

    // ========================================================================
    // Empty diff edge case (#103)
    // ========================================================================

    #[test]
    fn test_parse_unified_diff_empty() {
        let files = parse_unified_diff("");
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_unified_diff_whitespace_only() {
        let files = parse_unified_diff("  \n\n  \n");
        assert!(files.is_empty());
    }
}
