//! df pass-through handler (ADR-009).
//!
//! df output is byte-identical to native at typical filesystem counts; any
//! structured re-encoding adds overhead rather than saving tokens:
//!
//! - `df -h` (1294 B/14 L) and `df` (1476 B/14 L) are byte-identical to
//!   native — entries were pushed verbatim so the compressed view was always
//!   larger and the net-savings guard always rejected it.
//! - The 100-entry cap is unreachable on any real system (filesystem counts
//!   are typically <20), so the structured parser provided no protection.
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
    program: "df",
    env_overrides: &[],
    install_hint: "df is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

/// Run `skim df [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Parse function: always native passthrough.
///
/// df's stdout is already in native column format — the header row plus one
/// data row per mounted filesystem.  The structured parser re-emitted those
/// rows verbatim (no text compression), so any positive token delta came
/// entirely from the FileResult wrapper overhead.  The net-savings guard
/// therefore rejected every invocation, making the structured parser dead
/// code in practice.
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
        // serves output.stdout byte-faithfully (native header+data format).
        let input = "Filesystem      Size  Used Avail Use% Mounted on\n/dev/sda1        50G   20G   30G  40% /\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough tier (native df output): got {}",
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
            "Empty df output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    /// parse_impl returns RawPassthrough regardless of filesystem count —
    /// confirming the caller serves output.stdout directly rather than
    /// re-assembling a structured result that was always larger than native.
    #[test]
    fn test_native_output_all_rows_pass_through() {
        let input: String =
            std::iter::once(
                "Filesystem     1K-blocks      Used Available Use% Mounted on\n".to_string(),
            )
            .chain((1..=5).map(|i| {
                format!("/dev/sd{i}     103081248  45234120  52583660  47% /mnt/disk{i}\n")
            }))
            .collect();
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_impl must return RawPassthrough for lossless stdout pass-through"
        );
    }

    /// df -h output (human-readable sizes) reaches the caller verbatim.
    /// The structured parser emitted these rows unchanged anyway, so
    /// passthrough is the correct and cheaper implementation.
    #[test]
    fn test_human_readable_output_passes_through() {
        let input = "Filesystem      Size  Used Avail Use% Mounted on\n/dev/sda1        50G   20G   30G  40% /\ntmpfs           7.8G  1.2G  6.6G  16% /dev/shm\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "human-readable df output must use RawPassthrough so stdout is served verbatim: {}",
            result.tier_name()
        );
    }
}
