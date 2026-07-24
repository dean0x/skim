//! ripgrep (rg) pass-through handler (#116, #317).
//!
//! rg's native `path:line:content` format is already structurally minimal and
//! truncation-safe.  The previous `--json` injection produced a grouped
//! header/footer envelope that inflates the line count and breaks downstream
//! pipes (`head -N`, `wc -l`, `sed -n`): a `head` cut can produce a dangling
//! bare path with no matches.  We therefore emit native output byte-faithfully,
//! mirroring the grep handler (see grep.rs).
//!
//! - **Match-listing modes**: Passthrough — native `path:line:content` from rg.
//! - **Count / file-list modes** (`-c/-l/--files/--files-with-matches`): Passthrough
//!   (already minimal; grouping would mislabel counts or file names as match lines).

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "rg",
    env_overrides: &[],
    install_hint: "Install ripgrep: https://github.com/BurntSushi/ripgrep",
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

/// Run `skim rg [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection — rg's native path:line:content format is already
    // minimal and safe for downstream pipes.
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Parse function: always native passthrough.
///
/// rg's stdout is already in native `path:line:content` format — structurally
/// minimal and truncation-safe for downstream pipes (`head -N`, `wc -l`, `sed -n`).
/// A grouped header/footer envelope inflates the line count and can produce
/// dangling bare-path lines after `head` truncation, so we emit native output
/// byte-faithfully instead.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::make_output;

    // ========================================================================
    // parse_impl: always passthrough
    // ========================================================================

    #[test]
    fn test_parse_impl_is_passthrough() {
        // parse_impl must always return Passthrough so that downstream pipes
        // (head -N / wc -l / sed -n) receive the native path:line:content
        // format rg already provides.  Line count == match count.
        let input = "src/a.rs:1:fn main() {}\nsrc/b.rs:2:fn run() {}\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough (native path:line:content): got {}",
            result.tier_name()
        );
        let content = result.content();
        assert!(content.contains("src/a.rs:1:fn main"), "{content}");
        assert!(content.contains("src/b.rs:2:fn run"), "{content}");
        // Line count == match count (no header/footer lines).
        let line_count = content.trim().lines().count();
        assert_eq!(
            line_count, 2,
            "line count must equal match count: {content}"
        );
    }

    #[test]
    fn test_parse_impl_empty_is_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty rg output should be Passthrough, got {}",
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
        let result = parse_impl(&output);
        assert!(result.is_passthrough());
        let content = result.content();
        let line_count = content.trim().lines().count();
        assert_eq!(
            line_count, 10,
            "line count must equal match count — no header/footer: {content}"
        );
    }
}
