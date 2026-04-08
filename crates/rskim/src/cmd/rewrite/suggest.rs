//! Suggest mode output and clap Command definition.

use super::types::{RewriteCategory, SuggestOutput};

/// Print the suggest-mode JSON output.
pub(super) fn print_suggest(
    original: &str,
    result: Option<(&str, RewriteCategory)>,
    compound: bool,
) {
    let output = SuggestOutput {
        version: 1,
        is_match: result.is_some(),
        original,
        rewritten: result.map_or("", |(r, _)| r),
        category: result.map(|(_, c)| c),
        confidence: if result.is_some() { "exact" } else { "" },
        compound,
        skim_hook_version: env!("CARGO_PKG_VERSION"),
    };
    // Struct contains only primitive types (&str, u8, bool) — serialization cannot fail.
    let json = serde_json::to_string(&output)
        .expect("BUG: SuggestOutput serialization failed — struct contains only primitive types");
    println!("{json}");
}

/// Build the clap `Command` definition for the rewrite subcommand.
///
/// Used by `completions.rs` to generate accurate shell completions without
/// duplicating the argument definitions.
pub(crate) fn command() -> clap::Command {
    clap::Command::new("rewrite")
        .about("Rewrite common developer commands into skim equivalents")
        .arg(
            clap::Arg::new("suggest")
                .long("suggest")
                .action(clap::ArgAction::SetTrue)
                .help("Output JSON suggestion instead of plain text"),
        )
        .arg(
            clap::Arg::new("hook")
                .long("hook")
                .action(clap::ArgAction::SetTrue)
                .help("Run as agent PreToolUse hook (reads JSON from stdin)"),
        )
        .arg(
            clap::Arg::new("agent")
                .long("agent")
                .value_name("NAME")
                .help("Agent type for hook mode (e.g., claude-code, codex, gemini)"),
        )
        .arg(
            clap::Arg::new("command")
                .value_name("COMMAND")
                .num_args(1..)
                .help("Command to rewrite"),
        )
}

/// Print the help text for the rewrite subcommand.
pub(super) fn print_help() {
    println!("skim rewrite");
    println!();
    println!("  Rewrite common developer commands into skim equivalents");
    println!();
    println!("Usage: skim rewrite [--suggest] <COMMAND>...");
    println!("       echo \"cargo test\" | skim rewrite [--suggest]");
    println!("       skim rewrite --hook  (agent PreToolUse hook mode)");
    println!();
    println!("Options:");
    println!("  --suggest         Output JSON suggestion instead of plain text");
    println!("  --hook            Run as agent PreToolUse hook (reads JSON from stdin)");
    println!("  --agent <name>    Agent type for hook mode (default: claude-code)");
    println!("  --help, -h        Print help information");
    println!();
    println!("Examples:");
    println!("  skim rewrite cargo test -- --nocapture");
    println!("  skim rewrite git status");
    println!("  skim rewrite cat src/main.rs");
    println!("  echo \"pytest -v\" | skim rewrite --suggest");
    println!();
    println!("Hook mode:");
    println!("  Reads agent PreToolUse JSON from stdin, rewrites command if matched,");
    println!("  and emits agent-specific hook-protocol JSON (see --agent flag).");
    println!();
    println!("Exit codes:");
    println!("  0  Rewrite found (or --suggest/--hook mode)");
    println!("  1  No rewrite match");
}
