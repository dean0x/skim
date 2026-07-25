//! wc pass-through handler (ADR-009).
//!
//! wc's native `count [file]` format is already minimal; any re-encoding
//! silently breaks downstream pipes:
//!
//! - Below 7 files output is byte-identical to native.
//! - The structured parser added a `wc N` header line and reformatted
//!   `      300 total` into ` total: 300`, so `| tail -1 | awk '{print $1}'`
//!   yielded `total:` instead of `300`.
//! - Past 100 files `elision_marker` displaced the grand total entirely
//!   (silent lossy truncation — a contract violation: parsers must return
//!   `None` at MAX_INPUT_LINES, not `break`, per cmd/file/mod.rs).
//!
//! We therefore emit native output byte-faithfully (ADR-009) via
//! [`super::passthrough_parse`].

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "wc",
    env_overrides: &[],
    install_hint: "wc is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

/// Run `skim wc [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Parse function: always native passthrough.
///
/// wc's stdout is already in native `count [file]` format — structurally
/// minimal and pipe-safe.  Re-encoding the output reformats numeric columns
/// and adds a header line that breaks `tail -1 | awk '{print $1}'` and
/// similar pipeline patterns.  Past the 100-entry cap the grand total row
/// was displaced entirely, violating the lossless-degrade contract (ADR-009).
///
/// Delegates to [`super::passthrough_parse`] so the byte-faithful contract
/// has a single implementation across all pure-passthrough handlers.
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

    #[test]
    fn test_parse_impl_is_passthrough() {
        // parse_impl must always return RawPassthrough so that execution.rs
        // serves output.stdout byte-faithfully (native count [file] format).
        let input = "      42    170   1234 src/main.rs\n     100    400   5678 src/lib.rs\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough tier (native wc output): got {}",
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
            "Empty wc output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    /// parse_impl returns RawPassthrough regardless of input volume — confirming that
    /// execution.rs emits output.stdout directly, not a parse-result transformation.
    #[test]
    fn test_native_output_line_count_parity() {
        let input: String = (1..=10)
            .map(|i| format!("      {i}      {i}     {i} file{i}.rs\n"))
            .collect();
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_impl must return RawPassthrough for lossless stdout pass-through"
        );
    }

    /// The grand total row (`total` label) must reach the caller verbatim.
    /// The previous structured parser displaced it past the 100-entry cap
    /// via `elision_marker(...).or(footer_entry)`.
    #[test]
    fn test_grand_total_line_passes_through() {
        let input = "      10     20     30 a.rs\n      20     40     60 b.rs\n      30     60     90 total\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "total row must use RawPassthrough so stdout is served verbatim: {}",
            result.tier_name()
        );
    }
}
