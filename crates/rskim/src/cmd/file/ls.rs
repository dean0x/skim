//! `ls` pass-through and `tree` compression handler (#116, #317).
//!
//! Handles both `skim ls` and `skim tree`, dispatched from `mod.rs`
//! via the `tool_name` parameter.
//!
//! ## ls — native passthrough (ADR-009)
//!
//! `ls` and `ls -la` output is byte-identical to native for typical directory
//! sizes (n=1..80 entries) — the net-savings guard rejects the parsed view, so
//! the structured parser produced zero benefit in the common case.  At larger
//! counts (≥202 entries) the prior structured parser silently dropped entries
//! AND discarded the native `total N` block-count header, causing `| tail -1`
//! and `| wc -l` to diverge from native output — a violation of the
//! lossless-degrade contract.  We therefore emit native output byte-faithfully
//! (ADR-009), exactly as grep and rg do.
//!
//! ## tree — structured compression
//!
//! `tree` genuinely compresses because its JSON / box-drawing output is large
//! and reducible to a summary (dirs + files counts, depth-capped entry list).
//!
//! **tree tiers:**
//! - **Tier 1 (Full)**: Parse `tree -J` JSON output
//! - **Tier 2 (Degraded)**: Regex on box-drawing text, capture summary line
//! - **Tier 3 (Passthrough)**: Raw output

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use super::{MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

/// Maximum byte length of JSON input accepted for Tier 1 tree JSON parsing.
///
/// Inputs larger than this are skipped and fall through to the regex tier,
/// preventing unbounded allocation on pathological or adversarial responses.
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

const CONFIG_LS: ToolRunConfig<'static> = ToolRunConfig {
    program: "ls",
    env_overrides: &[],
    install_hint: "ls is typically pre-installed on Unix systems",
    family: "file",
    // skip_ansi_strip: false — ls is a pure passthrough (ADR-009); ANSI stripping
    // is not needed because native output is served byte-faithfully without parsing.
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    // parse_ls_impl always returns RawPassthrough, so the net-savings guard
    // never runs.  The flag is false (its semantically correct default) to
    // avoid implying a skip is needed when there is nothing to compare.
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

const CONFIG_TREE: ToolRunConfig<'static> = ToolRunConfig {
    program: "tree",
    env_overrides: &[],
    install_hint: "Install tree via your package manager (e.g., brew install tree)",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

/// Matches tree summary line: `N directories, M files`
static RE_TREE_SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+) director(?:y|ies),\s*(\d+) files?").unwrap());

/// Matches tree box-drawing lines (both Unicode and ASCII).
/// Unicode: `├── ` / `└── ` / `│   ` ; ASCII: `|-- ` / `+-- ` / `\-- `
static RE_TREE_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\|\+\\\u{251C}\u{2514}\u{2502}\s]").unwrap());

/// Run `skim ls [args...]` or `skim tree [args...]`.
///
/// `tool_name` is either "ls" or "tree", passed by the dispatcher.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
    tool_name: &str,
) -> anyhow::Result<std::process::ExitCode> {
    match tool_name {
        "tree" => run_tool(CONFIG_TREE, args, ctx, prepare_tree_args, parse_tree),
        _ => run_tool(CONFIG_LS, args, ctx, |_| {}, parse_ls_impl),
    }
}

// ============================================================================
// ls: parse (native passthrough)
// ============================================================================

/// Parse function: always native passthrough.
///
/// `ls` output is byte-faithful at native — the structured parser produced no
/// net savings for typical directory sizes and silently dropped entries at
/// large counts (≥202 entries), discarding the native `total N` block-count
/// header and causing `| tail -1` / `| wc -l` to diverge from native output.
/// We therefore emit native output byte-faithfully (ADR-009), exactly as grep
/// and rg do.
///
/// Delegates to [`super::passthrough_parse`] so the byte-faithful contract
/// (ADR-009) has a single implementation across all pure-passthrough handlers.
fn parse_ls_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    super::passthrough_parse(output)
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
            vec!["tree: structured parse failed, using regex".to_string()],
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
///
/// Entries deeper than `MAX_DEPTH` are elided to cap output size. The count of
/// elided entries is surfaced in the footer as `(N deeper entries hidden)` so
/// agents know the tree was truncated rather than inferring the directory is flat.
fn try_parse_tree_text(stdout: &str) -> Option<FileResult> {
    const MAX_DEPTH: usize = 3;
    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut total_count = 0usize;
    let mut summary: Option<String> = None;
    let mut depth_hidden: usize = 0;

    for line in stdout.lines().take(MAX_INPUT_LINES) {
        if let Some((dirs, files)) = parse_tree_summary_line(line) {
            total_count = dirs + files;
            summary = Some(format!("{dirs} dirs, {files} files"));
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
            depth_hidden += 1;
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
    let footer = build_tree_footer(depth_hidden, summary.as_deref());
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

/// Assemble the tree footer from depth-hidden count and summary parts.
///
/// When `depth_hidden > 0`, adds `"(N deeper entries hidden)"` so agents
/// know the tree was truncated at `MAX_DEPTH` rather than assuming the
/// displayed entries are exhaustive.
///
/// Returns `None` when neither part is present.
fn build_tree_footer(depth_hidden: usize, summary: Option<&str>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if depth_hidden > 0 {
        parts.push(format!("({depth_hidden} deeper entries hidden)"));
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
    use crate::cmd::test_utils::{load_fixture, make_output};

    // ========================================================================
    // parse_ls_impl: always passthrough
    // ========================================================================

    #[test]
    fn test_parse_ls_impl_is_passthrough() {
        // parse_ls_impl must always return RawPassthrough so that execution.rs
        // serves output.stdout byte-faithfully (native ls format).
        // ls output is byte-identical to native at typical sizes; at large counts
        // the prior structured parser silently dropped entries and discarded the
        // `total N` block-count header (ADR-009).
        let input = "file1.txt\nfile2.txt\nsubdir\n";
        let output = make_output(input);
        let result = parse_ls_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_ls_impl must return RawPassthrough (byte-faithful stdout pass-through): got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_ls_impl_empty_is_passthrough() {
        let output = make_output("");
        let result = parse_ls_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "Empty ls output must return RawPassthrough, got {}",
            result.tier_name()
        );
    }

    // ========================================================================
    // tree tests
    // ========================================================================

    #[test]
    fn test_tier2_tree_basic() {
        let input = load_fixture("file", "tree_basic.txt");
        let result = try_parse_tree_text(&input);
        assert!(result.is_some(), "Expected Tier 2 tree parse to succeed");
        let result = result.unwrap();
        assert!(result.total_count > 0);
    }

    #[test]
    fn test_parse_tree_impl_produces_result() {
        let input = load_fixture("file", "tree_basic.txt");
        let output = make_output(&input);
        let result = parse_tree(&output);
        assert!(
            result.is_degraded() || result.is_full(),
            "Tree text output should degrade gracefully, got {}",
            result.tier_name()
        );
    }

    /// Degradation marker for `parse_tree` must use "tree:" prefix, not "ls:".
    /// Regression for copy-paste bug where parse_tree emitted the sibling
    /// parse_ls marker, violating the cross-handler tool-name contract
    /// established in v2.3.0 (CHANGELOG consistency review HIGH-2).
    #[test]
    fn test_parse_tree_degradation_marker_uses_tree_prefix() {
        let input = load_fixture("file", "tree_basic.txt");
        let output = make_output(&input);
        let result = parse_tree(&output);
        if let ParseResult::Degraded(_, markers) = result {
            let joined = markers.join(" ");
            assert!(
                joined.contains("tree:"),
                "parse_tree degradation marker must start with 'tree:' but got: {joined}"
            );
            assert!(
                !joined.contains("ls:"),
                "parse_tree degradation marker must NOT contain 'ls:' but got: {joined}"
            );
        }
        // If Full or Passthrough, the marker contract is not exercised — that's OK.
    }

    #[test]
    fn test_empty_output_passthrough() {
        let output = make_output("");
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

    /// build_tree_footer must include count of depth-hidden entries when > 0.
    #[test]
    fn test_build_tree_footer_depth_hidden_count() {
        let footer = build_tree_footer(7, None);
        assert!(
            footer.is_some(),
            "Footer must be Some when depth_hidden > 0"
        );
        let footer = footer.unwrap();
        assert!(
            footer.contains("7 deeper entries hidden"),
            "Footer must include count: {footer}"
        );
    }

    /// build_tree_footer with depth_hidden == 0 and no summary returns None.
    #[test]
    fn test_build_tree_footer_no_hidden_no_summary() {
        let footer = build_tree_footer(0, None);
        assert!(
            footer.is_none(),
            "Footer must be None when nothing to report"
        );
    }

    /// tree text with deeply nested entries must report count of hidden entries.
    #[test]
    fn test_tree_text_depth_cap_reports_count() {
        // 4 levels deep (depth 3 is the max; depth 4 triggers elision)
        let text = "|-- src\n\
                    |   |-- lib.rs\n\
                    |   |   |-- deep.rs\n\
                    |   |   |   |-- very_deep.rs\n\
                    |   |   |   |   |-- too_deep.rs\n\
                    0 directories, 5 files\n";
        let result = try_parse_tree_text(text).expect("must parse");
        let footer = result.footer.as_deref().unwrap_or("");
        assert!(
            footer.contains("deeper entries hidden"),
            "Footer must mention hidden entries when depth cap is hit: {footer}"
        );
    }
}
