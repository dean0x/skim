//! `skim learn` -- detect CLI error patterns and generate correction rules (#64)
//!
//! Scans AI agent session files for error-retry patterns: a failed Bash command
//! followed by a similar successful command within the next few invocations.
//! Optionally generates a `.claude/rules/cli-corrections.md` rules file.

use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ExitCode;

use super::session::{self, parse_duration_ago, AgentKind, ToolInput, ToolInvocation};

/// Run the learn subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let config = parse_args(args)?;

    let providers = session::get_providers(config.agent_filter);
    if providers.is_empty() {
        println!("No AI agent sessions found.");
        println!("hint: skim learn scans for Claude Code sessions in ~/.claude/projects/");
        return Ok(ExitCode::SUCCESS);
    }

    let filter = session::TimeFilter {
        since: config.since,
        latest_only: false,
    };

    // Collect all invocations from all providers
    let all_invocations = session::collect_invocations(&providers, &filter)?;

    if all_invocations.is_empty() {
        println!("No tool invocations found in the specified time window.");
        return Ok(ExitCode::SUCCESS);
    }

    // Extract only Bash invocations (in order)
    let bash_invocations: Vec<&ToolInvocation> = all_invocations
        .iter()
        .filter(|inv| matches!(&inv.input, ToolInput::Bash { .. }))
        .collect();

    if bash_invocations.is_empty() {
        println!("No Bash commands found in sessions.");
        return Ok(ExitCode::SUCCESS);
    }

    // Detect error-retry correction patterns
    let corrections = detect_corrections(&bash_invocations);
    let corrections = deduplicate_and_filter(corrections);

    if corrections.is_empty() {
        println!("No CLI error patterns detected.");
        return Ok(ExitCode::SUCCESS);
    }

    if config.json_output {
        print_json_report(&corrections)?;
    } else if config.generate {
        let content = generate_rules_content(&corrections);
        write_rules_file(&content, config.dry_run)?;
    } else {
        print_text_report(&corrections);
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Config
// ============================================================================

#[derive(Debug)]
struct LearnConfig {
    since: Option<std::time::SystemTime>,
    generate: bool,
    dry_run: bool,
    json_output: bool,
    agent_filter: Option<AgentKind>,
}

fn parse_args(args: &[String]) -> anyhow::Result<LearnConfig> {
    let mut config = LearnConfig {
        since: Some(std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 86400)),
        generate: false,
        dry_run: false,
        json_output: false,
        agent_filter: None,
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--since requires a value (e.g., 7d, 24h, 1w)");
                }
                config.since = Some(parse_duration_ago(&args[i])?);
            }
            "--generate" => config.generate = true,
            "--dry-run" => config.dry_run = true,
            "--json" => config.json_output = true,
            "--agent" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--agent requires a value (e.g., claude-code)");
                }
                config.agent_filter = Some(AgentKind::from_str(&args[i]).ok_or_else(|| {
                    anyhow::anyhow!("unknown agent: '{}'\nSupported: claude-code", &args[i])
                })?);
            }
            other => {
                anyhow::bail!(
                    "unknown flag: '{other}'\n\nUsage: skim learn [--since <duration>] [--generate] [--dry-run] [--json]"
                );
            }
        }
        i += 1;
    }

    Ok(config)
}

// ============================================================================
// Types
// ============================================================================

/// A detected correction pair: failed command followed by successful fix.
#[derive(Debug, Clone)]
struct CorrectionPair {
    failed_command: String,
    successful_command: String,
    error_output: String,
    pattern_type: PatternType,
    occurrences: usize,
    sessions: Vec<String>,
}

/// Classification of how the correction differs from the original.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // Other variant reserved for future pattern types
enum PatternType {
    /// Edit distance 1-2 (e.g., `carg test` -> `cargo test`)
    FlagTypo,
    /// Missing separator (e.g., `cargo test --nocapture` -> `cargo test -- --nocapture`)
    MissingSeparator,
    /// Wrong flag (e.g., `cargo test --release` -> `cargo test --profile=release`)
    WrongFlag,
    /// Missing argument (e.g., `cargo test --package` -> `cargo test --package rskim`)
    MissingArg,
    /// Other pattern
    Other,
}

impl PatternType {
    fn label(&self) -> &'static str {
        match self {
            PatternType::FlagTypo => "Typo",
            PatternType::MissingSeparator => "Missing separator",
            PatternType::WrongFlag => "Wrong flag",
            PatternType::MissingArg => "Missing argument",
            PatternType::Other => "Correction",
        }
    }
}

// ============================================================================
// Pattern detection
// ============================================================================

/// Detect error-retry patterns in a sequence of Bash invocations.
///
/// For each failed Bash command, look at the next N (default 5) Bash commands.
/// If a similar successful command is found, create a `CorrectionPair`.
fn detect_corrections(bash_invocations: &[&ToolInvocation]) -> Vec<CorrectionPair> {
    let mut corrections = Vec::new();
    const LOOKAHEAD: usize = 5;

    for (i, inv) in bash_invocations.iter().enumerate() {
        // Only look at failed commands
        let result = match &inv.result {
            Some(r) if r.is_error || looks_like_error(&r.content) => r,
            _ => continue,
        };

        // Skip TDD cycles
        if is_tdd_cycle(bash_invocations, i) {
            continue;
        }

        let failed_cmd = match &inv.input {
            ToolInput::Bash { command } => command.as_str(),
            _ => continue,
        };

        // Look ahead for a similar successful command
        let end = (i + 1 + LOOKAHEAD).min(bash_invocations.len());
        for candidate in bash_invocations.iter().take(end).skip(i + 1) {
            let is_success = match &candidate.result {
                Some(r) => !r.is_error && !looks_like_error(&r.content),
                None => false,
            };
            if !is_success {
                continue;
            }

            let candidate_cmd = match &candidate.input {
                ToolInput::Bash { command } => command.as_str(),
                _ => continue,
            };

            // Check similarity
            if let Some(pattern) = classify_correction(failed_cmd, candidate_cmd) {
                corrections.push(CorrectionPair {
                    failed_command: failed_cmd.to_string(),
                    successful_command: candidate_cmd.to_string(),
                    error_output: result.content.chars().take(200).collect(),
                    pattern_type: pattern,
                    occurrences: 1,
                    sessions: vec![inv.session_id.clone()],
                });
                break; // Only match the first correction per failure
            }
        }
    }

    corrections
}

// ============================================================================
// Similarity matching
// ============================================================================

/// Classify how a correction differs from the failed command.
fn classify_correction(failed: &str, success: &str) -> Option<PatternType> {
    // Skip if commands are identical
    if failed == success {
        return None;
    }

    let failed_tokens: Vec<&str> = failed.split_whitespace().collect();
    let success_tokens: Vec<&str> = success.split_whitespace().collect();

    // Must have tokens
    if failed_tokens.is_empty() || success_tokens.is_empty() {
        return None;
    }

    // At minimum, first token must match (same tool) -- or be a typo
    if failed_tokens[0] != success_tokens[0] {
        // Check edit distance 1-2 on first token (typo in command name)
        if levenshtein(failed_tokens[0], success_tokens[0]) <= 2 {
            return Some(PatternType::FlagTypo);
        }
        return None;
    }

    // Check for missing separator: failed has no --, success has --
    if !failed.contains(" -- ") && success.contains(" -- ") {
        return Some(PatternType::MissingSeparator);
    }

    // Compare full strings via edit distance
    let edit_dist = levenshtein(failed, success);
    if edit_dist <= 3 {
        // Close enough to be a typo or minor fix
        if failed_tokens.len() < success_tokens.len() {
            return Some(PatternType::MissingArg);
        }
        if failed_tokens.len() == success_tokens.len() {
            // Check if exactly one token differs and it looks like a flag
            let diffs: Vec<usize> = failed_tokens
                .iter()
                .zip(success_tokens.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, _)| i)
                .collect();
            if diffs.len() == 1 {
                let diff_idx = diffs[0];
                if failed_tokens[diff_idx].starts_with('-')
                    || success_tokens[diff_idx].starts_with('-')
                {
                    return Some(PatternType::WrongFlag);
                }
                return Some(PatternType::FlagTypo);
            }
        }
        return Some(PatternType::FlagTypo);
    }

    // Looser match: same base command (first 2 tokens), different flags
    if failed_tokens.len() >= 2
        && success_tokens.len() >= 2
        && failed_tokens[..2] == success_tokens[..2]
    {
        if failed_tokens.len() < success_tokens.len() {
            return Some(PatternType::MissingArg);
        }
        return Some(PatternType::WrongFlag);
    }

    None
}

/// Simple Levenshtein distance implementation.
///
/// Includes length guards to prevent DoS from very long inputs:
/// - Caps input length at 500 chars (returns length difference for longer strings)
/// - Early-exits if length difference > 10 (obviously dissimilar)
fn levenshtein(a: &str, b: &str) -> usize {
    const MAX_INPUT_LEN: usize = 500;
    const MAX_LEN_DIFF: usize = 10;

    let a_chars: Vec<char> = a.chars().take(MAX_INPUT_LEN + 1).collect();
    let b_chars: Vec<char> = b.chars().take(MAX_INPUT_LEN + 1).collect();
    let m = a_chars.len();
    let n = b_chars.len();

    // If either input exceeds max length, return length difference as a
    // conservative estimate (will always exceed any reasonable threshold).
    if m > MAX_INPUT_LEN || n > MAX_INPUT_LEN {
        return m.abs_diff(n).max(MAX_LEN_DIFF + 1);
    }

    // Early-exit: obviously dissimilar strings
    let len_diff = m.abs_diff(n);
    if len_diff > MAX_LEN_DIFF {
        return len_diff;
    }

    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i;
    }
    for (j, val) in dp[0].iter_mut().enumerate().take(n + 1) {
        *val = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    dp[m][n]
}

// ============================================================================
// Error detection
// ============================================================================

/// Heuristic: does the output content look like a command error?
///
/// Checks for common error indicators in command output. Only examines
/// the first 1KB to avoid allocating a full lowercase copy of large output.
///
/// Uses prefix patterns for "error" to avoid false positives on benign
/// output like "0 errors generated" or filenames containing "error".
fn looks_like_error(content: &str) -> bool {
    // Limit analysis to first 1KB to avoid large allocations
    const MAX_CHECK_LEN: usize = 1024;
    let check_content = if content.len() > MAX_CHECK_LEN {
        // Find a safe UTF-8 boundary near the limit
        let mut end = MAX_CHECK_LEN;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        &content[..end]
    } else {
        content
    };

    let lower = check_content.to_lowercase();

    // Quick exclusion: "0 failed" is a success indicator in test output
    let has_failed = if lower.contains("failed") {
        // Only count as error if there's a non-zero count before "failed"
        !lower.contains("0 failed")
    } else {
        false
    };

    // Use prefix patterns to avoid matching benign occurrences like
    // "0 errors generated", "error_handler.rs", etc.
    let has_error = lower.starts_with("error:")
        || lower.starts_with("error[")
        || lower.contains("\nerror:")
        || lower.contains("\nerror[")
        || lower.contains(": error:")
        || lower.contains(": error[");

    has_error
        || lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("permission denied")
        || lower.contains("command not found")
        || has_failed
        || lower.starts_with("fatal:")
        || (check_content.contains("FAILED") && !lower.contains("0 failed"))
        || check_content.contains("Exit code")
}

// ============================================================================
// TDD cycle detection
// ============================================================================

/// Check if a sequence of commands represents a TDD cycle.
///
/// TDD cycles: same command alternates fail/pass/fail 3+ times.
/// These should be excluded from correction detection.
fn is_tdd_cycle(invocations: &[&ToolInvocation], start_idx: usize) -> bool {
    let cmd = match &invocations[start_idx].input {
        ToolInput::Bash { command } => command.as_str(),
        _ => return false,
    };

    let mut alternations = 0;
    let mut last_was_error = true; // We start from a failed command

    for inv in invocations.iter().skip(start_idx + 1).take(10) {
        let inv_cmd = match &inv.input {
            ToolInput::Bash { command } => command.as_str(),
            _ => continue,
        };

        // Must be the same (or very similar) command
        if normalize_command(inv_cmd) != normalize_command(cmd) {
            continue;
        }

        let is_error = inv
            .result
            .as_ref()
            .map(|r| r.is_error || looks_like_error(&r.content))
            .unwrap_or(false);

        if is_error != last_was_error {
            alternations += 1;
            last_was_error = is_error;
        }
    }

    alternations >= 2 // fail -> pass -> fail = 2 alternations = TDD cycle
}

// ============================================================================
// Deduplication and filtering
// ============================================================================

/// Merge duplicate correction pairs and apply exclusion filters.
fn deduplicate_and_filter(corrections: Vec<CorrectionPair>) -> Vec<CorrectionPair> {
    // Group by (normalized_failed, normalized_success)
    let mut groups: HashMap<(String, String), CorrectionPair> = HashMap::new();

    for pair in corrections {
        let key = (
            normalize_command(&pair.failed_command),
            normalize_command(&pair.successful_command),
        );
        groups
            .entry(key)
            .and_modify(|existing| {
                existing.occurrences += 1;
                if !existing.sessions.contains(&pair.sessions[0]) {
                    existing.sessions.extend(pair.sessions.clone());
                }
            })
            .or_insert(pair);
    }

    groups
        .into_values()
        .filter(|pair| {
            // Minimum 2 occurrences, except for edit distance 1 typos
            if pair.occurrences < 2 && pair.pattern_type != PatternType::FlagTypo {
                return false;
            }
            // Exclude path-only differences
            if is_path_only_difference(&pair.failed_command, &pair.successful_command) {
                return false;
            }
            true
        })
        .collect()
}

/// Normalize a command for grouping: keep first 3 tokens, replace paths.
fn normalize_command(cmd: &str) -> String {
    cmd.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
}

/// Check if two commands differ only in path arguments.
fn is_path_only_difference(a: &str, b: &str) -> bool {
    let a_tokens: Vec<&str> = a.split_whitespace().collect();
    let b_tokens: Vec<&str> = b.split_whitespace().collect();

    if a_tokens.len() != b_tokens.len() {
        return false;
    }

    let diffs: Vec<(&&str, &&str)> = a_tokens
        .iter()
        .zip(b_tokens.iter())
        .filter(|(x, y)| x != y)
        .collect();

    if diffs.is_empty() {
        return false; // identical commands -- not a "path-only difference"
    }

    // All differences are in path-like tokens
    diffs
        .iter()
        .all(|(a_tok, b_tok)| looks_like_path(a_tok) && looks_like_path(b_tok))
}

fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.starts_with("./") || s.starts_with("../")
}

// ============================================================================
// Rules file generation
// ============================================================================

/// Generate the rules file content from correction pairs.
fn generate_rules_content(corrections: &[CorrectionPair]) -> String {
    let mut output = String::new();
    output.push_str("# CLI Corrections\n\n");
    output
        .push_str("Generated by `skim learn`. Common CLI mistakes detected in your sessions.\n\n");

    for pair in corrections {
        output.push_str(&format!(
            "## {} (seen {} time{})\n\n",
            pair.pattern_type.label(),
            pair.occurrences,
            if pair.occurrences == 1 { "" } else { "s" },
        ));
        output.push_str(&format!(
            "Instead of: `{}`\n",
            sanitize_command_for_rules(&pair.failed_command)
        ));
        output.push_str(&format!(
            "Use: `{}`\n\n",
            sanitize_command_for_rules(&pair.successful_command)
        ));
    }

    output
}

/// Sanitize a command string for safe inclusion in a markdown rules file.
///
/// Prevents prompt injection by:
/// - Truncating to 200 chars (commands longer than this are not useful rules)
/// - Escaping backticks to prevent breaking out of inline code
/// - Stripping markdown heading markers at line start
/// - Collapsing to single line
fn sanitize_command_for_rules(cmd: &str) -> String {
    const MAX_COMMAND_LEN: usize = 200;

    // Collapse to single line, trim whitespace
    let single_line: String = cmd
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>();
    let single_line = single_line.trim();

    // Truncate to max length
    let truncated = if single_line.len() > MAX_COMMAND_LEN {
        let mut end = MAX_COMMAND_LEN;
        while end > 0 && !single_line.is_char_boundary(end) {
            end -= 1;
        }
        &single_line[..end]
    } else {
        single_line
    };

    // Escape backticks to prevent breaking out of inline code blocks
    // Strip leading '#' to prevent markdown heading injection
    let sanitized = truncated.replace('`', "'");
    let sanitized = sanitized.trim_start_matches('#').trim_start();

    sanitized.to_string()
}

/// Write the rules file to `.claude/rules/cli-corrections.md`.
fn write_rules_file(content: &str, dry_run: bool) -> anyhow::Result<()> {
    let rules_dir = std::path::Path::new(".claude").join("rules");
    let rules_path = rules_dir.join("cli-corrections.md");

    if dry_run {
        println!("Would write to: {}", rules_path.display());
        println!("---");
        print!("{content}");
        return Ok(());
    }

    std::fs::create_dir_all(&rules_dir)?;
    std::fs::write(&rules_path, content)?;
    println!("Wrote corrections to: {}", rules_path.display());
    Ok(())
}

// ============================================================================
// Output
// ============================================================================

fn print_text_report(corrections: &[CorrectionPair]) {
    println!(
        "skim learn -- {} correction{} detected\n",
        corrections.len(),
        if corrections.len() == 1 { "" } else { "s" }
    );

    for (i, pair) in corrections.iter().enumerate() {
        println!(
            "{}. {} (seen {} time{})",
            i + 1,
            pair.pattern_type.label(),
            pair.occurrences,
            if pair.occurrences == 1 { "" } else { "s" },
        );
        println!("   Failed:  {}", pair.failed_command);
        println!("   Correct: {}", pair.successful_command);
        if !pair.error_output.is_empty() {
            // Show first line of error output
            let first_line = pair.error_output.lines().next().unwrap_or("");
            if !first_line.is_empty() {
                println!("   Error:   {first_line}");
            }
        }
        println!();
    }

    println!(
        "hint: run `skim learn --generate` to write corrections to .claude/rules/cli-corrections.md"
    );
}

fn print_json_report(corrections: &[CorrectionPair]) -> anyhow::Result<()> {
    let report = serde_json::json!({
        "version": 1,
        "corrections": corrections.iter().map(|pair| {
            serde_json::json!({
                "failed_command": pair.failed_command,
                "successful_command": pair.successful_command,
                "error_output": pair.error_output,
                "pattern_type": format!("{:?}", pair.pattern_type),
                "occurrences": pair.occurrences,
                "sessions": pair.sessions,
            })
        }).collect::<Vec<_>>(),
    });

    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer_pretty(&mut handle, &report)?;
    writeln!(handle)?;
    Ok(())
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    println!("skim learn");
    println!();
    println!("  Detect CLI error patterns and generate correction rules");
    println!();
    println!("Usage: skim learn [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --since <duration>   Time window (e.g., 24h, 7d, 1w) [default: 7d]");
    println!("  --generate           Write rules to .claude/rules/cli-corrections.md");
    println!("  --dry-run            Preview rules without writing (requires --generate)");
    println!("  --agent <name>       Only scan sessions from a specific agent");
    println!("  --json               Output machine-readable JSON");
    println!("  --help, -h           Print help information");
    println!();
    println!("Examples:");
    println!("  skim learn                       Analyze last 7 days, print findings");
    println!("  skim learn --generate            Write correction rules file");
    println!("  skim learn --generate --dry-run  Preview without writing");
    println!("  skim learn --since 30d           Analyze last 30 days");
}

// ============================================================================
// Clap command (for completions)
// ============================================================================

pub(super) fn command() -> clap::Command {
    clap::Command::new("learn")
        .about("Detect CLI error patterns and generate correction rules")
        .arg(
            clap::Arg::new("since")
                .long("since")
                .value_name("DURATION")
                .help("Time window (e.g., 24h, 7d, 1w)"),
        )
        .arg(
            clap::Arg::new("generate")
                .long("generate")
                .action(clap::ArgAction::SetTrue)
                .help("Write rules file"),
        )
        .arg(
            clap::Arg::new("dry-run")
                .long("dry-run")
                .action(clap::ArgAction::SetTrue)
                .help("Preview without writing"),
        )
        .arg(
            clap::Arg::new("agent")
                .long("agent")
                .value_name("NAME")
                .help("Filter by agent"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("JSON output"),
        )
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::session::ToolResult;

    // ---- levenshtein ----

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("cargo", "cargo"), 0);
    }

    #[test]
    fn test_levenshtein_one_char() {
        assert_eq!(levenshtein("carg", "cargo"), 1);
    }

    #[test]
    fn test_levenshtein_two_chars() {
        // "crgo" -> "cargo" is 1 edit (insert 'a'), so test a true distance-2 case
        assert_eq!(levenshtein("crgo", "cargo"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("ab", "cd"), 2);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn test_levenshtein_completely_different() {
        assert_eq!(levenshtein("abc", "xyz"), 3);
    }

    // ---- looks_like_error ----

    #[test]
    fn test_looks_like_error_positive() {
        assert!(looks_like_error("error: command not found: carg"));
        assert!(looks_like_error("bash: carg: command not found"));
        assert!(looks_like_error("No such file or directory"));
        assert!(looks_like_error("permission denied"));
        assert!(looks_like_error("Build FAILED"));
        assert!(looks_like_error("fatal: not a git repository"));
        assert!(looks_like_error("Exit code 1"));
    }

    #[test]
    fn test_looks_like_error_negative() {
        // "0 failed" is excluded -- it appears in successful test output
        assert!(!looks_like_error("test result: ok. 5 passed; 0 failed"));
        assert!(!looks_like_error("test result: ok. 5 passed; 0 warnings"));
        assert!(!looks_like_error("Compiling rskim v1.0.0"));
        assert!(!looks_like_error("Finished dev profile"));
        assert!(!looks_like_error(""));
    }

    #[test]
    fn test_looks_like_error_empty() {
        assert!(!looks_like_error(""));
    }

    // ---- classify_correction ----

    #[test]
    fn test_classify_identical_commands() {
        assert_eq!(classify_correction("cargo test", "cargo test"), None);
    }

    #[test]
    fn test_classify_typo_in_command_name() {
        let result = classify_correction("carg test", "cargo test");
        assert_eq!(result, Some(PatternType::FlagTypo));
    }

    #[test]
    fn test_classify_missing_separator() {
        let result = classify_correction("cargo test --nocapture", "cargo test -- --nocapture");
        assert_eq!(result, Some(PatternType::MissingSeparator));
    }

    #[test]
    fn test_classify_wrong_flag() {
        // "--relase" -> "--release" is a flag correction (both start with -)
        let result = classify_correction("cargo test --relase", "cargo test --release");
        assert_eq!(result, Some(PatternType::WrongFlag));
    }

    #[test]
    fn test_classify_missing_arg() {
        // Same first 2 tokens, different length
        let result = classify_correction("cargo test", "cargo test --package rskim");
        assert_eq!(result, Some(PatternType::MissingArg));
    }

    #[test]
    fn test_classify_completely_different() {
        assert_eq!(classify_correction("ls", "cargo build"), None);
    }

    #[test]
    fn test_classify_empty_commands() {
        assert_eq!(classify_correction("", "cargo test"), None);
        assert_eq!(classify_correction("cargo test", ""), None);
    }

    // ---- normalize_command ----

    #[test]
    fn test_normalize_command_short() {
        assert_eq!(normalize_command("ls"), "ls");
    }

    #[test]
    fn test_normalize_command_truncates() {
        assert_eq!(
            normalize_command("cargo test --package rskim --verbose"),
            "cargo test --package"
        );
    }

    #[test]
    fn test_normalize_command_preserves_three() {
        assert_eq!(normalize_command("go test ./..."), "go test ./...");
    }

    // ---- is_path_only_difference ----

    #[test]
    fn test_path_only_difference_true() {
        assert!(is_path_only_difference("cat /tmp/a.rs", "cat /tmp/b.rs"));
    }

    #[test]
    fn test_path_only_difference_false_flag() {
        assert!(!is_path_only_difference(
            "cargo test --release",
            "cargo test --debug"
        ));
    }

    #[test]
    fn test_path_only_difference_different_length() {
        assert!(!is_path_only_difference(
            "cargo test",
            "cargo test --verbose"
        ));
    }

    #[test]
    fn test_path_only_difference_identical() {
        assert!(!is_path_only_difference("cargo test", "cargo test"));
    }

    // ---- looks_like_path ----

    #[test]
    fn test_looks_like_path_positive() {
        assert!(looks_like_path("/tmp/test.rs"));
        assert!(looks_like_path("./src/main.rs"));
        assert!(looks_like_path("../test"));
    }

    #[test]
    fn test_looks_like_path_negative() {
        assert!(!looks_like_path("--release"));
        assert!(!looks_like_path("cargo"));
    }

    // ---- detect_corrections ----

    fn make_bash_invocation(
        command: &str,
        result_content: &str,
        is_error: bool,
        session_id: &str,
    ) -> ToolInvocation {
        ToolInvocation {
            tool_name: "Bash".to_string(),
            input: ToolInput::Bash {
                command: command.to_string(),
            },
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: session_id.to_string(),
            agent: AgentKind::ClaudeCode,
            result: Some(ToolResult {
                content: result_content.to_string(),
                is_error,
            }),
        }
    }

    #[test]
    fn test_detect_corrections_basic() {
        let inv1 =
            make_bash_invocation("carg test", "error: command not found: carg", true, "sess1");
        let inv2 = make_bash_invocation("cargo test", "test result: ok. 5 passed", false, "sess1");
        let invocations = vec![&inv1, &inv2];
        let corrections = detect_corrections(&invocations);
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].pattern_type, PatternType::FlagTypo);
        assert_eq!(corrections[0].failed_command, "carg test");
        assert_eq!(corrections[0].successful_command, "cargo test");
    }

    #[test]
    fn test_detect_corrections_no_fix() {
        let inv1 =
            make_bash_invocation("carg test", "error: command not found: carg", true, "sess1");
        let inv2 = make_bash_invocation("ls", "file.rs\nmain.rs", false, "sess1");
        let invocations = vec![&inv1, &inv2];
        let corrections = detect_corrections(&invocations);
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_detect_corrections_missing_separator() {
        let inv1 = make_bash_invocation(
            "cargo test --nocapture",
            "error: unexpected argument '--nocapture'",
            true,
            "sess1",
        );
        let inv2 = make_bash_invocation(
            "cargo test -- --nocapture",
            "test result: ok. 5 passed",
            false,
            "sess1",
        );
        let invocations = vec![&inv1, &inv2];
        let corrections = detect_corrections(&invocations);
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].pattern_type, PatternType::MissingSeparator);
    }

    #[test]
    fn test_detect_corrections_respects_lookahead() {
        // Create a failed command, then 6 unrelated commands, then the fix.
        // The fix should NOT be found (lookahead is 5).
        let failed = make_bash_invocation("carg test", "error: command not found", true, "sess1");
        let filler = make_bash_invocation("ls", "ok", false, "sess1");
        let fix = make_bash_invocation("cargo test", "ok. 5 passed", false, "sess1");

        let invocations: Vec<&ToolInvocation> =
            vec![&failed, &filler, &filler, &filler, &filler, &filler, &fix];
        let corrections = detect_corrections(&invocations);
        assert!(corrections.is_empty());
    }

    // ---- is_tdd_cycle ----

    #[test]
    fn test_tdd_cycle_detected() {
        let inv1 = make_bash_invocation(
            "cargo test --test my_test",
            "test result: FAILED. 0 passed; 1 failed",
            true,
            "sess1",
        );
        let inv2 = make_bash_invocation(
            "cargo test --test my_test",
            "test result: ok. 1 passed; 0 failed",
            false,
            "sess1",
        );
        let inv3 = make_bash_invocation(
            "cargo test --test my_test",
            "test result: FAILED. 0 passed; 1 failed",
            true,
            "sess1",
        );
        let inv4 = make_bash_invocation(
            "cargo test --test my_test",
            "test result: ok. 1 passed; 0 failed",
            false,
            "sess1",
        );

        let invocations = vec![&inv1, &inv2, &inv3, &inv4];
        assert!(is_tdd_cycle(&invocations, 0));
    }

    #[test]
    fn test_tdd_cycle_not_detected() {
        // Only one fail -> pass, not enough for TDD
        let inv1 = make_bash_invocation("cargo test", "FAILED", true, "sess1");
        let inv2 = make_bash_invocation("cargo test", "ok. 5 passed", false, "sess1");

        let invocations = vec![&inv1, &inv2];
        assert!(!is_tdd_cycle(&invocations, 0));
    }

    // ---- deduplicate_and_filter ----

    #[test]
    fn test_deduplicate_merges_same_pair() {
        let pair1 = CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error: command not found".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
        };
        let pair2 = CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error: command not found".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess2".to_string()],
        };

        let result = deduplicate_and_filter(vec![pair1, pair2]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].occurrences, 2);
        assert_eq!(result[0].sessions.len(), 2);
    }

    #[test]
    fn test_filter_single_occurrence_non_typo() {
        let pair = CorrectionPair {
            failed_command: "cargo test --package".to_string(),
            successful_command: "cargo test --package rskim".to_string(),
            error_output: "error: requires value".to_string(),
            pattern_type: PatternType::MissingArg,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
        };

        let result = deduplicate_and_filter(vec![pair]);
        assert!(
            result.is_empty(),
            "Single-occurrence MissingArg should be filtered"
        );
    }

    #[test]
    fn test_filter_keeps_single_occurrence_typo() {
        let pair = CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error: not found".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
        };

        let result = deduplicate_and_filter(vec![pair]);
        assert_eq!(result.len(), 1, "Single-occurrence FlagTypo should be kept");
    }

    #[test]
    fn test_filter_excludes_path_only() {
        let pair = CorrectionPair {
            failed_command: "cat /tmp/a.rs".to_string(),
            successful_command: "cat /tmp/b.rs".to_string(),
            error_output: "no such file".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
        };

        let result = deduplicate_and_filter(vec![pair]);
        assert!(result.is_empty(), "Path-only difference should be filtered");
    }

    // ---- generate_rules_content ----

    #[test]
    fn test_generate_rules_content() {
        let corrections = vec![CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 3,
            sessions: vec!["sess1".to_string()],
        }];

        let content = generate_rules_content(&corrections);
        assert!(content.contains("# CLI Corrections"));
        assert!(content.contains("Typo (seen 3 times)"));
        assert!(content.contains("Instead of: `carg test`"));
        assert!(content.contains("Use: `cargo test`"));
    }

    // ---- parse_args ----

    #[test]
    fn test_parse_args_defaults() {
        let config = parse_args(&[]).unwrap();
        assert!(config.since.is_some());
        assert!(!config.generate);
        assert!(!config.dry_run);
        assert!(!config.json_output);
        assert!(config.agent_filter.is_none());
    }

    #[test]
    fn test_parse_args_generate() {
        let config = parse_args(&["--generate".to_string()]).unwrap();
        assert!(config.generate);
    }

    #[test]
    fn test_parse_args_dry_run() {
        let config = parse_args(&["--dry-run".to_string()]).unwrap();
        assert!(config.dry_run);
    }

    #[test]
    fn test_parse_args_json() {
        let config = parse_args(&["--json".to_string()]).unwrap();
        assert!(config.json_output);
    }

    #[test]
    fn test_parse_args_agent() {
        let config = parse_args(&["--agent".to_string(), "claude-code".to_string()]).unwrap();
        assert_eq!(config.agent_filter, Some(AgentKind::ClaudeCode));
    }

    #[test]
    fn test_parse_args_since() {
        let config = parse_args(&["--since".to_string(), "30d".to_string()]).unwrap();
        assert!(config.since.is_some());
    }

    #[test]
    fn test_parse_args_unknown_flag() {
        let result = parse_args(&["--nonexistent".to_string()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown flag"));
    }

    #[test]
    fn test_parse_args_unknown_agent() {
        let result = parse_args(&["--agent".to_string(), "nonexistent".to_string()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown agent"));
    }

    #[test]
    fn test_parse_args_since_missing_value() {
        let result = parse_args(&["--since".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_args_agent_missing_value() {
        let result = parse_args(&["--agent".to_string()]);
        assert!(result.is_err());
    }

    // ---- levenshtein guards ----

    #[test]
    fn test_levenshtein_large_length_difference() {
        // Length difference > 10 should early-exit with the difference
        let short = "abc";
        let long = "abcdefghijklmnop"; // 16 chars, diff = 13
        assert_eq!(levenshtein(short, long), 13);
    }

    #[test]
    fn test_levenshtein_oversized_input() {
        // Inputs exceeding 500 chars should return a large value
        let long_a: String = "a".repeat(600);
        let long_b: String = "b".repeat(600);
        let result = levenshtein(&long_a, &long_b);
        assert!(result > 10, "oversized inputs should return large distance");
    }

    #[test]
    fn test_levenshtein_normal_inputs_unchanged() {
        // Normal-length inputs should still compute correctly
        assert_eq!(levenshtein("cargo", "cargo"), 0);
        assert_eq!(levenshtein("carg", "cargo"), 1);
        assert_eq!(levenshtein("ab", "cd"), 2);
    }

    // ---- looks_like_error tightened matching ----

    #[test]
    fn test_looks_like_error_benign_error_word() {
        // "0 errors generated" should NOT be detected as error
        assert!(!looks_like_error("0 errors generated"));
        // Filename containing "error" should NOT match
        assert!(!looks_like_error("Compiling error_handler.rs"));
    }

    #[test]
    fn test_looks_like_error_real_error_patterns() {
        // Rust compiler error format
        assert!(looks_like_error("error[E0308]: mismatched types"));
        // Prefixed error on second line
        assert!(looks_like_error("some output\nerror: aborting due to previous error"));
        // Colon-prefixed error
        assert!(looks_like_error("rustc: error: could not compile"));
    }

    // ---- sanitize_command_for_rules ----

    #[test]
    fn test_sanitize_command_for_rules_basic() {
        assert_eq!(sanitize_command_for_rules("cargo test"), "cargo test");
    }

    #[test]
    fn test_sanitize_command_for_rules_backticks() {
        // Backticks should be escaped to prevent breaking inline code
        assert_eq!(
            sanitize_command_for_rules("echo `whoami`"),
            "echo 'whoami'"
        );
    }

    #[test]
    fn test_sanitize_command_for_rules_heading_injection() {
        // Leading '#' should be stripped to prevent heading injection
        assert_eq!(
            sanitize_command_for_rules("# Injected heading"),
            "Injected heading"
        );
    }

    #[test]
    fn test_sanitize_command_for_rules_truncation() {
        let long_cmd = "x".repeat(300);
        let result = sanitize_command_for_rules(&long_cmd);
        assert!(result.len() <= 200);
    }

    #[test]
    fn test_sanitize_command_for_rules_newlines() {
        // Multi-line commands should be collapsed
        assert_eq!(
            sanitize_command_for_rules("echo hello\necho world"),
            "echo hello echo world"
        );
    }
}
