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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;
use rskim_core::Language;

use crate::cmd::user_has_flag;
use crate::cmd::OutputFormat;
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
    println!("  skim git log -n 5");
    println!("  skim git diff --help                   Diff-specific options");
}

fn print_diff_help() {
    println!("skim git diff \u{2014} AST-aware diff compression");
    println!();
    println!("USAGE:");
    println!("    skim git diff [OPTIONS] [<commit>..] [-- <path>...]");
    println!();
    println!("SKIM OPTIONS:");
    println!("    --mode <MODE>    Diff rendering mode:");
    println!("                       (default)    Changed functions with boundaries");
    println!("                       structure    + unchanged functions as signatures");
    println!("                       full         Entire files with change markers");
    println!("    --json           Machine-readable JSON output");
    println!("    --show-stats     Show token savings statistics");
    println!();
    println!("GIT OPTIONS:");
    println!("    --staged, --cached    Diff staged changes");
    println!("    --stat, --shortstat   Passthrough to git (no AST processing)");
    println!("    --name-only           Passthrough to git");
    println!();
    println!("EXAMPLES:");
    println!("    skim git diff                    Working tree changes");
    println!("    skim git diff --staged           Staged changes");
    println!("    skim git diff HEAD~3             Last 3 commits");
    println!("    skim git diff main..feature      Branch comparison");
    println!("    skim git diff --mode structure   With context signatures");
    println!("    skim git diff --json             JSON output");
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
    output_format: OutputFormat,
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

/// Extract `--json` flag from args and return the corresponding `OutputFormat`.
///
/// Convenience wrapper that combines `extract_json_flag` with `OutputFormat`
/// conversion, keeping the git subcommand handlers consistent.
fn extract_output_format(args: &[String]) -> (Vec<String>, OutputFormat) {
    let (filtered, is_json) = super::extract_json_flag(args);
    let fmt = if is_json {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    };
    (filtered, fmt)
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

    let (filtered_args, output_format) = extract_output_format(args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend([
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--branch".to_string(),
    ]);
    full_args.extend_from_slice(&filtered_args);

    run_parsed_command(&full_args, show_stats, output_format, parse_status)
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

/// Controls how unchanged AST nodes are rendered alongside changed nodes.
///
/// - `Default`: Only changed nodes are shown (no unchanged context).
/// - `Structure`: Unchanged nodes are shown as signatures (`{ /* ... */ }`).
/// - `Full`: Unchanged nodes are shown in full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffMode {
    /// Only changed AST nodes with `+`/`-` markers.
    Default,
    /// Changed nodes + unchanged nodes rendered as signatures.
    Structure,
    /// Changed nodes + unchanged nodes shown in full.
    Full,
}

/// Extract `--mode <value>` or `--mode=<value>` from args.
///
/// Returns `(filtered_args, DiffMode)` where `filtered_args` has the mode
/// flag removed so it is not passed to git.
///
/// Returns an error if the mode value is not one of the recognized values.
fn extract_diff_mode(args: &[String]) -> anyhow::Result<(Vec<String>, DiffMode)> {
    let mut filtered: Vec<String> = Vec::with_capacity(args.len());
    let mut mode = DiffMode::Default;
    let mut skip_next = false;

    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }

        if arg == "--mode" {
            if let Some(val) = args.get(i + 1) {
                mode = parse_diff_mode_value(val)?;
                skip_next = true;
            } else {
                return Err(anyhow::anyhow!(
                    "{arg} requires a value\nValid modes: structure, full (default: changed-only)"
                ));
            }
            continue;
        }

        if let Some(val) = arg.strip_prefix("--mode=") {
            mode = parse_diff_mode_value(val)?;
            continue;
        }

        filtered.push(arg.clone());
    }

    Ok((filtered, mode))
}

/// Parse a mode value string into a DiffMode.
///
/// Returns an error for unrecognized mode values with a helpful message.
fn parse_diff_mode_value(val: &str) -> Result<DiffMode, anyhow::Error> {
    match val {
        "structure" | "signatures" => Ok(DiffMode::Structure),
        "full" => Ok(DiffMode::Full),
        _ => Err(anyhow::anyhow!(
            "unknown diff mode: '{val}'\nValid modes: structure, full (default: changed-only)"
        )),
    }
}

/// Resolve the working tree root from global flags.
///
/// Checks for `-C <path>`, `--work-tree <path>`, or `--work-tree=<path>`.
/// Returns `None` if no path override is present.
fn resolve_work_tree(global_flags: &[String]) -> Option<PathBuf> {
    let mut i = 0;
    while i < global_flags.len() {
        let flag = &global_flags[i];

        if flag == "-C" || flag == "--work-tree" {
            if let Some(val) = global_flags.get(i + 1) {
                return Some(PathBuf::from(val));
            }
        }

        if let Some(val) = flag.strip_prefix("--work-tree=") {
            return Some(PathBuf::from(val));
        }

        i += 1;
    }
    None
}

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
    // Guard against argument injection: a ref_spec starting with `-` could be
    // interpreted as a flag by `git show`.
    if ref_spec.starts_with('-') {
        anyhow::bail!("invalid ref spec: {ref_spec:?} (must not start with '-')");
    }
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
/// - Unstaged (working tree): read from disk (respecting `-C` / `--work-tree`)
/// - `--cached` / `--staged`: use `git show :path`
/// - Commit range (`A..B` or `A B`): use `git show B:path`
fn get_file_source(path: &str, global_flags: &[String], args: &[String]) -> anyhow::Result<String> {
    // Reject null bytes — they could truncate the ref spec passed to git.
    if path.contains('\0') {
        anyhow::bail!("invalid diff path: contains null byte");
    }

    if user_has_flag(args, &["--cached", "--staged"]) {
        return git_show(global_flags, &format!(":{path}"));
    }

    // Check for commit range in args (e.g., "HEAD~2..HEAD" or "A...B").
    // Try three-dot first so `find("..")` doesn't accidentally match at the
    // wrong position inside a `...` range.
    let range_commit = args.iter().find_map(|a| {
        if let Some(pos) = a.find("...") {
            let right = &a[pos + 3..];
            Some(if right.is_empty() {
                "HEAD".to_string()
            } else {
                right.to_string()
            })
        } else {
            let pos = a.find("..")?;
            let right = &a[pos + 2..];
            Some(if right.is_empty() {
                "HEAD".to_string()
            } else {
                right.to_string()
            })
        }
    });

    if let Some(commit) = range_commit {
        return git_show(global_flags, &format!("{commit}:{path}"));
    }

    // Default: read from working tree (disk).
    // When `-C` or `--work-tree` is set, prepend that path to the file path.
    let root = resolve_work_tree(global_flags);
    let disk_path = match &root {
        Some(r) => r.join(path),
        None => PathBuf::from(path),
    };

    // Path-traversal guard: canonicalize and verify the resolved path stays
    // within the work-tree root (or CWD when no explicit root is set).
    let canonical = disk_path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("failed to resolve {}: {e}", disk_path.display()))?;
    let base = match &root {
        Some(r) => r
            .canonicalize()
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default()),
        None => std::env::current_dir().unwrap_or_default(),
    };
    if !canonical.starts_with(&base) {
        anyhow::bail!(
            "path traversal detected: {} escapes work tree {}",
            canonical.display(),
            base.display()
        );
    }

    std::fs::read_to_string(&canonical)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", canonical.display()))
}

/// A resolved AST node range, with optional parent context for nested nodes.
#[derive(Debug, Clone)]
struct ChangedNodeRange {
    /// Start line of this node (1-indexed).
    start: usize,
    /// End line of this node (1-indexed).
    end: usize,
    /// If this node is a child of a container (class/struct/impl), store the
    /// parent's first line (declaration header) and last line (closing brace).
    parent_context: Option<ParentContext>,
}

/// Stores the declaration line and closing brace of a container node.
#[derive(Debug, Clone)]
struct ParentContext {
    /// The first line of the parent (declaration header), 1-indexed.
    header_line: usize,
    /// The last line of the parent (closing brace), 1-indexed.
    close_line: usize,
}

/// Build the set of changed line numbers from diff hunks.
///
/// Returns 1-indexed line numbers using new-file positions.
fn build_changed_lines(hunks: &[DiffHunk]) -> BTreeSet<usize> {
    let mut changed_lines: BTreeSet<usize> = BTreeSet::new();
    for hunk in hunks {
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
    changed_lines
}

/// Check whether a node is a container (class, struct, impl, module).
fn is_container_node(node: &tree_sitter::Node<'_>) -> bool {
    let kind = node.kind();
    matches!(
        kind,
        "class_declaration"
            | "class_definition"          // Python
            | "class"
            | "struct_item"               // Rust
            | "impl_item"                 // Rust
            | "enum_item"                 // Rust
            | "trait_item"                // Rust
            | "interface_declaration"     // TypeScript
            | "module"
            | "namespace_definition" // C++
    )
}

/// Find which AST nodes overlap with changed line ranges from hunks.
///
/// Performs one level of nesting: if a top-level container node (class/struct/impl)
/// overlaps with hunks, walks its children to find the specific changed child
/// nodes. Returns child-level ranges with parent context instead of the entire
/// parent range.
///
/// Lines are 1-indexed to match diff output.
fn find_changed_node_ranges(tree: &tree_sitter::Tree, hunks: &[DiffHunk]) -> Vec<ChangedNodeRange> {
    if hunks.is_empty() {
        return Vec::new();
    }

    let changed_lines = build_changed_lines(hunks);

    if changed_lines.is_empty() {
        return Vec::new();
    }

    let root = tree.root_node();
    let mut ranges: Vec<ChangedNodeRange> = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let node_start = child.start_position().row + 1;
        let node_end = child.end_position().row + 1;

        let overlaps = changed_lines
            .range(node_start..=node_end)
            .next()
            .is_some();

        if !overlaps {
            continue;
        }

        // If this is a container node, try to narrow down to child methods/fields
        if is_container_node(&child) {
            let mut child_cursor = child.walk();
            let mut found_child = false;

            for grandchild in child.children(&mut child_cursor) {
                let gc_start = grandchild.start_position().row + 1;
                let gc_end = grandchild.end_position().row + 1;

                let gc_overlaps = changed_lines
                    .range(gc_start..=gc_end)
                    .next()
                    .is_some();

                if gc_overlaps {
                    found_child = true;
                    ranges.push(ChangedNodeRange {
                        start: gc_start,
                        end: gc_end,
                        parent_context: Some(ParentContext {
                            header_line: node_start,
                            close_line: node_end,
                        }),
                    });
                }
            }

            // If no child matched (change is in parent's direct body), use the whole parent
            if !found_child {
                ranges.push(ChangedNodeRange {
                    start: node_start,
                    end: node_end,
                    parent_context: None,
                });
            }
        } else {
            ranges.push(ChangedNodeRange {
                start: node_start,
                end: node_end,
                parent_context: None,
            });
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
///
/// `diff_mode` controls how unchanged nodes are rendered:
/// - `Default`: Only changed nodes.
/// - `Structure`: Changed + unchanged nodes as signatures.
/// - `Full`: Changed + unchanged nodes in full.
fn render_diff_file(
    file_diff: &FileDiff,
    global_flags: &[String],
    args: &[String],
    diff_mode: DiffMode,
) -> String {
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

    // For modified/renamed files, attempt AST-aware rendering.
    // Falls back to raw hunks when AST rendering is not possible.
    if let Some(ast_output) = try_ast_render(file_diff, global_flags, args, diff_mode) {
        output.push_str(&ast_output);
    } else {
        return render_raw_hunks(file_diff, &output);
    }

    output
}

/// Attempt AST-aware rendering for a modified/renamed file.
///
/// Returns `Some(rendered)` on success, `None` when the file cannot be
/// processed via tree-sitter (unsupported language, file too large, parse
/// failure, or no overlapping AST nodes).
fn try_ast_render(
    file_diff: &FileDiff,
    global_flags: &[String],
    args: &[String],
    diff_mode: DiffMode,
) -> Option<String> {
    let lang = Language::from_path(Path::new(&file_diff.path))?;

    // Languages that don't use tree-sitter (JSON, YAML, TOML)
    if lang.is_serde_based() {
        return None;
    }

    let source = get_file_source(&file_diff.path, global_flags, args).ok()?;

    // Skip AST for files > 100KB
    if source.len() > MAX_AST_FILE_SIZE {
        return None;
    }

    let mut parser = rskim_core::Parser::new(lang).ok()?;
    let tree = parser.parse(&source).ok()?;

    let changed_ranges = find_changed_node_ranges(&tree, &file_diff.hunks);
    if changed_ranges.is_empty() {
        return None;
    }

    let source_lines: Vec<&str> = source.lines().collect();
    let mut output = String::new();

    if diff_mode != DiffMode::Default {
        let ctx = ModeRenderContext {
            changed_ranges: &changed_ranges,
            hunks: &file_diff.hunks,
            source_lines: &source_lines,
            source: &source,
            lang,
            diff_mode,
        };
        render_with_unchanged_context(&mut output, &tree, &ctx);
    } else {
        render_changed_only(
            &mut output,
            &changed_ranges,
            &file_diff.hunks,
            &source_lines,
        );
    }

    Some(output)
}

/// Render only changed nodes (default mode).
///
/// For nested nodes (inside a class/struct), emits the parent declaration
/// header line before the changed child node.
fn render_changed_only(
    output: &mut String,
    changed_ranges: &[ChangedNodeRange],
    hunks: &[DiffHunk],
    source_lines: &[&str],
) {
    // Track which parent headers we have already emitted
    let mut emitted_parent_headers: HashSet<usize> = HashSet::new();

    // Pre-compute the last range index for each parent header to avoid O(N^2)
    // scanning on every iteration.
    let mut last_index_for_parent: HashMap<usize, usize> = HashMap::new();
    for (idx, range) in changed_ranges.iter().enumerate() {
        if let Some(ref ctx) = range.parent_context {
            last_index_for_parent.insert(ctx.header_line, idx);
        }
    }

    for (idx, range) in changed_ranges.iter().enumerate() {
        // Emit parent header if this is a nested node
        if let Some(ref ctx) = range.parent_context {
            if emitted_parent_headers.insert(ctx.header_line) {
                if let Some(line) = source_lines.get(ctx.header_line - 1) {
                    let _ = writeln!(output, " {line}");
                }
            }
        }

        render_node_with_hunks(output, range.start, range.end, hunks, source_lines);

        // Emit parent closing brace if this is the last child with this parent
        if let Some(ref ctx) = range.parent_context {
            let is_last = last_index_for_parent
                .get(&ctx.header_line)
                .is_some_and(|&last_idx| last_idx == idx);
            if is_last {
                if let Some(line) = source_lines.get(ctx.close_line - 1) {
                    let _ = writeln!(output, " {line}");
                }
            }
        }
    }
}

/// Shared context for mode-aware rendering functions.
///
/// Groups the parameters that are threaded through the rendering call chain
/// to stay within clippy's 7-argument limit.
struct ModeRenderContext<'a> {
    changed_ranges: &'a [ChangedNodeRange],
    hunks: &'a [DiffHunk],
    source_lines: &'a [&'a str],
    source: &'a str,
    lang: Language,
    diff_mode: DiffMode,
}

/// Render changed nodes with unchanged nodes as context (structure/full mode).
///
/// Walks all top-level AST nodes. Changed nodes get full patch rendering;
/// unchanged nodes are rendered as signatures (structure mode) or in full
/// (full mode).
fn render_with_unchanged_context(
    output: &mut String,
    tree: &tree_sitter::Tree,
    ctx: &ModeRenderContext<'_>,
) {
    let root = tree.root_node();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let node_start = child.start_position().row + 1;
        let node_end = child.end_position().row + 1;

        // Check if this top-level node contains any changed range
        let has_changes = ctx.changed_ranges.iter().any(|r| {
            // Either the range is directly this node, or it's a child within this node
            (r.start >= node_start && r.end <= node_end)
                || r.parent_context
                    .as_ref()
                    .is_some_and(|p| p.header_line == node_start)
        });

        if has_changes {
            // This node contains changes — render with full patch detail.
            // If it's a container, render parent header + changed children + context children.
            if is_container_node(&child) {
                render_container_with_mode(output, &child, ctx);
            } else {
                // Non-container changed node: render with patch
                render_node_with_hunks(output, node_start, node_end, ctx.hunks, ctx.source_lines);
            }
        } else {
            // Unchanged node: render at mode level
            render_unchanged_node(
                output,
                &child,
                ctx.source_lines,
                ctx.source,
                ctx.lang,
                ctx.diff_mode,
            );
        }
    }
}

/// Render a container node (class/struct) with mode-aware child rendering.
fn render_container_with_mode(
    output: &mut String,
    node: &tree_sitter::Node<'_>,
    ctx: &ModeRenderContext<'_>,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    // Emit parent header
    if let Some(line) = ctx.source_lines.get(node_start - 1) {
        let _ = writeln!(output, " {line}");
    }

    // Walk children of the container
    let mut child_cursor = node.walk();
    for child in node.children(&mut child_cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;

        // Skip the header line itself (already emitted)
        if child_start == node_start {
            continue;
        }

        let child_changed = ctx.changed_ranges.iter().any(|r| {
            r.start == child_start
                && r.end == child_end
                && r.parent_context
                    .as_ref()
                    .is_some_and(|p| p.header_line == node_start)
        });

        if child_changed {
            render_node_with_hunks(output, child_start, child_end, ctx.hunks, ctx.source_lines);
        } else {
            render_unchanged_node(
                output,
                &child,
                ctx.source_lines,
                ctx.source,
                ctx.lang,
                ctx.diff_mode,
            );
        }
    }

    // Emit closing brace
    if node_end > node_start {
        if let Some(line) = ctx.source_lines.get(node_end - 1) {
            let _ = writeln!(output, " {line}");
        }
    }
}

/// Render an unchanged node at the appropriate mode level.
fn render_unchanged_node(
    output: &mut String,
    node: &tree_sitter::Node<'_>,
    source_lines: &[&str],
    source: &str,
    lang: Language,
    diff_mode: DiffMode,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    match diff_mode {
        DiffMode::Full => {
            // Show unchanged nodes in full
            for line_num in node_start..=node_end {
                if let Some(line) = source_lines.get(line_num - 1) {
                    let _ = writeln!(output, " {line}");
                }
            }
        }
        DiffMode::Structure => {
            // Show unchanged nodes as structure (signatures)
            let node_text = node.utf8_text(source.as_bytes()).unwrap_or("");

            // Try to transform the node text at structure level
            let config = rskim_core::TransformConfig::with_mode(rskim_core::Mode::Structure);
            match rskim_core::transform_with_config(node_text, lang, &config) {
                Ok(transformed) => {
                    for line in transformed.lines() {
                        let _ = writeln!(output, " {line}");
                    }
                }
                Err(_) => {
                    // Fall back to showing just the first line (declaration)
                    if let Some(line) = source_lines.get(node_start - 1) {
                        let _ = writeln!(output, " {line}");
                    }
                }
            }
        }
        DiffMode::Default => {
            // Default mode: unchanged nodes are omitted (handled by caller)
        }
    }
}

/// Render a node region with hunk patch lines overlaid.
fn render_node_with_hunks(
    output: &mut String,
    node_start: usize,
    node_end: usize,
    hunks: &[DiffHunk],
    source_lines: &[&str],
) {
    let relevant_hunks: Vec<&DiffHunk> = hunks
        .iter()
        .filter(|h| {
            let hunk_start = h.new_start;
            let hunk_end = h.new_start + h.new_count.saturating_sub(1);
            hunk_start <= node_end && hunk_end >= node_start
        })
        .collect();

    if relevant_hunks.is_empty() {
        // No hunks overlap — show as unchanged context
        for line_num in node_start..=node_end {
            if let Some(line) = source_lines.get(line_num - 1) {
                let _ = writeln!(output, " {line}");
            }
        }
        return;
    }

    let mut current_new_line = node_start;

    for hunk in &relevant_hunks {
        // Output unchanged source lines before this hunk's position
        while current_new_line < hunk.new_start && current_new_line <= node_end {
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
            } else if patch_line.starts_with(' ') {
                let _ = writeln!(output, "{patch_line}");
                current_new_line += 1;
            } else if patch_line.starts_with('\\') {
                let _ = writeln!(output, "{patch_line}");
            }
        }
    }

    // Output remaining unchanged source lines to end of node
    while current_new_line <= node_end {
        if let Some(line) = source_lines.get(current_new_line - 1) {
            let _ = writeln!(output, " {line}");
        }
        current_new_line += 1;
    }
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
/// Supports:
/// - `--mode structure|full` to control context rendering
/// - `--json` for machine-readable output
///
/// Default: parses unified diff, overlays changed lines on tree-sitter AST,
/// renders changed nodes with full function boundaries and `+`/`-` markers.
fn run_diff(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_diff_help();
        return Ok(ExitCode::SUCCESS);
    }

    if user_has_flag(
        args,
        &[
            "--stat",
            "--shortstat",
            "--numstat",
            "--name-only",
            "--name-status",
            "--check",
        ],
    ) {
        return run_passthrough(global_flags, "diff", args, show_stats);
    }

    // Extract skim-specific flags before passing args to git
    let (args_no_mode, diff_mode) = extract_diff_mode(args)?;
    let (git_args, output_format) = extract_output_format(&args_no_mode);

    // Run `git diff --no-color [user args]` to get unified diff output
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["diff".to_string(), "--no-color".to_string()]);
    full_args.extend_from_slice(&git_args);

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

    let duration = output.duration;
    let raw_diff = output.stdout;

    // Handle empty diff
    if raw_diff.trim().is_empty() {
        eprintln!("No changes");
        return Ok(ExitCode::SUCCESS);
    }

    // Parse unified diff into per-file structures
    let file_diffs = parse_unified_diff(&raw_diff);

    if file_diffs.is_empty() {
        eprintln!("No changes");
        return Ok(ExitCode::SUCCESS);
    }

    // Render each file with AST-aware context
    let mut rendered_output = String::new();
    let mut diff_file_entries: Vec<DiffFileEntry> = Vec::new();

    for file_diff in &file_diffs {
        rendered_output.push_str(&render_diff_file(
            file_diff,
            global_flags,
            &git_args,
            diff_mode,
        ));
        diff_file_entries.push(DiffFileEntry {
            path: file_diff.path.clone(),
            status: DiffFileStatus::from(&file_diff.status),
            changed_regions: file_diff.hunks.len(),
        });
    }

    let result = DiffResult::new(diff_file_entries, rendered_output);

    let result_str = match output_format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize diff result: {e}"))?;
            println!("{json}");
            json
        }
        OutputFormat::Text => {
            let s = result.to_string();
            print!("{s}");
            s
        }
    };

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&raw_diff, &result_str);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking).
    // Move `raw_diff` into the call to avoid cloning the entire diff string.
    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            raw_diff,
            result_str,
            format!("skim git diff {}", args.join(" ")),
            crate::analytics::CommandType::Diff,
            duration,
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

    let (filtered_args, output_format) = extract_output_format(args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["log".to_string(), "--format=%h %s (%cr) <%an>".to_string()]);

    if !has_limit_flag(&filtered_args) {
        full_args.extend(["-n".to_string(), "20".to_string()]);
    }

    full_args.extend_from_slice(&filtered_args);

    run_parsed_command(&full_args, show_stats, output_format, parse_log)
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
    // --mode without value error (edge case)
    // ========================================================================

    #[test]
    fn test_parse_diff_mode_missing_value() {
        let args: Vec<String> = vec!["--mode".into()];
        let result = extract_diff_mode(&args);
        assert!(result.is_err(), "expected error when --mode has no value");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("requires a value"),
            "expected 'requires a value' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_parse_diff_mode_short_m_passed_through_to_git() {
        // `-m` is a valid git flag, so it must NOT be consumed by skim.
        let args: Vec<String> = vec!["-m".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Default);
        assert_eq!(filtered, vec!["-m"]);
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
        assert!(
            ranges[0].start <= 2,
            "changed range should start at or before line 2"
        );
        assert!(
            ranges[0].end >= 2,
            "changed range should end at or after line 2"
        );
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

    #[test]
    fn test_find_changed_nodes_nested_class_method() {
        // Gap 3: verify nested node detection narrows to child method
        let source = "class Greeter {\n  greet(name: string) {\n    return `Hello, ${name}`;\n  }\n  farewell(name: string) {\n    return `Bye, ${name}`;\n  }\n}\n";

        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        // Simulate a hunk that changes line 3 (inside greet method)
        let hunks = vec![DiffHunk {
            old_start: 3,
            old_count: 1,
            new_start: 3,
            new_count: 1,
            patch_lines: vec![
                "-    return `Hello, ${name}`;".to_string(),
                "+    return `Hi, ${name}`;".to_string(),
            ],
        }];

        let ranges = find_changed_node_ranges(&tree, &hunks);
        assert!(
            !ranges.is_empty(),
            "expected at least one changed node range"
        );

        // Should have parent context since greet is inside Greeter class
        let first = &ranges[0];
        assert!(
            first.parent_context.is_some(),
            "expected parent context for nested node"
        );
        let parent = first.parent_context.as_ref().unwrap();
        assert_eq!(
            parent.header_line, 1,
            "parent header should be class declaration"
        );
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

    // (resolve_work_tree tests are in the Gap 4 section below)

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
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default);
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
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default);
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
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default);
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
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default);
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

    // ========================================================================
    // DiffMode extraction tests (Gap 1)
    // ========================================================================

    #[test]
    fn test_parse_diff_mode_extraction_structure() {
        let args: Vec<String> = vec!["--cached".into(), "--mode".into(), "structure".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Structure);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_parse_diff_mode_extraction_full() {
        let args: Vec<String> = vec!["--mode=full".into(), "--cached".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Full);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_parse_diff_mode_extraction_default() {
        let args: Vec<String> = vec!["--cached".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Default);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_parse_diff_mode_short_m_not_consumed_as_mode() {
        // `-m` conflicts with git's own `-m` flag, so skim should NOT treat
        // it as `--mode`. Both `-m` and the next arg should pass through.
        let args: Vec<String> = vec!["-m".into(), "structure".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Default);
        assert_eq!(filtered, vec!["-m", "structure"]);
    }

    #[test]
    fn test_parse_diff_mode_extraction_unknown_mode() {
        let args: Vec<String> = vec!["--mode".into(), "unknown".into()];
        let result = extract_diff_mode(&args);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("unknown diff mode"),
            "expected 'unknown diff mode' in error, got: {err_msg}"
        );
    }

    // ========================================================================
    // Path resolution with -C flag tests (Gap 4)
    // ========================================================================

    #[test]
    fn test_resolve_work_tree_with_c_flag() {
        let flags: Vec<String> = vec!["-C".into(), "/other/repo".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, Some(PathBuf::from("/other/repo")));
    }

    #[test]
    fn test_resolve_work_tree_with_work_tree_flag() {
        let flags: Vec<String> = vec!["--work-tree".into(), "/other/repo".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, Some(PathBuf::from("/other/repo")));
    }

    #[test]
    fn test_resolve_work_tree_with_work_tree_equals() {
        let flags: Vec<String> = vec!["--work-tree=/other/repo".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, Some(PathBuf::from("/other/repo")));
    }

    #[test]
    fn test_resolve_work_tree_none() {
        let flags: Vec<String> = vec!["--no-pager".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_file_source_with_c_flag_path() {
        // Create a temp dir with a file
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let global_flags: Vec<String> =
            vec!["-C".into(), dir.path().to_string_lossy().into_owned()];
        let args: Vec<String> = vec![];

        let result = get_file_source("test.txt", &global_flags, &args);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "hello world");
    }
}
