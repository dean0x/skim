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

#[cfg(test)]
mod tests {
    use super::super::types::{RewriteCategory, SuggestOutput};
    use super::command;

    // ========================================================================
    // command() — clap Command definition
    // ========================================================================

    #[test]
    fn command_name_is_rewrite() {
        let cmd = command();
        assert_eq!(cmd.get_name(), "rewrite");
    }

    #[test]
    fn command_has_suggest_flag() {
        let cmd = command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "suggest");
        assert!(arg.is_some(), "Expected --suggest flag in command definition");
    }

    #[test]
    fn command_has_hook_flag() {
        let cmd = command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "hook");
        assert!(arg.is_some(), "Expected --hook flag in command definition");
    }

    #[test]
    fn command_has_agent_option() {
        let cmd = command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "agent");
        assert!(arg.is_some(), "Expected --agent option in command definition");
    }

    #[test]
    fn command_has_command_positional() {
        let cmd = command();
        let arg = cmd.get_arguments().find(|a| a.get_id() == "command");
        assert!(
            arg.is_some(),
            "Expected COMMAND positional arg in command definition"
        );
    }

    // ========================================================================
    // SuggestOutput JSON serialization — must never panic
    // ========================================================================

    #[test]
    fn suggest_output_serialization_match() {
        let output = SuggestOutput {
            version: 1,
            is_match: true,
            original: "cargo test",
            rewritten: "skim test cargo",
            category: Some(RewriteCategory::Test),
            confidence: "exact",
            compound: false,
            skim_hook_version: "0.0.0",
        };
        let json = serde_json::to_string(&output).expect("serialization must not fail");
        assert!(json.contains("\"match\":true"));
        assert!(json.contains("\"original\":\"cargo test\""));
        assert!(json.contains("\"rewritten\":\"skim test cargo\""));
        assert!(json.contains("\"category\":\"test\""));
        assert!(json.contains("\"confidence\":\"exact\""));
        assert!(json.contains("\"compound\":false"));
    }

    #[test]
    fn suggest_output_serialization_no_match() {
        let output = SuggestOutput {
            version: 1,
            is_match: false,
            original: "python3 -c 'print(1)'",
            rewritten: "",
            category: None,
            confidence: "",
            compound: false,
            skim_hook_version: "0.0.0",
        };
        let json = serde_json::to_string(&output).expect("serialization must not fail for no-match");
        assert!(json.contains("\"match\":false"));
        // None category serializes as empty string via serialize_category
        assert!(json.contains("\"category\":\"\""));
        assert!(json.contains("\"confidence\":\"\""));
    }

    #[test]
    fn suggest_output_serialization_compound() {
        let output = SuggestOutput {
            version: 1,
            is_match: true,
            original: "cargo test && cargo build",
            rewritten: "skim test cargo && skim build cargo",
            category: Some(RewriteCategory::Test),
            confidence: "exact",
            compound: true,
            skim_hook_version: "0.0.0",
        };
        let json =
            serde_json::to_string(&output).expect("serialization must not fail for compound");
        assert!(json.contains("\"compound\":true"));
    }
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
