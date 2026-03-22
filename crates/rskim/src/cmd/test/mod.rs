//! Test subcommand dispatcher (#48, #49)
//!
//! Routes `skim test <runner> [args...]` to the appropriate test parser.
//! Currently supported runners: `go`, `vitest`, `jest`.

pub(crate) mod go;
pub(crate) mod vitest;

use std::process::ExitCode;

/// Known test runners that `skim test` can dispatch to.
const KNOWN_RUNNERS: &[&str] = &["go", "vitest", "jest"];

/// Entry point for `skim test <runner> [args...]`.
///
/// If no runner is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the runner-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let runner = args[0].as_str();
    let runner_args = &args[1..];

    match runner {
        "go" => go::run(runner_args),
        "vitest" | "jest" => vitest::run(runner, runner_args),
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
    println!("  skim test vitest               Run vitest");
    println!("  skim test vitest --run math    Run specific vitest");
    println!("  skim test jest                 Run jest");
    println!("  jest --json | skim test jest   Pipe jest output");
}
