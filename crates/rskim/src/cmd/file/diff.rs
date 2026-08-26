//! Standalone `diff` wrapper — byte-faithful passthrough (PF-011).
//!
//! # Why there is no parser here
//!
//! Standalone `diff` is an already-minimal tool: there is nothing in its output
//! to compress.  Measured across 52 controlled cases (10→5000 lines × 1-line→100%
//! change density × scattered vs contiguous × text vs code) plus 10 real file
//! pairs, **no region of the space exists where skim's own `diff` compression
//! beats native `diff`**.  Every sub-1.0× ratio came from the fidelity guard
//! falling back and emitting verbatim `diff -u`, so that win belonged to unified
//! encoding, not to skim; wherever the guard actually KEPT skim's render, skim was
//! 1.04×–1.45× worse.  The worst case is the region intuition predicts skim should
//! win — scattered single-line changes at n=5000 / 1% scatter measured **5.02×
//! worse** (55 KB → 276 KB).
//!
//! Root cause, proven by a path-length sweep with content held identical: the
//! "compression" was header-collapse only, worth exactly **1 byte per path
//! character and independent of content**, while `FileResult::render`
//! (`output/canonical.rs`) ADDS one leading space per patch line.  Break-even sat
//! at `patch_lines < pathlen + ~30`, which real diffs never reach.  There was no
//! AST step anywhere in this module — `build_file_result` re-emitted hunk bodies
//! verbatim.
//!
//! This is the class PF-011 describes (thin wrappers over already-minimal tools),
//! generalising the ADR-009 grep conclusion.  `git diff` is explicitly NOT in this
//! class: it is natively unified, injects only the presentation-only `--no-color`,
//! and genuinely compresses (0.64×–0.94× on single-file Rust diffs).
//!
//! # Exit code semantics
//! - 0: Files are identical (no output)
//! - 1: Files differ (normal diff output) — `expected_exit_codes`
//! - 2: Error (e.g. file not found) → `UnexpectedFailure`, raw-forwarded by
//!   `execution.rs` before `parse_impl` is ever called

use std::process::ExitCode;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "diff",
    env_overrides: &[],
    install_hint: "diff is typically pre-installed on Unix systems",
    family: "file",
    // skip_ansi_strip MUST be true — `parse_impl` returns `RawPassthrough`, and
    // the ANSI-strip step in `execution.rs` runs BEFORE `parse()` and SHADOWS the
    // `output` binding, so `RawPassthrough` does NOT bypass it.  With the flag
    // false, the reader would get stripped bytes the raw tool never emitted:
    //
    // 1. PF-006 tab preservation: `diff -u` emits `--- path\t<mtime>` headers.
    // 2. ADR-012 content-byte fidelity: hunk body lines (`+`/`-`/` `) carry file
    //    CONTENT that may contain ESC/CSI bytes; removing them would diverge from
    //    raw with no loss marker (#317).  Standalone `diff` never colorizes its
    //    own output, so every ESC byte present IS content.
    //
    // The cross-family `debug_assert!` in `execution.rs` fires if this is ever
    // set back to false while `parse_impl` returns `RawPassthrough`.
    skip_ansi_strip: true,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[1],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
    raw_override: None,
    never_passthrough: false,
};

/// Run `skim diff [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_tool(CONFIG, args, ctx, prepare_args, parse_impl)
}

/// No-op: `parse_impl` is `RawPassthrough` for every input, so there is
/// nothing to normalise the format for.  The previous `-u` injection is
/// removed: it silently converted the user's normal-format request into
/// unified format while buying nothing (see module header, PF-011, PF-024).
fn prepare_args(_args: &mut Vec<String>) {}

/// Route every `diff` invocation to byte-faithful raw passthrough (A3b / PF-011).
///
/// See the module header for the measurement: there is no region of the input
/// space where compressing standalone `diff` output beats the native tool, so the
/// only correct render is the tool's own bytes.
///
/// `RawPassthrough` (not `Passthrough(String)`): the variant is payload-less and
/// `execution.rs` serves `CommandOutput::stdout` directly, avoiding a full clone
/// of the diff.  Its `tier_name()` is `"passthrough"`, which is also what
/// suppresses the compressed-output hint on this path.
fn parse_impl(_output: &CommandOutput) -> ParseResult<FileResult> {
    ParseResult::RawPassthrough
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::{load_fixture, make_output_full};

    // --- config-lock tests ---
    //
    // skip_ansi_strip MUST be true for two independent reasons (ADR-012):
    // (1) PF-006: prevents `strip_escape_sequences` from running at all, so the
    //     `\t` delimiter in `--- path\t<mtime>` headers survives verbatim.
    // (2) ADR-012: hunk body lines are file CONTENT; stripping ESC/CSI bytes with
    //     `strip_escape_sequences` would diverge from the raw tool without a loss
    //     marker (#317).
    //
    // Since A3b both reasons apply to the SAME bytes: `parse_impl` returns
    // `RawPassthrough`, so `CommandOutput::stdout` reaches the reader unparsed.
    // End-to-end byte parity is asserted in `tests/cli_e2e_diff_parity.rs`.

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_config_skip_ansi_strip_is_true() {
        assert!(
            CONFIG.skip_ansi_strip,
            "diff CONFIG.skip_ansi_strip must be true — \
             (1) PF-006: flag prevents strip_escape_sequences from running, guaranteeing \
             \\t in --- headers survives; \
             (2) ADR-012: ESC/CSI bytes in diff body CONTENT must survive byte-faithfully"
        );
    }

    // ---- prepare_args: no-op (injection removed) ----
    //
    // `prepare_args` no longer injects `-u` — `parse_impl` returns
    // `RawPassthrough` for every input, so there is nothing to normalise the
    // format for.  The E2E contract (all flag forms produce native bytes) is
    // pinned in `tests/cli_e2e_diff_parity.rs`.

    #[test]
    fn test_prepare_args_is_noop() {
        // After the injection removal, `prepare_args` must not modify args.
        let original = vec!["file1.txt".to_string(), "file2.txt".to_string()];
        let mut args = original.clone();
        prepare_args(&mut args);
        assert_eq!(
            args, original,
            "prepare_args must not modify args — no injection, no removal"
        );
    }

    // ---- parse_impl: unconditional raw passthrough (A3b / PF-011) ----
    //
    // There is no parser left to tier against.  `parse_impl` returns
    // `RawPassthrough` for EVERY input, because no region of the measured input
    // space exists where compressing standalone `diff` beats the native tool
    // (see the module header).  These tests pin that unconditionality — a future
    // "just re-enable the parser for the big-diff case" change must fail here.

    #[test]
    fn a3b_parse_impl_exit_1_differing_files_is_raw_passthrough() {
        let input = load_fixture("file", "diff_unified.txt");
        let output = make_output_full(&input, "", Some(1));
        assert!(
            matches!(parse_impl(&output), ParseResult::RawPassthrough),
            "exit 1 (files differ) must be RawPassthrough, not a compressed render"
        );
    }

    #[test]
    fn a3b_parse_impl_exit_0_identical_files_is_raw_passthrough() {
        let output = make_output_full("", "", Some(0));
        assert!(
            matches!(parse_impl(&output), ParseResult::RawPassthrough),
            "exit 0 (identical files) must be RawPassthrough — skim must not \
             synthesize a 'files are identical' line the tool never emitted"
        );
    }

    #[test]
    fn a3b_parse_impl_exit_2_error_is_raw_passthrough() {
        // `execution.rs` raw-forwards exit 2 as an UnexpectedFailure before
        // `parse_impl` is reached; this pins the fallback answer regardless.
        let output = make_output_full("diff: no such file", "", Some(2));
        assert!(
            matches!(parse_impl(&output), ParseResult::RawPassthrough),
            "exit 2 (error) must be RawPassthrough"
        );
    }

    /// `tier_name()` is what suppresses the compressed-output hint, and
    /// `content()` is empty by design — the bytes live in `CommandOutput::stdout`.
    /// Calling `content()` and treating `""` as the output is the documented
    /// misuse of this variant, so both are pinned here.
    #[test]
    fn a3b_raw_passthrough_tier_name_and_empty_content() {
        let input = load_fixture("file", "diff_unified.txt");
        let output = make_output_full(&input, "", Some(1));
        let result = parse_impl(&output);
        assert_eq!(
            result.tier_name(),
            "passthrough",
            "tier_name must be \"passthrough\" — it is what suppresses the hint"
        );
        assert_eq!(
            result.content(),
            "",
            "RawPassthrough is payload-less by design"
        );
    }
}
