//! npm package manager parser (#105)
//!
//! Parses `npm install`, `npm audit`, `npm outdated`, and `npm ls` output
//! using three-tier degradation: JSON -> regex -> passthrough.
//!
//! npm 7+ JSON schemas are supported. If JSON fails to parse, tier 2 regex
//! is attempted on plain text output.

mod audit;
mod install;
mod ls;
mod outdated;

use std::process::ExitCode;

// Re-exported from parent so submodules can access via `super::`.
use super::{combine_output, run_pkg_subcommand, PkgSubcommandConfig};

/// Run `skim pkg npm <subcmd> [args...]`.
///
/// Sub-dispatches to install, audit, outdated, or ls based on the first arg.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Safe: args.is_empty() is handled above.
    let (subcmd, subcmd_args) = args.split_first().expect("already verified non-empty");

    match subcmd.as_str() {
        "install" | "i" | "ci" => install::run_install(subcmd_args, show_stats, json_output),
        "audit" => audit::run_audit(subcmd_args, show_stats, json_output),
        "outdated" => outdated::run_outdated(subcmd_args, show_stats, json_output),
        "ls" | "list" => ls::run_ls(subcmd_args, show_stats, json_output),
        other => {
            let safe = crate::cmd::sanitize_for_display(other);
            eprintln!(
                "skim pkg npm: unknown subcommand '{safe}'\n\
                 Available: install, audit, outdated, ls\n\
                 Run 'skim pkg npm --help' for usage"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim pkg npm <subcmd> [args...]");
    println!();
    println!("  Parse npm output for AI context windows.");
    println!();
    println!("Subcommands:");
    println!("  install    Parse npm install output");
    println!("  audit      Parse npm audit output");
    println!("  outdated   Parse npm outdated output");
    println!("  ls         Parse npm ls output");
    println!();
    println!("Examples:");
    println!("  skim pkg npm install");
    println!("  skim pkg npm audit");
    println!("  skim pkg npm outdated");
    println!("  skim pkg npm ls");
    println!("  npm install 2>&1 | skim pkg npm install");
}
