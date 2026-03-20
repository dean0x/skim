//! Shell completion generation (#63)
//!
//! Generates shell completion scripts for bash, zsh, fish, powershell, and elvish
//! using clap_complete. Builds a synthetic Command that includes all Args flags
//! and known subcommands for accurate completions.

use std::io;
use std::process::ExitCode;

use clap::{value_parser, Arg, Command};
use clap_complete::Shell;

/// Run the `completions` subcommand.
///
/// Parses a shell name from `args`, builds a full synthetic command that
/// includes all file-operation flags and known subcommands, then generates
/// the completion script to stdout.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle --help / -h (matches existing dispatch pattern)
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Require shell argument
    let shell_name = args.first().ok_or_else(|| {
        anyhow::anyhow!(
            "missing required argument: <SHELL>\n\n\
             Usage: skim completions <SHELL>\n\n\
             Supported shells: bash, zsh, fish, powershell, elvish"
        )
    })?;

    // Parse shell name via FromStr (Shell implements it)
    let shell: Shell = shell_name.parse().map_err(|_| {
        anyhow::anyhow!(
            "unknown shell: '{shell_name}'\n\n\
             Supported shells: bash, zsh, fish, powershell, elvish"
        )
    })?;

    let mut cmd = build_full_command();
    clap_complete::generate(shell, &mut cmd, "skim", &mut io::stdout());

    Ok(ExitCode::SUCCESS)
}

/// Build a synthetic `Command` that merges file-operation flags with
/// known subcommands for accurate shell completions.
///
/// Starts with `file_operation_command()` (inherits all Args flags,
/// value enums, and aliases), then adds synthetic subcommands so tab
/// completion surfaces them.
fn build_full_command() -> Command {
    let mut cmd = crate::file_operation_command()
        .subcommand_required(false)
        .args_conflicts_with_subcommands(true);

    // Add the completions subcommand with a proper shell positional arg
    // so that tab-completing `skim completions <TAB>` lists shells.
    let completions_sub = Command::new("completions")
        .about("Generate shell completion scripts")
        .arg(
            Arg::new("shell")
                .value_parser(value_parser!(Shell))
                .help("Shell to generate completions for"),
        );
    cmd = cmd.subcommand(completions_sub);

    // Add stub subcommands for all OTHER known subcommands
    for name in super::KNOWN_SUBCOMMANDS {
        if *name == "completions" {
            continue; // already added with full arg definition above
        }
        cmd = cmd.subcommand(Command::new(*name).about("Planned subcommand"));
    }

    cmd
}

/// Print help text for the completions subcommand.
fn print_help() {
    println!("skim completions");
    println!();
    println!("  Generate shell completion scripts");
    println!();
    println!("Usage: skim completions <SHELL>");
    println!();
    println!("Supported shells: bash, zsh, fish, powershell, elvish");
    println!();
    println!("Installation:");
    println!("  bash:  skim completions bash > ~/.local/share/bash-completion/completions/skim");
    println!("  zsh:   skim completions zsh > ~/.zfunc/_skim");
    println!("  fish:  skim completions fish > ~/.config/fish/completions/skim.fish");
}
