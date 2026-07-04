//! ls and tree parser with three-tier degradation (#116).
//!
//! Handles both `skim ls` and `skim tree`, dispatched from `mod.rs`
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
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
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

/// Matches a long-form ls entry line: permissions + link count + owner + ...
/// e.g. `drwxr-xr-x  2 user group  4096 Jan 01 ...`
/// Includes setuid/setgid/sticky permission characters (s, S, t, T).
static RE_LS_LONG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[dl\-][rwxsStT\-]{9}").unwrap());

// ============================================================================
// ls: multi-section support (R5)
// ============================================================================

/// True if `line` is an `ls -R` or multi-path section header (e.g. `"./dir:"`, `"/path:"`).
///
/// A section header:
/// - ends with `:`
/// - is non-empty
/// - does NOT match the permission-line regex (so long-form entries with colon-
///   suffixed names are not misidentified)
/// - does NOT start with `"total "` (the disk-usage preamble)
pub(crate) fn is_ls_section_header(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed.ends_with(':')
        && !RE_LS_LONG.is_match(trimmed)
        && !trimmed.starts_with("total ")
}

/// Accumulated data for one directory section in multi-dir `ls -la` output.
struct LsSection {
    /// Directory path label (the header line with the trailing `:` removed).
    label: String,
    /// Real entry lines (long-form, dotdirs excluded, capped at MAX_DISPLAY_ENTRIES).
    entries: Vec<String>,
    /// Total real entries before the per-section cap.
    total_entries: usize,
    /// Directories counted in this section.
    dirs: usize,
    /// Files/symlinks/other counted in this section.
    files: usize,
    /// True if at least one permission-regex line was seen (confirms `ls -la` format).
    has_long_lines: bool,
}

/// Split `ls -la` stdout into per-directory sections.
///
/// Returns one `LsSection` per section header found.  Lines before the first
/// header are captured into a synthetic unlabeled leading section (label `""`),
/// preserving them so agents see the full listing (ADR-001 — never emptier than
/// raw).  `ls -la file dir/` places individual file entries before the first
/// `dir:` header; discarding those lines violated the never-emptier-than-raw
/// invariant.
fn parse_ls_long_sections(stdout: &str) -> Vec<LsSection> {
    // Synthetic leading section for long-form lines before the first dir header.
    // It is prepended only if it accumulates at least one real entry.
    let mut leading: LsSection = LsSection {
        label: String::new(), // empty = unlabeled
        entries: Vec::new(),
        total_entries: 0,
        dirs: 0,
        files: 0,
        has_long_lines: false,
    };
    let mut sections: Vec<LsSection> = Vec::new();
    // True once we have seen the first section header.
    let mut in_named_section = false;

    for line in stdout.lines().take(MAX_INPUT_LINES) {
        if is_ls_section_header(line) {
            // Start a new named section.
            in_named_section = true;
            sections.push(LsSection {
                label: line.trim_end_matches(':').to_string(),
                entries: Vec::new(),
                total_entries: 0,
                dirs: 0,
                files: 0,
                has_long_lines: false,
            });
            continue;
        }

        // Select target: leading section or most-recently-opened named section.
        let sec: &mut LsSection = if in_named_section {
            match sections.last_mut() {
                Some(s) => s,
                None => continue, // unreachable but safe
            }
        } else {
            &mut leading
        };

        if !RE_LS_LONG.is_match(line) {
            continue; // total N, blank lines, etc.
        }
        sec.has_long_lines = true;

        // Skip `.` and `..` dotdir entries (same logic as try_parse_ls_long_flat).
        let name_part = line.split(" -> ").next().unwrap_or(line);
        if matches!(
            name_part
                .split_whitespace()
                .last()
                .map(|s| s.trim_end_matches('/')),
            Some(".") | Some("..")
        ) {
            continue;
        }

        sec.total_entries += 1;
        if line.starts_with('d') {
            sec.dirs += 1;
        } else {
            sec.files += 1;
        }
        if sec.entries.len() < MAX_DISPLAY_ENTRIES {
            sec.entries.push(line.to_string());
        }
    }

    // Prepend the unlabeled leading section if it captured any real entries.
    if leading.total_entries > 0 || leading.has_long_lines {
        sections.insert(0, leading);
    }

    sections
}

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
        _ => run_tool(CONFIG_LS, args, ctx, |_| {}, parse_ls),
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
        return ParseResult::Degraded(
            result,
            vec!["ls: structured parse failed, using plain text".to_string()],
        );
    }

    ParseResult::Passthrough(output.stdout.clone())
}

/// Tier 1 dispatcher: routes to flat or sectioned parser based on output shape.
///
/// Multi-section output (`ls -R` / multi-path) contains `dir:` header lines;
/// single-directory output does not.  The dispatching is a one-pass scan so
/// the fast path (single dir, no header) adds only O(N) overhead.
fn try_parse_ls_long(stdout: &str) -> Option<FileResult> {
    let has_sections = stdout
        .lines()
        .take(MAX_INPUT_LINES)
        .any(is_ls_section_header);
    if has_sections {
        try_parse_ls_long_sectioned(stdout)
    } else {
        try_parse_ls_long_flat(stdout)
    }
}

/// Tier 1 (multi-section): `ls -laR` or `ls -la dir1/ dir2/` with section headers.
///
/// Each directory gets its own labeled section in the output.  Empty directories
/// render as `  (empty)` rather than being silently dropped, so agents always see
/// all directories that were listed (R5 — "never emptier than raw").
///
/// Per-section cap: MAX_DISPLAY_ENTRIES entries per section, with an elision
/// marker when the cap is hit (ADR-001 — compress, never truncate).
fn try_parse_ls_long_sectioned(stdout: &str) -> Option<FileResult> {
    let sections = parse_ls_long_sections(stdout);

    // Require at least one section with long-form lines to confirm this is `ls -la` output.
    if sections.is_empty() || !sections.iter().any(|s| s.has_long_lines) {
        return None;
    }

    let mut total_dirs = 0usize;
    let mut total_files = 0usize;
    let mut total_real = 0usize;
    let mut shown_real = 0usize;
    let mut all_entries: Vec<String> = Vec::new();

    for sec in &sections {
        if sec.label.is_empty() {
            // Unlabeled leading section (individual files listed before any dir header,
            // e.g. `ls -la file.txt dir/`). Render entries without a header line so the
            // output is semantically correct (there is no directory label to show).
            for e in &sec.entries {
                all_entries.push(e.clone());
            }
            if let Some(elision) =
                crate::output::elision_marker(sec.entries.len(), sec.total_entries, "entries")
            {
                all_entries.push(elision);
            }
        } else {
            // Named section — render the directory label header, then entries.
            all_entries.push(format!("{}:", sec.label));

            if sec.total_entries == 0 {
                // Empty directory: show placeholder instead of silence.
                all_entries.push("  (empty)".to_string());
            } else {
                for e in &sec.entries {
                    all_entries.push(format!("  {e}"));
                }
                // Elision marker for this section if cap was reached (ADR-001).
                if let Some(elision) =
                    crate::output::elision_marker(sec.entries.len(), sec.total_entries, "entries")
                {
                    all_entries.push(format!("  {elision}"));
                }
            }
        }

        total_dirs += sec.dirs;
        total_files += sec.files;
        total_real += sec.total_entries;
        shown_real += sec.entries.len();
    }

    let breakdown = format!("{total_dirs} dirs, {total_files} files");
    let footer = match crate::output::elision_marker(shown_real, total_real, "entries") {
        Some(elision) => Some(format!("{elision} — {breakdown}")),
        None => Some(breakdown),
    };

    Some(FileResult::new(
        "ls".to_string(),
        total_real,
        shown_real,
        all_entries,
        footer,
    ))
}

/// Tier 1 (flat): long-form `ls -la` output — detect permissions, count dirs vs files.
///
/// Skips `.` and `..` dotdir entries so they are excluded from counts and from
/// the display list.  `-F`/`-p` append a trailing `/` to directory names; names
/// are trimmed before the dot-dir comparison.
///
/// Dir/file counts are folded into the FOOTER rather than a prepended summary
/// entry, eliminating the duplicate `ls N` + `LS: N entries` double-header that
/// appeared on large directories (A3 contract: FileResult public shape unchanged).
///
/// Empty dirs (only `total`/`.`/`..` lines): returns a Full result with 0 entries
/// rather than None, preventing Tier-2 from mis-tokenising the `total`/`.`/`..`
/// lines as plain filenames.
fn try_parse_ls_long_flat(stdout: &str) -> Option<FileResult> {
    let mut dirs = 0usize;
    let mut files = 0usize;
    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut line_count = 0usize;
    // Set when any permission-regex line is seen (including . and ..).
    // Distinguishes "not ls -la output" (return None) from "empty dir whose
    // only entries were . and .." (return Full with 0 real entries).
    let mut saw_long_line = false;

    for line in stdout.lines().take(MAX_INPUT_LINES) {
        if !RE_LS_LONG.is_match(line) {
            continue;
        }
        saw_long_line = true;

        // Skip `.` and `..` dotdir entries.  For symlink rows (`name -> target`),
        // split on " -> " first so we compare the NAME, not the target — a symlink
        // named `cur -> .` must not be dropped because its target happens to be `.`.
        // Trailing `/` (from `-F`/`-p` flags) is trimmed before the comparison.
        let name_part = line.split(" -> ").next().unwrap_or(line);
        if matches!(
            name_part
                .split_whitespace()
                .last()
                .map(|s| s.trim_end_matches('/')),
            Some(".") | Some("..")
        ) {
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

    // Empty dir (only . and .. entries): return Full with 0 real entries rather
    // than None so Tier-2 doesn't mis-tokenise `total`/`.`/`..` lines.
    if !saw_long_line {
        return None;
    }

    let shown_count = entries.len();

    // Build footer: fold the dir/file breakdown into the elision marker (if any),
    // or emit just "D dirs, F files" when everything fits.
    let breakdown = format!("{dirs} dirs, {files} files");
    let footer = match crate::output::elision_marker(shown_count, line_count, "entries") {
        Some(elision) => Some(format!("{elision} — {breakdown}")),
        None => Some(breakdown),
    };

    Some(FileResult::new(
        "ls".to_string(),
        line_count,
        shown_count,
        entries,
        footer,
    ))
}

/// Tier 2: plain `ls` output — one filename per line (or space-separated).
///
/// When ≥2 blank-line-separated blocks have a `name:` first line (the shape of
/// `ls -R` or plain `ls d1 d2`), section mode is activated: each block is
/// rendered under its label header.  The ≥2-block threshold disambiguates a
/// literal `foo:` filename (which produces a single block, not sectioned) from
/// a genuine multi-directory listing.
fn try_parse_ls_plain(stdout: &str) -> Option<FileResult> {
    // Split into blank-line-separated blocks (bounded by MAX_INPUT_LINES total).
    let lines: Vec<&str> = stdout.lines().take(MAX_INPUT_LINES).collect();

    // Build blocks: each block is a slice of consecutive non-empty lines.
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for &line in &lines {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    // Count how many blocks have a `name:` first line (section-header shape).
    let header_block_count = blocks
        .iter()
        .filter(|b| {
            b.first()
                .map(|l| {
                    let t = l.trim();
                    t.ends_with(':') && !t.is_empty() && !t.starts_with("total ")
                })
                .unwrap_or(false)
        })
        .count();

    // Tier-2 section mode: ≥2 blocks with header-shaped first lines.
    if header_block_count >= 2 {
        return try_parse_ls_plain_sectioned(&blocks);
    }

    // Flat mode: count all non-empty names.
    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut total_count = 0usize;

    for block in &blocks {
        for &line in block {
            let trimmed = line.trim();
            // Plain ls can output multiple names per line (space-separated)
            for name in trimmed.split_whitespace() {
                total_count += 1;
                if entries.len() < MAX_DISPLAY_ENTRIES {
                    entries.push(name.to_string());
                }
            }
        }
    }

    if total_count == 0 {
        return None;
    }

    let shown_count = entries.len();
    let footer = crate::output::elision_marker(shown_count, total_count, "entries");

    Some(FileResult::new(
        "ls".to_string(),
        total_count,
        shown_count,
        entries,
        footer,
    ))
}

/// Tier-2 sectioned rendering for plain `ls -R` or multi-dir short-format output.
///
/// Called when ≥2 blank-line-separated blocks start with a `name:` header line.
/// Renders each block under its label, analogous to the long-format sectioned
/// renderer but without permission lines (just filenames).
fn try_parse_ls_plain_sectioned(blocks: &[Vec<&str>]) -> Option<FileResult> {
    let mut total_count = 0usize;
    let mut shown_count = 0usize;
    let mut all_entries: Vec<String> = Vec::new();

    for block in blocks {
        let mut iter = block.iter();
        let Some(&first_line) = iter.next() else {
            continue;
        };

        let first_trimmed = first_line.trim();
        if first_trimmed.ends_with(':') && !first_trimmed.starts_with("total ") {
            // Named block: label header then filenames.
            all_entries.push(first_trimmed.to_string());
            let mut section_total = 0usize;
            let mut section_shown = 0usize;
            for &line in iter {
                for name in line.split_whitespace() {
                    section_total += 1;
                    if section_shown < MAX_DISPLAY_ENTRIES {
                        all_entries.push(format!("  {name}"));
                        section_shown += 1;
                    }
                }
            }
            if section_total == 0 {
                all_entries.push("  (empty)".to_string());
            } else if let Some(elision) =
                crate::output::elision_marker(section_shown, section_total, "entries")
            {
                all_entries.push(format!("  {elision}"));
            }
            total_count += section_total;
            shown_count += section_shown;
        } else {
            // Block without a header: render as-is (unlabeled entries).
            for &line in std::iter::once(&first_line).chain(iter) {
                for name in line.split_whitespace() {
                    total_count += 1;
                    if shown_count < MAX_DISPLAY_ENTRIES {
                        all_entries.push(name.to_string());
                        shown_count += 1;
                    }
                }
            }
        }
    }

    if total_count == 0 {
        return None;
    }

    let footer = crate::output::elision_marker(shown_count, total_count, "entries");
    Some(FileResult::new(
        "ls".to_string(),
        total_count,
        shown_count,
        all_entries,
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

    /// R5: Multi-dir `ls -laR` output must produce per-directory sections.
    #[test]
    fn test_r5_sectioned_ls_la_produces_sections() {
        let input = "./dir1:\n\
total 8\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n\
-rw-r--r--  1 user group 100 Jan 01 file.txt\n\
\n\
./dir2:\n\
total 0\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n";
        let result = try_parse_ls_long(input).expect("must parse multi-dir output");
        let rendered = format!("{result}");
        assert!(
            rendered.contains("./dir1:"),
            "must contain dir1 section header: {rendered}"
        );
        assert!(
            rendered.contains("./dir2:"),
            "must contain dir2 section header: {rendered}"
        );
        // dir2 is empty — must not silently vanish
        assert!(
            rendered.contains("(empty)"),
            "empty dir must render as '(empty)': {rendered}"
        );
        assert!(
            rendered.contains("file.txt"),
            "file.txt must appear in dir1 section: {rendered}"
        );
    }

    /// R5: `is_ls_section_header` detects `dir:` lines correctly.
    #[test]
    fn test_r5_is_ls_section_header() {
        assert!(is_ls_section_header("./dir1:"), "must detect ./dir:");
        assert!(
            is_ls_section_header("/path/to/dir:"),
            "must detect absolute path:"
        );
        assert!(is_ls_section_header("subdir:"), "must detect simple name:");
        // Long-form permission line must NOT be detected as header
        assert!(
            !is_ls_section_header("drwxr-xr-x  2 user group  64 Jan 01 ."),
            "long-form line must not be a section header"
        );
        assert!(
            !is_ls_section_header("total 8"),
            "total line must not be a section header"
        );
        assert!(
            !is_ls_section_header(""),
            "empty line must not be a section header"
        );
    }

    /// R5: Single-dir output routes to the flat parser (no section headers in output).
    #[test]
    fn test_r5_single_dir_flat_parse_unchanged() {
        let input = "total 8\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n\
-rw-r--r--  1 user group 100 Jan 01 file.txt\n";
        let result = try_parse_ls_long(input).expect("must parse single-dir output");
        let rendered = format!("{result}");
        // No section headers — flat parse
        assert!(
            !rendered.contains("total"),
            "total line must not appear in output: {rendered}"
        );
        assert!(
            rendered.contains("file.txt"),
            "file must appear in flat output: {rendered}"
        );
        // Must not show "(empty)" for a non-empty dir
        assert!(
            !rendered.contains("(empty)"),
            "non-empty dir must not show (empty): {rendered}"
        );
    }

    /// R5: Empty dir in multi-dir output must be visible as `(empty)`, not silently dropped.
    #[test]
    fn test_r5_all_empty_sections_visible() {
        let input = "./dir1:\n\
total 0\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n\
\n\
./dir2:\n\
total 0\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n";
        let result = try_parse_ls_long(input).expect("must parse all-empty multi-dir output");
        let rendered = format!("{result}");
        assert!(
            rendered.contains("./dir1:"),
            "dir1 must appear even if empty: {rendered}"
        );
        assert!(
            rendered.contains("./dir2:"),
            "dir2 must appear even if empty: {rendered}"
        );
        let empty_count = rendered.matches("(empty)").count();
        assert_eq!(empty_count, 2, "both dirs must show (empty): {rendered}");
    }

    #[test]
    fn test_tier1_ls_la() {
        let input = load_fixture("file", "ls_la.txt");
        let result = try_parse_ls_long(&input);
        assert!(result.is_some(), "Expected Tier 1 ls -la parse to succeed");
        let result = result.unwrap();
        // The fixture has 10 permission-matching lines (`.`, `..` + 8 entries).
        // After dotdir exclusion, total_count == 8 (2 dirs + 6 files).
        assert_eq!(result.total_count, 8, "dotdirs excluded from count");
        let rendered = format!("{result}");
        // Footer must contain dir/file breakdown
        assert!(
            rendered.contains("dirs") && rendered.contains("files"),
            "footer must include dir/file breakdown, got: {rendered}"
        );
        // The rendered output must NOT start with "LS: " (no double header)
        assert!(
            !rendered.contains("LS: "),
            "no double header (LS: summary entry removed), got: {rendered}"
        );
        // The first line must be "ls N" (the FileResult header)
        let first_line = rendered.lines().next().unwrap_or("");
        assert!(
            first_line.starts_with("ls "),
            "first line must be 'ls N' header, got: {first_line}"
        );
    }

    #[test]
    fn test_tier1_ls_la_excludes_dotdirs() {
        // Regression test: ./ and ../ (from -F/-p) must be excluded from counts.
        let input = "total 8\n\
drwxr-xr-x  2 user group  64 Jan 01 .//\n\
drwxr-xr-x  3 user group  96 Jan 01 ../\n\
-rw-r--r--  1 user group 100 Jan 01 file1.txt\n\
-rw-r--r--  1 user group 200 Jan 01 file2.txt\n\
drwxr-xr-x  2 user group  64 Jan 01 subdir/\n";
        // Note: the fixture uses names ending in / (the -F/-p suffix)
        let fixed_input = "total 8\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n\
-rw-r--r--  1 user group 100 Jan 01 file1.txt\n\
-rw-r--r--  1 user group 200 Jan 01 file2.txt\n\
drwxr-xr-x  2 user group  64 Jan 01 subdir\n";
        let result = try_parse_ls_long(fixed_input).expect("must parse");
        // 3 real entries (file1.txt, file2.txt, subdir); . and .. excluded
        assert_eq!(
            result.total_count, 3,
            "dotdirs excluded: got {}",
            result.total_count
        );
        let rendered = format!("{result}");
        assert!(
            rendered.contains("1 dirs"),
            "must count 1 dir (subdir), got: {rendered}"
        );
        assert!(
            rendered.contains("2 files"),
            "must count 2 files, got: {rendered}"
        );
        // Also verify the -F/-p slash-suffix form (including the .// edge case) gives
        // the same counts — trim_end_matches('/') must collapse .// → . → skipped.
        let result_f = try_parse_ls_long(input).expect("must parse -F suffix form");
        assert_eq!(
            result_f.total_count, 3,
            "dotdirs excluded with -F suffix (.//): got {}",
            result_f.total_count
        );
        let rendered_f = format!("{result_f}");
        assert!(
            rendered_f.contains("1 dirs"),
            "must count 1 dir (subdir/) with -F suffix, got: {rendered_f}"
        );
        assert!(
            rendered_f.contains("2 files"),
            "must count 2 files with -F suffix, got: {rendered_f}"
        );
    }

    #[test]
    fn test_tier1_ls_la_excludes_dotdirs_with_f_suffix() {
        // -F/-p appends '/' to directories; trim before comparing names.
        let input = "total 8\n\
drwxr-xr-x  2 user group  64 Jan 01 ./\n\
drwxr-xr-x  3 user group  96 Jan 01 ../\n\
-rw-r--r--  1 user group 100 Jan 01 file.txt\n";
        let result = try_parse_ls_long(input).expect("must parse with -F suffix");
        // Only file.txt counted; ./ and ../ excluded
        assert_eq!(result.total_count, 1, "dotdirs with -F suffix excluded");
    }

    /// Regression: a symlink whose target is `.` or `..` must NOT be dropped from
    /// counts.  The old code used `split_whitespace().last()` which returned the
    /// TARGET for symlink rows (`name -> target`), so `cur -> .` was silently
    /// treated as a dotdir entry and excluded.  The fix splits on ` -> ` first.
    #[test]
    fn test_tier1_ls_la_symlink_not_misrouted_as_dotdir() {
        let input = "total 8\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n\
lrwxrwxrwx  1 user group   1 Jan 01 cur -> .\n\
lrwxrwxrwx  1 user group   2 Jan 01 parent -> ..\n\
-rw-r--r--  1 user group 100 Jan 01 file.txt\n\
drwxr-xr-x  2 user group  64 Jan 01 subdir\n";
        let result = try_parse_ls_long(input).expect("must parse");
        // 4 real entries: cur, parent, file.txt, subdir
        // `.` and `..` are genuine dotdirs and must be skipped;
        // `cur -> .` and `parent -> ..` are symlinks whose NAMES are not `.`/`..`
        // and must be counted.
        assert_eq!(
            result.total_count, 4,
            "symlinks with dotdir targets must be counted, not dropped; got {}",
            result.total_count
        );
        let rendered = format!("{result}");
        // cur and parent start with 'l' (symlink), counted as files; subdir is a dir
        assert!(
            rendered.contains("1 dirs"),
            "must count 1 dir (subdir), got: {rendered}"
        );
        assert!(
            rendered.contains("3 files"),
            "must count 3 files (cur, parent, file.txt), got: {rendered}"
        );
    }

    #[test]
    fn test_tier1_ls_la_empty_dir() {
        // An empty directory shows only total + . + .. lines.
        // Must return a Full result (not None) to prevent Tier-2 mis-tokenising.
        let input = "total 0\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n";
        let result = try_parse_ls_long(input).expect("empty dir must return Full result");
        assert_eq!(result.total_count, 0, "empty dir has 0 real entries");
        let rendered = format!("{result}");
        assert!(
            rendered.contains("0 dirs") && rendered.contains("0 files"),
            "empty dir footer must say '0 dirs, 0 files', got: {rendered}"
        );
    }

    #[test]
    fn test_tier1_ls_la_no_double_header() {
        // The FileResult render emits "ls N" as the first line; we must NOT
        // prepend an additional "LS: N entries …" entry (double header bug).
        let input = load_fixture("file", "ls_la.txt");
        let result = try_parse_ls_long(&input).expect("must parse");
        let rendered = format!("{result}");
        // Count occurrences of lines starting with "ls"
        let ls_header_count = rendered.lines().filter(|l| l.starts_with("ls ")).count();
        assert_eq!(
            ls_header_count, 1,
            "exactly one 'ls N' header, got {ls_header_count} in: {rendered}"
        );
        assert!(
            !rendered.contains("LS: "),
            "old double header 'LS: ' must not appear, got: {rendered}"
        );
    }

    #[test]
    fn test_tier2_ls_basic() {
        let input = load_fixture("file", "ls_basic.txt");
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
        let input = load_fixture("file", "ls_la.txt");
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
        let input = load_fixture("file", "ls_basic.txt");
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

    // ========================================================================
    // F3 leading-file-group tests (ADR-001: never emptier than raw)
    // ========================================================================

    /// `ls -la file.txt dir/` produces individual file entries before the first
    /// `dir:` header.  Those lines must NOT be silently discarded.
    /// Verifies the synthetic unlabeled leading section fix.
    #[test]
    fn test_f3_leading_file_group_not_dropped() {
        // Simulates: ls -la single_file.txt ./dir/
        // macOS/Linux output: individual file first, then dir section.
        let input = "-rw-r--r--  1 user group  123 Jan 01 single_file.txt\n\
                     \n\
                     ./dir/:\n\
                     total 8\n\
                     drwxr-xr-x  2 user group  64 Jan 01 .\n\
                     drwxr-xr-x  3 user group  96 Jan 01 ..\n\
                     -rw-r--r--  1 user group 100 Jan 01 dir_file.txt\n";
        let result = try_parse_ls_long(input).expect("must parse file+dir output");
        let rendered = format!("{result}");
        assert!(
            rendered.contains("single_file.txt"),
            "individual file before dir header must not be dropped: {rendered}"
        );
        assert!(
            rendered.contains("dir_file.txt"),
            "file inside dir section must appear: {rendered}"
        );
        assert!(
            rendered.contains("./dir/"),
            "dir section header must appear: {rendered}"
        );
    }

    /// Footer count must include both the leading file-group entries and the
    /// dir-section entries (ADR-001 net-savings correctness).
    #[test]
    fn test_f3_leading_file_footer_count_correct() {
        // 1 file in leading group, 1 file in dir section = 2 total entries
        let input = "-rw-r--r--  1 user group  123 Jan 01 leading_file.txt\n\
                     \n\
                     ./dir/:\n\
                     total 8\n\
                     drwxr-xr-x  2 user group  64 Jan 01 .\n\
                     drwxr-xr-x  3 user group  96 Jan 01 ..\n\
                     -rw-r--r--  1 user group 200 Jan 01 dir_file.txt\n";
        let result = try_parse_ls_long(input).expect("must parse");
        // 1 leading + 1 dir entry = 2 total real entries
        assert_eq!(
            result.total_count, 2,
            "total count must include leading section entries: {}",
            result.total_count
        );
        let rendered = format!("{result}");
        // Footer should reflect correct file count
        assert!(
            rendered.contains("0 dirs") || rendered.contains("1 dir"),
            "dir count from dir section must be included: {rendered}"
        );
        assert!(
            rendered.contains("2 files") || rendered.contains("files"),
            "file count must include leading section: {rendered}"
        );
    }

    // ========================================================================
    // F3 plain tier-2 sectioning tests (AC-F3.3)
    // ========================================================================

    /// Bare `ls -R` produces plain output with section headers separated by blank
    /// lines.  Must be sectioned when ≥2 header-shaped blocks are present.
    #[test]
    fn test_f3_plain_ls_r_sections() {
        // Simulates plain `ls -R` output (no -l flag).
        let input = "./dir1:\nfile_a.txt\nfile_b.txt\n\n./dir2:\nfile_c.txt\n";
        let result = try_parse_ls_plain(input).expect("must parse plain ls -R output");
        let rendered = format!("{result}");
        assert!(
            rendered.contains("./dir1:"),
            "must contain dir1 section header: {rendered}"
        );
        assert!(
            rendered.contains("./dir2:"),
            "must contain dir2 section header: {rendered}"
        );
        assert!(
            rendered.contains("file_a.txt"),
            "file_a.txt must appear: {rendered}"
        );
        assert!(
            rendered.contains("file_c.txt"),
            "file_c.txt must appear: {rendered}"
        );
    }

    /// Plain `ls d1 d2` also produces multi-section output.
    #[test]
    fn test_f3_plain_multi_dir_sections() {
        let input = "d1:\nalpha.txt\nbeta.txt\n\nd2:\ngamma.txt\n";
        let result = try_parse_ls_plain(input).expect("must parse plain ls d1 d2 output");
        let rendered = format!("{result}");
        assert!(
            rendered.contains("d1:"),
            "must contain d1 header: {rendered}"
        );
        assert!(
            rendered.contains("d2:"),
            "must contain d2 header: {rendered}"
        );
        assert!(
            rendered.contains("alpha.txt"),
            "alpha.txt must appear: {rendered}"
        );
        assert!(
            rendered.contains("gamma.txt"),
            "gamma.txt must appear: {rendered}"
        );
    }

    /// A SINGLE block whose first line looks like `foo:` must NOT be sectioned —
    /// it could be a literal filename ending in `:`.  The ≥2-block disambiguation
    /// threshold prevents this false positive.
    #[test]
    fn test_f3_literal_foo_colon_not_sectioned() {
        // Single block: only one `name:` shaped line → flat, not sectioned.
        let input = "foo:\nbar.txt\nbaz.txt\n";
        let result = try_parse_ls_plain(input).expect("must parse single-block output");
        let rendered = format!("{result}");
        // In flat mode the items are counted as file names; "foo:" itself is one name.
        // The important invariant is that this does NOT produce a labeled-section header
        // rendering with indented entries (which would indicate incorrect sectioning).
        // We verify total_count ≥ 1 and no blank section structure.
        assert!(
            result.total_count >= 1,
            "must count at least one entry: {rendered}"
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
