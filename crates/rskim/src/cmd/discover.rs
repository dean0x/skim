//! `skim discover` -- identify missed optimization opportunities (#61)
//!
//! Scans AI agent session files to find tool invocations where skim could
//! have saved tokens. Agent-agnostic: uses SessionProvider trait.

use std::io::{self, Write};
use std::process::ExitCode;

use super::session::{self, parse_duration_ago, AgentKind, ToolInput, ToolInvocation};

/// Run the discover subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let config = parse_args(args)?;

    let providers = session::get_providers(config.agent_filter);
    if providers.is_empty() {
        println!("No AI agent sessions found.");
        println!("hint: skim discover scans for Claude Code sessions in ~/.claude/projects/");
        return Ok(ExitCode::SUCCESS);
    }

    let filter = session::TimeFilter {
        since: config.since,
        latest_only: config.session_latest,
    };

    // Collect invocations from all providers
    let all_invocations = session::collect_invocations(&providers, &filter)?;

    if all_invocations.is_empty() {
        println!("No tool invocations found in the specified time window.");
        return Ok(ExitCode::SUCCESS);
    }

    // Classify and analyze
    let analysis = analyze_invocations(&all_invocations, &config);

    if config.json_output {
        print_json_report(&analysis, &config)?;
    } else {
        print_text_report(&analysis, &config);
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Config
// ============================================================================

#[derive(Debug)]
struct DiscoverConfig {
    since: Option<std::time::SystemTime>,
    session_latest: bool,
    agent_filter: Option<AgentKind>,
    json_output: bool,
    debug: bool,
}

/// Parse CLI arguments into a [`DiscoverConfig`].
///
/// SYNC NOTE: This function must remain in sync with [`command()`] — any flag
/// added here must also be added there (for shell completions), and vice versa.
fn parse_args(args: &[String]) -> anyhow::Result<DiscoverConfig> {
    let mut config = DiscoverConfig {
        since: Some(std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600)),
        session_latest: false,
        agent_filter: None,
        json_output: false,
        // Honor SKIM_DEBUG env var as well as the --debug flag
        debug: crate::debug::is_debug_enabled(),
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
            "--session" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--session requires a value (e.g., latest)");
                }
                if args[i] == "latest" {
                    config.session_latest = true;
                } else {
                    anyhow::bail!("--session only supports 'latest'");
                }
            }
            "--agent" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!("--agent requires a value (e.g., claude-code)");
                }
                config.agent_filter = Some(AgentKind::parse_cli_arg(&args[i])?);
            }
            "--json" => {
                config.json_output = true;
            }
            "--debug" => {
                config.debug = true;
                crate::debug::force_enable_debug();
            }
            other => {
                anyhow::bail!(
                    "unknown flag: '{other}'\n\nUsage: skim discover [--since <duration>] [--session latest] [--agent <name>] [--json] [--debug]"
                );
            }
        }
        i += 1;
    }

    Ok(config)
}

// ============================================================================
// Analysis
// ============================================================================

struct DiscoverAnalysis {
    total_invocations: usize,
    code_reads: Vec<CodeReadInfo>,
    bash_commands: Vec<BashCommandInfo>,
    total_read_tokens: usize,
    potential_savings_tokens: usize,
    /// Non-rewritable command prefixes (first 3 tokens), with occurrence counts.
    /// Only populated when debug mode is enabled.
    non_rewritable_commands: Vec<(String, usize)>,
}

struct CodeReadInfo {
    file_path: String,
    #[allow(dead_code)]
    result_bytes: usize,
    result_tokens: usize,
    could_skim: bool,
}

struct BashCommandInfo {
    command: String,
    has_rewrite: bool,
    rewrite_target: Option<String>,
}

fn analyze_invocations(
    invocations: &[ToolInvocation],
    config: &DiscoverConfig,
) -> DiscoverAnalysis {
    let mut code_reads = Vec::new();
    let mut bash_commands = Vec::new();
    let mut total_read_tokens = 0usize;
    let mut potential_savings = 0usize;
    // Only allocate the HashMap when debug mode is enabled; it is unused on the
    // common (non-debug) path and would otherwise impose an unconditional allocation.
    let mut non_rewritable_counts: Option<std::collections::HashMap<String, usize>> =
        if config.debug {
            Some(std::collections::HashMap::new())
        } else {
            None
        };

    for inv in invocations {
        match &inv.input {
            ToolInput::Read { file_path } => {
                let result_content = inv
                    .result
                    .as_ref()
                    .map(|r| r.content.as_str())
                    .unwrap_or_default();
                let result_bytes = result_content.len();
                let result_tokens = estimate_tokens(result_content);
                total_read_tokens += result_tokens;

                // Check if this is a code file that skim supports
                let could_skim = is_skimmable_file(file_path);
                if could_skim && !result_content.is_empty() {
                    // Estimate savings: structure mode typically saves 60-80%
                    let estimated_savings = (result_tokens as f64 * 0.7) as usize;
                    potential_savings += estimated_savings;
                }

                code_reads.push(CodeReadInfo {
                    file_path: file_path.clone(),
                    result_bytes,
                    result_tokens,
                    could_skim,
                });
            }
            ToolInput::Bash { command } => {
                if let Some(info) = classify_bash_command(command) {
                    // Accumulate non-rewritable prefix counts when debug is enabled.
                    // Responsibility lives in the caller, not classify_bash_command,
                    // so the classification function stays pure and easily testable.
                    if let Some(ref mut counts) = non_rewritable_counts {
                        if !info.has_rewrite {
                            let prefix: String = command
                                .split_whitespace()
                                .take(3)
                                .collect::<Vec<_>>()
                                .join(" ");
                            if !prefix.is_empty() {
                                *counts.entry(prefix).or_insert(0) += 1;
                            }
                        }
                    }
                    bash_commands.push(info);
                }
            }
            _ => {}
        }
    }

    // Sort non-rewritable by frequency descending, then alphabetically for stability
    let mut non_rewritable_commands: Vec<(String, usize)> = non_rewritable_counts
        .map(|m| m.into_iter().collect())
        .unwrap_or_default();
    non_rewritable_commands.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    DiscoverAnalysis {
        total_invocations: invocations.len(),
        code_reads,
        bash_commands,
        total_read_tokens,
        potential_savings_tokens: potential_savings,
        non_rewritable_commands,
    }
}

/// Classify a single Bash invocation.
///
/// Returns `None` when the command should be skipped entirely (already a `skim`
/// invocation), otherwise returns the classified [`BashCommandInfo`].
///
/// The caller is responsible for any debug-mode accumulation (e.g. collecting
/// non-rewritable prefix counts); this function only classifies.
fn classify_bash_command(command: &str) -> Option<BashCommandInfo> {
    // Skip commands already rewritten by the hook (start with "skim ")
    if command.starts_with("skim ") {
        return None;
    }

    // `classify_command` is the single source of truth for rewrite eligibility.
    // Using the tri-state API (AD-2) means AlreadyCompact commands (e.g.,
    // `git worktree list`) are treated as "has_rewrite = true" — discover stops
    // flagging them as gaps even though no handler rewrites them.
    //
    // Single destructure: extract both `has_rewrite` and `rewrite_target` in
    // one `match` arm rather than a `!matches!` check followed by a second
    // `match` on the same value (complexity-6).
    let (has_rewrite, rewrite_target) = match super::rewrite::classify_command(command) {
        super::rewrite::CommandClassification::Rewritten(s) => (true, Some(s)),
        super::rewrite::CommandClassification::AlreadyCompact => (true, None),
        super::rewrite::CommandClassification::Unhandled => (false, None),
    };

    Some(BashCommandInfo {
        command: command.to_string(),
        has_rewrite,
        rewrite_target,
    })
}

/// Token estimation using the real tiktoken tokenizer.
///
/// Falls back to chars/4 approximation if tokenizer fails (should not
/// happen in practice since tiktoken-rs is robust).
fn estimate_tokens(content: &str) -> usize {
    crate::tokens::count_tokens(content).unwrap_or(content.len() / 4)
}

/// Check if a file path has a skim-supported code extension.
fn is_skimmable_file(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(rskim_core::Language::from_extension)
        .is_some()
}

// ============================================================================
// Output
// ============================================================================

fn print_text_report(analysis: &DiscoverAnalysis, config: &DiscoverConfig) {
    println!("skim discover -- optimization opportunities\n");
    println!(
        "Sessions scanned: found {} tool invocations",
        analysis.total_invocations
    );
    println!();

    print_code_reads_section(analysis);
    print_commands_section(analysis, config.debug);

    println!();
    println!("hint: run `skim init` to install the PreToolUse hook for automatic optimization");
}

fn print_code_reads_section(analysis: &DiscoverAnalysis) {
    let skimmable_count = analysis.code_reads.iter().filter(|r| r.could_skim).count();
    let non_skimmable_count = analysis.code_reads.iter().filter(|r| !r.could_skim).count();

    println!(
        "Code Reads: {} total ({} skimmable, {} non-code)",
        analysis.code_reads.len(),
        skimmable_count,
        non_skimmable_count,
    );

    if skimmable_count > 0 {
        println!("  Tokens consumed: {}", analysis.total_read_tokens);
        println!(
            "  Estimated savings with skim: ~{} tokens (~{:.0}%)",
            analysis.potential_savings_tokens,
            if analysis.total_read_tokens > 0 {
                (analysis.potential_savings_tokens as f64 / analysis.total_read_tokens as f64)
                    * 100.0
            } else {
                0.0
            },
        );
        println!();

        let mut sorted_reads: Vec<_> = analysis
            .code_reads
            .iter()
            .filter(|r| r.could_skim)
            .collect();
        sorted_reads.sort_by(|a, b| b.result_tokens.cmp(&a.result_tokens));
        println!("  Top files by token count:");
        for read in sorted_reads.iter().take(10) {
            println!("    {} ({} tokens)", read.file_path, read.result_tokens);
        }
    }
    println!();
}

fn print_commands_section(analysis: &DiscoverAnalysis, debug: bool) {
    let rewritable_count = analysis
        .bash_commands
        .iter()
        .filter(|c| c.has_rewrite)
        .count();
    println!(
        "Commands: {} total ({} rewritable)",
        analysis.bash_commands.len(),
        rewritable_count,
    );

    if rewritable_count > 0 {
        println!();
        println!("  Rewritable commands:");
        // Deduplicate by command prefix
        let mut seen = std::collections::HashSet::new();
        for cmd in analysis.bash_commands.iter().filter(|c| c.has_rewrite) {
            let prefix: String = cmd
                .command
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            if seen.insert(prefix.clone()) {
                println!(
                    "    {} -> {}",
                    prefix,
                    cmd.rewrite_target.as_deref().unwrap_or("skim equivalent"),
                );
            }
        }
    }

    print_debug_section(analysis, debug);
}

fn print_debug_section(analysis: &DiscoverAnalysis, debug: bool) {
    if debug && !analysis.non_rewritable_commands.is_empty() {
        println!();
        println!(
            "  Non-rewritable commands ({}):",
            analysis.non_rewritable_commands.len()
        );
        for (prefix, count) in &analysis.non_rewritable_commands {
            println!(
                "    {} ({} occurrence{})",
                prefix,
                count,
                if *count == 1 { "" } else { "s" }
            );
        }
    }
}

fn print_json_report(analysis: &DiscoverAnalysis, config: &DiscoverConfig) -> anyhow::Result<()> {
    let skimmable_reads: Vec<_> = analysis
        .code_reads
        .iter()
        .filter(|r| r.could_skim)
        .collect();
    let rewritable: Vec<_> = analysis
        .bash_commands
        .iter()
        .filter(|c| c.has_rewrite)
        .collect();

    let mut commands_obj = serde_json::json!({
        "total": analysis.bash_commands.len(),
        "rewritable": rewritable.len(),
        "rewrites": rewritable.iter().filter_map(|c| {
            c.rewrite_target.as_ref().map(|t| {
                serde_json::json!({
                    "original": c.command,
                    "target": t,
                })
            })
        }).collect::<Vec<_>>(),
    });

    // Include non-rewritable commands array when debug is enabled
    if config.debug && !analysis.non_rewritable_commands.is_empty() {
        let non_rewritable: Vec<_> = analysis
            .non_rewritable_commands
            .iter()
            .map(|(prefix, count)| {
                serde_json::json!({
                    "prefix": prefix,
                    "count": count,
                })
            })
            .collect();
        commands_obj["non_rewritable_commands"] = serde_json::json!(non_rewritable);
    }

    let report = serde_json::json!({
        "version": 1,
        "total_invocations": analysis.total_invocations,
        "code_reads": {
            "total": analysis.code_reads.len(),
            "skimmable": skimmable_reads.len(),
            "total_tokens": analysis.total_read_tokens,
            "potential_savings_tokens": analysis.potential_savings_tokens,
            "top_files": skimmable_reads.iter().take(10).map(|r| {
                serde_json::json!({
                    "path": r.file_path,
                    "tokens": r.result_tokens,
                })
            }).collect::<Vec<_>>(),
        },
        "commands": commands_obj,
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
    println!("skim discover");
    println!();
    println!("  Identify missed optimization opportunities in AI agent sessions");
    println!();
    println!("Usage: skim discover [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --since <duration>   Time window (e.g., 24h, 7d, 1w) [default: 24h]");
    println!("                       (24h default suits recent-session exploration;");
    println!("                        use --since 7d for broader analysis)");
    println!("  --session latest     Only scan the most recent session");
    println!("  --agent <name>       Only scan sessions from a specific agent");
    println!("  --json               Output machine-readable JSON");
    println!("  --debug              Show non-rewritable commands (also: SKIM_DEBUG=1)");
    println!("  --help, -h           Print help information");
    println!();
    println!("Supported agents:");
    println!("  claude-code          Claude Code (~/.claude/projects/)");
    println!();
    println!("Examples:");
    println!("  skim discover                      Last 24h, all detected agents");
    println!("  skim discover --since 7d           Last 7 days");
    println!("  skim discover --session latest      Most recent session only");
    println!("  skim discover --agent claude-code   Only Claude Code sessions");
    println!("  skim discover --json               Machine-readable output");
}

// ============================================================================
// Clap command for completions
// ============================================================================

/// Build the clap [`Command`] for `skim discover` (used for shell completions).
///
/// SYNC NOTE: This must remain in sync with [`parse_args()`] — any flag added
/// here must also be handled in `parse_args`, and vice versa.
pub(super) fn command() -> clap::Command {
    clap::Command::new("discover")
        .about("Identify missed optimization opportunities in AI agent sessions")
        .arg(
            clap::Arg::new("since")
                .long("since")
                .value_name("DURATION")
                .help("Time window (e.g., 24h, 7d, 1w) [default: 24h]"),
        )
        .arg(
            clap::Arg::new("session")
                .long("session")
                .value_name("VALUE")
                .help("Session filter (e.g., latest)"),
        )
        .arg(
            clap::Arg::new("agent")
                .long("agent")
                .value_name("NAME")
                .help("Only scan sessions from a specific agent"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Output machine-readable JSON"),
        )
        .arg(
            clap::Arg::new("debug")
                .long("debug")
                .action(clap::ArgAction::SetTrue)
                .help("Show non-rewritable commands (also: SKIM_DEBUG=1)"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::rewrite::would_rewrite;

    #[test]
    fn test_parse_duration_ago_days() {
        let result = parse_duration_ago("7d");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_duration_ago_hours() {
        let result = parse_duration_ago("24h");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_duration_ago_weeks() {
        let result = parse_duration_ago("1w");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_duration_ago_invalid() {
        assert!(parse_duration_ago("abc").is_err());
        assert!(parse_duration_ago("7x").is_err());
        assert!(parse_duration_ago("").is_err());
    }

    #[test]
    fn test_is_skimmable_file() {
        assert!(is_skimmable_file("/tmp/test.rs"));
        assert!(is_skimmable_file("/tmp/test.ts"));
        assert!(is_skimmable_file("/tmp/test.py"));
        assert!(is_skimmable_file("/tmp/test.go"));
        assert!(is_skimmable_file("/tmp/test.java"));
        assert!(is_skimmable_file("/tmp/test.json"));
        assert!(!is_skimmable_file("/tmp/test.txt"));
        assert!(!is_skimmable_file("/tmp/test"));
        assert!(!is_skimmable_file("/tmp/.env"));
    }

    /// Verify that `would_rewrite` (from the rewrite engine) correctly identifies
    /// rewritable commands — replaces the deleted `check_has_rewrite` heuristic.
    #[test]
    fn test_would_rewrite_basic() {
        // Rewritable commands
        assert!(would_rewrite("cargo test").is_some());
        assert!(would_rewrite("cargo clippy").is_some());
        assert!(would_rewrite("cargo build").is_some());
        assert!(would_rewrite("pytest").is_some());
        assert!(would_rewrite("go test ./...").is_some());
        assert!(would_rewrite("git status").is_some());
        assert!(would_rewrite("git diff").is_some());
        assert!(would_rewrite("tsc").is_some());
        assert!(would_rewrite("cat file.rs").is_some());
        // Non-rewritable commands
        assert!(would_rewrite("cat file.txt").is_none());
        assert!(would_rewrite("ls").is_none());
        assert!(would_rewrite("echo hello").is_none());
        assert!(would_rewrite("cargo run").is_none());
        // Already a skim command
        assert!(would_rewrite("skim test cargo").is_none());
    }

    #[test]
    fn test_would_rewrite_lint_and_infra_tools() {
        // AD-11: prettier --check and rustfmt --check are now acknowledged as
        // already-compact. would_rewrite returns None (not Rewritten) for them.
        assert!(
            would_rewrite("prettier --check .").is_none(),
            "prettier --check is now acknowledged, not rewritten"
        );
        assert!(
            would_rewrite("rustfmt --check src/main.rs").is_none(),
            "rustfmt --check is now acknowledged, not rewritten"
        );
        assert!(
            would_rewrite("npx prettier --check .").is_none(),
            "npx prettier --check is now acknowledged, not rewritten"
        );
        // infra tools (still rewritten)
        assert!(would_rewrite("gh pr list").is_some());
        assert!(would_rewrite("gh issue list").is_some());
        assert!(would_rewrite("gh run list").is_some());
        assert!(would_rewrite("gh release list").is_some());
        assert!(would_rewrite("aws s3 ls").is_some());
        assert!(would_rewrite("curl https://api.example.com").is_some());
        assert!(would_rewrite("wget https://example.com/file").is_some());
        // gh without a recognized subcommand should not match
        assert!(would_rewrite("gh auth login").is_none());
    }

    #[test]
    fn test_would_rewrite_targets() {
        assert_eq!(
            would_rewrite("cargo test"),
            Some("skim test cargo".to_string())
        );
        assert_eq!(
            would_rewrite("git status"),
            Some("skim git status".to_string())
        );
        assert_eq!(
            would_rewrite("cat file.rs"),
            Some("skim file.rs --mode=pseudo".to_string())
        );
        assert_eq!(would_rewrite("ls"), None);
    }

    #[test]
    fn test_would_rewrite_file_ops() {
        // find always matches
        assert!(would_rewrite("find . -name '*.rs'").is_some());
        // tree always matches
        assert!(would_rewrite("tree src/").is_some());
        // rg always matches
        assert!(would_rewrite("rg fn main src/").is_some());
        // grep -r/-rn matches; bare grep does not
        assert!(would_rewrite("grep -r TODO src/").is_some());
        assert!(would_rewrite("grep -rn TODO src/").is_some());
        assert!(would_rewrite("grep TODO file.rs").is_none());
        // ls -la/-R matches; bare ls does not
        assert!(would_rewrite("ls -la").is_some());
        assert!(would_rewrite("ls -R src/").is_some());
        assert!(would_rewrite("ls").is_none());
    }

    #[test]
    fn test_parse_args_defaults() {
        let config = parse_args(&[]).unwrap();
        assert!(config.since.is_some());
        assert!(!config.session_latest);
        assert!(config.agent_filter.is_none());
        assert!(!config.json_output);
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
    fn test_parse_args_unknown_agent() {
        let result = parse_args(&["--agent".to_string(), "nonexistent".to_string()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown agent"));
    }

    #[test]
    fn test_parse_args_unknown_flag() {
        let result = parse_args(&["--bogus".to_string()]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown flag"));
    }

    #[test]
    fn test_parse_args_session_latest() {
        let config = parse_args(&["--session".to_string(), "latest".to_string()]).unwrap();
        assert!(config.session_latest);
    }

    #[test]
    fn test_parse_args_since() {
        let config = parse_args(&["--since".to_string(), "7d".to_string()]).unwrap();
        assert!(config.since.is_some());
    }

    // ---- classify_bash_command: unit tests ----

    #[test]
    fn test_classify_bash_skips_skim_commands() {
        // Commands that start with "skim " are already rewritten — skip entirely.
        assert!(classify_bash_command("skim test cargo").is_none());
        assert!(classify_bash_command("skim build clippy").is_none());
        assert!(classify_bash_command("skim git status").is_none());
        assert!(classify_bash_command("skim file.rs").is_none());
    }

    #[test]
    fn test_classify_bash_rewritable_commands() {
        // Rewritable commands return Some with has_rewrite=true and a target.
        let info = classify_bash_command("cargo test").unwrap();
        assert!(info.has_rewrite);
        assert!(info.rewrite_target.is_some());
        assert_eq!(info.command, "cargo test");

        let info = classify_bash_command("git status").unwrap();
        assert!(info.has_rewrite);
        assert!(info.rewrite_target.is_some());

        let info = classify_bash_command("cat file.rs").unwrap();
        assert!(info.has_rewrite);
        assert!(info.rewrite_target.is_some());
    }

    #[test]
    fn test_classify_bash_non_rewritable_commands() {
        // Non-rewritable commands return Some with has_rewrite=false and no target.
        let info = classify_bash_command("ls").unwrap();
        assert!(!info.has_rewrite);
        assert!(info.rewrite_target.is_none());
        assert_eq!(info.command, "ls");

        let info = classify_bash_command("echo hello").unwrap();
        assert!(!info.has_rewrite);
        assert!(info.rewrite_target.is_none());

        let info = classify_bash_command("node server.js").unwrap();
        assert!(!info.has_rewrite);
        assert!(info.rewrite_target.is_none());
    }

    /// `AlreadyCompact` commands (acknowledged as near-optimal by skim) must be
    /// reported as `has_rewrite = true` with `rewrite_target = None` so that
    /// `discover` does not flag them as compression gaps (testing-7 / AD-2).
    #[test]
    fn test_classify_bash_already_compact_commands() {
        // `git worktree list` is acknowledged compact — output is already minimal.
        let info = classify_bash_command("git worktree list").unwrap();
        assert!(
            info.has_rewrite,
            "AlreadyCompact commands must report has_rewrite=true to suppress gap reporting"
        );
        assert!(
            info.rewrite_target.is_none(),
            "AlreadyCompact commands must have no rewrite_target (no replacement command)"
        );
        assert_eq!(info.command, "git worktree list");

        // Compound variant: AlreadyCompact && AlreadyCompact → Unhandled by
        // classify_command (mixed compound with non-ack second segment returns
        // Unhandled).  Verify the single-segment case is sufficient here.
        // A pure all-ack compound is AlreadyCompact and therefore has_rewrite=true.
        let compound_ack = classify_bash_command("git worktree list && git worktree list").unwrap();
        assert!(
            compound_ack.has_rewrite,
            "All-AlreadyCompact compound must also report has_rewrite=true"
        );
        assert!(compound_ack.rewrite_target.is_none());
    }

    // ---- analyze_invocations: skim command exclusion ----

    fn default_config() -> DiscoverConfig {
        DiscoverConfig {
            since: None,
            session_latest: false,
            agent_filter: None,
            json_output: false,
            debug: false,
        }
    }

    fn make_bash_invocation(command: &str) -> ToolInvocation {
        ToolInvocation {
            tool_name: "Bash".to_string(),
            input: ToolInput::Bash {
                command: command.to_string(),
            },
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            session_id: "sess1".to_string(),
            agent: AgentKind::ClaudeCode,
            result: Some(session::ToolResult {
                content: "output".to_string(),
                is_error: false,
            }),
        }
    }

    #[test]
    fn test_analyze_excludes_already_rewritten_commands() {
        // Commands starting with "skim " should NOT be counted as rewritable
        let inv1 = make_bash_invocation("skim test cargo --nocapture");
        let inv2 = make_bash_invocation("skim build clippy");
        let inv3 = make_bash_invocation("cargo test"); // this one IS rewritable
        let invocations = vec![inv1, inv2, inv3];

        let config = default_config();
        let analysis = analyze_invocations(&invocations, &config);

        // Only "cargo test" should be in bash_commands, not the skim commands
        assert_eq!(analysis.bash_commands.len(), 1);
        assert_eq!(analysis.bash_commands[0].command, "cargo test");
        assert!(analysis.bash_commands[0].has_rewrite);
    }

    #[test]
    fn test_analyze_counts_non_skim_commands() {
        let inv1 = make_bash_invocation("ls -la");
        let inv2 = make_bash_invocation("cargo test");
        let invocations = vec![inv1, inv2];

        let config = default_config();
        let analysis = analyze_invocations(&invocations, &config);

        assert_eq!(analysis.bash_commands.len(), 2);
    }

    #[test]
    fn test_analyze_collects_non_rewritable_when_debug() {
        // Use commands whose first 3 tokens are identical so they collapse into
        // a single deduplicated entry with count=2.
        let inv1 = make_bash_invocation("node server.js --port 3000");
        let inv2 = make_bash_invocation("node server.js --port 4000");
        let inv3 = make_bash_invocation("cargo run --bin myapp");
        let invocations = vec![inv1, inv2, inv3];

        // debug=false: non_rewritable_commands should be empty
        let analysis = analyze_invocations(&invocations, &default_config());
        assert!(analysis.non_rewritable_commands.is_empty());

        // debug=true: non_rewritable_commands should be populated
        let config_debug = DiscoverConfig {
            debug: true,
            ..default_config()
        };
        let analysis = analyze_invocations(&invocations, &config_debug);
        assert!(!analysis.non_rewritable_commands.is_empty());
        // "node server.js --port 3000" and "node server.js --port 4000" share
        // prefix "node server.js --port" (first 3 tokens)
        let node_entry = analysis
            .non_rewritable_commands
            .iter()
            .find(|(p, _)| p.starts_with("node server.js"));
        assert!(node_entry.is_some());
        assert_eq!(node_entry.unwrap().1, 2);
    }

    #[test]
    fn test_parse_args_debug() {
        let _guard = crate::debug::DebugTestGuard::acquire();
        // --debug flag sets debug=true
        let config = parse_args(&["--debug".to_string()]).unwrap();
        assert!(config.debug);
    }

    /// Sync test: verifies that `parse_args` and `command()` accept the same flags.
    ///
    /// If this test fails, a flag was added to one but not the other. Update both
    /// `parse_args` and `command` together.
    #[test]
    fn test_parse_args_and_command_are_in_sync() {
        let _guard = crate::debug::DebugTestGuard::acquire();
        // Build the clap command for validation
        let cmd = command();

        // Flags exercised: --since, --agent, --json, --debug, --session
        let all_args = [
            "--since",
            "7d",
            "--agent",
            "claude-code",
            "--json",
            "--debug",
            "--session",
            "latest",
        ];

        // clap must accept these flags without error
        cmd.clone()
            .try_get_matches_from(std::iter::once("discover").chain(all_args.iter().copied()))
            .expect("clap rejected flags that parse_args accepts — sync is broken");

        // parse_args must also accept these flags without error
        let string_args: Vec<String> = all_args.iter().map(|s| s.to_string()).collect();
        parse_args(&string_args)
            .expect("parse_args rejected flags that clap accepts — sync is broken");

        // Verify individual flag values agree between parse_args and clap
        let matches = cmd
            .try_get_matches_from(std::iter::once("discover").chain(all_args.iter().copied()))
            .unwrap();

        // --json: both must agree it is set
        assert!(matches.get_flag("json"), "clap should see --json as true");

        // --debug: both must agree it is set
        assert!(matches.get_flag("debug"), "clap should see --debug as true");

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

        // --session: clap must surface the value
        assert_eq!(
            matches.get_one::<String>("session").map(|s| s.as_str()),
            Some("latest"),
            "clap --session value should be 'latest'"
        );
    }
}
