//! ps pass-through handler (ADR-009).
//!
//! ps rows are verbatim — zero per-row text compression — so the structured
//! parser's only effect was truncation; for the same 101 records skim emitted
//! 30098 B vs native 29918 B (+180 bytes LARGER).  The apparent 81% saving
//! for `ps aux` was entirely 705 dropped processes, never text compression.
//! Callers that need fewer rows should pipe through `head`.
//!
//! The parser also used a silent `break` past MAX_INPUT_LINES rather than
//! returning `None`, violating the lossless-degrade contract (ADR-009).
//!
//! We therefore emit native output byte-faithfully via [`super::passthrough_parse`].

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "ps",
    env_overrides: &[],
    install_hint: "ps is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

/// Run `skim ps [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Parse function: always native passthrough.
///
/// ps rows are copied verbatim by the structured parser (zero text reduction),
/// so the compressed view was always larger than native output.  The only
/// reduction came from dropping processes past the 100-row cap — truncation,
/// not compression.  Callers wanting fewer rows should pipe through `head`.
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
        // serves output.stdout byte-faithfully (native header+process-row format).
        let input = "USER       PID %CPU %MEM COMMAND\nroot         1  0.0  0.1 /sbin/init\nuser      1234  0.5  1.2 bash\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough tier (native ps output): got {}",
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
            "Empty ps output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    /// parse_impl returns RawPassthrough regardless of process count — confirming
    /// execution.rs emits output.stdout directly so no processes are dropped.
    #[test]
    fn test_native_output_all_processes_pass_through() {
        let header = "USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND\n";
        let input: String = std::iter::once(header.to_string())
            .chain((1..=10).map(|i| {
                format!(
                    "user     {i:>5}  0.0  0.0      0     0 ?        S    00:00   0:00 proc-{i}\n"
                )
            }))
            .collect();
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_impl must return RawPassthrough for lossless stdout pass-through"
        );
    }

    /// ps rows are emitted verbatim — zero per-row compression — so RawPassthrough
    /// is both correct and cheaper than the structured parser path.
    #[test]
    fn test_verbatim_rows_pass_through() {
        let input = "USER       PID %CPU %MEM COMMAND\nroot         1  0.0  0.1 /sbin/init\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "ps verbatim rows must use RawPassthrough so stdout is served unchanged: {}",
            result.tier_name()
        );
    }
}
