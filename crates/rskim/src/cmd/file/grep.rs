//! grep pass-through handler (#116, #317).
//!
//! grep's native `path:line:content` (multi-file / `-r` / `-H`) and
//! `lineno:content` (single-file) formats are already structurally minimal and
//! truncation-safe.  A 2-line-per-match grouped envelope (path header + indented
//! line) breaks `head -N` / `wc -l` / `sed -n` pipes because header lines inflate
//! the count above the match count, and a `head` cut can produce a dangling bare
//! path with no matches.  We therefore emit native output byte-faithfully.
//!
//! - **Match-listing modes**: Passthrough — native `path:line:content` or
//!   `lineno:content` from grep itself.
//! - **Count / file-list modes** (`-c/-l/-L`): Passthrough (already minimal;
//!   grouping would mislabel counts or file names as match lines).

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

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
    // parse_impl always returns Passthrough, so the net-savings guard never
    // runs.  The flag is false (its semantically correct default) to avoid
    // implying a skip is needed when there is nothing to compare.
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

/// Run `skim grep [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection for grep — flags are too varied.
    // GrepArgs::scan is still consulted for -c/-l/-L detection; all other
    // modes pass through native grep output unchanged.
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

/// Parse function: always native passthrough.
///
/// grep's stdout is already in native `path:line:content` (multi-file / `-r` /
/// `-H`) or `lineno:content` (single-file) format — structurally minimal and
/// truncation-safe for downstream pipes (`head -N`, `wc -l`, `sed -n`).
/// A grouped header/footer envelope inflates the line count and can produce
/// dangling bare-path lines after `head` truncation, so we emit native output
/// byte-faithfully instead.
///
/// `-c/-l/-L` output (counts / file lists) is also passed through; their
/// format is already minimal and grouping would mislabel counts as match lines.
fn parse_impl(output: &CommandOutput, _grep_args: &GrepArgs) -> ParseResult<FileResult> {
    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::make_output;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // ========================================================================
    // parse_impl: always passthrough
    // ========================================================================

    #[test]
    fn test_parse_impl_is_passthrough() {
        // parse_impl must always return Passthrough so that downstream pipes
        // (head -N / wc -l / sed -n) receive the native path:line:content
        // format grep already provides. Line count == match count.
        let input = "src/a.rs:1:fn main() {}\nsrc/b.rs:2:fn run() {}\n";
        let output = make_output(input);
        let grep_args = GrepArgs::scan(&args(&["-rn", "pattern", "src/"]));
        let result = parse_impl(&output, &grep_args);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough (native path:line:content): got {}",
            result.tier_name()
        );
        // Native output is emitted verbatim — both match lines present.
        let content = result.content();
        assert!(content.contains("src/a.rs:1:fn main"), "{content}");
        assert!(content.contains("src/b.rs:2:fn run"), "{content}");
        // Line count == match count (no header/footer lines inflating the count).
        let line_count = content.trim().lines().count();
        assert_eq!(line_count, 2, "line count must equal match count: {content}");
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

    /// Native output preserves content verbatim — match lines and no extra
    /// header/footer lines (line count parity).
    #[test]
    fn test_native_output_line_count_parity() {
        let input: String = (1..=10)
            .map(|i| format!("src/big.rs:{i}:match line {i}\n"))
            .collect();
        let output = make_output(&input);
        let grep_args = GrepArgs::scan(&args(&["-rn", "match", "src/"]));
        let result = parse_impl(&output, &grep_args);
        assert!(result.is_passthrough());
        let content = result.content();
        let line_count = content.trim().lines().count();
        assert_eq!(
            line_count, 10,
            "line count must equal match count — no header/footer: {content}"
        );
    }

    /// Binary-match notices pass through verbatim in native output.
    #[test]
    fn test_binary_notice_passes_through() {
        let input = "src/a.rs:1:hit\nBinary file img.png matches\n";
        let output = make_output(input);
        let grep_args = GrepArgs::scan(&args(&["-rn", "hit", "src/"]));
        let result = parse_impl(&output, &grep_args);
        assert!(result.is_passthrough());
        let content = result.content();
        assert!(
            content.contains("Binary file img.png matches"),
            "binary notices must pass through verbatim: {content}"
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
}
