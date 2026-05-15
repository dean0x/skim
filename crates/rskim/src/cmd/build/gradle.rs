//! Gradle build output compression (#118)
//!
//! Three-tier parser for `gradle` and `gradlew` output:
//!
//! - **Tier 1 (Full)**: Regex on task outcomes, Java/Kotlin diagnostics, and
//!   BUILD SUCCESSFUL/FAILED summary.
//! - **Tier 2 (Degraded)**: Noise strip — daemon startup, download progress,
//!   configure project lines, UP-TO-DATE/FROM-CACHE task lines, empty lines.
//! - **Tier 3 (Passthrough)**: Return raw output when no Gradle patterns found.
//!
//! Supports `gradle` and `gradlew` aliases. When verbose flags are present
//! (`--stacktrace`, `--info`, `--debug`, `--full-stacktrace`), falls through
//! to passthrough to avoid suppressing developer-requested detail.

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

/// Run `gradle`/`gradlew` with output compression.
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
        "Install Gradle: https://gradle.org/install/ (or use ./gradlew)",
        show_stats,
        rec,
        parse_gradle,
    )
}

// ============================================================================
// Regex patterns (compiled once via LazyLock)
// ============================================================================

/// Gradle task: `> Task :name OUTCOME`
static GRADLE_TASK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^> Task :(\S+)(?:\s+(UP-TO-DATE|FROM-CACHE|SKIPPED|FAILED))?$")
        .expect("valid regex")
});

/// Java diagnostic: `file.java:line: error: message`
static JAVA_DIAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+\.java):(\d+): (error|warning|note): (.+)$").expect("valid regex")
});

/// Kotlin diagnostic: `e: file.kt: (line, col): message` or `w:`
static KOTLIN_DIAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([ew]): (.+\.kt): \((\d+), \d+\): (.+)$").expect("valid regex"));

/// BUILD SUCCESSFUL: `BUILD SUCCESSFUL in Xs`
static BUILD_SUCCESS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"BUILD SUCCESSFUL in (.+)").expect("valid regex"));

/// BUILD FAILED
static BUILD_FAILED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"BUILD FAILED").expect("valid regex"));

/// Noise patterns: daemon messages, download progress, configure project lines
static GRADLE_NOISE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:Starting a Gradle Daemon|Gradle daemon|Configure project|Download http|> Configure|Deprecated Gradle|Welcome to Gradle|See https://docs\.gradle|To honour the JVM settings|https://docs\.gradle\.org|Daemon will be stopped|detached from console)").expect("valid regex")
});

/// Task lines that are noise (UP-TO-DATE, FROM-CACHE lines)
static GRADLE_TASK_NOISE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^> Task :.+ (?:UP-TO-DATE|FROM-CACHE|SKIPPED)$").expect("valid regex")
});

// ============================================================================
// Three-tier parser
// ============================================================================

/// Parse gradle output through three degradation tiers.
fn parse_gradle(output: &CommandOutput) -> ParseResult<BuildResult> {
    // Empty output → reflect exit code
    if output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
        let success = output.exit_code == Some(0);
        return ParseResult::Full(BuildResult::new(success, 0, 0, None, vec![]));
    }

    // Zero-copy when stderr is empty (Cow::Borrowed fast path), owned otherwise.
    let combined = crate::cmd::combine_output(output);

    // Verbose bypass: if verbose flags appear in the output metadata, pass through
    // (The flags are already checked in the rewrite rule; this is a safety net
    //  for piped input that contains these patterns.)
    // NOTE: We don't check args here since we're parsing output; the verbose
    // bypass is handled at the run() call site by the rewrite rule.

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

/// Tier 1: Extract Gradle task outcomes, Java/Kotlin diagnostics, and build summary.
fn try_tier1_diagnostics(
    combined: &str,
    exit_code: Option<i32>,
) -> Option<ParseResult<BuildResult>> {
    let mut errors: usize = 0;
    let mut warnings: usize = 0;
    let mut error_messages: Vec<String> = Vec::new();
    let mut any_match = false;
    let mut build_time: Option<String> = None;
    let mut saw_build_failed = false;

    for line in combined.lines() {
        if let Some(caps) = GRADLE_TASK_RE.captures(line) {
            any_match = true;
            let task = caps.get(1).map_or("", |m| m.as_str());
            let outcome = caps.get(2).map(|m| m.as_str()).unwrap_or("SUCCESS");
            if outcome == "FAILED" {
                errors += 1;
                error_messages.push(format!("Task :{task} FAILED"));
            }
        } else if let Some(caps) = JAVA_DIAG_RE.captures(line) {
            any_match = true;
            let severity = caps.get(3).map_or("", |m| m.as_str());
            let file = caps.get(1).map_or("", |m| m.as_str());
            let lineno = caps.get(2).map_or("", |m| m.as_str());
            let message = caps.get(4).map_or("", |m| m.as_str());
            if severity == "error" {
                errors += 1;
            } else if severity == "warning" {
                warnings += 1;
            }
            error_messages.push(format!("{severity}: {message} ({file}:{lineno})"));
        } else if let Some(caps) = KOTLIN_DIAG_RE.captures(line) {
            any_match = true;
            let kind = caps.get(1).map_or("", |m| m.as_str());
            let file = caps.get(2).map_or("", |m| m.as_str());
            let lineno = caps.get(3).map_or("", |m| m.as_str());
            let message = caps.get(4).map_or("", |m| m.as_str());
            if kind == "e" {
                errors += 1;
            } else {
                warnings += 1;
            }
            error_messages.push(format!(
                "{}: {message} ({file}:{lineno})",
                if kind == "e" { "error" } else { "warning" }
            ));
        } else if let Some(caps) = BUILD_SUCCESS_RE.captures(line) {
            any_match = true;
            build_time = caps.get(1).map(|m| m.as_str().to_string());
        } else if BUILD_FAILED_RE.is_match(line) {
            any_match = true;
            saw_build_failed = true;
        }
    }

    if !any_match {
        return None;
    }

    let success = exit_code == Some(0) && errors == 0 && !saw_build_failed;
    let duration_ms = build_time.as_deref().and_then(parse_gradle_duration);

    Some(ParseResult::Full(BuildResult::new(
        success,
        warnings,
        errors,
        duration_ms,
        error_messages,
    )))
}

/// Parse Gradle duration string like "3.456 secs" or "1 min 2 secs" to milliseconds.
fn parse_gradle_duration(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        // "3.456 secs" → 3456ms
        [secs_str, _unit] => {
            let f = secs_str.parse::<f64>().ok()?;
            Some((f * 1000.0).max(0.0).round() as u64)
        }
        // "1 min 2 secs" → 62000ms
        [mins_str, "min", secs_str, _unit] => {
            let mins: u64 = mins_str.parse().ok()?;
            let secs: f64 = secs_str.parse().ok()?;
            Some(mins * 60_000 + (secs * 1000.0).max(0.0).round() as u64)
        }
        _ => None,
    }
}

/// Tier 2: Strip daemon startup, download progress, configure project lines.
fn try_tier2_noise_strip(
    combined: &str,
    exit_code: Option<i32>,
) -> Option<ParseResult<BuildResult>> {
    let mut remaining_lines: Vec<&str> = Vec::new();
    let mut any_stripped = false;

    for line in combined.lines() {
        if GRADLE_NOISE_RE.is_match(line) || GRADLE_TASK_NOISE_RE.is_match(line) {
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
        vec!["gradle: no diagnostics found, noise-stripped".to_string()],
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            duration: Duration::from_millis(100),
        }
    }

    #[test]
    fn test_gradle_tier1_success() {
        let input = "> Task :compileJava\n> Task :processResources UP-TO-DATE\n> Task :classes\nBUILD SUCCESSFUL in 2.345 secs\n2 actionable tasks: 1 executed, 1 up-to-date\n";
        let output = make_output(input, "", Some(0));
        let result = parse_gradle(&output);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.success, "expected success");
            assert_eq!(br.errors, 0);
        }
    }

    #[test]
    fn test_gradle_tier1_failure() {
        let input = "> Task :compileJava FAILED\n\nFAILURE: Build failed with an exception.\n\nBUILD FAILED\n";
        let output = make_output(input, "", Some(1));
        let result = parse_gradle(&output);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(!br.success, "expected failure");
            assert!(br.errors >= 1);
        }
    }

    #[test]
    fn test_gradle_tier1_java_errors() {
        let input = "src/main/java/com/example/App.java:10: error: cannot find symbol\nsymbol: variable foo\nBUILD FAILED\n";
        let output = make_output(input, "", Some(1));
        let result = parse_gradle(&output);
        assert!(
            result.is_full(),
            "expected Full, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(br) = &result {
            assert!(br.errors >= 1);
        }
    }

    #[test]
    fn test_gradle_tier2_noise_strip() {
        let input = "Starting a Gradle Daemon (subsequent builds will be faster)\n> Configure project :app\nTask :build UP-TO-DATE\nDownload https://example.com/artifact.jar\n";
        let output = make_output(input, "", Some(0));
        let result = parse_gradle(&output);
        assert!(
            result.is_degraded(),
            "expected Degraded, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_gradle_tier3_passthrough() {
        let output = make_output("some random unrecognized output\n", "", Some(1));
        let result = parse_gradle(&output);
        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_empty_output_is_success() {
        let output = make_output("", "", Some(0));
        let result = parse_gradle(&output);
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
    fn test_gradle_duration_simple() {
        // "3.456 secs" → 3456ms
        assert_eq!(parse_gradle_duration("3.456 secs"), Some(3456));
    }

    #[test]
    fn test_gradle_duration_multi_part() {
        // "1 min 2 secs" → 62000ms (was returning 1000ms before fix)
        assert_eq!(parse_gradle_duration("1 min 2 secs"), Some(62_000));
        // "2 min 30 secs" → 150000ms
        assert_eq!(parse_gradle_duration("2 min 30 secs"), Some(150_000));
    }

    #[test]
    fn test_gradle_duration_invalid() {
        assert_eq!(parse_gradle_duration("invalid"), None);
        assert_eq!(parse_gradle_duration(""), None);
    }

    #[test]
    fn test_gradle_tier1_success_with_duration() {
        // Verify that a 1-minute build is parsed as 62000ms, not 1000ms
        let input = "> Task :compileJava\nBUILD SUCCESSFUL in 1 min 2 secs\n";
        let output = make_output(input, "", Some(0));
        let result = parse_gradle(&output);
        if let ParseResult::Full(br) = &result {
            assert_eq!(
                br.duration_ms,
                Some(62_000),
                "multi-part duration must be 62s"
            );
        }
    }
}
