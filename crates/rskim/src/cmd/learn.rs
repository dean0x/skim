//! `skim learn` -- detect CLI error patterns and generate correction rules (#64)
//!
//! Scans AI agent session files for error-retry patterns: a failed Bash command
//! followed by a similar successful command within the next few invocations.
//! Optionally generates an agent-specific rules file (e.g., `.claude/rules/skim-corrections.md`).

use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ExitCode;

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{ContentArrangement, Table};

use super::session::{self, parse_duration_ago, AgentKind, ToolInput, ToolInvocation};

/// Run the learn subcommand.
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
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

    // Collect all invocations — show a spinner on TTY; JSON mode bypasses all UI (D5).
    let spinner = if !config.json_output {
        Some(crate::cmd::ux::spinner("Scanning agent sessions..."))
    } else {
        None
    };
    let all_invocations = session::collect_invocations(&providers, &filter)?;
    if let Some(s) = spinner {
        s.finish_and_clear();
    }

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
    let corrections = deduplicate_and_filter(corrections, config.min_occurrences);

    if corrections.is_empty() {
        println!("No CLI error patterns detected.");
        return Ok(ExitCode::SUCCESS);
    }

    if config.json_output {
        print_json_report(&corrections)?;
    } else {
        // Use the agent filter for rules output format, default to ClaudeCode
        let rules_agent = config.agent_filter.unwrap_or(AgentKind::ClaudeCode);
        if config.generate {
            let content = generate_rules_content(&corrections, rules_agent);
            write_rules_file(&content, rules_agent, config.dry_run)?;
        } else {
            print_text_report(&corrections, rules_agent);
        }
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
    min_occurrences: usize,
}

/// Parse CLI arguments into a [`LearnConfig`].
///
/// SYNC NOTE: This function must remain in sync with [`command()`] — any flag
/// added here must also be added there (for shell completions), and vice versa.
fn parse_args(args: &[String]) -> anyhow::Result<LearnConfig> {
    let mut config = LearnConfig {
        since: Some(std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 86400)),
        generate: false,
        dry_run: false,
        json_output: false,
        agent_filter: None,
        min_occurrences: 3,
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
                config.agent_filter = Some(AgentKind::parse_cli_arg(&args[i])?);
            }
            "--min-occurrences" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--min-occurrences requires a value (e.g., 3)");
                }
                let n: usize = args[i].parse().map_err(|_| {
                    anyhow::anyhow!(
                        "--min-occurrences must be a positive integer, got: '{}'",
                        args[i]
                    )
                })?;
                if n == 0 {
                    anyhow::bail!("--min-occurrences must be at least 1");
                }
                config.min_occurrences = n;
            }
            other => {
                anyhow::bail!(
                    "unknown flag: '{other}'\n\nUsage: skim learn [--since <duration>] [--generate] [--dry-run] [--json] [--min-occurrences <N>]"
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
    /// Which agent produced this correction (for per-agent rules output).
    #[allow(dead_code)] // Read in Phase 2 for per-agent filtering
    agent: AgentKind,
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

/// Check if tool result content indicates an agent-user permission denial.
///
/// Permission denials happen when the user rejects a tool invocation. These are
/// not CLI errors — they produce garbage pairings because the "failed" command
/// was never actually executed. Standard Unix "permission denied" (filesystem
/// errors) is NOT excluded.
fn is_permission_denial(content: &str) -> bool {
    let check = truncate_utf8(content, 512);
    let lower = check.to_ascii_lowercase();
    lower.contains("has been denied")
        || lower.contains("user denied")
        || lower.contains("aborted by user")
        || lower.contains("user rejected")
        || lower.contains("permission was denied by user")
}

/// Detect error-retry patterns in a sequence of Bash invocations.
///
/// For each failed Bash command, look at the next N (default 5) Bash commands.
/// If a similar successful command is found, create a `CorrectionPair`.
fn detect_corrections(bash_invocations: &[&ToolInvocation]) -> Vec<CorrectionPair> {
    let mut corrections = Vec::new();

    for (i, inv) in bash_invocations.iter().enumerate() {
        let result = match &inv.result {
            Some(r) if r.is_error || looks_like_error(&r.content) => r,
            _ => continue,
        };

        // Skip permission denials — these are agent-user rejections, not CLI errors
        if is_permission_denial(&result.content) {
            continue;
        }

        if is_tdd_cycle(bash_invocations, i) {
            continue;
        }

        let failed_cmd = match &inv.input {
            ToolInput::Bash { command } => command.as_str(),
            _ => continue,
        };

        if let Some(pair) = find_correction(
            bash_invocations,
            i,
            failed_cmd,
            result,
            &inv.session_id,
            inv.agent,
        ) {
            corrections.push(pair);
        }
    }

    corrections
}

/// Search the next LOOKAHEAD Bash invocations for a successful correction.
fn find_correction(
    invocations: &[&ToolInvocation],
    failed_idx: usize,
    failed_cmd: &str,
    error_result: &session::ToolResult,
    session_id: &str,
    agent: AgentKind,
) -> Option<CorrectionPair> {
    const LOOKAHEAD: usize = 5;
    let end = (failed_idx + 1 + LOOKAHEAD).min(invocations.len());

    for candidate in invocations.iter().take(end).skip(failed_idx + 1) {
        let succeeded = candidate
            .result
            .as_ref()
            .is_some_and(|r| !r.is_error && !looks_like_error(&r.content));
        if !succeeded {
            continue;
        }

        let candidate_cmd = match &candidate.input {
            ToolInput::Bash { command } => command.as_str(),
            _ => continue,
        };

        // Pre-filter: base command must match (or be a typo with edit distance ≤1)
        let failed_base = failed_cmd.split_whitespace().next().unwrap_or_default();
        let candidate_base = candidate_cmd.split_whitespace().next().unwrap_or_default();
        if failed_base != candidate_base && levenshtein(failed_base, candidate_base) > 1 {
            continue;
        }

        if let Some(pattern) = classify_correction(failed_cmd, candidate_cmd) {
            return Some(CorrectionPair {
                failed_command: failed_cmd.to_string(),
                successful_command: candidate_cmd.to_string(),
                error_output: sanitize_error_output(&error_result.content),
                pattern_type: pattern,
                occurrences: 1,
                sessions: vec![session_id.to_string()],
                agent,
            });
        }
    }

    None
}

// ============================================================================
// Similarity matching
// ============================================================================

/// Classify how a correction differs from the failed command.
fn classify_correction(failed: &str, success: &str) -> Option<PatternType> {
    if failed == success {
        return None;
    }

    let failed_tokens: Vec<&str> = failed.split_whitespace().collect();
    let success_tokens: Vec<&str> = success.split_whitespace().collect();

    if failed_tokens.is_empty() || success_tokens.is_empty() {
        return None;
    }

    // Strings differ only in whitespace — not a real correction
    if failed_tokens == success_tokens {
        return None;
    }

    // Strategy 1: Command name typo (different first token)
    if failed_tokens[0] != success_tokens[0] {
        return classify_by_command_typo(failed_tokens[0], success_tokens[0]);
    }

    // Strategy 2: Missing separator
    if !failed.contains(" -- ") && success.contains(" -- ") {
        return Some(PatternType::MissingSeparator);
    }

    // Strategy 3: Edit-distance based (close strings)
    // Strategy 4 (fallback): Shared prefix (same first 2 tokens)
    classify_by_edit_distance(failed, success, &failed_tokens, &success_tokens)
        .or_else(|| classify_by_shared_prefix(&failed_tokens, &success_tokens))
}

/// Classify a correction where the command name itself is a typo (edit distance 1-2).
fn classify_by_command_typo(failed_cmd: &str, success_cmd: &str) -> Option<PatternType> {
    if levenshtein(failed_cmd, success_cmd) <= 2 {
        Some(PatternType::FlagTypo)
    } else {
        None
    }
}

/// Classify corrections where the full command edit distance is small (at most 3).
fn classify_by_edit_distance(
    failed: &str,
    success: &str,
    failed_tokens: &[&str],
    success_tokens: &[&str],
) -> Option<PatternType> {
    if levenshtein(failed, success) > 3 {
        return None;
    }

    match failed_tokens.len().cmp(&success_tokens.len()) {
        std::cmp::Ordering::Less => Some(PatternType::MissingArg),
        std::cmp::Ordering::Equal => classify_same_length_tokens(failed_tokens, success_tokens),
        std::cmp::Ordering::Greater => Some(PatternType::FlagTypo),
    }
}

/// Classify when token counts match and edit distance is small.
fn classify_same_length_tokens(
    failed_tokens: &[&str],
    success_tokens: &[&str],
) -> Option<PatternType> {
    let diffs: Vec<usize> = failed_tokens
        .iter()
        .zip(success_tokens.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();

    // Whitespace-only difference: strings differ but tokens are identical
    if diffs.is_empty() {
        return None;
    }

    if diffs.len() == 1 {
        let idx = diffs[0];
        if failed_tokens[idx].starts_with('-') || success_tokens[idx].starts_with('-') {
            return Some(PatternType::WrongFlag);
        }
        return Some(PatternType::FlagTypo);
    }

    Some(PatternType::FlagTypo)
}

/// Classify corrections where the first two tokens match (same base command).
fn classify_by_shared_prefix(
    failed_tokens: &[&str],
    success_tokens: &[&str],
) -> Option<PatternType> {
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

    // Two-row DP: O(n) space instead of O(m*n).
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

// ============================================================================
// Error detection
// ============================================================================

/// Truncate a string to at most `max_len` bytes at a valid UTF-8 boundary.
fn truncate_utf8(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Heuristic: does the output content look like a command error?
///
/// Checks for common error indicators in command output. Only examines
/// the first 1KB to avoid allocating a full lowercase copy of large output.
///
/// Uses prefix patterns for "error" to avoid false positives on benign
/// output like "0 errors generated" or filenames containing "error".
fn looks_like_error(content: &str) -> bool {
    let check_content = truncate_utf8(content, 1024);

    let lower = check_content.to_ascii_lowercase();

    // "0 failed" is a success indicator in test output — exclude it
    let has_failed = lower.contains("failed") && !lower.contains("0 failed");

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
    let normalized_cmd = normalize_command(cmd);

    for inv in invocations.iter().skip(start_idx + 1).take(10) {
        let inv_cmd = match &inv.input {
            ToolInput::Bash { command } => command.as_str(),
            _ => continue,
        };

        // Must be the same (or very similar) command
        if normalize_command(inv_cmd) != normalized_cmd {
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
fn deduplicate_and_filter(
    corrections: Vec<CorrectionPair>,
    min_occurrences: usize,
) -> Vec<CorrectionPair> {
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
            pair.occurrences >= min_occurrences
                && !is_path_only_difference(&pair.failed_command, &pair.successful_command)
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

    let diffs: Vec<_> = a_tokens
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
    s.contains('/')
}

// ============================================================================
// Rules file generation
// ============================================================================

/// Generate the rules file content from correction pairs.
///
/// Adds agent-specific frontmatter for Cursor (.mdc) and Copilot (.instructions.md).
fn generate_rules_content(corrections: &[CorrectionPair], agent: AgentKind) -> String {
    let mut output = String::new();

    // Agent-specific frontmatter
    match agent {
        AgentKind::Cursor => {
            output.push_str(
                "---\ndescription: CLI corrections learned by skim\nalwaysApply: true\n---\n\n",
            );
        }
        AgentKind::CopilotCli => {
            output.push_str("---\napplyTo: \"**/*\"\n---\n\n");
        }
        _ => {}
    }

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

/// Sanitize a string for safe inclusion in a markdown rules file.
///
/// Prevents prompt injection by:
/// - Collapsing to single line
/// - Truncating to `max_len` chars (longer strings are not useful in rules)
/// - Escaping backticks to prevent breaking out of inline code
/// - Stripping markdown heading markers at line start
fn sanitize_for_rules(s: &str, max_len: usize) -> String {
    let single_line: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let single_line = single_line.trim();

    truncate_utf8(single_line, max_len)
        .replace('`', "'")
        .trim_start_matches('#')
        .trim_start()
        .to_string()
}

/// Sanitize error output to prevent data leakage and prompt injection.
fn sanitize_error_output(error: &str) -> String {
    sanitize_for_rules(error, 200)
}

/// Sanitize a command string for safe inclusion in a markdown rules file.
fn sanitize_command_for_rules(cmd: &str) -> String {
    sanitize_for_rules(cmd, 200)
}

/// Count correction pairs in a rules file by counting `## ` heading lines.
fn corrections_count(content: &str) -> usize {
    content
        .lines()
        .filter(|l| l.starts_with("## "))
        .count()
}

/// Write the rules file to the appropriate agent-specific location.
///
/// For agents with a rules directory (Claude Code, Cursor, Copilot),
/// creates the file automatically. For single-file agents (Codex, Gemini,
/// OpenCode), prints the content with instructions to paste.
fn write_rules_file(content: &str, agent: AgentKind, dry_run: bool) -> anyhow::Result<()> {
    match agent.rules_dir() {
        Some(dir) => {
            // Directory-based agents: auto-create file
            let rules_dir = std::path::Path::new(&dir);
            let filename = agent.rules_filename();
            let rules_path = rules_dir.join(filename);

            // Migrate legacy filename (cli-corrections.md -> skim-corrections.md)
            let legacy_path = rules_dir.join("cli-corrections.md");
            if legacy_path.exists() && !rules_path.exists() {
                std::fs::rename(&legacy_path, &rules_path)?;
            }

            if dry_run {
                println!("Would write to: {}", rules_path.display());
                println!("---");
                print!("{content}");
                return Ok(());
            }

            std::fs::create_dir_all(rules_dir)?;
            let correction_count = corrections_count(content);
            std::fs::write(&rules_path, content)?;
            println!(
                "{} Wrote {} correction{} to {}",
                crate::cmd::ux::success_mark(),
                correction_count,
                if correction_count == 1 { "" } else { "s" },
                rules_path.display(),
            );
        }
        None => {
            // Single-file agents: print content with instructions
            println!(
                "Add the following to your {} configuration:\n",
                agent.display_name()
            );
            println!("---");
            print!("{content}");
            println!("---");
        }
    }
    Ok(())
}

// ============================================================================
// Output
// ============================================================================

fn print_text_report(corrections: &[CorrectionPair], agent: AgentKind) {
    println!(
        "skim learn -- {} correction{} detected\n",
        corrections.len(),
        if corrections.len() == 1 { "" } else { "s" }
    );

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["#", "Pattern", "Seen", "Failed", "Correct"]);

    for (i, pair) in corrections.iter().enumerate() {
        table.add_row(vec![
            (i + 1).to_string(),
            pair.pattern_type.label().to_string(),
            format!("{}x", pair.occurrences),
            pair.failed_command.clone(),
            pair.successful_command.clone(),
        ]);
    }

    println!("{table}");
    println!();

    let target = match agent.rules_dir() {
        Some(dir) => std::path::Path::new(dir)
            .join(agent.rules_filename())
            .display()
            .to_string(),
        None => format!("{} configuration", agent.display_name()),
    };
    println!("hint: run `skim learn --generate` to write corrections to {target}");
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
    println!("  --since <duration>        Time window (e.g., 24h, 7d, 1w) [default: 7d]");
    println!("                            (7d default provides enough history for");
    println!("                             reliable error-pattern detection)");
    println!("  --generate                Write rules to agent-specific rules file");
    println!("  --dry-run                 Preview rules without writing (requires --generate)");
    println!("  --agent <name>            Only scan sessions from a specific agent");
    println!("  --min-occurrences <N>     Minimum occurrences to report [default: 3]");
    println!("  --json                    Output machine-readable JSON");
    println!("  --help, -h                Print help information");
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

/// Build the clap [`Command`] for `skim learn` (used for shell completions).
///
/// SYNC NOTE: This must remain in sync with [`parse_args()`] — any flag added
/// here must also be handled in `parse_args`, and vice versa.
pub(super) fn command() -> clap::Command {
    clap::Command::new("learn")
        .about("Detect CLI error patterns and generate correction rules")
        .arg(
            clap::Arg::new("since")
                .long("since")
                .value_name("DURATION")
                .help("Time window (e.g., 24h, 7d, 1w) [default: 7d]"),
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
        .arg(
            clap::Arg::new("min-occurrences")
                .long("min-occurrences")
                .value_name("N")
                .help("Minimum occurrences to report [default: 3]"),
        )
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::session::ToolResult;

    // ---- truncate_utf8 ----

    #[test]
    fn test_truncate_utf8_ascii_within_limit() {
        assert_eq!(truncate_utf8("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_utf8_ascii_at_boundary() {
        assert_eq!(truncate_utf8("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_utf8_ascii_over_limit() {
        assert_eq!(truncate_utf8("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_utf8_empty_string() {
        assert_eq!(truncate_utf8("", 10), "");
    }

    #[test]
    fn test_truncate_utf8_zero_limit() {
        assert_eq!(truncate_utf8("hello", 0), "");
    }

    #[test]
    fn test_truncate_utf8_multibyte_boundary() {
        // "café" is 5 bytes: c(1) a(1) f(1) é(2)
        // max_len=4 must not split the 2-byte 'é' at byte 4 — result is "caf"
        let s = "café";
        let result = truncate_utf8(s, 4);
        assert_eq!(result, "caf");
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn test_truncate_utf8_multibyte_exactly_fits() {
        // max_len=5 fits "café" exactly (5 bytes)
        let s = "café";
        assert_eq!(truncate_utf8(s, 5), "café");
    }

    #[test]
    fn test_truncate_utf8_overflow_limit_larger_than_string() {
        assert_eq!(truncate_utf8("hi", 1000), "hi");
    }

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

    fn make_bash_invocation_no_result(command: &str, session_id: &str) -> ToolInvocation {
        ToolInvocation {
            result: None,
            ..make_bash_invocation(command, "", false, session_id)
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
            agent: AgentKind::ClaudeCode,
        };
        let pair2 = CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error: command not found".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess2".to_string()],
            agent: AgentKind::ClaudeCode,
        };
        let pair3 = CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error: command not found".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess3".to_string()],
            agent: AgentKind::ClaudeCode,
        };

        let result = deduplicate_and_filter(vec![pair1, pair2, pair3], 3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].occurrences, 3);
        assert_eq!(result[0].sessions.len(), 3);
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
            agent: AgentKind::ClaudeCode,
        };

        let result = deduplicate_and_filter(vec![pair], 3);
        assert!(
            result.is_empty(),
            "Single-occurrence MissingArg should be filtered"
        );
    }

    #[test]
    fn test_filter_rejects_single_occurrence_typo() {
        // Requires ≥3 occurrences by default
        let pair = CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error: not found".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
            agent: AgentKind::ClaudeCode,
        };

        let result = deduplicate_and_filter(vec![pair], 3);
        assert!(
            result.is_empty(),
            "Single-occurrence FlagTypo should be filtered (requires ≥3)"
        );
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
            agent: AgentKind::ClaudeCode,
        };

        let result = deduplicate_and_filter(vec![pair], 3);
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
            agent: AgentKind::ClaudeCode,
        }];

        let content = generate_rules_content(&corrections, AgentKind::ClaudeCode);
        assert!(content.contains("# CLI Corrections"));
        assert!(content.contains("Typo (seen 3 times)"));
        assert!(content.contains("Instead of: `carg test`"));
        assert!(content.contains("Use: `cargo test`"));
        // Claude Code: no frontmatter
        assert!(!content.starts_with("---"));
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

    #[test]
    fn test_parse_args_min_occurrences() {
        let config = parse_args(&["--min-occurrences".to_string(), "5".to_string()]).unwrap();
        assert_eq!(config.min_occurrences, 5);
    }

    #[test]
    fn test_parse_args_min_occurrences_zero_rejected() {
        let result = parse_args(&["--min-occurrences".to_string(), "0".to_string()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("at least 1"), "got: {msg}");
    }

    #[test]
    fn test_parse_args_min_occurrences_default() {
        let config = parse_args(&[]).unwrap();
        assert_eq!(config.min_occurrences, 3);
    }

    #[test]
    fn test_parse_args_min_occurrences_non_integer_rejected() {
        let result = parse_args(&["--min-occurrences".to_string(), "abc".to_string()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("positive integer"),
            "error should mention 'positive integer', got: {msg}"
        );
        assert!(
            msg.contains("abc"),
            "error should echo the bad value, got: {msg}"
        );
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
        assert!(looks_like_error(
            "some output\nerror: aborting due to previous error"
        ));
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
        assert_eq!(sanitize_command_for_rules("echo `whoami`"), "echo 'whoami'");
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

    // ---- classify_correction edge cases ----

    #[test]
    fn test_classify_same_length_single_non_flag_diff() {
        // Small edit distance, same token count, single diff on a non-flag token
        let result = classify_correction("cargo test mytest", "cargo test urtest");
        assert_eq!(result, Some(PatternType::FlagTypo));
    }

    #[test]
    fn test_classify_same_length_multiple_diffs() {
        // Small edit distance, same token count, multiple token diffs
        let result = classify_correction("cargo tst rskim", "cargo test skim");
        assert_eq!(result, Some(PatternType::FlagTypo));
    }

    #[test]
    fn test_classify_shared_prefix_more_failed_tokens() {
        // Shared first 2 tokens, large edit distance, more failed tokens
        let result = classify_correction(
            "cargo test --release --verbose --all",
            "cargo test --profile release",
        );
        assert_eq!(result, Some(PatternType::WrongFlag));
    }

    #[test]
    fn test_classify_command_typo_edit_distance_2() {
        // First token edit distance exactly 2 ("crago" -> "cargo")
        let result = classify_correction("crago test", "cargo test");
        assert_eq!(result, Some(PatternType::FlagTypo));
    }

    #[test]
    fn test_classify_whitespace_only_diff() {
        // Whitespace-only difference (tokens identical after split_whitespace)
        // is not a real correction — return None.
        let result = classify_correction("cargo  test", "cargo test");
        assert_eq!(result, None);
    }

    #[test]
    fn test_classify_shared_prefix_fewer_failed_tokens() {
        // Shared first 2 tokens, large edit distance, fewer failed tokens
        let result = classify_correction("cargo test --lib", "cargo test --lib --release -v");
        assert_eq!(result, Some(PatternType::MissingArg));
    }

    #[test]
    fn test_classify_shared_prefix_equal_tokens() {
        // Shared first 2 tokens, large edit distance, equal token count
        let result =
            classify_correction("cargo test --debug --extra", "cargo test --release --opt");
        assert_eq!(result, Some(PatternType::WrongFlag));
    }

    #[test]
    fn test_classify_single_token_no_match() {
        // Single shared token, insufficient for prefix match, large edit distance
        let result = classify_correction("cargo", "cargo test --release");
        assert_eq!(result, None);
    }

    #[test]
    fn test_classify_edit_dist_more_failed_tokens() {
        // Small edit distance, more failed tokens than success
        let result = classify_correction("cargo test -v", "cargo test");
        assert_eq!(result, Some(PatternType::FlagTypo));
    }

    // ---- detect_corrections edge cases ----

    #[test]
    fn test_detect_corrections_multiple_pairs() {
        // Two separate fail-fix pairs should produce 2 corrections
        let inv1 = make_bash_invocation("carg test", "error: command not found", true, "sess1");
        let inv2 = make_bash_invocation("cargo test", "ok. 5 passed", false, "sess1");
        let inv3 = make_bash_invocation("cargo tset", "error: command not found", true, "sess1");
        let inv4 = make_bash_invocation("cargo test", "ok. 5 passed", false, "sess1");
        let invocations = vec![&inv1, &inv2, &inv3, &inv4];
        let corrections = detect_corrections(&invocations);
        assert_eq!(corrections.len(), 2);
    }

    #[test]
    fn test_detect_corrections_skips_no_result() {
        // Failed command with result: None should be skipped entirely
        let inv = make_bash_invocation_no_result("carg test", "sess1");
        let inv2 = make_bash_invocation("cargo test", "ok", false, "sess1");
        let invocations = vec![&inv, &inv2];
        let corrections = detect_corrections(&invocations);
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_detect_corrections_candidate_no_result_skipped() {
        // Candidate with result: None is skipped; fix after it is still found
        let failed = make_bash_invocation("carg test", "error: command not found", true, "sess1");
        let no_result = make_bash_invocation_no_result("cargo test", "sess1");
        let fix = make_bash_invocation("cargo test", "ok. 5 passed", false, "sess1");
        let invocations = vec![&failed, &no_result, &fix];
        let corrections = detect_corrections(&invocations);
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].successful_command, "cargo test");
    }

    #[test]
    fn test_detect_corrections_tdd_cycle_excluded() {
        // TDD cycle (same command alternates fail/pass 3+ times) produces no corrections
        let inv1 = make_bash_invocation("cargo test", "FAILED. 1 failed", true, "sess1");
        let inv2 = make_bash_invocation("cargo test", "ok. 1 passed; 0 failed", false, "sess1");
        let inv3 = make_bash_invocation("cargo test", "FAILED. 1 failed", true, "sess1");
        let inv4 = make_bash_invocation("cargo test", "ok. 1 passed; 0 failed", false, "sess1");
        let invocations = vec![&inv1, &inv2, &inv3, &inv4];
        let corrections = detect_corrections(&invocations);
        assert!(
            corrections.is_empty(),
            "TDD cycles should not produce corrections"
        );
    }

    // ---- per-agent rules file output ----
    // Note: rules_filename() tests moved to session::types::tests (AgentKind method)

    #[test]
    fn test_generate_rules_content_cursor_frontmatter() {
        let corrections = vec![CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
            agent: AgentKind::ClaudeCode,
        }];

        let content = generate_rules_content(&corrections, AgentKind::Cursor);
        assert!(content.starts_with("---\ndescription: CLI corrections learned by skim\n"));
        assert!(content.contains("alwaysApply: true"));
        assert!(content.contains("# CLI Corrections"));
    }

    #[test]
    fn test_generate_rules_content_copilot_frontmatter() {
        let corrections = vec![CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
            agent: AgentKind::ClaudeCode,
        }];

        let content = generate_rules_content(&corrections, AgentKind::CopilotCli);
        assert!(content.starts_with("---\napplyTo:"));
        assert!(content.contains("# CLI Corrections"));
    }

    // ---- is_permission_denial ----

    #[test]
    fn test_is_permission_denial_positive() {
        assert!(is_permission_denial(
            "The tool use has been denied by the user"
        ));
        assert!(is_permission_denial("User denied this tool execution"));
        assert!(is_permission_denial("Operation aborted by user"));
        assert!(is_permission_denial("This user rejected the request"));
        assert!(is_permission_denial(
            "Permission was denied by user for this action"
        ));
    }

    #[test]
    fn test_is_permission_denial_negative() {
        // Standard Unix permission denied — this is a real CLI error, not agent denial
        assert!(!is_permission_denial("permission denied: /etc/shadow"));
        assert!(!is_permission_denial("error: command not found: carg"));
        assert!(!is_permission_denial("Build FAILED"));
        assert!(!is_permission_denial("test result: ok. 5 passed; 0 failed"));
        assert!(!is_permission_denial(""));
    }

    #[test]
    fn test_detect_corrections_skips_permission_denial() {
        // A permission denial followed by a successful command should NOT produce a correction
        let denied = make_bash_invocation(
            "rm -rf /tmp/test",
            "The tool use has been denied by the user",
            true,
            "sess1",
        );
        let success = make_bash_invocation("rm -rf /tmp/test", "removed", false, "sess1");
        let invocations = vec![&denied, &success];
        let corrections = detect_corrections(&invocations);
        assert!(
            corrections.is_empty(),
            "Permission denials should not produce corrections"
        );
    }

    // ---- base command pre-filter ----

    #[test]
    fn test_find_correction_requires_same_base_command() {
        // python (failed) → cargo (success) — completely different base commands, rejected
        let failed = make_bash_invocation("python main.py", "error: file not found", true, "sess1");
        let success = make_bash_invocation("cargo run", "ok", false, "sess1");
        let invocations = vec![&failed, &success];
        let corrections = detect_corrections(&invocations);
        assert!(
            corrections.is_empty(),
            "python→cargo should be rejected (different base command)"
        );
    }

    #[test]
    fn test_find_correction_allows_typo_base_command() {
        // carg → cargo has edit distance 1 on base command — allowed
        let failed = make_bash_invocation("carg test", "error: command not found", true, "sess1");
        let success = make_bash_invocation("cargo test", "ok. 5 passed", false, "sess1");
        let invocations = vec![&failed, &success];
        let corrections = detect_corrections(&invocations);
        assert_eq!(
            corrections.len(),
            1,
            "carg→cargo should be allowed (base command edit distance 1)"
        );
    }

    #[test]
    fn test_find_correction_rejects_distant_base_command() {
        // gh → git has edit distance 2 on base command — rejected
        let failed = make_bash_invocation("gh status", "error: not found", true, "sess1");
        let success = make_bash_invocation("git status", "On branch main", false, "sess1");
        let invocations = vec![&failed, &success];
        let corrections = detect_corrections(&invocations);
        assert!(
            corrections.is_empty(),
            "gh→git should be rejected (base command edit distance 2)"
        );
    }

    #[test]
    fn test_generate_rules_content_codex_no_frontmatter() {
        let corrections = vec![CorrectionPair {
            failed_command: "carg test".to_string(),
            successful_command: "cargo test".to_string(),
            error_output: "error".to_string(),
            pattern_type: PatternType::FlagTypo,
            occurrences: 1,
            sessions: vec!["sess1".to_string()],
            agent: AgentKind::ClaudeCode,
        }];

        let content = generate_rules_content(&corrections, AgentKind::CodexCli);
        assert!(!content.starts_with("---"));
        assert!(content.starts_with("# CLI Corrections"));
    }

    /// Sync test: verifies that `parse_args` and `command()` accept the same flags.
    ///
    /// If this test fails, a flag was added to one but not the other. Update both
    /// `parse_args` and `command` together.
    #[test]
    fn test_parse_args_and_command_are_in_sync() {
        // Build the clap command for validation
        let cmd = command();

        // Flags exercised: --since, --generate, --dry-run, --json, --agent, --min-occurrences
        let all_args = [
            "--since",
            "7d",
            "--generate",
            "--dry-run",
            "--json",
            "--agent",
            "claude-code",
            "--min-occurrences",
            "3",
        ];

        // clap must accept these flags without error
        cmd.clone()
            .try_get_matches_from(std::iter::once("learn").chain(all_args.iter().copied()))
            .expect("clap rejected flags that parse_args accepts — sync is broken");

        // parse_args must also accept these flags without error
        let string_args: Vec<String> = all_args.iter().map(|s| s.to_string()).collect();
        parse_args(&string_args)
            .expect("parse_args rejected flags that clap accepts — sync is broken");

        // Verify individual flag values agree between parse_args and clap
        let matches = cmd
            .try_get_matches_from(std::iter::once("learn").chain(all_args.iter().copied()))
            .unwrap();

        // --generate: both must agree it is set
        assert!(
            matches.get_flag("generate"),
            "clap should see --generate as true"
        );

        // --dry-run: both must agree it is set
        assert!(
            matches.get_flag("dry-run"),
            "clap should see --dry-run as true"
        );

        // --json: both must agree it is set
        assert!(matches.get_flag("json"), "clap should see --json as true");

        // --since: clap must surface the value
        assert_eq!(
            matches.get_one::<String>("since").map(|s| s.as_str()),
            Some("7d"),
            "clap --since value should be '7d'"
        );

        // --agent: clap must surface the value
        assert_eq!(
            matches.get_one::<String>("agent").map(|s| s.as_str()),
            Some("claude-code"),
            "clap --agent value should be 'claude-code'"
        );

        // --min-occurrences: clap must surface the value
        assert_eq!(
            matches
                .get_one::<String>("min-occurrences")
                .map(|s| s.as_str()),
            Some("3"),
            "clap --min-occurrences value should be '3'"
        );
    }
}
