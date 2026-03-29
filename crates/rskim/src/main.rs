//! skim CLI - Command-line interface for rskim-core
//!
//! ARCHITECTURE: Thin I/O layer over rskim-core library.
//! This binary handles:
//! - File I/O (reading from disk/stdin)
//! - CLI argument parsing (clap)
//! - Output formatting (stdout/stderr)
//! - Process exit codes
//! - Multi-file glob pattern matching
//! - File-based caching with mtime invalidation

mod analytics;
mod cache;
mod cascade;
mod cmd;
mod multi;
mod output;
mod process;
mod runner;
mod tokens;

use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

use rskim_core::{Language, Mode};

// ============================================================================
// Pre-parse routing (subcommand disambiguation)
// ============================================================================

/// Resolved invocation after pre-parse disambiguation.
enum Invocation {
    /// Classic file/directory/glob/stdin operation (existing behavior).
    FileOperation,
    /// A known subcommand with its remaining args.
    Subcommand { name: String, args: Vec<String> },
}

/// Returns true if `flag` is a flag that consumes the next token as its value.
///
/// SYNC NOTE: If you add a new flag with a value to `Args`, add it here too.
/// Failure to sync only causes a bug if the flag's value happens to match a
/// known subcommand name AND no file with that name exists on disk.
fn is_flag_with_value(flag: &str) -> bool {
    matches!(
        flag,
        "--mode"
            | "-m"
            | "--language"
            | "-l"
            | "--lang"
            | "--filename"
            | "--jobs"
            | "-j"
            | "--max-lines"
            | "--last-lines"
            | "--tokens"
            | "--since"
            | "--session"
            | "--agent"
            | "--format"
    )
}

/// Returns true if `token` looks like a file path, directory, or glob pattern
/// rather than a subcommand name.
///
/// Heuristics (any match means file-like):
/// - Contains `.` (file extension)
/// - Contains `/` or `\` (path separator)
/// - Is `-` (stdin)
/// - Contains `*`, `?`, `[`, or `{` (glob metacharacter via [`multi::GLOB_METACHARACTERS`])
fn looks_like_file_or_glob(token: &str) -> bool {
    token == "-" || token.contains(['.', '/', '\\']) || token.contains(multi::GLOB_METACHARACTERS)
}

/// Pre-parse `std::env::args()` to decide whether to route to a subcommand
/// or fall through to the existing file operation path.
///
/// Disambiguation rules (priority-ordered, first match wins):
///
/// | Condition                                    | Route         |
/// |----------------------------------------------|---------------|
/// | No positional arg found                      | FileOperation |
/// | `--` appears before first positional          | FileOperation |
/// | Contains `.`                                  | FileOperation |
/// | Contains `/` or `\`                           | FileOperation |
/// | Is `-`                                        | FileOperation |
/// | Contains `*`, `?`, `[`, or `{`                  | FileOperation |
/// | Is known subcommand AND no file/dir on disk   | Subcommand    |
/// | Everything else                               | FileOperation |
fn resolve_invocation() -> Invocation {
    let raw_args: Vec<String> = std::env::args().collect();
    // Skip argv[0] (the binary name)
    let args = &raw_args[1..];

    let mut first_positional: Option<(usize, &str)> = None;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        // CRITICAL: `--` must be checked before `starts_with('-')`.
        // Without this, `skim -- test` would skip `--`, find `test`,
        // and incorrectly route to Subcommand.
        if arg == "--" {
            return Invocation::FileOperation;
        }

        if arg.starts_with('-') {
            // Check for `--flag=value` (value embedded in same token — skip nothing)
            if arg.contains('=') {
                i += 1;
                continue;
            }
            // Check if this flag consumes the next token
            if is_flag_with_value(arg) {
                i += 2; // skip flag + its value
                continue;
            }
            // Boolean flag — skip it
            i += 1;
            continue;
        }

        // Found a positional argument
        first_positional = Some((i, arg));
        break;
    }

    let Some((pos_idx, positional)) = first_positional else {
        return Invocation::FileOperation;
    };

    // File-like heuristics: if it looks like a file/path/glob, treat as file
    if looks_like_file_or_glob(positional) {
        return Invocation::FileOperation;
    }

    // Known subcommand check — only if no file/dir with that name exists on disk
    if cmd::is_known_subcommand(positional) {
        let path = std::path::Path::new(positional);
        if path.exists() {
            // On-disk file/dir takes precedence (backward compat)
            return Invocation::FileOperation;
        }

        let name = positional.to_string();
        let remaining_args: Vec<String> = args[pos_idx + 1..].to_vec();
        return Invocation::Subcommand {
            name,
            args: remaining_args,
        };
    }

    // Unknown word — fall through to FileOperation (clap handles errors)
    Invocation::FileOperation
}

/// Maximum number of parallel jobs (threads) to prevent resource exhaustion
const MAX_JOBS: usize = 128;

/// Maximum value for --max-lines to prevent unreasonable memory allocation
const MAX_MAX_LINES: usize = 1_000_000;

/// Maximum value for --tokens to prevent unreasonable values
const MAX_TOKEN_BUDGET: usize = 10_000_000;

/// skim - Smart code reader for AI agents
///
/// Transform source code by stripping implementation details while
/// preserving structure, signatures, and types.
#[derive(Parser, Debug)]
#[command(name = "skim")]
#[command(author, version, about, long_about = None)]
#[command(after_help = "EXAMPLES:\n  \
    skim file.ts                             Read TypeScript with structure mode (cached)\n  \
    skim file.py --mode signatures           Extract Python signatures\n  \
    skim file.rs | bat -l rust               Skim Rust and highlight\n  \
    cat code.ts | skim - --lang=ts           Read from stdin with --lang alias\n  \
    skim - -l python < script.py             Short form language flag\n  \
    skim - --filename=main.rs < main.rs      Detect language from filename hint\n  \
    skim src/                                Process all files in directory recursively\n  \
    skim 'src/**/*.ts'                       Process all TypeScript files (glob pattern)\n  \
    skim '*.{js,ts}' --no-header             Process multiple files without headers\n  \
    skim . --jobs 8                          Process current directory with 8 threads\n  \
    skim file.ts --no-cache                  Disable caching for pure transformation\n  \
    skim --clear-cache                       Clear all cached files\n\n\
SUBCOMMANDS:\n  \
    build                                    Build with output parsing\n  \
    completions <SHELL>                      Generate shell completions (bash, zsh, fish, ...)\n  \
    discover                                 Identify missed optimization opportunities\n  \
    git                                      Git integration helpers\n  \
    init                                     Initialize skim configuration\n  \
    learn                                    Detect CLI error patterns and generate correction rules\n  \
    rewrite [--suggest] <COMMAND>...          Rewrite commands into skim equivalents\n  \
    stats [--since N] [--format json]        Show token analytics dashboard\n  \
    test                                     Run test with output parsing\n\n\
For more info: https://github.com/dean0x/skim")]
struct Args {
    /// File, directory, or glob pattern to process (use '-' for stdin)
    #[arg(value_name = "FILE", required_unless_present = "clear_cache")]
    file: Option<String>,

    /// Transformation mode
    #[arg(short, long, value_enum, default_value = "structure")]
    #[arg(help = "Transformation mode: structure, signatures, types, full, minimal, or pseudo")]
    mode: ModeArg,

    /// Override language detection (required for stdin unless --filename is given)
    #[arg(short, long, alias = "lang", value_enum)]
    #[arg(
        help = "Programming language: typescript, javascript, python, rust, go, java, c, cpp, csharp, ruby, sql, markdown, json, yaml, toml (or use --filename for auto-detection from stdin)"
    )]
    language: Option<LanguageArg>,

    /// Filename hint for language detection when reading from stdin
    #[arg(long, value_name = "NAME")]
    #[arg(help = "Filename hint for stdin language detection (e.g., main.rs)")]
    filename: Option<String>,

    /// Deprecated: accepted for backward compatibility but has no effect.
    ///
    /// This flag was dead code (never referenced in logic) and will be
    /// removed in a future major version. Hidden from --help output.
    #[arg(long, hide = true)]
    _force: bool,

    /// Disable file headers when processing multiple files
    #[arg(long, help = "Don't print file path headers for multi-file output")]
    no_header: bool,

    /// Number of parallel jobs (default: number of CPUs)
    #[arg(
        short,
        long,
        help = "Number of parallel jobs for multi-file processing"
    )]
    jobs: Option<usize>,

    /// Don't respect .gitignore rules when scanning directories or globs.
    /// Also includes hidden files and directories (dotfiles) that are excluded by default.
    #[arg(
        long,
        help = "Don't respect .gitignore rules (include all files, including hidden/dotfiles)"
    )]
    no_ignore: bool,

    /// Disable caching (caching is enabled by default for performance)
    #[arg(long, help = "Disable caching of transformed output")]
    no_cache: bool,

    /// Clear the entire cache directory (~/.cache/skim/)
    #[arg(long, help = "Clear all cached files and exit")]
    clear_cache: bool,

    /// Show token count statistics (output to stderr)
    #[arg(long, help = "Show token reduction statistics")]
    show_stats: bool,

    /// Maximum output lines (AST-aware smart truncation)
    ///
    /// Truncates output to at most N lines using priority-based selection.
    /// Types and signatures are kept over imports, which are kept over bodies.
    /// Never cuts mid-signature or mid-type-definition.
    #[arg(
        long,
        value_name = "N",
        help = "Truncate output to at most N lines (AST-aware)"
    )]
    max_lines: Option<usize>,

    /// Keep only the last N lines of output
    ///
    /// Keeps the last N lines of output, prepending a language-appropriate
    /// truncation marker indicating how many lines were omitted above.
    /// Mutually exclusive with --max-lines.
    #[arg(long, value_name = "N", help = "Keep only the last N lines of output")]
    last_lines: Option<usize>,

    /// Token budget - cascade through modes until output fits within N tokens
    ///
    /// Progressively applies more aggressive modes (full -> minimal -> structure
    /// -> signatures -> types) until the output fits within the specified token
    /// budget. If --mode is also specified, cascade starts at that mode.
    /// Final fallback: line-based truncation of the most aggressive mode's output.
    #[arg(
        long,
        value_name = "N",
        help = "Cascade through modes until output fits within N tokens"
    )]
    tokens: Option<usize>,

    /// Disable analytics recording for this invocation
    #[arg(long, help = "Disable analytics recording")]
    disable_analytics: bool,
}

/// Build the clap `Command` from `Args` for use by shell completion generation.
///
/// This exposes only the `Command`, not the `Args` struct itself. Used by
/// `cmd/completions.rs` to build a synthetic completion-aware command.
pub(crate) fn file_operation_command() -> clap::Command {
    <Args as clap::CommandFactory>::command()
}

/// Mode argument (clap value_enum wrapper)
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Structure,
    Signatures,
    Types,
    Full,
    Minimal,
    /// Pseudo mode — strips syntactic noise (types, visibility, decorators) while preserving logic
    Pseudo,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Structure => Mode::Structure,
            ModeArg::Signatures => Mode::Signatures,
            ModeArg::Types => Mode::Types,
            ModeArg::Full => Mode::Full,
            ModeArg::Minimal => Mode::Minimal,
            ModeArg::Pseudo => Mode::Pseudo,
        }
    }
}

/// Language argument (clap value_enum wrapper)
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum LanguageArg {
    #[value(name = "typescript", alias = "ts")]
    TypeScript,
    #[value(name = "javascript", alias = "js")]
    JavaScript,
    #[value(alias = "py")]
    Python,
    #[value(alias = "rs")]
    Rust,
    Go,
    Java,
    #[value(alias = "md")]
    Markdown,
    Json,
    #[value(alias = "yml")]
    Yaml,
    C,
    #[value(alias = "c++", alias = "cxx")]
    Cpp,
    Toml,
    #[value(name = "csharp", alias = "cs", alias = "c#")]
    CSharp,
    #[value(alias = "rb")]
    Ruby,
    Sql,
}

impl From<LanguageArg> for Language {
    fn from(arg: LanguageArg) -> Self {
        match arg {
            LanguageArg::TypeScript => Language::TypeScript,
            LanguageArg::JavaScript => Language::JavaScript,
            LanguageArg::Python => Language::Python,
            LanguageArg::Rust => Language::Rust,
            LanguageArg::Go => Language::Go,
            LanguageArg::Java => Language::Java,
            LanguageArg::Markdown => Language::Markdown,
            LanguageArg::Json => Language::Json,
            LanguageArg::Yaml => Language::Yaml,
            LanguageArg::C => Language::C,
            LanguageArg::Cpp => Language::Cpp,
            LanguageArg::Toml => Language::Toml,
            LanguageArg::CSharp => Language::CSharp,
            LanguageArg::Ruby => Language::Ruby,
            LanguageArg::Sql => Language::Sql,
        }
    }
}

/// Validate a numeric CLI flag is within `[1, max]`.
///
/// `zero_hint` is appended to the zero-value error when present (e.g.
/// "Use --max-lines 1 to get a single line of output."). Pass `None`
/// for flags like `--jobs` where no extra guidance is needed.
fn validate_bounded_arg(
    value: Option<usize>,
    flag_name: &str,
    max: usize,
    zero_hint: Option<&str>,
    max_reason: &str,
) -> anyhow::Result<()> {
    let Some(v) = value else {
        return Ok(());
    };

    if v == 0 {
        let suffix = zero_hint.map_or(String::new(), |hint| format!("\n{hint}"));
        anyhow::bail!("{flag_name} must be at least 1{suffix}");
    }
    if v > max {
        anyhow::bail!("{flag_name} value too high: {v} (maximum: {max})\n{max_reason}");
    }

    Ok(())
}

/// Validate all numeric CLI flags (`--jobs`, `--max-lines`, `--last-lines`, `--tokens`)
fn validate_args(args: &Args) -> anyhow::Result<()> {
    validate_bounded_arg(
        args.jobs,
        "--jobs",
        MAX_JOBS,
        None,
        "Using too many threads can exhaust system resources.\n\
         Recommended: Use default (number of CPUs) or specify a moderate value.",
    )?;
    validate_bounded_arg(
        args.max_lines,
        "--max-lines",
        MAX_MAX_LINES,
        Some("Use --max-lines 1 to get a single line of output."),
        "Files exceeding this limit should be processed without truncation.",
    )?;
    validate_bounded_arg(
        args.last_lines,
        "--last-lines",
        MAX_MAX_LINES,
        Some("Use --last-lines 1 to get a single line of output."),
        "Files exceeding this limit should be processed without truncation.",
    )?;
    validate_bounded_arg(
        args.tokens,
        "--tokens",
        MAX_TOKEN_BUDGET,
        Some("Use --tokens 1 to get the minimum possible output."),
        "This exceeds any reasonable LLM context window.",
    )?;

    if args.max_lines.is_some() && args.last_lines.is_some() {
        anyhow::bail!(
            "--max-lines and --last-lines are mutually exclusive\n\
             Use --max-lines to keep the first N lines, or --last-lines to keep the last N lines."
        );
    }

    if args.filename.is_some() && args.file.as_deref() != Some("-") {
        anyhow::bail!(
            "--filename is only valid when reading from stdin (file argument is '-')\n\
             For files on disk, language is auto-detected from the file extension."
        );
    }

    Ok(())
}

fn main() -> ExitCode {
    // Extract --disable-analytics from raw args before routing, so it
    // applies to both file operations and subcommands. Uses an AtomicBool
    // instead of env::set_var to avoid unsoundness in multi-threaded context.
    if std::env::args().any(|a| a == "--disable-analytics") {
        analytics::force_disable_analytics();
    }

    let result: anyhow::Result<ExitCode> = match resolve_invocation() {
        Invocation::FileOperation => run_file_operation().map(|()| ExitCode::SUCCESS),
        Invocation::Subcommand { name, args } => cmd::dispatch(&name, &args),
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// File/directory/glob/stdin processing pipeline.
///
/// Parses CLI args via clap, validates constraints, then delegates to
/// the appropriate processor (stdin, directory, glob, or single file).
fn run_file_operation() -> anyhow::Result<()> {
    let args = Args::parse();
    validate_args(&args)?;

    // --disable-analytics is handled in main() before routing via
    // analytics::force_disable_analytics(). No env var mutation needed.

    if args.clear_cache {
        cache::clear_cache()?;
        println!("Cache cleared successfully");
        return Ok(());
    }

    let file = args
        .file
        .clone()
        .ok_or_else(|| anyhow::anyhow!("FILE argument is required"))?;

    let process_options = process::ProcessOptions {
        mode: Mode::from(args.mode),
        explicit_lang: args.language.map(Language::from),
        use_cache: !args.no_cache,
        show_stats: args.show_stats,
        trunc: cascade::TruncationOptions {
            max_lines: args.max_lines,
            last_lines: args.last_lines,
            token_budget: args.tokens,
        },
    };

    let disable_analytics = args.disable_analytics;

    if file == "-" {
        let result = process::process_stdin(process_options, args.filename.as_deref())?;
        process::write_result_and_stats(&result, args.show_stats)?;
        if !disable_analytics {
            record_file_analytics(&result, "skim -", &args);
        }
        return Ok(());
    }

    let path = PathBuf::from(&file);

    let multi_options = multi::MultiFileOptions {
        process: process_options,
        no_header: args.no_header,
        jobs: args.jobs,
        no_ignore: args.no_ignore,
        disable_analytics,
    };

    if path.is_dir() {
        return multi::process_directory(&path, multi_options);
    }

    if multi::has_glob_pattern(&file) {
        return multi::process_glob(&file, multi_options);
    }

    let result = process::process_file(&path, process_options)?;
    process::write_result_and_stats(&result, args.show_stats)?;
    if !disable_analytics {
        record_file_analytics(&result, &format!("skim {file}"), &args);
    }
    Ok(())
}

/// Record token analytics for file operations (single file or stdin).
fn record_file_analytics(result: &process::ProcessResult, cmd: &str, args: &Args) {
    if !analytics::is_analytics_enabled() {
        return;
    }
    if let (Some(raw), Some(comp)) = (result.original_tokens, result.transformed_tokens) {
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string();
        let lang = args
            .language
            .map(|l| format!("{:?}", Language::from(l)).to_lowercase());
        let mode = format!("{:?}", Mode::from(args.mode)).to_lowercase();
        analytics::record_with_counts(analytics::TokenSavingsRecord {
            timestamp: analytics::now_unix_secs(),
            command_type: analytics::CommandType::File,
            original_cmd: cmd.to_string(),
            raw_tokens: raw,
            compressed_tokens: comp,
            savings_pct: analytics::savings_percentage(raw, comp),
            duration_ms: 0,
            project_path: cwd,
            mode: Some(mode),
            language: lang,
            parse_tier: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // validate_bounded_arg unit tests (B3)
    // ========================================================================

    #[test]
    fn test_validate_bounded_arg_none_passes() {
        let result = validate_bounded_arg(None, "--test", 128, None, "reason");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bounded_arg_valid_value_passes() {
        let result = validate_bounded_arg(Some(4), "--test", 128, None, "reason");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bounded_arg_at_max_passes() {
        let result = validate_bounded_arg(Some(128), "--test", 128, None, "reason");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bounded_arg_zero_without_hint() {
        let result = validate_bounded_arg(Some(0), "--jobs", 128, None, "reason");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--jobs must be at least 1"), "got: {msg}");
        // Should NOT contain a hint line
        assert_eq!(msg.lines().count(), 1, "expected single line, got: {msg}");
    }

    #[test]
    fn test_validate_bounded_arg_zero_with_hint() {
        let result = validate_bounded_arg(
            Some(0),
            "--max-lines",
            1_000_000,
            Some("Use --max-lines 1 to get a single line of output."),
            "reason",
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--max-lines must be at least 1"), "got: {msg}");
        assert!(
            msg.contains("Use --max-lines 1"),
            "expected hint in message, got: {msg}"
        );
    }

    #[test]
    fn test_validate_bounded_arg_over_max() {
        let result = validate_bounded_arg(Some(200), "--jobs", 128, None, "Too many threads.");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("200"), "expected value in message, got: {msg}");
        assert!(
            msg.contains("maximum: 128"),
            "expected max in message, got: {msg}"
        );
        assert!(
            msg.contains("Too many threads."),
            "expected reason in message, got: {msg}"
        );
    }

    // ========================================================================
    // is_flag_with_value sync tests (batch-A flag-sync)
    // ========================================================================

    /// Exhaustive list of flags that consume the next token as a value.
    /// Derived from `Args` struct fields that are NOT bool, plus subcommand
    /// flags (--since, --session, --agent) registered in `is_flag_with_value`.
    ///
    /// UPDATE THIS LIST if you add/remove a value-consuming flag.
    const VALUE_FLAGS: &[&str] = &[
        "--mode",
        "-m",
        "--language",
        "-l",
        "--lang", // alias for --language
        "--filename",
        "--jobs",
        "-j",
        "--max-lines",
        "--last-lines",
        "--tokens",
        "--since",
        "--session",
        "--agent",
        "--format",
    ];

    /// Ensure every value-consuming flag (non-boolean, non-positional) in `Args`
    /// is registered in `is_flag_with_value()`.
    ///
    /// If you add a new flag with a value to `Args`, this test will remind you
    /// to register it in `is_flag_with_value()`.
    #[test]
    fn test_is_flag_with_value_covers_all_value_flags() {
        for flag in VALUE_FLAGS {
            assert!(
                is_flag_with_value(flag),
                "Value-consuming flag {flag} is NOT registered in is_flag_with_value(). \
                 Add it to prevent subcommand mis-routing."
            );
        }
    }

    /// Ensure boolean flags are NOT registered as value-consuming.
    #[test]
    fn test_is_flag_with_value_rejects_boolean_flags() {
        let boolean_flags: &[&str] = &[
            "--no-header",
            "--no-ignore",
            "--no-cache",
            "--clear-cache",
            "--show-stats",
            "--disable-analytics",
        ];

        for flag in boolean_flags {
            assert!(
                !is_flag_with_value(flag),
                "Boolean flag {flag} is incorrectly registered as value-consuming \
                 in is_flag_with_value(). Remove it."
            );
        }
    }

    /// Behavioral test: a flag's value that matches a subcommand name must be
    /// consumed as the flag's value, not treated as a subcommand.
    ///
    /// Example: `skim --mode test file.ts` should parse `test` as the value
    /// for `--mode`, not route to the `test` subcommand.
    #[test]
    fn test_flag_value_matching_subcommand_is_consumed() {
        // Verify "test" is actually a known subcommand (precondition)
        assert!(
            cmd::is_known_subcommand("test"),
            "precondition: 'test' must be a known subcommand for this test"
        );

        // All value-consuming flags should consume "test" as their value,
        // so resolve_invocation should never route to Subcommand when the
        // flag is followed by a subcommand name as its value.
        //
        // We can't call resolve_invocation() directly (it reads env args),
        // so we test the building blocks: is_flag_with_value must return
        // true for every flag that takes a value, ensuring the pre-parser
        // skips past the value token.
        for flag in VALUE_FLAGS {
            assert!(
                is_flag_with_value(flag),
                "If {flag} does not consume its value, `skim {flag} test` would \
                 incorrectly route to the 'test' subcommand."
            );
        }
    }
}
