//! yarn package manager parser (#118)
//!
//! Parses `yarn install`, `yarn audit`, and `yarn outdated` output
//! using three-tier degradation: JSON/NDJSON -> regex -> passthrough.
//!
//! Supports yarn v1 classic only. yarn v2+ berry NDJSON differs and is a
//! documented limitation.

mod audit;
mod install;
mod outdated;

use std::process::ExitCode;

// Re-exported from parent so submodules can access via `super::`.
use super::{PkgSubcommandConfig, combine_output, run_pkg_subcommand};

/// Run `skim yarn <subcmd> [args...]`.
///
/// Sub-dispatches to install, audit, or outdated based on the first arg.
/// Unknown subcommands fall through to raw passthrough so `yarn build`,
/// `yarn run`, etc. are not interrupted.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Safe: args.is_empty() is handled above.
    let (subcmd, subcmd_args) = args.split_first().expect("already verified non-empty");

    match subcmd.as_str() {
        "install" | "add" | "remove" | "i" => {
            install::run_install(subcmd_args, show_stats, json_output, rec)
        }
        "audit" => audit::run_audit(subcmd_args, show_stats, json_output, rec),
        "outdated" => outdated::run_outdated(subcmd_args, show_stats, json_output, rec),
        other => {
            // Unknown subcommand → raw passthrough. Don't compress things we
            // don't understand (e.g., `yarn build`, `yarn run dev`).
            let safe = crate::cmd::sanitize_for_display(other);
            eprintln!("skim yarn: unknown subcommand '{safe}' — passing through to yarn");
            // Run the raw command without compression
            use crate::runner::CommandRunner;
            let mut all_args: Vec<String> = vec![other.to_string()];
            all_args.extend_from_slice(subcmd_args);
            let arg_refs: Vec<&str> = all_args.iter().map(String::as_str).collect();
            let runner = CommandRunner::new(Some(crate::cmd::DEFAULT_CMD_TIMEOUT));
            match runner.run_with_env("yarn", &arg_refs, &[]) {
                Ok(output) => {
                    print!("{}", output.stdout);
                    if !output.stderr.is_empty() {
                        eprint!("{}", output.stderr);
                    }
                    let code = output.exit_code.unwrap_or(1).clamp(0, 255) as u8;
                    Ok(ExitCode::from(code))
                }
                Err(e) => Err(e),
            }
        }
    }
}

fn print_help() {
    println!("skim yarn <subcmd> [args...]");
    println!();
    println!("  Parse yarn output for AI context windows.");
    println!();
    println!("Subcommands:");
    println!("  install    Parse yarn install output (also: add, remove)");
    println!("  audit      Parse yarn audit output");
    println!("  outdated   Parse yarn outdated output");
    println!();
    println!("Examples:");
    println!("  skim yarn install");
    println!("  skim yarn audit");
    println!("  skim yarn outdated");
    println!("  yarn install 2>&1 | skim yarn install");
}
