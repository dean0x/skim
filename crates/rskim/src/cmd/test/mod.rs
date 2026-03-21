//! Test subcommand dispatcher (`skim test <runner> [args...]`).
//!
//! Routes to runner-specific parsers (cargo, pytest, jest, etc.) based on the
//! first positional argument. Each runner module implements the three-tier
//! parse degradation pattern.

mod cargo;

use std::process::ExitCode;

/// Known test runners that `skim test` can dispatch to.
const KNOWN_RUNNERS: &[&str] = &["cargo"];

/// Dispatch `skim test <runner> [args...]`.
///
/// If no runner is specified or `--help` / `-h` is passed, prints usage.
pub(crate) fn dispatch(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle --help / -h
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let runner = args[0].as_str();
    let runner_args = &args[1..];

    match runner {
        "cargo" => cargo::run(runner_args),
        _ => {
            eprintln!(
                "skim test: unknown runner '{runner}'\n\n\
                 Available runners: {}\n\
                 Run 'skim test --help' for usage information",
                KNOWN_RUNNERS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim test <runner> [args...]");
    println!();
    println!("  Run tests through a runner and parse the output into a compact summary.");
    println!();
    println!("  Available runners:");
    for runner in KNOWN_RUNNERS {
        println!("    {runner}");
    }
    println!();
    println!("  Examples:");
    println!("    skim test cargo                  # Run cargo test with structured output");
    println!("    skim test cargo -- --lib -q       # Pass flags to cargo test");
    println!("    cargo test 2>&1 | skim test cargo # Pipe existing output");
}
