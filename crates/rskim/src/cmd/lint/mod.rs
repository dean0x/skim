//! Lint subcommand dispatcher (#104)
//!
//! Routes `skim lint <linter> [args...]` to the appropriate linter parser.
//! Currently supported linters: `eslint`, `ruff`, `mypy`, `golangci`.

pub(crate) mod eslint;
pub(crate) mod golangci;
pub(crate) mod mypy;
pub(crate) mod ruff;

use std::collections::BTreeMap;
use std::process::ExitCode;

use crate::output::canonical::{LintGroup, LintIssue, LintResult, LintSeverity};

/// Known linters that `skim lint` can dispatch to.
const KNOWN_LINTERS: &[&str] = &["eslint", "ruff", "mypy", "golangci"];

/// Entry point for `skim lint <linter> [args...]`.
///
/// If no linter is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the linter-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);

    // Extract --json flag
    let (filtered_args, json_output) = extract_json_flag(&filtered_args);

    let Some((linter_name, linter_args)) = filtered_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let linter = linter_name.as_str();

    match linter {
        "eslint" => eslint::run(linter_args, show_stats, json_output),
        "ruff" => ruff::run(linter_args, show_stats, json_output),
        "mypy" => mypy::run(linter_args, show_stats, json_output),
        "golangci" => golangci::run(linter_args, show_stats, json_output),
        _ => {
            eprintln!(
                "skim lint: unknown linter '{linter}'\n\
                 Available linters: {}\n\
                 Run 'skim lint --help' for usage information",
                KNOWN_LINTERS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim lint <linter> [args...]");
    println!();
    println!("  Run linters and parse the output for AI context windows.");
    println!();
    println!("Available linters:");
    for linter in KNOWN_LINTERS {
        println!("  {linter}");
    }
    println!();
    println!("Flags:");
    println!("  --json          Emit structured JSON output");
    println!("  --show-stats    Show token statistics");
    println!();
    println!("Examples:");
    println!("  skim lint eslint .             Run eslint");
    println!("  skim lint ruff check .         Run ruff check");
    println!("  skim lint mypy src/            Run mypy");
    println!("  skim lint golangci run ./...   Run golangci-lint");
    println!("  eslint . 2>&1 | skim lint eslint  Pipe eslint output");
}

/// Extract `--json` flag from args, returning (filtered_args, json_output).
fn extract_json_flag(args: &[String]) -> (Vec<String>, bool) {
    let json_output = args.iter().any(|a| a == "--json");
    let filtered: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect();
    (filtered, json_output)
}

/// Group individual lint issues by rule into a `LintResult`.
///
/// Uses `BTreeMap` for deterministic ordering of rule groups.
pub(crate) fn group_issues(tool: &str, issues: Vec<LintIssue>) -> LintResult {
    let mut groups: BTreeMap<String, LintGroup> = BTreeMap::new();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for issue in &issues {
        match issue.severity {
            LintSeverity::Error => errors += 1,
            LintSeverity::Warning => warnings += 1,
            LintSeverity::Info => {}
        }
        let group = groups
            .entry(issue.rule.clone())
            .or_insert_with(|| LintGroup {
                rule: issue.rule.clone(),
                count: 0,
                severity: issue.severity.clone(),
                locations: Vec::new(),
            });
        group.count += 1;
        group.locations
            .push(format!("{}:{}", issue.file, issue.line));
    }
    LintResult::new(
        tool.to_string(),
        errors,
        warnings,
        groups.into_values().collect(),
    )
}
