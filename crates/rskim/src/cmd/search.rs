//! `skim search` — code search across indexed files (#3)
//!
//! Provides intelligent code search using the 3-layer search architecture
//! defined in rskim-search. Currently a stub; implementation arrives in Wave 1+.

use std::process::ExitCode;

/// Run the search subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Stub: search is not yet implemented
    eprintln!("skim search: not yet implemented");
    eprintln!("hint: this command will be available after index support lands (Wave 1+)");
    Ok(ExitCode::FAILURE)
}

/// Build clap command definition for shell completions.
pub(super) fn command() -> clap::Command {
    clap::Command::new("search")
        .about("Search code using the 3-layer index")
        .arg(
            clap::Arg::new("query")
                .help("Search query string")
                .value_name("QUERY"),
        )
        .arg(
            clap::Arg::new("build")
                .long("build")
                .action(clap::ArgAction::SetTrue)
                .help("Build the search index before querying"),
        )
        .arg(
            clap::Arg::new("rebuild")
                .long("rebuild")
                .action(clap::ArgAction::SetTrue)
                .help("Force rebuild the entire search index"),
        )
        .arg(
            clap::Arg::new("update")
                .long("update")
                .action(clap::ArgAction::SetTrue)
                .help("Update the search index incrementally"),
        )
        .arg(
            clap::Arg::new("ast")
                .long("ast")
                .value_name("PATTERN")
                .help("AST pattern to search for"),
        )
        .arg(
            clap::Arg::new("blast_radius")
                .long("blast-radius")
                .action(clap::ArgAction::SetTrue)
                .help("Filter results by blast radius (high-impact changes)"),
        )
        .arg(
            clap::Arg::new("limit")
                .long("limit")
                .value_name("N")
                .help("Maximum number of results to return"),
        )
        .arg(
            clap::Arg::new("hot")
                .long("hot")
                .action(clap::ArgAction::SetTrue)
                .help("Filter for recently active files"),
        )
        .arg(
            clap::Arg::new("cold")
                .long("cold")
                .action(clap::ArgAction::SetTrue)
                .help("Filter for stable/unchanged files"),
        )
        .arg(
            clap::Arg::new("risky")
                .long("risky")
                .action(clap::ArgAction::SetTrue)
                .help("Filter for files with high churn or complexity"),
        )
}

fn print_help() {
    // Delegate to clap's command definition — single source of truth for flags.
    let _ = command().name("skim search").print_help();
    println!();
}
