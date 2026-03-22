//! Subcommand infrastructure for skim CLI.
//!
//! Provides pre-parse routing for optional subcommands while keeping
//! backward compatibility with file-first invocations. Each subcommand
//! is currently a stub that will be implemented in later Phase B tickets.

mod completions;
mod git;
mod rewrite;

use std::process::ExitCode;

/// Known subcommands that the pre-parse router will recognize.
///
/// IMPORTANT: Only register subcommands we will actually implement.
/// Keep this list exact — no broad patterns. See GRANITE lesson #336.
pub(crate) const KNOWN_SUBCOMMANDS: &[&str] =
    &["build", "completions", "git", "init", "rewrite", "test"];

/// Check whether `name` is a registered subcommand.
pub(crate) fn is_known_subcommand(name: &str) -> bool {
    KNOWN_SUBCOMMANDS.contains(&name)
}

// Phase B extensibility: when subcommands are implemented, introduce a
// `SubcommandHandler` trait here. Each handler should receive raw remaining
// args as `&[String]` (not pre-parsed) so it can do its own parsing — this
// avoids the class of rewrite-layer bugs found in GRANITE's arg handling.

/// Dispatch a subcommand by name. Returns the process exit code.
///
/// Exit code semantics (GRANITE lesson — exit code corruption is P1):
/// - `--help` / `-h`: prints description to stdout, returns SUCCESS
/// - Otherwise: prints "not yet implemented" to stderr, returns FAILURE
pub(crate) fn dispatch(subcommand: &str, args: &[String]) -> anyhow::Result<ExitCode> {
    if !is_known_subcommand(subcommand) {
        anyhow::bail!(
            "Unknown subcommand: '{subcommand}'\n\
             Available subcommands: {}\n\
             Run 'skim --help' for usage information",
            KNOWN_SUBCOMMANDS.join(", ")
        );
    }

    // Dispatch implemented subcommands
    match subcommand {
        "completions" => return completions::run(args),
        "git" => return git::run(args),
        "rewrite" => return rewrite::run(args),
        _ => {}
    }

    // Check for --help / -h in remaining args
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        println!("skim {subcommand}");
        println!();
        println!("  Status: not yet implemented");
        println!();
        println!("  This subcommand is planned for a future release.");
        println!("  See: https://github.com/dean0x/skim/issues/19");
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("skim {subcommand}: not yet implemented");
    eprintln!();
    eprintln!("This subcommand is planned for a future release.");
    eprintln!("See: https://github.com/dean0x/skim/issues/19");

    Ok(ExitCode::FAILURE)
}
