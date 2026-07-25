//! find pass-through handler (ADR-009).
//!
//! find output is byte-identical to native at small result sets; any
//! structured re-encoding drops paths and breaks agent enumeration workflows:
//!
//! - Byte-exact at n=1..80; entries were `trimmed.to_string()` (verbatim).
//! - `find crates -name '*.rs'` lost 355 of 457 paths — the worst case for
//!   an agent enumerating a codebase.
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
    program: "find",
    env_overrides: &[],
    install_hint: "find is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
};

/// Run `skim find [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // find has no useful flag injections — its output format is always line-per-path.
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Parse function: always native passthrough.
///
/// find's stdout is already in native line-per-path format — structurally
/// minimal and pipe-safe for `xargs`, `wc -l`, and downstream path consumers.
/// The structured parser re-emitted paths verbatim and silently dropped any
/// path past the 100-entry cap, losing the majority of results for large
/// codebases (`find crates -name '*.rs'` lost 355 of 457 paths).
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
        // serves output.stdout byte-faithfully (native line-per-path format).
        let input = "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "parse_impl must be Passthrough tier (native line-per-path): got {}",
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
            "Empty find output should be Passthrough, got {}",
            result.tier_name()
        );
    }

    /// parse_impl returns RawPassthrough regardless of path count — confirming
    /// execution.rs emits output.stdout directly so no paths are dropped.
    #[test]
    fn test_native_output_line_count_parity() {
        let input: String = (1..=10)
            .map(|i| format!("./crates/rskim/src/file{i}.rs\n"))
            .collect();
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "parse_impl must return RawPassthrough for lossless stdout pass-through"
        );
    }

    /// find can exit non-zero when some paths are inaccessible but still
    /// emit results for paths it could read.  RawPassthrough ensures those
    /// partial results reach the caller verbatim regardless of exit code.
    #[test]
    fn test_partial_results_on_error_pass_through() {
        let input = "./src/main.rs\n./src/lib.rs\n";
        let output = make_output(input);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::RawPassthrough),
            "partial find results must use RawPassthrough so all found paths are served: {}",
            result.tier_name()
        );
    }
}
