//! Test subcommand dispatcher (#46, #47, #48, #49)
//!
//! Routes `skim <runner> [args...]` to the appropriate test parser.
//! Currently supported runners: `cargo`, `go`, `vitest`, `jest`, `pytest`.

pub(crate) mod cargo;
pub(crate) mod go;
mod pytest;
mod shared;
pub(crate) mod vitest;

use std::process::ExitCode;

/// Known test runners that `skim test` can dispatch to.
const KNOWN_RUNNERS: &[&str] = &["cargo", "go", "vitest", "jest", "pytest"];

/// Entry point for `skim <runner> [args...]` (test runners).
///
/// If no runner is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the runner-specific handler.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);

    let Some((runner_name, runner_args)) = filtered_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let runner = runner_name.as_str();
    let rec = crate::analytics::RecordingContext {
        enabled: analytics.enabled,
        command_type: crate::analytics::CommandType::Test,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };

    match runner {
        "cargo" => cargo::run(runner_args, show_stats, rec),
        "go" => go::run(runner_args, show_stats, rec),
        "vitest" | "jest" => vitest::run(runner, runner_args, show_stats, rec),
        "pytest" => pytest::run(runner_args, show_stats, rec),
        _ => {
            let safe_runner = crate::cmd::sanitize_for_display(runner);
            eprintln!(
                "skim: unknown runner '{safe_runner}'\n\
                 Available runners: {}\n\
                 Run 'skim <runner> --help' for usage information",
                KNOWN_RUNNERS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim <runner> [args...]");
    println!();
    println!("  Run tests through a runner and parse the output.");
    println!();
    println!("Available runners:");
    for runner in KNOWN_RUNNERS {
        println!("  skim {runner}");
    }
    println!();
    println!("Examples:");
    println!("  skim cargo test                Run cargo test");
    println!("  skim go test ./...             Run all Go tests");
    println!("  skim vitest                    Run vitest");
    println!("  skim pytest                    Run pytest");
    println!("  cargo test 2>&1 | skim cargo test  Pipe cargo output");
}
