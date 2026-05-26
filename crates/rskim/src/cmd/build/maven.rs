//! Maven build output compression (#118)
//!
//! Three-tier parser for `mvn`/`mvnw` output:
//!
//! - **Tier 1 (Full)**: Regex on `[ERROR]`/`[WARNING]` lines and BUILD
//!   SUCCESS/FAILURE summary with total time.
//! - **Tier 2 (Degraded)**: Noise strip — `Downloading from`/`Downloaded from`,
//!   plugin scanning lines, `[INFO] ---`, progress lines, scanning info.
//! - **Tier 3 (Passthrough)**: Return raw output when no Maven patterns found.
//!
//! When verbose flags `-X` or `-e` are present in the args, the caller passes
//! through to avoid suppressing the requested detail.

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

/// Run `mvn`/`mvnw` with output compression.
pub(crate) fn run(
    program: &str,
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_parsed_command(
        program,
        args,
        &[],
        "Install Maven: https://maven.apache.org/install.html (or use ./mvnw)",
        show_stats,
        rec,
        parse_maven,
    )
}

// ============================================================================
// Regex patterns
// ============================================================================

/// Maven ERROR line: `[ERROR] ...`
static MAVEN_ERROR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[ERROR\] (.+)$").expect("valid regex"));

/// Maven WARNING line: `[WARNING] ...`
static MAVEN_WARN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[WARNING\] (.+)$").expect("valid regex"));

/// BUILD SUCCESS
static MAVEN_SUCCESS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[INFO\] BUILD SUCCESS").expect("valid regex"));

/// BUILD FAILURE
static MAVEN_FAILURE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[INFO\] BUILD FAILURE").expect("valid regex"));

/// Total time: `[INFO] Total time:  2.345 s`
static MAVEN_TIME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[INFO\] Total time:\s+(.+)").expect("valid regex"));

/// Download noise: `Downloading from central:` or `Downloaded from central:`
static MAVEN_DOWNLOAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[INFO\] Downloa(?:ding|ded) from ").expect("valid regex"));

/// Maven INFO separator lines and scanning/building markers
static MAVEN_INFO_NOISE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[INFO\] (?:---|\+--|\s*$|Scanning for projects|Building |Using the MultiThreadedBuilder|BUILD TARGET|Reactor Summary|Reactor Build Order|--------------------------|========================================)").expect("valid regex")
});

// ============================================================================
// Three-tier parser
// ============================================================================

fn parse_maven(output: &CommandOutput) -> ParseResult<BuildResult> {
    // Empty output → reflect exit code
    if output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
        let success = output.exit_code == Some(0);
        return ParseResult::Full(BuildResult::new(success, 0, 0, None, vec![]));
    }

    // Zero-copy when stderr is empty (Cow::Borrowed fast path), owned otherwise.
    let combined = crate::cmd::combine_output(output);

    // Tier 1: Diagnostics
    if let Some(result) = try_tier1_diagnostics(&combined, output.exit_code) {
        return result;
    }

    // Tier 2: Noise stripping
    if let Some(result) = try_tier2_noise_strip(&combined, output.exit_code) {
        return result;
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_tier1_diagnostics(
    combined: &str,
    exit_code: Option<i32>,
) -> Option<ParseResult<BuildResult>> {
    let mut errors: usize = 0;
    let mut warnings: usize = 0;
    let mut error_messages: Vec<String> = Vec::new();
    let mut any_match = false;
    let mut duration_ms: Option<u64> = None;
    let mut saw_build_success = false;

    for line in combined.lines() {
        if let Some(caps) = MAVEN_ERROR_RE.captures(line) {
            any_match = true;
            errors += 1;
            error_messages.push(caps[1].to_string());
        } else if let Some(caps) = MAVEN_WARN_RE.captures(line) {
            any_match = true;
            warnings += 1;
            error_messages.push(format!("warning: {}", &caps[1]));
        } else if MAVEN_SUCCESS_RE.is_match(line) {
            any_match = true;
            saw_build_success = true;
        } else if MAVEN_FAILURE_RE.is_match(line) {
            any_match = true;
        } else if let Some(caps) = MAVEN_TIME_RE.captures(line) {
            any_match = true;
            duration_ms = parse_maven_duration(caps[1].trim());
        }
    }

    if !any_match {
        return None;
    }

    let success = exit_code == Some(0) && saw_build_success && errors == 0;

    Some(ParseResult::Full(BuildResult::new(
        success,
        warnings,
        errors,
        duration_ms,
        error_messages,
    )))
}

/// Parse Maven duration string: "2.345 s" → 2345ms, "1:23 min" → 83000ms
fn parse_maven_duration(s: &str) -> Option<u64> {
    if let Some(secs_str) = s.strip_suffix(" s") {
        return secs_str
            .trim()
            .parse::<f64>()
            .ok()
            .map(|f| (f * 1000.0) as u64);
    }
    if let Some(min_str) = s.strip_suffix(" min") {
        let parts: Vec<&str> = min_str.trim().split(':').collect();
        if parts.len() == 2 {
            let mins: u64 = parts[0].parse().ok()?;
            let secs: u64 = parts[1].parse().ok()?;
            return Some((mins * 60 + secs) * 1000);
        }
    }
    None
}

fn try_tier2_noise_strip(
    combined: &str,
    exit_code: Option<i32>,
) -> Option<ParseResult<BuildResult>> {
    let mut remaining_lines: Vec<&str> = Vec::new();
    let mut any_stripped = false;

    for line in combined.lines() {
        if MAVEN_DOWNLOAD_RE.is_match(line) || MAVEN_INFO_NOISE_RE.is_match(line) {
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
        vec!["maven: no diagnostics found, noise-stripped".to_string()],
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{make_output_full};

    const MAVEN_SUCCESS: &str = "[INFO] Scanning for projects...\n[INFO] Building MyApp 1.0.0\n[INFO] BUILD SUCCESS\n[INFO] ------------------------------------------------------------------------\n[INFO] Total time:  2.345 s\n[INFO] Finished at: 2026-01-01T00:00:00Z\n";

    const MAVEN_FAILURE: &str = "[INFO] Scanning for projects...\n[ERROR] COMPILATION ERROR : \n[ERROR] /src/main/java/App.java:[10,5] cannot find symbol\n[INFO] BUILD FAILURE\n[INFO] Total time:  1.234 s\n";

    const MAVEN_NOISY: &str = "[INFO] Downloading from central: https://repo1.maven.org/maven2/junit/junit/4.13/junit-4.13.pom\n[INFO] Downloaded from central: https://repo1.maven.org/maven2/junit/junit/4.13/junit-4.13.pom (3.8 kB at 22 kB/s)\n[INFO] --- maven-compiler-plugin:3.8.0:compile (default-compile) @ myproject ---\n";

    #[test]
    fn test_maven_tier1_success() {
        let output = make_output_full(MAVEN_SUCCESS, "", Some(0));
        let result = parse_maven(&output);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success);
            assert_eq!(br.errors, 0);
        }
    }

    #[test]
    fn test_maven_tier1_failure() {
        let output = make_output_full(MAVEN_FAILURE, "", Some(1));
        let result = parse_maven(&output);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(!br.success);
            assert!(br.errors >= 1);
        }
    }

    #[test]
    fn test_maven_tier2_noise_strip() {
        let output = make_output_full(MAVEN_NOISY, "", Some(0));
        let result = parse_maven(&output);
        assert!(
            result.is_degraded(),
            "expected Degraded, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_maven_tier3_passthrough() {
        let output = make_output_full("random unrecognized output\n", "", Some(1));
        let result = parse_maven(&output);
        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_maven_empty_success() {
        let output = make_output_full("", "", Some(0));
        let result = parse_maven(&output);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success);
        }
    }

    #[test]
    fn test_maven_duration_parsing() {
        assert_eq!(parse_maven_duration("2.345 s"), Some(2345));
        assert_eq!(parse_maven_duration("1:23 min"), Some(83000));
    }
}
