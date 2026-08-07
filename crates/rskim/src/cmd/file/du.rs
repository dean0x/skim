//! du pass-through handler (ADR-009).
//!
//! du output is byte-identical to native at small counts; structured
//! re-encoding drops directory summary rows and breaks agent workflows:
//!
//! - Byte-exact at n=1..80; `du -sh` (the dominant invocation) is identical.
//! - `du` emits directory totals LAST, so a 201-entry invocation dropped the
//!   summary row — the most useful line in the output.
//! - The parser used a silent `break` past MAX_INPUT_LINES rather than
//!   returning `None`, violating the lossless-degrade contract (ADR-009).
//!
//! We therefore emit native output byte-faithfully via [`super::passthrough_parse`].

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "du",
    env_overrides: &[],
    install_hint: "du is typically pre-installed on Unix systems",
    family: "file",
    // skip_ansi_strip: true — execution.rs runs the ANSI-strip step BEFORE parse()
    // and shadows the `output` binding.  du's parse_impl returns RawPassthrough
    // (ignoring its argument), so the stripped bytes ARE what the reader receives.
    // du's POSIX format is `size<TAB>path`; strip_ansi_escapes drops ALL C0
    // controls including \t (0x09) whenever any ESC byte appears anywhere in the
    // buffer, destroying the tab on every line.  Setting true ensures byte-faithful
    // passthrough (ADR-014 / PF-006).
    skip_ansi_strip: true,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

/// Run `skim du [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Parse function: always native passthrough.
///
/// du's stdout is already in native `size<TAB>path` format — the structured
/// parser re-emitted entries in the same format with no text compression.
/// Because `du` emits subdirectory totals before the final summary row, any
/// cap-based truncation silently dropped the summary — the most useful line.
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

    #[test]
    fn test_parse_impl_is_passthrough() {
        // parse_impl must always return RawPassthrough so that execution.rs
        // serves output.stdout byte-faithfully (native size<TAB>path format).
        let input = "4\t./src/main.rs\n8\t./src/lib.rs\n12\t.\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough tier (native du output): got {}",
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
            "Empty du output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    /// parse_impl returns RawPassthrough regardless of entry count — confirming
    /// the directory summary row (emitted last by du) always reaches the caller.
    #[test]
    fn test_native_output_summary_row_passes_through() {
        let input: String = (1..=5)
            .map(|i| format!("4\t./file{i}.rs\n"))
            .chain(std::iter::once("20\t.\n".to_string()))
            .collect();
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_impl must return RawPassthrough so the summary row is not dropped"
        );
    }

    /// du -sh output (single summary line) reaches the caller verbatim.
    /// This is the dominant invocation; the structured parser was byte-identical
    /// to native here anyway, making passthrough the correct implementation.
    #[test]
    fn test_du_sh_single_summary_passes_through() {
        let input = " 42M\t.\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "du -sh output must use RawPassthrough so stdout is served verbatim: {}",
            result.tier_name()
        );
    }
}
