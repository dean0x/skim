//! grep parser (#116, #317).
//!
//! Parses `grep` output into structured `FileResult`.
//! grep has no JSON output mode, so regex is the best (and only) structured tier.
//!
//! Tiers:
//! - **Tier 1 (Full)**: Parse `file:line:content` format, group by file
//! - **Tier 2 (Passthrough)**: Raw output
//!
//! Attribution is argv-aware (#317): grep only prints `file:` prefixes when
//! searching multiple files (or `-r`/`-H`). [`GrepArgs::scan`] classifies the
//! arguments so single-file output is attributed to the real operand path
//! instead of the `<stdin>` mislabel, and so lines are never run through the
//! `file:line:content` regex when grep could not have produced that format.

use std::collections::BTreeMap;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use super::{MAX_INPUT_LINES, build_file_result, try_parse_file_line_content};
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "grep",
    env_overrides: &[],
    install_hint: "grep is typically pre-installed. For better compression, install ripgrep: https://github.com/BurntSushi/ripgrep",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[1],
    forward_stderr: true,
    // Group-by-file ALWAYS, regardless of result size. The net-savings guard
    // would flip small result sets back to the raw `file:line:content` format,
    // so the SAME `grep -n` produced two different shapes depending on match
    // volume — the cross-invocation inconsistency agents flagged. Skipping the
    // guard makes the grouped format the single, predictable output. The cost is
    // that tiny greps may render a few bytes larger than raw; that trade-off is
    // an accepted, deliberate relaxation of the never-larger-than-raw guard for
    // grep/rg (not the byte-faithful #317 *content* guarantee — every matching
    // line is still emitted exactly once). Passthrough cases (-c/-l/-L,
    // unparseable output, over the line-bound) are unaffected: parse returns the
    // passthrough tier and the guard branch is skipped regardless of this flag.
    skip_net_savings_guard: true,
};

/// Run `skim grep [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection for grep -- flags are too varied
    let grep_args = GrepArgs::scan(args);
    run_tool(
        CONFIG,
        args,
        ctx,
        |_| {},
        move |output| parse_impl(output, &grep_args),
    )
}

// ============================================================================
// Argv classification
// ============================================================================

/// Long grep options whose value is the NEXT token (unless given as `--opt=value`).
const LONG_VALUE_FLAGS: &[&str] = &[
    "regexp",
    "file",
    "max-count",
    "after-context",
    "before-context",
    "context",
    "include",
    "exclude",
    "exclude-dir",
    "exclude-from",
    "devices",
    "directories",
    "binary-files",
    "label",
];

/// What the grep argv tells us about the shape of grep's output.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct GrepArgs {
    /// File operands (everything positional after the pattern), `-` included.
    file_operands: Vec<String>,
    /// `-r` / `-R` / `--recursive` / `--dereference-recursive`.
    recursive: bool,
    /// `-H` / `--with-filename` — forces `file:` prefixes.
    with_filename: bool,
    /// `-h` / `--no-filename` — suppresses `file:` prefixes.
    no_filename: bool,
    /// `-n` / `--line-number`.
    line_numbers: bool,
    /// `-c` / `-l` / `-L` — output is counts or file lists, not match lines.
    count_or_list: bool,
}

impl GrepArgs {
    /// Classify a grep argv: value-consuming flags, `--` terminator, pattern
    /// extraction, and output-shape flags.
    ///
    /// Positionals are collected first and the pattern resolved at the end:
    /// GNU grep permutes options, so `grep foo -e bar file` takes its pattern
    /// from `-e` and treats `foo` as a file operand even though it appears
    /// before the flag.
    pub(super) fn scan(args: &[String]) -> Self {
        let mut g = GrepArgs::default();
        // Pattern comes from -e/-f when given; otherwise the first positional.
        let mut has_pattern_source = false;
        let mut positionals: Vec<String> = Vec::new();
        let mut after_terminator = false;

        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();

            // Operand: after `--`, a bare `-` (stdin), or any non-flag token.
            if after_terminator || arg == "-" || !arg.starts_with('-') {
                positionals.push(arg.to_string());
                i += 1;
                continue;
            }

            if arg == "--" {
                after_terminator = true;
                i += 1;
                continue;
            }

            if let Some(long) = arg.strip_prefix("--") {
                if g.scan_long_flag(long, &mut has_pattern_source) {
                    i += 1; // long flag's value was the next token
                }
                i += 1;
                continue;
            }

            // Short flag cluster (e.g. `-rn`, `-A3`, `-epat`).
            let cluster = &arg[1..];
            if g.scan_short_cluster(cluster, &mut has_pattern_source) {
                i += 1; // cluster's value-flag consumed the next token
            }
            i += 1;
        }

        if !has_pattern_source && !positionals.is_empty() {
            positionals.remove(0); // first positional is the pattern
        }
        g.file_operands = positionals;

        g
    }

    /// Parse a single long flag (everything after `--`).
    ///
    /// Returns `true` when the flag's value is the next argv token (caller must
    /// advance the index past it).
    fn scan_long_flag(&mut self, long: &str, has_pattern_source: &mut bool) -> bool {
        let (name, has_inline_value) = match long.split_once('=') {
            Some((n, _)) => (n, true),
            None => (long, false),
        };
        match name {
            "recursive" | "dereference-recursive" => self.recursive = true,
            "with-filename" => self.with_filename = true,
            "no-filename" => self.no_filename = true,
            "line-number" => self.line_numbers = true,
            "count" | "files-with-matches" | "files-without-match" => {
                self.count_or_list = true;
            }
            "regexp" | "file" => {
                *has_pattern_source = true;
                return !has_inline_value;
            }
            _ if LONG_VALUE_FLAGS.contains(&name) => {
                return !has_inline_value;
            }
            _ => {}
        }
        false
    }

    /// Parse a short flag cluster (everything after the leading `-`).
    ///
    /// Returns `true` when a value-consuming flag was last in the cluster and
    /// its value is the next argv token (caller must advance the index past it).
    fn scan_short_cluster(&mut self, cluster: &str, has_pattern_source: &mut bool) -> bool {
        for (pos, c) in cluster.char_indices() {
            match c {
                'r' | 'R' => self.recursive = true,
                'H' => self.with_filename = true,
                'h' => self.no_filename = true,
                'n' => self.line_numbers = true,
                'c' | 'l' | 'L' => self.count_or_list = true,
                'e' | 'f' | 'm' | 'A' | 'B' | 'C' | 'D' | 'd' => {
                    if matches!(c, 'e' | 'f') {
                        *has_pattern_source = true;
                    }
                    // Value is the rest of the cluster, or (if exhausted) the next token.
                    return pos + c.len_utf8() >= cluster.len();
                }
                _ => {}
            }
        }
        false
    }

    /// When grep prints NO `file:` prefix and we know the single real target:
    /// exactly one file operand that is not `-` (stdin), no recursion, no `-H`.
    ///
    /// **Label provenance**: the returned label is the argv token verbatim —
    /// it is not verified against grep's actual filesystem access (e.g., brace
    /// expansion or shell variables are not resolved here). Downstream consumers
    /// should treat this label as "what the user asked grep to read", not as a
    /// canonical resolved path.
    fn single_unprefixed_target(&self) -> Option<&str> {
        if self.recursive || self.with_filename || self.file_operands.len() != 1 {
            return None;
        }
        let op = self.file_operands[0].as_str();
        (op != "-").then_some(op)
    }

    /// Label for output lines that carry no `file:` prefix on the multi-target
    /// or stdin paths. `None` means prefixes are expected and an unprefixed
    /// line must abort the structured parse.
    fn fallback_label(&self) -> Option<&'static str> {
        if self.no_filename {
            Some("(no filename)")
        } else if self.file_operands.is_empty() {
            Some("<stdin>")
        } else {
            None
        }
    }
}

/// Two-tier parse function: Tier 1 regex -> Passthrough.
///
/// grep has no JSON output mode, so regex is the best available format
/// and is returned as `Full` (not Degraded).
fn parse_impl(output: &CommandOutput, grep_args: &GrepArgs) -> ParseResult<FileResult> {
    // -c/-l/-L output is already minimal; regrouping would mislabel counts
    // or file names as match lines.
    if grep_args.count_or_list {
        return ParseResult::Passthrough(output.stdout.clone());
    }

    if let Some(target) = grep_args.single_unprefixed_target() {
        if let Some(result) =
            try_parse_single_target(&output.stdout, target, grep_args.line_numbers)
        {
            return ParseResult::Full(result);
        }
        return ParseResult::Passthrough(output.stdout.clone());
    }

    if let Some(result) =
        try_parse_file_line_content("grep", &output.stdout, grep_args.fallback_label())
    {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1a: single-target output (no file: prefix)
// ============================================================================

/// Matches the `line:content` prefix grep emits under `-n` for a single target.
static RE_LINENO_CONTENT: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^(\d+):(.*)$").unwrap());

/// Parse single-target grep output (no `file:` prefixes), attributing every
/// line to `label` — the real operand path, `<stdin>`, or `(no filename)`.
///
/// With `-n`, match lines are reformatted as `:{lineno}: {content}`; any line
/// that does not carry the prefix (e.g. `-A`/`-B` context lines, which use a
/// `-` separator) is kept verbatim — never dropped.
fn try_parse_single_target(text: &str, label: &str, line_numbers: bool) -> Option<FileResult> {
    if text.lines().nth(MAX_INPUT_LINES).is_some() {
        return None;
    }

    let mut matches: Vec<String> = Vec::new();
    let mut binary_notices: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line == "--" {
            continue;
        }
        if line.starts_with("Binary file ") {
            binary_notices.push(line.to_string());
            continue;
        }
        let formatted = if line_numbers {
            match RE_LINENO_CONTENT.captures(line) {
                Some(caps) => format!("  :{}: {}", &caps[1], caps[2].trim()),
                None => format!("  {line}"),
            }
        } else {
            format!("  {line}")
        };
        matches.push(formatted);
    }

    if matches.is_empty() {
        return None;
    }

    let total = matches.len();
    let mut file_matches = BTreeMap::new();
    file_matches.insert(label.to_string(), matches);
    build_file_result("grep", total, file_matches, binary_notices)
}

// ============================================================================
// Tier 1b: file:line:content regex (multi-target / recursive)
// ============================================================================

// Multi-target parsing delegates to the shared `try_parse_file_line_content`
// in `file/mod.rs` (see `GrepArgs::fallback_label` for label semantics).

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::{load_fixture, make_output};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn parse_multi(input: &str) -> Option<FileResult> {
        try_parse_file_line_content("grep", input, None)
    }

    #[test]
    fn test_tier1_grep_basic() {
        let input = load_fixture("file", "grep_basic.txt");
        let result = parse_multi(&input);
        assert!(result.is_some(), "Expected Tier 1 grep parse to succeed");
        let result = result.unwrap();
        assert!(result.total_count > 0);
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("file", "grep_basic.txt");
        let output = make_output(&input);
        let grep_args = GrepArgs::scan(&args(&["-rn", "pattern", "src/"]));
        let result = parse_impl(&output, &grep_args);
        assert!(
            result.is_full(),
            "grep regex output should be Full tier (best available), got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_empty_is_passthrough() {
        let output = make_output("");
        let grep_args = GrepArgs::scan(&args(&["pattern"]));
        let result = parse_impl(&output, &grep_args);
        assert!(
            result.is_passthrough(),
            "Empty grep output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_file_grouping() {
        let input = "src/a.rs:1:fn main() {}\nsrc/a.rs:2:    println!()\nsrc/b.rs:5:fn run() {}";
        let result = parse_multi(input).unwrap();
        let rendered = format!("{result}");
        assert!(rendered.contains("src/a.rs"), "Should include file a.rs");
        assert!(rendered.contains("src/b.rs"), "Should include file b.rs");
    }

    #[test]
    fn test_all_matches_emitted_no_per_file_cap() {
        // 10 matches in one file — every one must appear (#317: never truncate).
        let input: String = (1..=10)
            .map(|i| format!("src/big.rs:{i}:match line {i}\n"))
            .collect();
        let result = parse_multi(&input).unwrap();
        assert_eq!(result.total_count, 10);
        assert_eq!(result.shown_count, 10, "shown must equal total");
        let rendered = format!("{result}");
        let match_lines: usize = rendered
            .lines()
            .filter(|l| l.trim().starts_with(':'))
            .count();
        assert_eq!(match_lines, 10, "all 10 matches must be emitted");
        assert!(
            !rendered.contains("showing"),
            "no '(showing K)' qualifier: {rendered}"
        );
    }

    #[test]
    fn test_all_files_emitted_no_file_cap() {
        // 60 files (former MAX_FILES_SHOWN was 50) — every file must appear.
        let input: String = (1..=60)
            .map(|i| format!("src/file{i:02}.rs:1:hit\n"))
            .collect();
        let result = parse_multi(&input).unwrap();
        let rendered = format!("{result}");
        for i in 1..=60 {
            assert!(
                rendered.contains(&format!("src/file{i:02}.rs")),
                "file {i} missing from output"
            );
        }
        assert!(!rendered.contains("more files"), "no elision footer");
    }

    /// D1 (#370): single file → header `grep 1`, footer `1 file`.
    /// No `GREP:` prefix, no `matches in` double-header.
    #[test]
    fn test_file_count_footer_singular() {
        let input = "src/a.rs:1:hello world\n";
        let result = parse_multi(input).unwrap();
        let rendered = format!("{result}");
        assert!(
            rendered.contains("grep "),
            "canonical header must contain tool name: {rendered}"
        );
        assert!(
            !rendered.contains("matches in"),
            "must not contain double-header 'matches in': {rendered}"
        );
        assert!(
            !rendered.contains("GREP:"),
            "must not contain 'GREP:' prefix: {rendered}"
        );
        assert!(
            rendered.trim_end().ends_with("1 file"),
            "footer must be '1 file' (singular): {rendered}"
        );
    }

    /// D1 (#370): two files → header `grep N`, footer `2 files`.
    #[test]
    fn test_file_count_footer_plural() {
        let input = "src/a.rs:1:hello\nsrc/b.rs:2:world\n";
        let result = parse_multi(input).unwrap();
        let rendered = format!("{result}");
        assert!(
            !rendered.contains("matches in"),
            "must not contain double-header: {rendered}"
        );
        assert!(
            rendered.trim_end().ends_with("2 files"),
            "footer must be '2 files' (plural): {rendered}"
        );
    }

    #[test]
    fn test_display_format() {
        let input = "src/a.rs:1:fn main() {}\nsrc/b.rs:2:fn run() {}";
        let result = parse_multi(input).unwrap();
        let rendered = format!("{result}");
        assert!(
            rendered.contains("grep "),
            "Header should contain tool name"
        );
    }

    #[test]
    fn test_input_over_line_bound_degrades_to_passthrough() {
        let mut input = String::new();
        for i in 0..(MAX_INPUT_LINES + 1) {
            input.push_str(&format!("src/a.rs:{i}:x\n"));
        }
        assert!(
            parse_multi(&input).is_none(),
            "over-bound input must return None (lossless passthrough), not truncate"
        );
    }

    // ========================================================================
    // GrepArgs::scan
    // ========================================================================

    #[test]
    fn test_scan_single_file_operand() {
        let g = GrepArgs::scan(&args(&["-n", "pattern", "/tmp/t.txt"]));
        assert_eq!(g.file_operands, vec!["/tmp/t.txt"]);
        assert!(g.line_numbers);
        assert_eq!(g.single_unprefixed_target(), Some("/tmp/t.txt"));
    }

    #[test]
    fn test_scan_pattern_via_dash_e() {
        // With -e, ALL positionals are file operands.
        let g = GrepArgs::scan(&args(&["-e", "pat", "a.txt", "b.txt"]));
        assert_eq!(g.file_operands, vec!["a.txt", "b.txt"]);
        assert!(g.single_unprefixed_target().is_none());
    }

    #[test]
    fn test_scan_attached_short_value_not_operand() {
        // -epat: pattern attached to the flag; lone positional is the file.
        let g = GrepArgs::scan(&args(&["-epat", "file.txt"]));
        assert_eq!(g.file_operands, vec!["file.txt"]);
        // -m5 / -A3: values attached, not consumed from next token.
        let g = GrepArgs::scan(&args(&["-m5", "-A3", "pat", "file.txt"]));
        assert_eq!(g.file_operands, vec!["file.txt"]);
    }

    #[test]
    fn test_scan_cluster_flags() {
        let g = GrepArgs::scan(&args(&["-rn", "pat", "src/"]));
        assert!(g.recursive);
        assert!(g.line_numbers);
        assert!(
            g.single_unprefixed_target().is_none(),
            "recursive output has file: prefixes"
        );
    }

    #[test]
    fn test_scan_terminator() {
        let g = GrepArgs::scan(&args(&["--", "pat", "file.txt"]));
        assert_eq!(g.file_operands, vec!["file.txt"]);
    }

    #[test]
    fn test_scan_long_value_flag_consumes_next_token() {
        // "src" is --include's value, not a file operand.
        let g = GrepArgs::scan(&args(&["--include", "*.rs", "pat", "src"]));
        assert_eq!(g.file_operands, vec!["src"]);
        let g = GrepArgs::scan(&args(&["--include=*.rs", "pat", "src"]));
        assert_eq!(g.file_operands, vec!["src"]);
    }

    #[test]
    fn test_scan_stdin_dash_and_h_flags() {
        let g = GrepArgs::scan(&args(&["pat", "-"]));
        assert_eq!(g.file_operands, vec!["-"]);
        assert!(g.single_unprefixed_target().is_none(), "- is stdin");
        assert_eq!(g.fallback_label(), None);

        let g = GrepArgs::scan(&args(&["pat"]));
        assert_eq!(g.fallback_label(), Some("<stdin>"));

        let g = GrepArgs::scan(&args(&["-h", "pat", "a.txt", "b.txt"]));
        assert_eq!(g.fallback_label(), Some("(no filename)"));

        let g = GrepArgs::scan(&args(&["-H", "pat", "a.txt"]));
        assert!(
            g.single_unprefixed_target().is_none(),
            "-H forces file: prefixes"
        );
    }

    #[test]
    fn test_scan_count_and_list_modes() {
        for flag in ["-c", "-l", "-L", "--count", "--files-with-matches"] {
            let g = GrepArgs::scan(&args(&[flag, "pat", "a.txt"]));
            assert!(g.count_or_list, "{flag} must set count_or_list");
        }
    }

    // ========================================================================
    // Single-target attribution (#317: <stdin> mislabel fix)
    // ========================================================================

    #[test]
    fn test_single_file_attributed_to_operand_not_stdin() {
        let grep_args = GrepArgs::scan(&args(&["-n", "7", "/tmp/t.txt"]));
        let output = make_output("3:line 7 content\n14:another 7\n");
        let result = parse_impl(&output, &grep_args);
        assert!(result.is_full());
        let rendered = result.content().to_string();
        assert!(
            rendered.contains("/tmp/t.txt"),
            "must attribute to real operand: {rendered}"
        );
        assert!(
            !rendered.contains("<stdin>"),
            "must not mislabel as <stdin>: {rendered}"
        );
        assert!(rendered.contains(":3: line 7 content"), "{rendered}");
        assert!(rendered.contains(":14: another 7"), "{rendered}");
    }

    #[test]
    fn test_single_file_without_n_kept_verbatim() {
        let grep_args = GrepArgs::scan(&args(&["7", "/tmp/t.txt"]));
        let output = make_output("line with 7\n17 again\n");
        let result = parse_impl(&output, &grep_args);
        assert!(result.is_full());
        let rendered = result.content().to_string();
        assert!(rendered.contains("/tmp/t.txt"), "{rendered}");
        assert!(rendered.contains("line with 7"), "{rendered}");
        assert!(rendered.contains("17 again"), "{rendered}");
    }

    #[test]
    fn test_single_file_misattribution_killed() {
        // Content "12:34: x" previously matched the file:line:content regex and
        // was attributed to a phantom file "12". Single-target path kills this.
        let grep_args = GrepArgs::scan(&args(&["x", "/tmp/t.txt"]));
        let output = make_output("12:34: x\n");
        let result = parse_impl(&output, &grep_args);
        assert!(result.is_full());
        let rendered = result.content().to_string();
        assert!(rendered.contains("/tmp/t.txt"), "{rendered}");
        assert!(
            rendered.contains("12:34: x"),
            "content verbatim: {rendered}"
        );
    }

    #[test]
    fn test_stdin_label_when_no_operands() {
        let grep_args = GrepArgs::scan(&args(&["7"]));
        let output = make_output("7\n17\n27\n");
        let result = parse_impl(&output, &grep_args);
        assert!(result.is_full());
        let rendered = result.content().to_string();
        assert!(
            rendered.contains("<stdin>"),
            "zero operands => stdin is the honest label: {rendered}"
        );
        // D1 (#370): count lives in the canonical `grep N` header now; old
        // `"N matches in M files"` summary was removed.
        assert!(
            rendered.contains("grep 3"),
            "count in canonical header: {rendered}"
        );
        assert!(
            !rendered.contains("matches in"),
            "must not contain old summary: {rendered}"
        );
        assert!(
            !rendered.contains("GREP:"),
            "must not contain old GREP: prefix: {rendered}"
        );
        assert!(
            rendered.trim_end().ends_with("1 file"),
            "footer must show '1 file' (not contains — avoid false match on 'N files'): {rendered}"
        );
    }

    #[test]
    fn test_multi_file_unattributable_line_degrades_to_passthrough() {
        // Multi-file output without -n is `file:content` — not reliably
        // parseable. Must passthrough rather than mislabel or drop.
        let grep_args = GrepArgs::scan(&args(&["pat", "a.txt", "b.txt"]));
        let output = make_output("a.txt:some match\nb.txt:other match\n");
        let result = parse_impl(&output, &grep_args);
        assert!(
            result.is_passthrough(),
            "unattributable lines must passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_binary_notice_preserved() {
        let input = "src/a.rs:1:hit\nBinary file img.png matches\n";
        let result = parse_multi(input).unwrap();
        let rendered = format!("{result}");
        assert!(
            rendered.contains("Binary file img.png matches"),
            "binary notices are information, not noise: {rendered}"
        );
        assert_eq!(result.total_count, 1, "notice not counted as a match");
    }
}
