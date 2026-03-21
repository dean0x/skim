//! Test subcommand dispatcher (#49)
//!
//! Routes `skim test <runner> [args...]` to the appropriate test parser.
//! Currently supported runners: `go`.

pub(crate) mod go;

use std::process::ExitCode;

/// Known test runners that `skim test` can dispatch to.
const KNOWN_RUNNERS: &[&str] = &["go"];

/// Entry point for `skim test <runner> [args...]`.
///
/// If no runner is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the runner-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle help and empty args
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let runner = args[0].as_str();
    let runner_args = &args[1..];

    match runner {
        "go" => go::run(runner_args),
        _ => {
            eprintln!(
                "skim test: unknown runner '{runner}'\n\
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
    println!("  Run tests through a runner and parse the output.");
    println!();
    println!("Available runners:");
    for runner in KNOWN_RUNNERS {
        println!("  {runner}");
    }
    println!();
    println!("Examples:");
    println!("  skim test go ./...             Run all Go tests");
    println!("  skim test go -run TestFoo      Run specific Go test");
    println!("  skim test go -v ./pkg/...      Verbose Go tests");
}
