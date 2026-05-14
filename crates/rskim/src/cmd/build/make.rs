//! GNU Make build output compression (#167)
//!
//! Three-tier parser for `make` output:
//!
//! - **Tier 1 (regex on combined):** Parse GCC/Clang diagnostics
//!   (`file:line:col: error|warning|note: message`), make failure lines
//!   (`make: *** [target] Error N`), and linker errors from combined
//!   stdout+stderr.
//!
//! - **Tier 2 (noise-stripped):** Strip compiler/linker invocation lines,
//!   directory-change messages, CMake progress indicators, and BSD make
//!   markers, returning remaining lines.
//!
//! - **Tier 3 (passthrough):** Return raw output when no make patterns detected.
//!
//! # Why regex, not JSON
//!
//! GNU make has no native JSON or structured output mode. Introspection
//! modes (`-p`, `-d`, `-n`, `--trace`) all produce unstructured text.
//! Regex on combined stdout+stderr is the best available strategy.

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use super::run_parsed_command;
use crate::output::ParseResult;
use crate::output::canonical::BuildResult;
use crate::runner::CommandOutput;

// ============================================================================
// Public entry point
// ============================================================================

/// Run `make` with output compression.
///
/// make writes diagnostics to both stdout and stderr depending on the compiler
/// invoked. No flag injection is needed.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_parsed_command(
        "make",
        args,
        &[],
        "install GNU make (apt install make / brew install make)",
        show_stats,
        rec,
        parse_make,
    )
}

// ============================================================================
// Regex patterns (compiled once via LazyLock)
// ============================================================================

/// GCC/Clang diagnostic: `file:line:col: (fatal )?error|warning|note: message`
static GCC_DIAGNOSTIC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+):(\d+):(\d+): ((?:fatal )?error|warning|note): (.+)$").expect("valid regex")
});

/// Make failure: `make[N]: *** [target] Error N` or `Stop`
static MAKE_FAILURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^make(?:\[\d+\])?: \*\*\* \[.+\] (?:Error \d+|Stop)").expect("valid regex")
});

/// Makefile syntax error: `Makefile:5: *** missing separator.  Stop.`
static MAKEFILE_ERROR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.+:\d+: \*\*\* .+\. +Stop\.$").expect("valid regex"));

/// Nothing-to-do / up-to-date noop messages
static MAKE_NOOP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^make(?:\[\d+\])?: (?:Nothing to be done for|'.+' is up to date)")
        .expect("valid regex")
});

/// Linker errors (GNU ld and macOS ld)
static LINKER_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:undefined reference to|cannot find -l|multiple definition of|symbol\(s\) not found|duplicate symbol|library not found for -l|ld: )").expect("valid regex")
});

/// Compiler/linker invocation lines (noise)
static COMPILER_INVOCATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[\w.\-]+-)?(?:gcc|g\+\+|cc|c\+\+|clang|clang\+\+)\s+").expect("valid regex")
});

/// Directory-change messages, section markers, and archiver lines (noise)
static DIR_NOISE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:make\[\d+\]: (?:Entering|Leaving) directory|--- .+ ---|(?:ar|ranlib|strip)\s+)",
    )
    .expect("valid regex")
});

/// CMake-style progress indicators: `[ 25%] Building ...`
static CMAKE_PROGRESS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[\s*\d+%\] (?:Building|Linking|Scanning|Generating)").expect("valid regex")
});

// ============================================================================
// Three-tier parser
// ============================================================================

/// Parse make output through three degradation tiers.
fn parse_make(output: &CommandOutput) -> ParseResult<BuildResult> {
    // Empty output → reflect exit code (`make -s` silent mode, signal-killed process)
    if output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
        let success = output.exit_code == Some(0);
        return ParseResult::Full(BuildResult::new(success, 0, 0, None, vec![]));
    }

    let combined = format!("{}\n{}", output.stdout, output.stderr);

    // Tier 1: Diagnostics
    if let Some(result) = try_tier1_diagnostics(&combined, output.exit_code) {
        return result;
    }

    // Tier 2: Noise stripping
    if let Some(result) = try_tier2_noise_strip(&combined, output.exit_code) {
        return result;
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined)
}

/// Tier 1: Extract GCC/Clang diagnostics, make failure lines, and linker errors.
///
/// Returns `Some(Full(...))` when any diagnostic pattern matches, or
/// `Some(Full(success))` immediately on a noop line. Returns `None` if no
/// make-related patterns are detected.
fn try_tier1_diagnostics(
    combined: &str,
    exit_code: Option<i32>,
) -> Option<ParseResult<BuildResult>> {
    let mut errors: usize = 0;
    let mut warnings: usize = 0;
    let mut error_messages = Vec::with_capacity(16);
    let mut any_match = false;

    for line in combined.lines() {
        if let Some(caps) = GCC_DIAGNOSTIC_RE.captures(line) {
            // GCC/Clang diagnostic: file:line:col: (fatal )?error|warning|note: message
            any_match = true;
            let severity = caps.get(4).map_or("", |m| m.as_str());
            let file = caps.get(1).map_or("", |m| m.as_str());
            let line_num = caps.get(2).map_or("", |m| m.as_str());
            let message = caps.get(5).map_or("", |m| m.as_str());

            if severity == "warning" {
                warnings += 1;
            } else if severity != "note" {
                // "error" and "fatal error" count as errors; "note" does not
                errors += 1;
            }
            error_messages.push(format!("{severity}: {message} ({file}:{line_num})"));
        } else if MAKE_FAILURE_RE.is_match(line) {
            // Make failure: make: *** [target] Error N
            any_match = true;
            errors += 1;
            error_messages.push(line.to_string());
        } else if MAKEFILE_ERROR_RE.is_match(line) {
            // Makefile syntax error: Makefile:5: *** missing separator.  Stop.
            any_match = true;
            errors += 1;
            error_messages.push(line.to_string());
        } else if LINKER_ERROR_RE.is_match(line) {
            // Linker error (GNU ld or macOS ld)
            any_match = true;
            errors += 1;
            error_messages.push(line.to_string());
        } else if !any_match && MAKE_NOOP_RE.is_match(line) {
            // Nothing-to-do / up-to-date → immediate success (only when no
            // diagnostics have been collected; a trailing noop after real errors
            // must not discard those accumulated diagnostics)
            return Some(ParseResult::Full(BuildResult::new(
                true,
                0,
                0,
                None,
                vec![],
            )));
        }
    }

    if !any_match {
        return None;
    }

    let success = exit_code == Some(0) && errors == 0;
    Some(ParseResult::Full(BuildResult::new(
        success,
        warnings,
        errors,
        None,
        error_messages,
    )))
}

/// Tier 2: Strip compiler invocations, directory-change messages, CMake progress
/// indicators, and archiver lines, returning any remaining significant lines.
///
/// Returns `Some(Degraded(...))` when at least one noise line was stripped.
/// Returns `None` if nothing matched (caller falls through to passthrough).
fn try_tier2_noise_strip(
    combined: &str,
    exit_code: Option<i32>,
) -> Option<ParseResult<BuildResult>> {
    let mut remaining_lines: Vec<&str> = Vec::new();
    let mut any_stripped = false;

    for line in combined.lines() {
        if COMPILER_INVOCATION_RE.is_match(line)
            || DIR_NOISE_RE.is_match(line)
            || CMAKE_PROGRESS_RE.is_match(line)
            || line.starts_with("compilation terminated.")
        {
            any_stripped = true;
        } else if !line.trim().is_empty() {
            remaining_lines.push(line);
        }
    }

    if !any_stripped {
        return None;
    }

    let success = exit_code == Some(0);
    let error_messages: Vec<String> = remaining_lines.iter().map(|l| l.to_string()).collect();
    Some(ParseResult::Degraded(
        BuildResult::new(success, 0, 0, None, error_messages),
        vec!["make: no diagnostics found, noise-stripped".to_string()],
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cmd/build")
    }

    fn load_fixture(name: &str) -> String {
        std::fs::read_to_string(fixtures_dir().join(name))
            .unwrap_or_else(|e| panic!("Failed to load fixture {name}: {e}"))
    }

    fn make_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            duration: Duration::from_millis(100),
        }
    }

    // ========================================================================
    // Tier 1: GCC/Clang diagnostics, make failures, linker errors, noops
    // ========================================================================

    #[test]
    fn test_tier1_errors() {
        // Compiler errors come through combined stderr (compiler + make)
        let fixture = load_fixture("make_errors.txt");
        let output = make_output("", &fixture, Some(1));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(!br.success, "expected failure");
            assert_eq!(br.errors, 3, "error + fatal error + make failure = 3");
            assert_eq!(br.warnings, 1, "1 warning");
        }
    }

    #[test]
    fn test_tier1_warnings_only() {
        let fixture = load_fixture("make_warnings_only.txt");
        let output = make_output(&fixture, "", Some(0));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success, "warnings-only with exit 0 = success");
            assert_eq!(br.errors, 0);
            assert_eq!(br.warnings, 2);
        }
    }

    #[test]
    fn test_tier1_nothing_to_do() {
        let fixture = load_fixture("make_nothing.txt");
        let output = make_output("", &fixture, Some(0));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success);
            assert_eq!(br.errors, 0);
            assert_eq!(br.warnings, 0);
        }
    }

    #[test]
    fn test_tier1_up_to_date() {
        let output = make_output("", "make: 'app' is up to date.", Some(0));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success);
        }
    }

    #[test]
    fn test_tier1_make_failure_line() {
        let output = make_output("", "make: *** [Makefile:10: all] Error 1", Some(2));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.errors >= 1);
            assert!(!br.success);
        }
    }

    #[test]
    fn test_tier1_makefile_syntax_error() {
        let output = make_output("", "Makefile:5: *** missing separator.  Stop.", Some(2));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.errors >= 1);
            assert!(!br.success);
        }
    }

    #[test]
    fn test_tier1_fatal_error() {
        let output = make_output(
            "",
            "foo.c:1:10: fatal error: bar.h: No such file or directory",
            Some(1),
        );
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.errors >= 1);
            assert!(br.error_messages.iter().any(|m| m.contains("fatal error")));
        }
    }

    #[test]
    fn test_tier1_linker_error_gnu() {
        let output = make_output("", "main.o: undefined reference to 'foo'", Some(1));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.errors >= 1);
        }
    }

    #[test]
    fn test_tier1_linker_error_macos() {
        let output = make_output(
            "",
            "ld: symbol(s) not found for architecture arm64",
            Some(1),
        );
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.errors >= 1);
        }
    }

    #[test]
    fn test_tier1_note_not_counted() {
        let input =
            "main.c:10:5: error: undeclared identifier\nmain.c:10:5: note: did you mean 'x'?\n";
        let output = make_output("", input, Some(1));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert_eq!(br.errors, 1, "note should not be counted as error");
            assert_eq!(
                br.error_messages.len(),
                2,
                "both error and note in messages"
            );
        }
    }

    // ========================================================================
    // Tier 2: Noise stripping
    // ========================================================================

    #[test]
    fn test_tier2_noise_strip_recursive() {
        let fixture = load_fixture("make_recursive.txt");
        let output = make_output(&fixture, "", Some(0));
        let result = parse_make(&output);
        assert!(
            result.is_degraded(),
            "expected Degraded, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Degraded(_, markers) = &result {
            assert!(markers.iter().any(|m| m.contains("noise-stripped")));
        }
    }

    #[test]
    fn test_tier2_noise_strip_success() {
        let fixture = load_fixture("make_success.txt");
        let output = make_output(&fixture, "", Some(0));
        let result = parse_make(&output);
        assert!(
            result.is_degraded(),
            "expected Degraded, got {:?}",
            result.tier_name()
        );
    }

    // ========================================================================
    // Tier 3: Passthrough
    // ========================================================================

    #[test]
    fn test_tier3_passthrough() {
        let output = make_output("some random unrecognized output\n", "", Some(1));
        let result = parse_make(&output);
        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {:?}",
            result.tier_name()
        );
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[test]
    fn test_empty_output_is_success() {
        let output = make_output("", "", Some(0));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success);
            assert_eq!(br.errors, 0);
            assert_eq!(br.warnings, 0);
        }
    }

    #[test]
    fn test_signal_killed_make_is_failure() {
        // exit_code: None means the process was killed by a signal (e.g. SIGKILL).
        // Empty output + None exit code must be treated as failure, not success.
        let output = make_output("", "", None);
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(!br.success, "signal-killed process must not be success");
        }
    }

    #[test]
    fn test_noop_after_errors_preserves_diagnostics() {
        // A trailing noop line must not discard previously-accumulated diagnostics.
        // Regression test for the noop-early-return bug (make.rs:176).
        let input = "main.c:1:1: error: use of undeclared identifier 'x'\nmake: Nothing to be done for 'all'\n";
        let output = make_output("", input, Some(1));
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(!br.success, "errors must not be discarded by trailing noop");
            assert_eq!(br.errors, 1, "error line must be counted");
        }
    }

    #[test]
    fn test_tier1_recursive_noop() {
        // MAKE_NOOP_RE handles make[N]: prefix; verify it fires on recursive make.
        let output = make_output(
            "",
            "make[1]: Nothing to be done for 'target'\n",
            Some(0),
        );
        let result = parse_make(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success, "recursive noop must be success");
            assert_eq!(br.errors, 0);
            assert_eq!(br.warnings, 0);
        }
    }
}
