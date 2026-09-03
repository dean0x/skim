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

/// Run `skim grep [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection for grep — native path:line:content is already minimal.
    super::run_passthrough_tool(
        super::PassthroughSpec {
            program: "grep",
            install_hint: "grep is typically pre-installed. For better compression, install ripgrep: https://github.com/BurntSushi/ripgrep",
            // grep exits 1 when there is no match — this is benign, not an error.
            expected_exit_codes: &[1],
        },
        args,
        ctx,
        parse_impl,
    )
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
///
/// Delegates to [`super::passthrough_parse`] so the byte-faithful contract
/// (ADR-009) has a single implementation across all pure-passthrough handlers.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    super::passthrough_parse(output)
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
        // parse_impl must always return RawPassthrough so that execution.rs
        // serves output.stdout byte-faithfully (native path:line:content format).
        // The content is NOT in the parse result payload — it comes from
        // CommandOutput::stdout directly, avoiding an unnecessary clone.
        // Line-count invariant (count == match count) is verified by e2e tests.
        let input = "src/a.rs:1:fn main() {}\nsrc/b.rs:2:fn run() {}\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough tier (native path:line:content): got {}",
            result.tier_name()
        );
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_impl must return RawPassthrough (zero-clone stdout pass-through): got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_empty_is_passthrough() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty grep output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    /// parse_impl returns RawPassthrough regardless of input volume — confirming that
    /// the line-count invariant (count == match count) is enforced by execution.rs
    /// emitting output.stdout directly, not by any parse-result transformation.
    #[test]
    fn test_native_output_line_count_parity() {
        let input: String = (1..=10)
            .map(|i| format!("src/big.rs:{i}:match line {i}\n"))
            .collect();
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_impl must return RawPassthrough for lossless stdout pass-through"
        );
    }

    /// Binary-match notices are part of output.stdout — RawPassthrough ensures they
    /// reach the caller verbatim via execution.rs without going through a parse payload.
    #[test]
    fn test_binary_notice_passes_through() {
        let input = "src/a.rs:1:hit\nBinary file img.png matches\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "binary notices must use RawPassthrough so stdout is served verbatim: {}",
            result.tier_name()
        );
    }
}
