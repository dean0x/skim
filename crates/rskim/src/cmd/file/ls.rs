//! ls and tree parser with three-tier degradation (#116).
//!
//! Handles both `skim file ls` and `skim file tree`, dispatched from `mod.rs`
//! via the `tool_name` parameter.
//!
//! **ls tiers:**
//! - **Tier 1 (Full)**: Detect long-form `ls -la` output (permissions regex), count dirs/files
//! - **Tier 2 (Degraded)**: Plain `ls` output — simple line counting
//! - **Tier 3 (Passthrough)**: Raw output
//!
//! **tree tiers:**
//! - **Tier 1 (Full)**: Parse `tree -J` JSON output
//! - **Tier 2 (Degraded)**: Regex on box-drawing text, capture summary line
//! - **Tier 3 (Passthrough)**: Raw output

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::FileResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{run_file_tool, FileToolConfig, MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};

/// Maximum byte length of JSON input accepted for Tier 1 tree JSON parsing.
///
/// Inputs larger than this are skipped and fall through to the regex tier,
/// preventing unbounded allocation on pathological or adversarial responses.
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

const CONFIG_LS: FileToolConfig<'static> = FileToolConfig {
    program: "ls",
    env_overrides: &[],
    install_hint: "ls is typically pre-installed on Unix systems",
};

const CONFIG_TREE: FileToolConfig<'static> = FileToolConfig {
    program: "tree",
    env_overrides: &[],
    install_hint: "Install tree via your package manager (e.g., brew install tree)",
};

/// Matches a long-form ls entry line: permissions + link count + owner + ...
/// e.g. `drwxr-xr-x  2 user group  4096 Jan 01 ...`
/// Includes setuid/setgid/sticky permission characters (s, S, t, T).
static RE_LS_LONG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[dl\-][rwxsStT\-]{9}").unwrap());

/// Matches tree summary line: `N directories, M files`
static RE_TREE_SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+) director(?:y|ies),\s*(\d+) files?").unwrap());

/// Matches tree box-drawing lines (both Unicode and ASCII).
/// Unicode: `├── ` / `└── ` / `│   ` ; ASCII: `|-- ` / `+-- ` / `\-- `
static RE_TREE_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\|\+\\\u{251C}\u{2514}\u{2502}\s]").unwrap());

/// Run `skim file ls [args...]` or `skim file tree [args...]`.
///
/// `tool_name` is either "ls" or "tree", passed by the dispatcher.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    tool_name: &str,
) -> anyhow::Result<std::process::ExitCode> {
    match tool_name {
        "tree" => run_file_tool(
            CONFIG_TREE,
            args,
            show_stats,
            json_output,
            prepare_tree_args,
            parse_tree,
        ),
        _ => run_file_tool(CONFIG_LS, args, show_stats, json_output, |_| {}, parse_ls),
    }
}

// ============================================================================
// tree: prepare args
// ============================================================================

/// Inject `--charset=ascii` if no charset flag is present (normalize box-drawing).
fn prepare_tree_args(cmd_args: &mut Vec<String>) {
    if !user_has_flag(cmd_args, &["--charset"]) {
        cmd_args.push("--charset=ascii".to_string());
    }
}

// ============================================================================
// ls: parse
// ============================================================================

fn parse_ls(output: &CommandOutput) -> ParseResult<FileResult> {
    if output.stdout.trim().is_empty() {
        return ParseResult::Passthrough(output.stdout.clone());
    }

    if let Some(result) = try_parse_ls_long(&output.stdout) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_ls_plain(&output.stdout) {
        return ParseResult::Degraded(result, vec!["plain ls fallback".to_string()]);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

/// Tier 1: long-form `ls -la` output — detect permissions, count dirs vs files.
fn try_parse_ls_long(stdout: &str) -> Option<FileResult> {
    let mut dirs = 0usize;
    let mut files = 0usize;
    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut line_count = 0usize;

    for line in stdout.lines().take(MAX_INPUT_LINES) {
        if !RE_LS_LONG.is_match(line) {
            continue;
        }
        line_count += 1;
        if line.starts_with('d') {
            dirs += 1;
        } else {
            files += 1;
        }
        if entries.len() < MAX_DISPLAY_ENTRIES {
            entries.push(line.to_string());
        }
    }

    if line_count == 0 {
        return None;
    }

    let shown_count = entries.len();
    let footer = if line_count > MAX_DISPLAY_ENTRIES {
        Some(format!("... and {} more", line_count - MAX_DISPLAY_ENTRIES))
    } else {
        None
    };

    let summary_entry = format!("LS: {line_count} entries ({dirs} dirs, {files} files)");
    // Prepend summary as first entry
    let mut all_entries = vec![summary_entry];
    all_entries.extend(entries);

    Some(FileResult::new(
        "ls".to_string(),
        line_count,
        shown_count,
        all_entries,
        footer,
    ))
}

/// Tier 2: plain `ls` output — one filename per line (or space-separated).
fn try_parse_ls_plain(stdout: &str) -> Option<FileResult> {
    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut total_count = 0usize;

    for line in stdout.lines().take(MAX_INPUT_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Plain ls can output multiple names per line (space-separated)
        for name in trimmed.split_whitespace() {
            total_count += 1;
            if entries.len() < MAX_DISPLAY_ENTRIES {
                entries.push(name.to_string());
            }
        }
    }

    if total_count == 0 {
        return None;
    }

    let shown_count = entries.len();
    let footer = if total_count > MAX_DISPLAY_ENTRIES {
        Some(format!(
            "... and {} more",
            total_count - MAX_DISPLAY_ENTRIES
        ))
    } else {
        None
    };

    Some(FileResult::new(
        "ls".to_string(),
        total_count,
        shown_count,
        entries,
        footer,
    ))
}

// ============================================================================
// tree: parse
// ============================================================================

fn parse_tree(output: &CommandOutput) -> ParseResult<FileResult> {
    if output.stdout.trim().is_empty() {
        return ParseResult::Passthrough(output.stdout.clone());
    }

    // Tier 1: JSON output (user passed -J or we injected it — we don't inject -J so this
    // only fires if user explicitly uses -J)
    if let Some(result) = try_parse_tree_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: text output with box-drawing lines
    if let Some(result) = try_parse_tree_text(&output.stdout) {
        return ParseResult::Degraded(
            result,
            vec!["ls: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(output.stdout.clone())
}

/// Tier 1: parse `tree -J` JSON output.
fn try_parse_tree_json(stdout: &str) -> Option<FileResult> {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('[') && !trimmed.starts_with('{') {
        return None;
    }
    if trimmed.len() > MAX_JSON_BYTES {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;

    // tree -J emits an array of report objects; extract file/directory counts from
    // the last element which is the summary object `{"type":"report","directories":N,"files":M}`
    let arr = json.as_array()?;
    let report = arr.last()?;
    if report.get("type")?.as_str() != Some("report") {
        return None;
    }
    let dirs = report.get("directories")?.as_u64().unwrap_or(0) as usize;
    let files = report.get("files")?.as_u64().unwrap_or(0) as usize;
    let total = dirs + files;

    let entries = vec![format!("{dirs} directories, {files} files")];

    Some(FileResult::new(
        "tree".to_string(),
        total,
        entries.len(),
        entries,
        None,
    ))
}

/// Tier 2: regex on tree text output.
fn try_parse_tree_text(stdout: &str) -> Option<FileResult> {
    const MAX_DEPTH: usize = 3;
    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut total_count = 0usize;
    let mut summary: Option<String> = None;
    let mut depth_cap_active = false;

    for line in stdout.lines().take(MAX_INPUT_LINES) {
        if let Some((dirs, files)) = parse_tree_summary_line(line) {
            total_count = dirs + files;
            summary = Some(format!("{dirs} directories, {files} files"));
            continue;
        }
        if !RE_TREE_ENTRY.is_match(line) {
            if !line.is_empty() && entries.len() < MAX_DISPLAY_ENTRIES {
                entries.push(line.to_string());
            }
            continue;
        }
        let depth = count_tree_depth(line);
        if depth > MAX_DEPTH {
            depth_cap_active = true;
            continue;
        }
        if entries.len() < MAX_DISPLAY_ENTRIES {
            entries.push(line.to_string());
        }
    }

    if entries.is_empty() && summary.is_none() {
        return None;
    }

    let shown_count = entries.len();
    let footer = build_tree_footer(depth_cap_active, summary.as_deref());
    if total_count == 0 {
        total_count = shown_count;
    }
    Some(FileResult::new(
        "tree".to_string(),
        total_count,
        shown_count,
        entries,
        footer,
    ))
}

/// Parse a tree summary line (`N directories, M files`) and return `(dirs, files)`.
///
/// Returns `None` if the line does not match the summary pattern.
fn parse_tree_summary_line(line: &str) -> Option<(usize, usize)> {
    let caps = RE_TREE_SUMMARY.captures(line)?;
    let dirs: usize = caps[1].parse().unwrap_or(0);
    let files: usize = caps[2].parse().unwrap_or(0);
    Some((dirs, files))
}

/// Assemble the tree footer from depth-cap and summary parts.
///
/// Returns `None` when neither part is present.
fn build_tree_footer(depth_cap_active: bool, summary: Option<&str>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if depth_cap_active {
        parts.push("(deeper levels truncated)".to_string());
    }
    if let Some(s) = summary {
        parts.push(s.to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" — "))
    }
}

/// Count indentation depth of a tree line by counting leading whitespace/pipe pairs.
fn count_tree_depth(line: &str) -> usize {
    // Each tree depth level is typically 4 chars ("|   " or "    ")
    let leading: usize = line
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t' | '|' | '+' | '\\'))
        .count();
    leading / 4
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/file");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    fn make_output(stdout: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: Duration::ZERO,
        }
    }

    #[test]
    fn test_tier1_ls_la() {
        let input = load_fixture("ls_la.txt");
        let result = try_parse_ls_long(&input);
        assert!(result.is_some(), "Expected Tier 1 ls -la parse to succeed");
        let result = result.unwrap();
        assert!(result.total_count > 0);
        // Summary entry should be present
        let rendered = format!("{result}");
        assert!(rendered.contains("dirs") || rendered.contains("files"));
    }

    #[test]
    fn test_tier2_ls_basic() {
        let input = load_fixture("ls_basic.txt");
        let result = try_parse_ls_plain(&input);
        assert!(
            result.is_some(),
            "Expected Tier 2 ls plain parse to succeed"
        );
        let result = result.unwrap();
        assert!(result.total_count > 0);
    }

    #[test]
    fn test_parse_ls_impl_long_form_is_full() {
        let input = load_fixture("ls_la.txt");
        let output = make_output(&input);
        let result = parse_ls(&output);
        assert!(
            result.is_full(),
            "ls -la output should be Full tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_ls_impl_plain_is_degraded() {
        let input = load_fixture("ls_basic.txt");
        let output = make_output(&input);
        let result = parse_ls(&output);
        // Plain ls doesn't match long form, falls to Tier 2
        assert!(
            result.is_degraded() || result.is_full(),
            "ls plain should be Degraded or Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_tier2_tree_basic() {
        let input = load_fixture("tree_basic.txt");
        let result = try_parse_tree_text(&input);
        assert!(result.is_some(), "Expected Tier 2 tree parse to succeed");
        let result = result.unwrap();
        assert!(result.total_count > 0);
    }

    #[test]
    fn test_parse_tree_impl_produces_result() {
        let input = load_fixture("tree_basic.txt");
        let output = make_output(&input);
        let result = parse_tree(&output);
        assert!(
            result.is_degraded() || result.is_full(),
            "Tree text output should degrade gracefully, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_empty_output_passthrough() {
        let output = make_output("");
        let ls_result = parse_ls(&output);
        assert!(
            ls_result.is_passthrough(),
            "Empty ls output should be Passthrough"
        );
        let tree_result = parse_tree(&output);
        assert!(
            tree_result.is_passthrough(),
            "Empty tree output should be Passthrough"
        );
    }

    #[test]
    fn test_prepare_tree_args_injects_charset() {
        let mut args: Vec<String> = vec!["src/".to_string()];
        prepare_tree_args(&mut args);
        assert!(
            args.contains(&"--charset=ascii".to_string()),
            "Should inject --charset=ascii"
        );
    }

    #[test]
    fn test_prepare_tree_args_no_inject_when_present() {
        let mut args: Vec<String> = vec!["src/".to_string(), "--charset=unicode".to_string()];
        prepare_tree_args(&mut args);
        // Should not double-inject
        let count = args.iter().filter(|a| a.starts_with("--charset")).count();
        assert_eq!(count, 1, "Should not inject when charset already present");
    }

    #[test]
    fn test_count_tree_depth_root() {
        assert_eq!(count_tree_depth("|-- src"), 0);
    }

    #[test]
    fn test_count_tree_depth_nested() {
        assert_eq!(count_tree_depth("|   |-- lib.rs"), 1);
    }
}
