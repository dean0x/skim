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
    let analysis = analyze_invocations(&all_invocations);

    if config.json_output {
        print_json_report(&analysis)?;
    } else {
        print_text_report(&analysis);
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
}

fn parse_args(args: &[String]) -> anyhow::Result<DiscoverConfig> {
    let mut config = DiscoverConfig {
        since: Some(std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600)),
        session_latest: false,
        agent_filter: None,
        json_output: false,
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
            other => {
                anyhow::bail!(
                    "unknown flag: '{other}'\n\nUsage: skim discover [--since <duration>] [--session latest] [--agent <name>] [--json]"
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

fn analyze_invocations(invocations: &[ToolInvocation]) -> DiscoverAnalysis {
    let mut code_reads = Vec::new();
    let mut bash_commands = Vec::new();
    let mut total_read_tokens = 0usize;
    let mut potential_savings = 0usize;

    for inv in invocations {
        match &inv.input {
            ToolInput::Read { file_path } => {
                let result_content = inv
                    .result
                    .as_ref()
                    .map(|r| r.content.as_str())
                    .unwrap_or("");
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
                // Skip commands already rewritten by the hook (start with "skim ")
                if command.starts_with("skim ") {
                    continue;
                }

                // Check if this command has a skim rewrite
                let tokens: Vec<&str> = command.split_whitespace().collect();
                let has_rewrite = check_has_rewrite(&tokens);
                let rewrite_target = if has_rewrite {
                    get_rewrite_target(&tokens)
                } else {
                    None
                };

                bash_commands.push(BashCommandInfo {
                    command: command.clone(),
                    has_rewrite,
                    rewrite_target,
                });
            }
            _ => {}
        }
    }

    DiscoverAnalysis {
        total_invocations: invocations.len(),
        code_reads,
        bash_commands,
        total_read_tokens,
        potential_savings_tokens: potential_savings,
    }
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

/// Check if a command would be rewritten by skim rewrite.
///
/// Simple prefix-based heuristic matching the same command families as
/// the rewrite engine's declarative rules. We use a heuristic rather
/// than importing `try_rewrite` directly to avoid tight coupling with
/// the rewrite module's internal types.
fn check_has_rewrite(tokens: &[&str]) -> bool {
    let cmd = tokens.first().copied().unwrap_or("");
    match cmd {
        "cargo" => matches!(
            tokens.get(1).copied(),
            Some("test" | "nextest" | "clippy" | "build")
        ),
        "pytest" | "python" | "python3" => true,
        "npx" => matches!(
            tokens.get(1).copied(),
            Some("vitest" | "jest" | "tsc" | "prettier")
        ),
        "vitest" | "jest" => true,
        "go" => tokens.get(1) == Some(&"test"),
        "git" => matches!(tokens.get(1).copied(), Some("status" | "diff" | "log")),
        "tsc" => true,
        "prettier" => true,
        "rustfmt" => true,
        "gh" => matches!(
            tokens.get(1).copied(),
            Some("pr" | "issue" | "run" | "release")
        ),
        "aws" => true,
        "curl" => true,
        "wget" => true,
        "cat" | "head" | "tail" => {
            // Only rewritable if operating on code files
            tokens
                .iter()
                .skip(1)
                .any(|t| !t.starts_with('-') && is_skimmable_file(t))
        }
        _ => false,
    }
}

/// Get the skim rewrite target description for a command.
fn get_rewrite_target(tokens: &[&str]) -> Option<String> {
    if tokens.is_empty() {
        return None;
    }
    match tokens[0] {
        "cargo" if matches!(tokens.get(1), Some(&"test") | Some(&"nextest")) => {
            Some("skim test cargo".to_string())
        }
        "cargo" if tokens.get(1) == Some(&"clippy") => Some("skim build clippy".to_string()),
        "cargo" if tokens.get(1) == Some(&"build") => Some("skim build cargo".to_string()),
        "pytest" | "python" | "python3" => Some("skim test pytest".to_string()),
        "vitest" | "jest" => Some("skim test vitest".to_string()),
        "npx" if tokens.get(1) == Some(&"vitest") || tokens.get(1) == Some(&"jest") => {
            Some("skim test vitest".to_string())
        }
        "npx" if tokens.get(1) == Some(&"tsc") => Some("skim build tsc".to_string()),
        "npx" if tokens.get(1) == Some(&"prettier") => Some("skim lint prettier".to_string()),
        "go" if tokens.get(1) == Some(&"test") => Some("skim test go".to_string()),
        "git" => Some(format!("skim git {}", tokens.get(1).unwrap_or(&""))),
        "tsc" => Some("skim build tsc".to_string()),
        "prettier" => Some("skim lint prettier".to_string()),
        "rustfmt" => Some("skim lint rustfmt".to_string()),
        "gh" => Some(format!(
            "skim infra gh {}",
            tokens.get(1).unwrap_or(&"")
        )),
        "aws" => Some("skim infra aws".to_string()),
        "curl" => Some("skim infra curl".to_string()),
        "wget" => Some("skim infra wget".to_string()),
        "cat" | "head" | "tail" => Some("skim <file> --mode=pseudo".to_string()),
        _ => None,
    }
}

// ============================================================================
// Output
// ============================================================================

fn print_text_report(analysis: &DiscoverAnalysis) {
    println!("skim discover -- optimization opportunities\n");

    println!(
        "Sessions scanned: found {} tool invocations",
        analysis.total_invocations
    );
    println!();

    // Code reads summary
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

        // Top files by token count
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

    // Bash commands summary
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

    println!();
    println!("hint: run `skim init` to install the PreToolUse hook for automatic optimization");
}

fn print_json_report(analysis: &DiscoverAnalysis) -> anyhow::Result<()> {
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
        "commands": {
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
        },
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_check_has_rewrite() {
        assert!(check_has_rewrite(&["cargo", "test"]));
        assert!(check_has_rewrite(&["cargo", "clippy"]));
        assert!(check_has_rewrite(&["cargo", "build"]));
        assert!(check_has_rewrite(&["pytest"]));
        assert!(check_has_rewrite(&["go", "test", "./..."]));
        assert!(check_has_rewrite(&["git", "status"]));
        assert!(check_has_rewrite(&["git", "diff"]));
        assert!(check_has_rewrite(&["tsc"]));
        assert!(check_has_rewrite(&["cat", "file.rs"]));
        assert!(!check_has_rewrite(&["cat", "file.txt"]));
        assert!(!check_has_rewrite(&["ls"]));
        assert!(!check_has_rewrite(&["echo", "hello"]));
        assert!(!check_has_rewrite(&["cargo", "run"]));
    }

    #[test]
    fn test_check_has_rewrite_lint_and_infra_tools() {
        // lint tools
        assert!(check_has_rewrite(&["prettier", "--check", "."]));
        assert!(check_has_rewrite(&["rustfmt", "--check", "src/main.rs"]));
        assert!(check_has_rewrite(&["npx", "prettier", "--check", "."]));
        // infra tools
        assert!(check_has_rewrite(&["gh", "pr", "list"]));
        assert!(check_has_rewrite(&["gh", "issue", "list"]));
        assert!(check_has_rewrite(&["gh", "run", "list"]));
        assert!(check_has_rewrite(&["gh", "release", "list"]));
        assert!(check_has_rewrite(&["aws", "s3", "ls"]));
        assert!(check_has_rewrite(&["curl", "https://api.example.com"]));
        assert!(check_has_rewrite(&["wget", "https://example.com/file"]));
        // gh without a recognized subcommand should not match
        assert!(!check_has_rewrite(&["gh", "auth", "login"]));
    }

    #[test]
    fn test_get_rewrite_target() {
        assert_eq!(
            get_rewrite_target(&["cargo", "test"]),
            Some("skim test cargo".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["git", "status"]),
            Some("skim git status".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["cat", "file.rs"]),
            Some("skim <file> --mode=pseudo".to_string())
        );
        assert_eq!(get_rewrite_target(&["ls"]), None);
    }

    #[test]
    fn test_get_rewrite_target_lint_and_infra_tools() {
        assert_eq!(
            get_rewrite_target(&["prettier"]),
            Some("skim lint prettier".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["rustfmt"]),
            Some("skim lint rustfmt".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["npx", "prettier"]),
            Some("skim lint prettier".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["gh", "pr"]),
            Some("skim infra gh pr".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["aws"]),
            Some("skim infra aws".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["curl"]),
            Some("skim infra curl".to_string())
        );
        assert_eq!(
            get_rewrite_target(&["wget"]),
            Some("skim infra wget".to_string())
        );
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

    // ---- analyze_invocations: skim command exclusion ----

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

        let analysis = analyze_invocations(&invocations);

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

        let analysis = analyze_invocations(&invocations);

        assert_eq!(analysis.bash_commands.len(), 2);
    }
}
