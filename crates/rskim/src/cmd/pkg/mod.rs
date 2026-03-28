//! Package manager output compression (#105)
//!
//! Routes `skim pkg <tool> [subcmd] [args...]` to the appropriate package
//! manager parser. Currently supported tools: `npm`, `pnpm`, `pip`, `cargo`.

mod cargo;
mod npm;
mod pip;
mod pnpm;

use std::process::ExitCode;

/// Known package manager tools that `skim pkg` can dispatch to.
const KNOWN_TOOLS: &[&str] = &["npm", "pnpm", "pip", "cargo"];

/// Entry point for `skim pkg <tool> [subcmd] [args...]`.
///
/// If no tool is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the tool-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);
    let (json_args, json_output) = extract_json_flag(&filtered_args);

    let Some((tool_name, tool_args)) = json_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let tool = tool_name.as_str();

    match tool {
        "npm" => npm::run(tool_args, show_stats, json_output),
        "pnpm" => pnpm::run(tool_args, show_stats, json_output),
        "pip" => pip::run(tool_args, show_stats, json_output),
        "cargo" => cargo::run(tool_args, show_stats, json_output),
        _ => {
            eprintln!(
                "skim pkg: unknown tool '{tool}'\n\
                 Available tools: {}\n\
                 Run 'skim pkg --help' for usage information",
                KNOWN_TOOLS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Extract the `--json` flag from args, returning filtered args and whether
/// the flag was present.
fn extract_json_flag(args: &[String]) -> (Vec<String>, bool) {
    let json_output = args.iter().any(|a| a == "--json");
    let filtered: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect();
    (filtered, json_output)
}

fn print_help() {
    println!("skim pkg <tool> [subcmd] [args...]");
    println!();
    println!("  Parse package manager output for AI context windows.");
    println!();
    println!("Available tools:");
    for tool in KNOWN_TOOLS {
        println!("  {tool}");
    }
    println!();
    println!("Examples:");
    println!("  skim pkg npm install              Run npm install");
    println!("  skim pkg npm audit                Run npm audit");
    println!("  skim pkg npm outdated             Run npm outdated");
    println!("  skim pkg pip install flask        Run pip install flask");
    println!("  skim pkg pip check                Run pip check");
    println!("  skim pkg cargo audit              Run cargo audit");
    println!("  skim pkg pnpm install             Run pnpm install");
    println!("  npm install 2>&1 | skim pkg npm install  Pipe npm output");
}
