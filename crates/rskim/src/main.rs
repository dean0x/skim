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
mod debug;
mod format;
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
// Thread-spawn guard
// ============================================================================

/// Set to `true` immediately before the first thread is spawned (just before
/// `cmd::dispatch()`).  `strip_skim_wrappers_from_path()` asserts this is
/// still `false` so that a future reordering of `main()` is caught at
/// runtime rather than silently producing a data race on `set_var`.
static THREADS_SPAWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
            | "--blast-radius"
            | "--session-id"
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
/// | Is known subcommand                           | Subcommand    |
/// | Everything else                               | FileOperation |
fn resolve_invocation() -> anyhow::Result<Invocation> {
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
            return Ok(Invocation::FileOperation);
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
        return Ok(Invocation::FileOperation);
    };

    // File-like heuristics: if it looks like a file/path/glob, treat as file
    if looks_like_file_or_glob(positional) {
        return Ok(Invocation::FileOperation);
    }

    // Known subcommand check — subcommands always take priority.
    // Use `skim ./name` or a full path to read a file that shares a subcommand name.
    if cmd::is_known_subcommand(positional) {
        let name = positional.to_string();
        let remaining_args: Vec<String> = args[pos_idx + 1..].to_vec();
        return Ok(Invocation::Subcommand {
            name,
            args: remaining_args,
        });
    }

    // #352: `proxy` is compiled out of default builds. Bare `skim proxy` must fail
    // actionably instead of falling into the file-op path (which would error with
    // "No such file or directory" — or silently skim a file named `proxy`). This
    // also keeps subcommand-over-file shadowing identical across feature configs.
    #[cfg(not(feature = "proxy"))]
    if positional == "proxy" {
        anyhow::bail!("'proxy' requires a build with --features proxy");
    }

    // Unknown word — fall through to FileOperation (clap handles errors)
    Ok(Invocation::FileOperation)
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
/// Version string: "X.Y.Z (shortsha)" — or "X.Y.Z (unknown)" for tarball builds.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("SKIM_GIT_COMMIT"),
    ")"
);

#[derive(Parser, Debug)]
#[command(name = "skim")]
#[command(author, version = VERSION, about, long_about = None)]
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
    cargo <test|build|clippy|nextest|audit>  Cargo subcommand compression\n  \
    go test                                  Go test compression\n  \
    pytest / vitest / jest                   Test runner compression\n  \
    tsc                                      TypeScript build compression\n  \
    eslint / ruff / mypy / biome / ...       Lint output compression\n  \
    npm / pnpm / pip                         Package manager compression\n  \
    gh / aws / curl / wget                   Infrastructure tool compression\n  \
    find / grep / ls / rg / tree             File operation compression\n  \
    git                                      Git output compression (diff, status, log, ...)\n  \
    heatmap                                  Git history risk/coupling analysis\n  \
    log                                      Log output compression\n  \
    agents                                   Show detected AI agents\n  \
    completions <SHELL>                      Generate shell completions\n  \
    discover                                 Identify missed optimizations\n  \
    doctor                                   Check skim installation health and report provenance drift\n  \
    init                                     Initialize skim configuration\n  \
    learn                                    Detect CLI error patterns\n  \
    rewrite <COMMAND>...                     Rewrite commands into skim equivalents\n  \
    search                                   Code search over project index\n  \
    stats [--since N] [--format json]        Token analytics dashboard\n\n\
For more info: https://github.com/dean0x/skim")]
struct Args {
    /// Files, directories, or glob patterns to process (use '-' for stdin).
    /// Multiple arguments are accepted: `skim file1.ts file2.ts` or `skim 'src/**/*.ts' file.py`.
    #[arg(value_name = "FILE")]
    files: Vec<String>,

    /// Transformation mode
    #[arg(short, long, value_enum, default_value = "structure")]
    #[arg(help = "Transformation mode: structure, signatures, types, full, minimal, or pseudo")]
    mode: ModeArg,

    /// Override language detection; stdin without this flag, --filename, or a shebang degrades to lossless passthrough (exit 0)
    #[arg(short, long, alias = "lang", value_enum)]
    #[arg(
        help = "Programming language: typescript, javascript, python, rust, go, java, c, cpp, csharp, ruby, sql, kotlin, swift, bash, markdown, json, yaml, toml (or use --filename for auto-detection from stdin)"
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
    /// Emits at most N lines total, including the elision marker. For N > 1:
    /// N-1 content lines + 1 marker = exactly N. For N = 1 (the irreconcilable
    /// case): 1 content line + 1 marker = 2 total — spending the only slot on
    /// the marker would return a view with no code, violating ADR-016.
    /// The marker only appears when the output is actually truncated; files
    /// with fewer than N lines are emitted verbatim with no marker.
    ///
    /// Types and signatures are kept over imports, which are kept over bodies.
    /// Never cuts mid-signature or mid-type-definition.
    #[arg(
        long,
        value_name = "N",
        help = "Emit at most N lines in total, including the elision marker \
                (N=1 emits one content line plus the marker); equivalent to `head -N` with disclosure"
    )]
    max_lines: Option<usize>,

    /// Keep only the last N lines of output
    ///
    /// Emits at most N lines total from the tail, including the elision marker.
    /// For N > 1: 1 marker + N-1 content lines = exactly N. For N = 1 (the
    /// irreconcilable case): 1 marker + 1 content line = 2 total — spending
    /// the only slot on the marker would return a view with no code, violating
    /// ADR-016. The marker only appears when the output is actually truncated;
    /// files with fewer than N lines are emitted verbatim with no marker.
    /// Mirrors `--max-lines` semantics; mutually exclusive with it.
    #[arg(
        long,
        value_name = "N",
        help = "Emit at most N lines in total from the tail, including the elision marker \
                (N=1 emits one content line plus the marker); equivalent to `tail -N` with disclosure"
    )]
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
        help = "Fit the output within N tokens by escalating modes, then line-truncating \
                with a marker; on very small budgets the count stays on stdout and the \
                SKIM_PASSTHROUGH=1 remedy is printed to stderr"
    )]
    tokens: Option<usize>,

    /// Annotate output with original source line numbers.
    ///
    /// Each output line is prefixed with its 1-indexed source line number and a tab:
    /// `{source_line}\t{content}`. Omission/truncation markers have no prefix.
    ///
    /// Useful when you need line numbers for Edit operations but want to survey
    /// structure first: `skim file.ts -n` gives both structure AND line numbers.
    #[arg(
        short = 'n',
        long,
        help = "Annotate output with original source line numbers"
    )]
    line_numbers: bool,

    /// Disable analytics recording for this invocation
    #[arg(long, help = "Disable analytics recording")]
    disable_analytics: bool,

    /// Session attribution ID injected by the rewrite hook (#317).
    ///
    /// Consumed pre-clap by `parse_session_id`; this field exists ONLY so the
    /// FileOperation path accepts the flag. Without it, every hook-rewritten
    /// `cat <file>` (→ `skim --session-id=… <file>`) errored with
    /// "unexpected argument '--session-id'".
    #[arg(
        long = "session-id",
        hide = true,
        require_equals = true,
        value_name = "ID"
    )]
    _session_id: Option<String>,

    /// Enable debug output (warnings/notices on stderr)
    #[arg(long, global = true)]
    debug: bool,

    /// Bypass all compression and exec the real tool with raw argv.
    ///
    /// Equivalent to setting `SKIM_PASSTHROUGH=1`. When set, skim-only flags
    /// are stripped from the forwarded argv so the underlying tool never sees
    /// flags it does not understand.
    ///
    /// Stripped flags (all tools): `--show-stats`, `--passthrough`, `--debug`
    /// (unless the tool owns `--debug` per its rewrite rule), `--max-lines N`
    /// / `--max-lines=N` (value-bearing), `--tokens N` / `--tokens=N`
    /// (value-bearing), `--line-numbers` (long form only; `-n` is NOT
    /// stripped — `git log -n <count>` is tool-owned), `--last-lines N` /
    /// `--last-lines=N` (value-bearing; stripped if present).
    ///
    /// Additionally stripped for `git` only: bare `--json` (before `--`),
    /// `--mode` / `--mode=<val>`.
    ///
    /// Nothing is stripped after a bare `--` end-of-options separator.
    ///
    /// ORDERING: detected and latched into an atomic BEFORE threads are
    /// spawned (see `main()` startup sequence), so analytics background
    /// threads always observe the correct passthrough state.
    #[arg(
        long,
        help = "Bypass all compression (equivalent to SKIM_PASSTHROUGH=1)"
    )]
    passthrough: bool,
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
    /// Pseudo mode — strips syntactic noise (types, decorators) while preserving logic and visibility
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
    #[value(alias = "kt")]
    Kotlin,
    Swift,
    #[value(alias = "sh")]
    Bash,
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
            LanguageArg::Kotlin => Language::Kotlin,
            LanguageArg::Swift => Language::Swift,
            LanguageArg::Bash => Language::Bash,
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

    // --filename is only valid when the single argument is '-' (stdin)
    if args.filename.is_some() && !(args.files.len() == 1 && args.files[0] == "-") {
        anyhow::bail!(
            "--filename is only valid when reading from stdin (file argument is '-')\n\
             For files on disk, language is auto-detected from the file extension."
        );
    }

    Ok(())
}

/// Detect whether this binary was invoked via a symlink with a tool name as argv[0].
///
/// When `~/.skim/bin/git` is invoked, argv[0] will be something like
/// `/Users/x/.skim/bin/git`. We extract the file stem (`"git"`), check that
/// it is a known non-meta subcommand, and return `Some((name, remaining_args))`.
///
/// Returns `None` when:
/// - argv[0] stem is `"skim"` or `"rskim"` (normal invocation)
/// - stem is not a known subcommand (unrecognized tool)
/// - stem is a meta subcommand (`init`, `stats`, etc.) — those should not be symlinked
///
/// This function is `pub(crate)` only for testability. `main()` calls it via
/// the `detect_argv0_dispatch()` wrapper that reads real `std::env::args()`.
///
/// DESIGN: passthrough mode (`SKIM_PASSTHROUGH=1`) is intentionally NOT checked
/// here. The handler dispatched from `cmd::dispatch()` already checks it
/// internally via `is_passthrough_mode()`.
pub(crate) fn detect_argv0_for(name: &str) -> bool {
    // Normal binary names: not a symlink dispatch
    if name == "skim" || name == "rskim" {
        return false;
    }
    // Must be a known subcommand
    if !cmd::is_known_subcommand(name) {
        return false;
    }
    // Meta subcommands should not be symlink targets
    if cmd::is_meta_subcommand(name) {
        return false;
    }
    true
}

/// Extract the file stem from an `argv[0]` string.
///
/// Returns the last path component (without extension) of `argv0` as a
/// `String`, or `None` if the path has no file name component or contains
/// non-UTF-8 bytes.
///
/// Examples:
/// - `"/Users/x/.skim/bin/git"` → `Some("git")`
/// - `"skim"` → `Some("skim")`
/// - `"rskim"` → `Some("rskim")`
///
/// Extracted as a pure function so it can be unit-tested independently of
/// `std::env::args()`.
fn extract_argv0_stem(argv0: &str) -> Option<String> {
    std::path::Path::new(argv0)
        .file_stem()?
        .to_str()
        .map(str::to_string)
}

/// Detect argv[0]-based dispatch for symlink invocations.
///
/// When the binary is invoked as `~/.skim/bin/git`, this returns
/// `Some(("git", remaining_args))`. Returns `None` for normal invocations.
fn detect_argv0_dispatch() -> Option<(String, Vec<String>)> {
    let mut args = std::env::args();
    let argv0 = args.next()?;
    let stem = extract_argv0_stem(&argv0)?;
    if detect_argv0_for(&stem) {
        Some((stem, args.collect()))
    } else {
        None
    }
}

/// Extract and validate `--session-id=VALUE` from a command-line argument iterator.
///
/// Returns `Some(value)` when exactly one `--session-id=VALUE` argument is present
/// and `value` passes [`analytics::is_safe_session_id`]. Returns `None` when the
/// flag is absent, the value is empty, the value is unsafe, or the value exceeds
/// 128 characters.
///
/// Only the equals form (`--session-id=VALUE`) is recognised. The space-separated
/// form (`--session-id VALUE`) is not supported — accepting the space form would
/// complicate the pre-parse routing logic.
///
/// **Priority**: this function is the forward-compat *fallback* only (#1.1).
/// The hook no longer injects `--session-id` into the rewritten command; the flag
/// is honoured here only so an OLD hook talking to a NEW binary (skew scenario)
/// is not silently dropped. New code should attribute via sidecar or env var.
///
/// This is a pure function over an iterator so it can be unit-tested without
/// mutating `std::env::args()`.
fn parse_session_id<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .find_map(|a| a.as_ref().strip_prefix("--session-id=").map(str::to_string))
        .filter(|s| analytics::is_safe_session_id(s))
}

/// Pure PATH filter: removes all entries that match the wrappers directory from `path`.
///
/// Returns `Some(filtered)` when at least one entry was removed, `None` when
/// the wrappers directory is absent from `path` or nothing was removed.
///
/// Extracted as a pure function (no `set_var`) so it can be unit-tested
/// directly without touching the process environment.
///
/// The `wrappers_dir` parameter is the resolved wrappers directory (from
/// `cmd::skim_wrappers_dir()`).  Accepting it explicitly rather than calling
/// `skim_wrappers_dir()` internally keeps the function testable with arbitrary
/// paths — including paths that do NOT contain `.skim` (D6 fix: the old
/// hardcoded `b".skim"` fast-path check would silently skip filtering when
/// `SKIM_WRAPPERS_DIR` pointed outside `~/.skim/`, leaving the wrapper dir in
/// PATH and causing infinite recursion in the tool handler).
fn filter_wrappers_from_path(
    path: &std::ffi::OsStr,
    wrappers_dir: Option<&std::path::Path>,
) -> Option<std::ffi::OsString> {
    let wrappers_dir = wrappers_dir?;

    // Fast-path: if the raw PATH bytes contain no substring matching the
    // wrappers directory, the directory cannot be present. Skip the expensive
    // split-normalize-filter-join — this is the common case when
    // `skim init --wrappers` has not been run.
    //
    // D6: the needle is derived from the ACTUAL resolved wrappers_dir rather
    // than a hardcoded b".skim" substring. This ensures that
    // SKIM_WRAPPERS_DIR=/custom/skim-wrappers (a path without ".skim") is
    // still correctly stripped from PATH.
    let dir_bytes = wrappers_dir.as_os_str().as_encoded_bytes();
    if !dir_bytes.is_empty()
        && !path
            .as_encoded_bytes()
            .windows(dir_bytes.len())
            .any(|w| w == dir_bytes)
    {
        return None;
    }

    // Syntactic normalization only: collapses trailing slashes and `..`
    // segments so they don't defeat the equality check.  Filesystem symlinks
    // in *parent* directories are NOT resolved — use std::fs::canonicalize
    // if that guarantee is ever needed (PF-003).
    let wrappers_dir_canonical: std::path::PathBuf = wrappers_dir.components().collect();

    let entries: Vec<_> = std::env::split_paths(path).collect();
    let filtered: Vec<_> = entries
        .iter()
        .filter(|p| {
            let normalized: std::path::PathBuf = p.components().collect();
            normalized != wrappers_dir_canonical
        })
        .cloned()
        .collect();

    if filtered.len() == entries.len() {
        // Nothing was removed; caller can skip the set_var.
        return None;
    }

    std::env::join_paths(&filtered).ok()
}

/// Remove `~/.skim/bin` from `PATH` to prevent infinite recursion when the
/// skim binary is invoked as a symlink (e.g. `~/.skim/bin/git`).
///
/// This MUST be the first thing called in `main()`, before any thread is
/// spawned, because `set_var` is not thread-safe.
///
/// # Why this is needed
///
/// When a symlink in `~/.skim/bin/git` invokes this binary, `~/.skim/bin`
/// is at the front of PATH. If we let that PATH entry persist, then when a
/// subcommand handler calls `CommandRunner::run("git", …)`, the shell will
/// find `~/.skim/bin/git` again — triggering infinite recursion.
///
/// # Safety
///
/// `set_var` is unsafe in multi-threaded programs. This function must be
/// called before any thread is spawned (before analytics background threads,
/// rayon pools, etc.).
fn strip_skim_wrappers_from_path() {
    // Machine-checked single-thread invariant: assert that no thread has been
    // spawned yet.  If a future refactor reorders main() and calls this
    // function after spawning threads, this panics loudly rather than
    // producing a silent data race on set_var.
    assert!(
        !THREADS_SPAWNED.load(std::sync::atomic::Ordering::SeqCst),
        "strip_skim_wrappers_from_path() called after threads were spawned"
    );
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return,
    };
    if let Some(new_path) = filter_wrappers_from_path(&path, cmd::skim_wrappers_dir()) {
        // SAFETY: THREADS_SPAWNED is false (asserted above), so no other
        // thread can be reading the environment concurrently.
        unsafe {
            std::env::set_var("PATH", &new_path);
        }
    }
}

// ============================================================================
// D2b (#370) / D5: stdout gate — serve raw when stdout is not a TTY or pipe
// ============================================================================

/// Testable seam: return `true` when stdout should serve raw bytes.
///
/// The gate compresses **iff fd 1 is a terminal or a FIFO**, and serves raw for
/// everything else — regular files, non-terminal character devices, sockets,
/// block devices, directories.
///
/// Kept separate from [`stdout_should_serve_raw`] so tests can drive it with a
/// `Metadata` obtained from a real fd of a chosen type plus an explicit
/// `is_tty`, without having to install that fd as the process's own fd 1.
///
/// # Why `is_tty` is a parameter and not `FileType::is_char_device()`
///
/// This gate used `is_char_device()` as a stand-in for `isatty(1)`. Every
/// character device that is *not* a terminal — `/dev/null`, `/dev/zero`,
/// `/dev/random` — was therefore misclassified as a terminal and compressed
/// into. `is_tty` must come from a real `isatty(1)` test
/// (`std::io::stdout().is_terminal()`); the file type alone cannot answer it.
///
/// # Pipes are ambiguous here by construction
///
/// `| cat` and `| tee out.txt` present fd 1 as the same FIFO. `fstat` cannot
/// see the far end of a pipe, so this gate deliberately defaults FIFOs to
/// *compress* — `| cat` is the overwhelmingly common shape and compressing it
/// is skim's core value. The byte-exact pipe shapes are resolved on the one
/// surface that can observe pipeline structure, the rewrite engine, which
/// hands its verdict to the wrapper out of band (see [`force_raw_requested`]).
#[cfg(unix)]
fn stdout_should_serve_raw_impl(meta: std::io::Result<std::fs::Metadata>, is_tty: bool) -> bool {
    use std::os::unix::fs::FileTypeExt;
    if is_tty {
        // A terminal is a live reader — compression is the whole point.
        return false;
    }
    match meta {
        // Cannot determine the sink (e.g. fd 1 closed) — compress. Preserves
        // the pre-existing defensive default; there is no file to corrupt.
        Err(_) => false,
        Ok(m) => !m.file_type().is_fifo(),
    }
}

/// Return `true` when the process's stdout (fd 1) should receive raw bytes.
///
/// Compress iff fd 1 is a terminal or a pipe; serve raw for every other sink.
///
/// **Shared invariant (cross-surface):** "stdout going somewhere that needs an
/// exact capture must receive the tool's raw bytes, never a skim summary."
/// This invariant is enforced by two independent mechanisms — one per
/// interception surface — because each surface observes the destination at a
/// different stage, and each can see something the other structurally cannot:
/// - **Wrapper surface (here, runtime):** `fstat(fd 1)` + `isatty(1)` after the
///   shell has already consumed `>` and opened the target; no redirect token
///   remains in argv. Ground truth about the fd — blind to the far end of a pipe.
/// - **Rewrite surface (static):** `stdout_redirected_to_file` and
///   `command_needs_exact_bytes` in `cmd/rewrite/compound.rs` — a syntactic scan
///   before the command runs. Blind to what the shell actually did — but it is
///   the only surface that can see `| tee out.txt` and `$(…)`.
///
/// Ground truth decides where it can observe; syntax fills the one gap it cannot
/// (see [`force_raw_requested`]).
///
/// Non-Unix: always returns `false` (compression proceeds normally).
#[cfg(unix)]
#[allow(clippy::disallowed_methods)] // fd 1 identity check: ManuallyDrop borrow without I/O; not a write sink
fn stdout_should_serve_raw() -> bool {
    use std::io::IsTerminal;
    use std::mem::ManuallyDrop;
    use std::os::unix::io::FromRawFd;
    // Borrow fd 1 via a ManuallyDrop wrapper so the destructor never closes it.
    // SAFETY: ManuallyDrop suppresses File's Drop, so fd 1 is never closed
    // (no double-close). If fd 1 is invalid (e.g. the process was started with
    // stdout closed), metadata() returns Err; we fall back to false (compress).
    let f = unsafe { ManuallyDrop::new(std::fs::File::from_raw_fd(1)) };
    stdout_should_serve_raw_impl(f.metadata(), std::io::stdout().is_terminal())
}

#[cfg(not(unix))]
fn stdout_should_serve_raw() -> bool {
    false
}

/// Return `true` when the rewrite surface marked the current command as needing
/// byte-exact stdout **for `tool`**.
///
/// This closes the one gap `fstat` cannot: a pipe's far end. When the PreToolUse
/// hook sees `| tee out.txt`, `$(…)`, or a redirect onto a file/named FIFO, it
/// records a force-raw marker in the PID-keyed sidecar; this wrapper invocation
/// discovers it by walking its process ancestry. The marker is re-evaluated —
/// set *or cleared* — by every hook invocation that reaches command extraction,
/// so it never outlives a command the hook actually processed; five early exits
/// (passthrough mode, AwarenessOnly agents, stdin read error, JSON parse error,
/// missing command field) skip the write.
///
/// # Why the tool name is part of the key
///
/// PPID is not a command identity. Every command an agent runs shares that one
/// PID, so a PPID-only marker was shared mutable state: a `| tee` verdict leaked
/// onto every *other* wrapper invocation under the same agent — a concurrent
/// tool call, a background job, a hook-less nested sub-agent — and an unrelated
/// command's clear could delete a live one. Keying on the command heads the hook
/// saw (`cmd/rewrite/compound.rs::command_heads`) narrows it to the tools that
/// command actually names. See `set_force_raw` for the residual exposure that
/// remains: two *same-tool* commands under one agent still share a key.
///
/// **ACCEPTED LIMITATION (documented, and pinned by
/// `no_hook_means_fstat_only_behaviour` in `tests/cli_stdout_destination.rs`):**
/// the marker exists only when the hook actually fires. A bare wrapper
/// invocation with no PreToolUse hook installed — a plain interactive shell with
/// `~/.skim/bin` on `PATH`, or an agent that bypasses hooks — gets `fstat`-only
/// behaviour, so `git log | tee out.txt` still compresses there. Closing that
/// would require the wrapper to inspect its sibling processes, which is neither
/// portable nor reliable. Skim does not pretend otherwise.
///
/// Failure direction: a stale or missing marker makes a FIFO compress. For
/// `| cat` that is lossless; for a byte-exact consumer (`| tee f`,
/// `| sha256sum`) it is byte loss — measured 304 bytes served instead of 6803,
/// with nothing on stderr. See `session_sidecar.rs` on the same-tool clear
/// (#514) and `no_hook_means_fstat_only_behaviour` for the hook-less case.
fn force_raw_requested(tool: &str) -> bool {
    cmd::resolve_cache_dir()
        .as_deref()
        .is_some_and(|dir| cmd::session_sidecar::read_force_raw(dir, tool))
}

fn main() -> ExitCode {
    // Strip ~/.skim/bin from PATH FIRST — before any thread is spawned.
    // This prevents infinite recursion when invoked as a symlink (PF-003).
    strip_skim_wrappers_from_path();

    // Initialise debug flag from SKIM_DEBUG env var once, before any threads
    // are spawned. After this call, is_debug_enabled() is a pure atomic load.
    debug::init_debug_from_env();

    // security-5: anchor the pre-routing flag scan to skim's own flag positions.
    //
    // The old `std::env::args().any(|a| a == "--passthrough")` matched the token
    // anywhere in argv, including inside a tool's data arguments:
    //   `skim grep -e --passthrough file`
    // would enable passthrough AND strip_skim_flags would drop the token, so
    // grep received `["-e", "file"]` — the pattern arg was corrupted.
    //
    // Fix: collect once, stop at POSIX `--` (same logic as
    // `cmd::rewrite::args_before_separator`, inlined here because the `rewrite`
    // module is private), then stop at the first non-`--flag` token (the
    // subcommand or file argument).  A skim flag is only legal before the first
    // positional argument.
    let argv_for_flags: Vec<String> = std::env::args().skip(1).collect();
    let sep_pos = argv_for_flags
        .iter()
        .position(|a| a == "--")
        .unwrap_or(argv_for_flags.len());
    let pre_sep = &argv_for_flags[..sep_pos];
    // Collect skim-flag-zone tokens (before the first positional arg or `--`)
    // into a Vec so we can scan it twice without reconstructing the iterator.
    let skim_flag_zone: Vec<&String> = pre_sep
        .iter()
        .take_while(|a| a.starts_with('-'))
        .collect();

    // Extract --debug before routing so it applies to all subcommands.
    if skim_flag_zone.iter().any(|a| a.as_str() == "--debug") {
        debug::force_enable_debug();
    }

    // C2: latch --passthrough into an atomic BEFORE THREADS_SPAWNED so that
    // analytics background threads always observe the correct value.
    // Mirrors the --debug pre-parse pattern above.
    //
    // ORDERING INVARIANT: this store happens before THREADS_SPAWNED.store(true)
    // below. strip_skim_wrappers_from_path() asserts THREADS_SPAWNED is still
    // false at the top of main(), so any future reordering that moves code
    // below THREADS_SPAWNED will be caught by that assertion.
    if skim_flag_zone.iter().any(|a| a.as_str() == "--passthrough") {
        cmd::set_passthrough_flag();
    }

    // B4: hidden early-exit used by `skim doctor` to identify each binary on $PATH.
    //
    // Must fire before analytics setup and thread spawning — it exits immediately.
    // Hidden from `--help` output (not a clap flag); handled here, before clap parsing.
    // Old skim binaries without this flag return non-zero; doctor treats that as "unknown".
    if std::env::args().skip(1).any(|a| a == "--commit") {
        println!("{}", option_env!("SKIM_GIT_COMMIT").unwrap_or("unknown"));
        return ExitCode::SUCCESS;
    }

    // B5a: emit a structured startup line when debug is active.
    //
    // MUST be suppressed in hook mode: Claude Code treats any stderr output on
    // exit 0 as an error (GRANITE #361). When `--hook` is present in argv we
    // route the startup line to hook.log instead. The `SKIM_DEBUG=1` env var
    // is a per-session global that could otherwise pollute every hook call.
    //
    // Zero-cost when off: `debug::is_debug_enabled()` is a single atomic load.
    // `current_exe()` and `id()` are never evaluated when debug is disabled —
    // they are inside the `if` branch, not eagerly computed before it.
    if debug::is_debug_enabled() {
        let in_hook_mode = std::env::args().any(|a| a == "--hook");
        let pid = std::process::id();
        let exe = std::env::current_exe()
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unknown)".to_string());
        let msg = format!("[skim] {VERSION} exe={exe} pid={pid}");
        if in_hook_mode {
            // Route to hook.log — never stderr in hook mode (GRANITE #361).
            cmd::hook_log::log_hook_warning(&msg);
        } else {
            eprintln!("{msg}");
        }
    }

    // Read analytics config from env + CLI flag once at the system boundary.
    // Thread the struct down to all callers — no per-call env reads.
    let cli_disable_analytics = std::env::args().any(|a| a == "--disable-analytics");
    // Resolution priority (skew-proof, #1.1):
    //   1. Sidecar (out-of-band, written by the hook) — preferred path.
    //      Resolves via ancestry walk so even a child two levels deep finds it.
    //   2. SKIM_SESSION_ID env var — wrapper surface attribution (profile export).
    //   3. --session-id=VALUE flag — forward-compat fallback only.
    //      Honoured so an OLD hook that still injects the flag is not lost, but
    //      it is never the primary path. Flag injection was removed from the
    //      hook (#1.1 / fix/rewrite-compression-batch) to prevent version-skew
    //      hard-failures ("unexpected argument --session-id" on older binaries).
    let session_id = {
        let dir = cmd::resolve_cache_dir();
        dir.as_deref()
            .and_then(cmd::session_sidecar::read_session_id)
    }
    .or_else(|| {
        std::env::var("SKIM_SESSION_ID")
            .ok()
            .filter(|s| analytics::is_safe_session_id(s))
    })
    .or_else(|| parse_session_id(std::env::args()));
    let analytics = analytics::AnalyticsConfig::from_process(cli_disable_analytics, session_id);

    // Mark the thread-spawn boundary.  Any code below this line may spawn
    // threads; any code above may not.  strip_skim_wrappers_from_path()
    // asserts this flag is still false, so future reorderings are caught.
    THREADS_SPAWNED.store(true, std::sync::atomic::Ordering::SeqCst);

    // argv[0] dispatch: when invoked as ~/.skim/bin/git, bypass normal clap
    // parsing and route directly to the appropriate handler. PATH stripping
    // above ensures the handler won't find the symlink again (no recursion).
    let result: anyhow::Result<ExitCode> = if let Some((name, args)) = detect_argv0_dispatch() {
        // D2b (#370): when stdout is going somewhere that needs an exact
        // capture, the shell has already wired fd 1 up before exec-ing us. Run
        // the real tool with inherited stdio so its raw bytes reach that
        // destination unmodified (#317).
        //
        // Two inputs, one per observable: `stdout_should_serve_raw` is
        // ground truth about fd 1 (files, non-terminal char devices, sockets);
        // `force_raw_requested` carries the rewrite surface's verdict about the
        // one thing fd 1 cannot reveal — the far end of a pipe (`| tee f`,
        // `$(…)`) — for THIS tool. TTYs and plain `| cat` match neither and
        // still compress.
        //
        // Guard is scoped to this wrapper-dispatch branch only. The Subcommand
        // and FileOperation branches below are intentionally NOT guarded:
        // `skim file.ts > out.txt` and `skim grep … > out.txt` are explicit skim
        // invocations where the user wants skim's output saved — hoisting this
        // guard above detect_argv0_dispatch() would break that workflow. See the
        // case-8 rationale on `stdout_redirected_to_file` in cmd/rewrite/compound.rs.
        if stdout_should_serve_raw() || force_raw_requested(&name) {
            if cmd::redaction_is_mandatory(&name) {
                // ADR-011 class 1: raw bytes were requested for a tool whose
                // handler enforces credential redaction.  Serving the raw tool
                // output here would expose secrets (`GITHUB_TOKEN`, etc.) that
                // the compressed view would have redacted to `***` — a
                // *different-bytes* path in ADR-011 terms.  Unconditional
                // marker: block the raw serve and fall through to the handler.
                // (PF-012 / security-1)
                eprintln!(
                    "[skim] {name}: raw output blocked — redaction is mandatory; \
                     routing through handler to protect secrets"
                );
                cmd::dispatch_for_wrapper(&name, &args, &analytics)
            } else {
                // ADR-011 class 2: choosing raw loses nothing (no redaction
                // control applies to this tool), so this is a debug-gated
                // banner, never an unconditional marker.
                crate::debug_log!(
                    "[skim] wrapper: stdout needs exact bytes; serving raw for '{name}'"
                );
                Ok(cmd::run_inherited_passthrough(&name, &args))
            }
        } else {
            // D3/D4/D5 gates live inside dispatch_for_wrapper → dispatch_inner
            // so `grep --help`, `git --help`, etc. forward to the real tool.
            cmd::dispatch_for_wrapper(&name, &args, &analytics)
        }
    } else {
        match resolve_invocation() {
            Ok(Invocation::FileOperation) => {
                run_file_operation(&analytics).map(|()| ExitCode::SUCCESS)
            }
            Ok(Invocation::Subcommand { name, args }) => {
                cmd::dispatch_explicit(&name, &args, &analytics)
            }
            Err(e) => Err(e),
        }
    };

    let exit_code = match result {
        Ok(code) => code,
        // A closed downstream pipe (`skim … | head -20`) is a normal
        // end-of-consumption event, not a failure.  Three sinks in
        // `cmd/execution.rs` return `StdoutStatus::PipeClosed` directly; this
        // boundary catches every *other* buffered write site so no
        // `Error: Broken pipe (os error 32)` can reach a user and, critically,
        // so the process never exits `1` — for grep/rg/diff exit 1 means "no
        // matches found", which would be a false negative.
        //
        // ADR-011 classification: nothing is lost here (the reader chose to stop
        // reading), so any diagnostic is a class-(2) no-loss raw-fallback
        // banner and is debug-gated.  It is emphatically NOT an elision marker:
        // no `output::elision_marker`, no unconditional stderr line.  Raw grep
        // is silent in this exact situation, so an unconditional notice would
        // itself be a divergence from raw.
        Err(e) if cmd::execution::is_broken_pipe_chain(&e) => {
            crate::debug_log!(
                "[skim] downstream pipe closed; exiting {}.",
                cmd::execution::pipe_closed_code()
            );
            cmd::execution::pipe_closed_exit()
        }
        Err(e) => {
            eprintln!("Error: {e:#}");
            // Map known SkimError variants to documented exit codes:
            //   exit 2 — parse error (grammar/syntax failure)
            //   exit 3 — unsupported language / detection failure
            //   exit 1 — all other errors (I/O, config, etc.)
            if let Some(skim_err) = e.downcast_ref::<rskim_core::SkimError>() {
                match skim_err {
                    rskim_core::SkimError::ParseError(_) => ExitCode::from(2),
                    rskim_core::SkimError::UnsupportedLanguage(_) => ExitCode::from(3),
                    _ => ExitCode::FAILURE,
                }
            } else {
                ExitCode::FAILURE
            }
        }
    };

    // Join all pending analytics background threads before the process exits.
    // This ensures DB writes complete even for fast/short-lived commands.
    analytics::flush_pending();

    exit_code
}

/// File/directory/glob/stdin processing pipeline.
///
/// Parses CLI args via clap, validates constraints, then routes to
/// the appropriate processor based on argument count:
/// - 0 args → usage error
/// - 1 arg  → `process_single_arg` (stdin, directory, glob, or single file)
/// - N args → explicit multi-file list (no stdin mixing allowed)
fn run_file_operation(analytics: &analytics::AnalyticsConfig) -> anyhow::Result<()> {
    let args = Args::parse();
    validate_args(&args)?;

    if args.clear_cache {
        cache::clear_cache()?;
        println!("Cache cleared successfully");
        return Ok(());
    }

    if args.files.is_empty() {
        anyhow::bail!(
            "FILE argument is required\n\
             Usage: skim <FILE|DIR|GLOB> [--mode structure|signatures|types|full|minimal|pseudo]\n\
             Use 'skim --help' for more information."
        );
    }

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
        line_numbers: args.line_numbers,
    };

    let multi_options = multi::MultiFileOptions {
        process: process_options,
        no_header: args.no_header,
        jobs: args.jobs,
        no_ignore: args.no_ignore,
        analytics_enabled: analytics.enabled,
        session_id: analytics.session_id.clone(),
    };

    if args.files.len() == 1 {
        return process_single_arg(
            &args.files[0],
            &args,
            analytics,
            process_options,
            multi_options,
        );
    }

    // === Multiple arguments: `skim file1.ts file2.ts` ===
    //
    // Stdin (`-`) cannot be mixed with other files: the single stdin stream
    // cannot be read once per file argument.
    if args.files.iter().any(|f| f == "-") {
        anyhow::bail!(
            "stdin ('-') cannot be combined with other file arguments\n\
             Use 'skim -' alone to read from stdin, or specify file paths directly."
        );
    }

    // Expand each argument: glob pattern → expand, directory → collect,
    // plain file → add directly.  All results are gathered into a single Vec
    // and processed together via process_files.
    multi::process_explicit_files(&args.files, multi_options)
}

/// Dispatch a single argument to the appropriate processor.
///
/// Handles four cases in priority order:
/// 1. `-`       → read from stdin
/// 2. directory → recursive directory walk
/// 3. glob      → glob pattern expansion
/// 4. file path → single file processing
#[allow(clippy::disallowed_methods)] // Top-level arg dispatcher; delegates to write_result_and_stats and process_files which hold the locks
fn process_single_arg(
    file: &str,
    args: &Args,
    analytics: &analytics::AnalyticsConfig,
    process_options: process::ProcessOptions,
    multi_options: multi::MultiFileOptions,
) -> anyhow::Result<()> {
    // B1 / ADR-011: structural passthrough gate for the read path.
    //
    // When SKIM_PASSTHROUGH=1, emit raw bytes without any transformation.
    // Covers all four dispatch shapes that `process_single_arg` handles:
    // stdin (`-`), directory, glob, and single file.
    //
    // architecture-1: write failures use bare `?` so the `io::Error` stays as
    // the chain head and `is_broken_pipe_chain` at the top-level boundary can
    // detect `EPIPE` and exit 141 rather than 1.  Wrapping with `anyhow::anyhow!`
    // produces a `Message` error with no source, which `chain()` cannot walk.
    //
    // consistency-3: the remedy line in the multi-file marker says
    // "SKIM_PASSTHROUGH=1 for raw output", so every shape the marker can fire
    // for must also work in passthrough mode.  Directories and globs are handled
    // here so the remedy is literally reachable from any invocation that prints it.
    if cmd::is_passthrough_mode() {
        use std::io::Write as _;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let path = std::path::Path::new(file);
        if file == "-" {
            // Stdin passthrough: copy bounded stdin → stdout.
            let buf = cmd::read_stdin_bounded()?;
            out.write_all(buf.as_bytes())?;
        } else if path.is_dir() {
            // Directory passthrough: collect all skim-supported files and
            // print their raw bytes concatenated (mirrors process_directory).
            let paths = multi::collect_passthrough_paths_dir(path, args.no_ignore);
            for p in &paths {
                let contents = std::fs::read(p)
                    .map_err(|e| anyhow::anyhow!("passthrough read {}: {e}", p.display()))?;
                out.write_all(&contents)?;
            }
        } else if multi::has_glob_pattern(file) {
            // Glob passthrough: expand, then print raw bytes of each match.
            let paths = multi::collect_passthrough_paths_glob(file, args.no_ignore)?;
            for p in &paths {
                let contents = std::fs::read(p)
                    .map_err(|e| anyhow::anyhow!("passthrough read {}: {e}", p.display()))?;
                out.write_all(&contents)?;
            }
        } else {
            // File passthrough: read raw bytes and copy to stdout.
            // Skip validation (size limits, UTF-8) — the point of passthrough
            // is byte-faithful forwarding without skim's transform guards.
            let contents = std::fs::read(file)
                .map_err(|e| anyhow::anyhow!("passthrough read {file}: {e}"))?;
            out.write_all(&contents)?;
        }
        return Ok(());
    }

    // Capture cwd on the main thread before any background threads are spawned.
    // (std::env::current_dir is not safe to call from background threads in general.)
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    let mode_str = format!("{:?}", Mode::from(args.mode)).to_lowercase();

    if file == "-" {
        let result = process::process_stdin(process_options, args.filename.as_deref())?;
        process::write_result_and_stats(&result, args.show_stats, &mode_str)?;
        record_file_analytics(
            analytics.enabled,
            result,
            "skim -",
            mode_str,
            analytics.session_id.as_deref(),
            cwd,
            None, // stdin: no path to re-read
        );
        return Ok(());
    }

    let path = PathBuf::from(file);

    if path.is_dir() {
        return multi::process_directory(&path, multi_options);
    }

    if multi::has_glob_pattern(file) {
        return multi::process_glob(file, multi_options);
    }

    let result = process::process_file(&path, process_options)?;
    process::write_result_and_stats(&result, args.show_stats, &mode_str)?;
    let cmd = format!("skim {file}");
    record_file_analytics(
        analytics.enabled,
        result,
        &cmd,
        mode_str,
        analytics.session_id.as_deref(),
        cwd,
        Some(path), // file: re-read from disk in background
    );
    Ok(())
}

/// Record token analytics for file operations (single file or stdin).
///
/// Takes `result` by value so `output` and `stdin_raw` can be moved into the
/// background thread without cloning.
///
/// `file_path` is `Some` for single-file ops (re-read on background thread) and
/// `None` for stdin (buffer already captured in `result.stdin_raw`).
fn record_file_analytics(
    enabled: bool,
    result: process::ProcessResult,
    cmd: &str,
    mode_str: String,
    session_id: Option<&str>,
    cwd: String,
    file_path: Option<PathBuf>,
) {
    // Determine counts variant: Known when both token counts are already computed
    // (i.e. --show-stats ran, or a count-carrying cache hit); Tokenize otherwise.
    let counts = match (result.original_tokens, result.transformed_tokens) {
        (Some(raw), Some(comp)) => {
            // AC F5: counts in hand — no re-read, no double work.
            analytics::FileCounts::Known {
                raw,
                compressed: comp,
            }
        }
        _ => match file_path {
            Some(p) => analytics::FileCounts::Tokenize {
                raw: analytics::RawSource::Reread(p),
                compressed: result.output,
            },
            None => {
                // Stdin: inline buffer retained by process_stdin when !show_stats.
                // If stdin_raw is None here it means show_stats was on and counts
                // should have been Some — this branch is unreachable in practice,
                // but we degrade gracefully: no row recorded.
                let Some(buf) = result.stdin_raw else { return };
                analytics::FileCounts::Tokenize {
                    raw: analytics::RawSource::Inline(buf),
                    compressed: result.output,
                }
            }
        },
    };

    let language = result.language.map(|l| l.as_str().to_string());
    let parse_tier = result.parse_tier.map(str::to_string);

    analytics::record_file_ops(
        enabled,
        vec![analytics::FileOpRow {
            counts,
            original_cmd: cmd.to_string(),
            language,
            parse_tier,
        }],
        analytics::FileOpCommon {
            mode: Some(mode_str),
            project_path: cwd,
            session_id: session_id.map(str::to_string),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // stdout_should_serve_raw_impl — compress iff terminal or FIFO
    //
    // Every case below is driven by a REAL fd of the relevant type (a real
    // /dev/null, a real socketpair, a real mkfifo), not by synthetic metadata:
    // the whole defect this gate had was believing a file-type bit answered a
    // question only isatty() can answer, so the tests must exercise the real
    // types the kernel reports.
    // ========================================================================

    /// Regular file → serve raw (the `> out.txt` case).
    #[cfg(unix)]
    #[test]
    fn test_stdout_should_serve_raw_true_for_regular_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        assert!(
            stdout_should_serve_raw_impl(tmp.as_file().metadata(), false),
            "regular file must serve raw"
        );
    }

    /// Error result → compress (defensive default; nothing to corrupt).
    #[cfg(unix)]
    #[test]
    fn test_stdout_should_serve_raw_false_for_err() {
        let meta: std::io::Result<std::fs::Metadata> =
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(
            !stdout_should_serve_raw_impl(meta, false),
            "Err metadata must fall through to compress (defensive)"
        );
    }

    /// Directory → serve raw (not a terminal, not a FIFO).
    #[cfg(unix)]
    #[test]
    fn test_stdout_should_serve_raw_true_for_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            stdout_should_serve_raw_impl(std::fs::metadata(tmp.path()), false),
            "directory metadata (not terminal, not FIFO) must serve raw"
        );
    }

    /// A terminal compresses — that is the whole point of skim.
    ///
    /// Driven through the `is_tty` parameter rather than a real pty because
    /// `is_tty` is exactly what `isatty(1)` produces at the call site.
    #[cfg(unix)]
    #[test]
    fn test_stdout_should_serve_raw_false_for_terminal() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        assert!(
            !stdout_should_serve_raw_impl(tmp.as_file().metadata(), true),
            "a terminal must compress regardless of the underlying file type"
        );
    }

    /// **The char-device bug.** `/dev/null` is a character device but NOT a
    /// terminal. The old gate used `is_char_device()` as an `isatty()` proxy and
    /// therefore compressed into `/dev/null`, `/dev/zero` and `/dev/random`
    /// alike. A real `/dev/null` fd, with `is_tty` false as `isatty(1)` would
    /// report it, must serve raw.
    #[cfg(unix)]
    #[test]
    fn test_stdout_should_serve_raw_true_for_non_terminal_char_device() {
        use std::os::unix::fs::FileTypeExt;
        let devnull = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("/dev/null must be openable");
        let meta = devnull.metadata().expect("metadata on /dev/null");
        // Guard the premise: if this is not a char device the test proves nothing.
        assert!(
            meta.file_type().is_char_device(),
            "/dev/null must be a character device for this test to be meaningful"
        );
        assert!(
            stdout_should_serve_raw_impl(devnull.metadata(), false),
            "/dev/null is a char device but not a terminal — must serve raw, \
             not be mistaken for a TTY"
        );
    }

    /// A FIFO compresses by default: `fstat` cannot see the far end, and
    /// `| cat` is the common shape. The byte-exact pipe shapes are resolved by
    /// `force_raw_requested`, not here.
    #[cfg(unix)]
    #[test]
    fn test_stdout_should_serve_raw_false_for_fifo() {
        use std::os::unix::fs::FileTypeExt;
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("p");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: mkfifo takes a NUL-terminated path and a mode; both are valid.
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
            0,
            "mkfifo failed"
        );
        let meta = std::fs::metadata(&fifo).expect("metadata on fifo");
        assert!(meta.file_type().is_fifo(), "premise: path must be a FIFO");
        assert!(
            !stdout_should_serve_raw_impl(std::fs::metadata(&fifo), false),
            "a FIFO must compress by default — `| cat` must not regress"
        );
    }

    /// An AF_UNIX socket is neither a terminal nor a FIFO → serve raw.
    #[cfg(unix)]
    #[test]
    fn test_stdout_should_serve_raw_true_for_socket() {
        use std::os::unix::io::FromRawFd;
        let mut fds = [0i32; 2];
        // SAFETY: socketpair fills a 2-element array of fds; the buffer is sized
        // correctly and the domain/type constants are valid.
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "socketpair failed");
        // SAFETY: fds[0] is a fresh, owned fd from socketpair; File takes ownership.
        let sock = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        // SAFETY: same for the peer end; dropped at end of scope.
        let _peer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        assert!(
            stdout_should_serve_raw_impl(sock.metadata(), false),
            "an AF_UNIX socket is neither a terminal nor a FIFO — must serve raw"
        );
    }

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
        "--blast-radius",
        "--session-id",
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
            "--debug",
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
        // Verify "cargo" is actually a known subcommand (precondition)
        assert!(
            cmd::is_known_subcommand("cargo"),
            "precondition: 'cargo' must be a known subcommand for this test"
        );

        // All value-consuming flags should consume "cargo" as their value,
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
                "If {flag} does not consume its value, `skim {flag} cargo` would \
                 incorrectly route to the 'cargo' subcommand."
            );
        }
    }

    // ========================================================================
    // parse_session_id tests (F7, F9, F10)
    // ========================================================================

    /// F7: --session-id=VALUE is extracted as Some(VALUE).
    #[test]
    fn test_parse_session_id_present() {
        let result = parse_session_id(["skim", "--session-id=abc-123"]);
        assert_eq!(result.as_deref(), Some("abc-123"));
    }

    /// F7: absent flag returns None.
    #[test]
    fn test_parse_session_id_absent() {
        let result = parse_session_id(["skim", "test", "cargo"]);
        assert!(result.is_none(), "no --session-id should yield None");
    }

    /// F7: empty value --session-id= returns None (rejects empty at validation).
    #[test]
    fn test_parse_session_id_empty() {
        let result = parse_session_id(["skim", "--session-id="]);
        assert!(
            result.is_none(),
            "--session-id= (empty value) must yield None"
        );
    }

    /// F7: unsafe value with shell metacharacters returns None.
    #[test]
    fn test_parse_session_id_unsafe() {
        let result = parse_session_id(["skim", "--session-id=a;b"]);
        assert!(
            result.is_none(),
            "--session-id=a;b (metacharacter) must yield None"
        );
    }

    /// F1: value exceeding 128 chars returns None.
    #[test]
    fn test_parse_session_id_too_long() {
        let long_value = format!("--session-id={}", "a".repeat(129));
        let result = parse_session_id(["skim", long_value.as_str()]);
        assert!(result.is_none(), "129-char session_id must be rejected");
    }

    /// F9: space-separated form --session-id VALUE is not recognised.
    #[test]
    fn test_parse_session_id_space_form() {
        // Space form: the hook always injects in equals form; space form is intentionally unsupported.
        let result = parse_session_id(["skim", "--session-id", "abc-123"]);
        assert!(
            result.is_none(),
            "--session-id <space> VALUE must not be recognised (only equals form supported)"
        );
    }

    // ========================================================================
    // Resolution priority: sidecar > env > flag (skew-proof, #1.1)
    //
    // New priority (fix/rewrite-compression-batch):
    //   1. Sidecar  — written by hook; skim child finds it via ancestry walk
    //   2. Env var  — SKIM_SESSION_ID (wrapper surface / profile export)
    //   3. Flag     — --session-id=VALUE forward-compat fallback (old hook)
    //
    // This is the inverse of the OLD priority (flag > sidecar > env).
    // The flag is no longer injected by the hook, so it must never win over
    // a fresh sidecar (which is more authoritative).
    // ========================================================================

    /// Priority 1: sidecar is used when present (with or without a flag).
    ///
    /// Exercises the composition in main():
    ///   sidecar.or_else(env).or_else(flag)
    ///
    /// Writes a sidecar for the current PID, then asserts it is found first.
    #[test]
    fn test_resolution_sidecar_wins() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join(format!("{}.id", std::process::id())),
            "sidecar-session-42",
        )
        .unwrap();

        // Compose sidecar > env > flag (as main() now does).
        let resolved = cmd::session_sidecar::read_session_id(dir.path())
            .or_else(|| {
                // env (not set in this test)
                std::env::var("SKIM_SESSION_ID")
                    .ok()
                    .filter(|s| analytics::is_safe_session_id(s))
            })
            .or_else(|| parse_session_id(["skim", "git", "status"]));

        assert_eq!(
            resolved.as_deref(),
            Some("sidecar-session-42"),
            "sidecar must be first in the priority chain"
        );
    }

    /// Priority 1b: sidecar wins over a stray --session-id flag (old hook compat).
    ///
    /// If the sidecar says "session-A" and an old hook injected "--session-id=session-B",
    /// the sidecar must still win so we don't regress to flag-first behaviour.
    #[test]
    fn test_resolution_sidecar_wins_over_flag() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join(format!("{}.id", std::process::id())),
            "sidecar-wins-session",
        )
        .unwrap();

        // Simulate: sidecar present, flag also present (old hook scenario).
        let resolved = cmd::session_sidecar::read_session_id(dir.path())
            .or_else(|| parse_session_id(["skim", "--session-id=flag-session"]));

        assert_eq!(
            resolved.as_deref(),
            Some("sidecar-wins-session"),
            "sidecar must win over stray --session-id flag (old-hook compat)"
        );
    }

    /// Priority 2: env var is used when sidecar is absent.
    ///
    /// `#[serial_test::serial]` prevents concurrent env-var mutation.
    #[serial_test::serial]
    #[test]
    fn test_resolution_env_wins_when_no_sidecar() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap(); // empty — no sidecar
        let env_session = "env-session-007";

        unsafe { std::env::set_var("SKIM_SESSION_ID", env_session) };

        let outcome = std::panic::catch_unwind(|| {
            // No sidecar → first leg is None.
            let from_sidecar = cmd::session_sidecar::read_session_id(dir.path());
            assert!(from_sidecar.is_none(), "precondition: no sidecar");

            // Env var supplies the session.
            let resolved = from_sidecar.or_else(|| {
                std::env::var("SKIM_SESSION_ID")
                    .ok()
                    .filter(|s| analytics::is_safe_session_id(s))
            });
            assert_eq!(
                resolved.as_deref(),
                Some(env_session),
                "SKIM_SESSION_ID env var must be the second priority when no sidecar"
            );
        });

        unsafe { std::env::remove_var("SKIM_SESSION_ID") };
        outcome.expect("test panicked while SKIM_SESSION_ID was set");
    }

    /// Priority 3: flag is the final fallback when sidecar and env are both absent.
    ///
    /// This is the forward-compat path: an OLD hook still injects the flag, and
    /// a new binary honours it — but only as a last resort.
    #[serial_test::serial]
    #[test]
    fn test_resolution_flag_is_last_resort() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap(); // empty — no sidecar

        // Ensure SKIM_SESSION_ID is not set.
        unsafe { std::env::remove_var("SKIM_SESSION_ID") };

        let outcome = std::panic::catch_unwind(|| {
            // No sidecar.
            let from_sidecar = cmd::session_sidecar::read_session_id(dir.path());
            assert!(from_sidecar.is_none(), "precondition: no sidecar");

            // No env var (not set).
            let after_env = from_sidecar.or_else(|| {
                std::env::var("SKIM_SESSION_ID")
                    .ok()
                    .filter(|s| analytics::is_safe_session_id(s))
            });
            assert!(after_env.is_none(), "precondition: no env var");

            // Flag (old hook forward-compat fallback).
            let resolved =
                after_env.or_else(|| parse_session_id(["skim", "--session-id=old-hook-flag"]));
            assert_eq!(
                resolved.as_deref(),
                Some("old-hook-flag"),
                "--session-id flag must be the last-resort fallback"
            );
        });

        outcome.expect("test panicked");
    }

    /// AD-SC-1: When neither sidecar, env, nor flag is present, result is None.
    ///
    /// Graceful degradation: no attribution — no hard failure.
    #[serial_test::serial]
    #[test]
    fn test_resolution_none_when_all_absent() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        unsafe { std::env::remove_var("SKIM_SESSION_ID") };

        let outcome = std::panic::catch_unwind(|| {
            let resolved = cmd::session_sidecar::read_session_id(dir.path())
                .or_else(|| {
                    std::env::var("SKIM_SESSION_ID")
                        .ok()
                        .filter(|s| analytics::is_safe_session_id(s))
                })
                .or_else(|| parse_session_id(["skim", "git", "status"]));
            assert!(
                resolved.is_none(),
                "all sources absent must yield None (no panic, graceful degradation)"
            );
        });

        outcome.expect("test panicked");
    }

    // ========================================================================
    // filter_wrappers_from_path tests (pure function, no set_var)
    // ========================================================================

    /// PATH containing ~/.skim/bin has that entry removed.
    #[test]
    fn test_strip_skim_wrappers_removes_wrapper_dir() {
        let home = dirs::home_dir().unwrap();
        let wrappers = home.join(".skim").join("bin");
        let other = std::path::PathBuf::from("/usr/bin");

        let input_paths = vec![wrappers.clone(), other.clone()];
        let path_str = std::env::join_paths(&input_paths).unwrap();

        // D6: pass wrappers_dir explicitly — the function no longer reads it
        // from the LazyLock cache internally.
        let result = filter_wrappers_from_path(&path_str, Some(&wrappers))
            .expect("wrappers dir present — filter must return Some");

        let result_paths: Vec<_> = std::env::split_paths(&result).collect();
        assert!(
            !result_paths.contains(&wrappers),
            "wrappers dir must be removed from PATH"
        );
        assert!(
            result_paths.contains(&other),
            "non-wrapper dirs must be preserved"
        );
    }

    /// PATH without ~/.skim/bin returns None (no change needed).
    #[test]
    fn test_strip_skim_wrappers_no_change_when_absent() {
        let home = dirs::home_dir().unwrap();
        let wrappers = home.join(".skim").join("bin");
        let other = std::path::PathBuf::from("/usr/local/bin");
        let other2 = std::path::PathBuf::from("/usr/bin");

        let input_paths = vec![other.clone(), other2.clone()];
        let path_str = std::env::join_paths(&input_paths).unwrap();

        // D6: pass wrappers_dir explicitly.
        let result = filter_wrappers_from_path(&path_str, Some(&wrappers));
        assert!(
            result.is_none(),
            "path without wrappers dir must return None (no change)"
        );
    }

    /// Wrappers dir in the middle of PATH: only that entry is removed, order preserved.
    #[test]
    fn test_strip_skim_wrappers_middle_entry_removed_order_preserved() {
        let home = dirs::home_dir().unwrap();
        let wrappers = home.join(".skim").join("bin");
        let before = std::path::PathBuf::from("/usr/local/bin");
        let after = std::path::PathBuf::from("/usr/bin");

        let input_paths = vec![before.clone(), wrappers.clone(), after.clone()];
        let path_str = std::env::join_paths(&input_paths).unwrap();

        let result = filter_wrappers_from_path(&path_str, Some(&wrappers))
            .expect("wrappers dir present — filter must return Some");
        let filtered: Vec<_> = std::env::split_paths(&result).collect();

        assert_eq!(filtered.len(), 2, "only the wrappers dir is removed");
        assert_eq!(
            filtered[0], before,
            "order before wrappers must be preserved"
        );
        assert_eq!(filtered[1], after, "order after wrappers must be preserved");
    }

    /// Duplicate ~/.skim/bin entries in PATH: both are removed.
    #[test]
    fn test_strip_skim_wrappers_removes_duplicate_entries() {
        let home = dirs::home_dir().unwrap();
        let wrappers = home.join(".skim").join("bin");
        let other = std::path::PathBuf::from("/usr/bin");

        // PATH=~/.skim/bin:/usr/bin:~/.skim/bin — duplicates must both be removed.
        let input_paths = vec![wrappers.clone(), other.clone(), wrappers.clone()];
        let path_str = std::env::join_paths(&input_paths).unwrap();

        let result = filter_wrappers_from_path(&path_str, Some(&wrappers))
            .expect("wrappers dir present — filter must return Some");
        let filtered: Vec<_> = std::env::split_paths(&result).collect();

        assert_eq!(
            filtered.len(),
            1,
            "both duplicate wrappers entries must be removed"
        );
        assert_eq!(filtered[0], other, "only /usr/bin must remain");
    }

    /// D6 regression: when SKIM_WRAPPERS_DIR points to a path without ".skim",
    /// filter_wrappers_from_path must still correctly remove it from PATH.
    ///
    /// The old code fast-pathed on `b".skim"` — a path like
    /// `/custom/skim-wrappers/bin` would pass the fast-path check (no ".skim"
    /// substring) and return None, leaving the wrapper dir in PATH and causing
    /// infinite recursion. The D6 fix derives the fast-path needle from the
    /// actual resolved wrappers_dir.
    #[test]
    fn test_strip_skim_wrappers_custom_dir_without_skim_substring() {
        // A custom wrappers dir whose path does NOT contain ".skim".
        let custom_wrappers = std::path::PathBuf::from("/opt/custom-wrappers/bin");
        let other = std::path::PathBuf::from("/usr/bin");

        let input_paths = vec![custom_wrappers.clone(), other.clone()];
        let path_str = std::env::join_paths(&input_paths).unwrap();

        // The old code (windows(5).any(|w| w == b".skim")) would return None here
        // because "/opt/custom-wrappers/bin" contains no ".skim" substring.
        // The D6 fix correctly detects and removes the custom wrappers dir.
        let result = filter_wrappers_from_path(&path_str, Some(&custom_wrappers))
            .expect("custom wrappers dir in PATH — filter must return Some (D6 regression)");

        let result_paths: Vec<_> = std::env::split_paths(&result).collect();
        assert!(
            !result_paths.contains(&custom_wrappers),
            "custom wrappers dir without '.skim' in path must be removed (D6)"
        );
        assert!(
            result_paths.contains(&other),
            "non-wrapper dirs must be preserved"
        );
    }

    // ========================================================================
    // extract_argv0_stem tests
    // ========================================================================

    /// Full absolute path: stem is the last component.
    #[test]
    fn test_extract_argv0_stem_full_path() {
        assert_eq!(
            extract_argv0_stem("/Users/x/.skim/bin/git").as_deref(),
            Some("git"),
            "full path must yield the filename stem"
        );
    }

    /// Bare binary name: stem is the name itself.
    #[test]
    fn test_extract_argv0_stem_bare_name() {
        assert_eq!(extract_argv0_stem("skim").as_deref(), Some("skim"),);
        assert_eq!(extract_argv0_stem("rskim").as_deref(), Some("rskim"),);
    }

    /// Deep nested path resolves correctly.
    #[test]
    fn test_extract_argv0_stem_nested_path() {
        assert_eq!(
            extract_argv0_stem("/home/runner/.skim/bin/npm").as_deref(),
            Some("npm"),
        );
    }

    /// Relative path is handled correctly.
    #[test]
    fn test_extract_argv0_stem_relative_path() {
        assert_eq!(
            extract_argv0_stem(".skim/bin/grep").as_deref(),
            Some("grep"),
        );
    }

    /// Empty string yields None (no file name component).
    #[test]
    fn test_extract_argv0_stem_empty_string() {
        // An empty string has no file name component.
        let result = extract_argv0_stem("");
        // Path::new("").file_stem() returns None on all platforms.
        assert!(result.is_none(), "empty argv0 must yield None");
    }

    /// Path with extension: stem strips the extension (covers Windows .exe).
    #[test]
    fn test_extract_argv0_stem_strips_extension() {
        assert_eq!(
            extract_argv0_stem("/Users/x/.skim/bin/git.exe").as_deref(),
            Some("git"),
            "file_stem() must strip .exe so Windows wrappers dispatch correctly"
        );
        assert_eq!(
            extract_argv0_stem("npm.cmd").as_deref(),
            Some("npm"),
            "file_stem() must strip .cmd extension"
        );
    }

    // ========================================================================
    // detect_argv0_for tests
    // ========================================================================

    /// "skim" stem: normal invocation, returns false.
    #[test]
    fn test_detect_argv0_for_skim() {
        assert!(
            !detect_argv0_for("skim"),
            "'skim' must not trigger argv0 dispatch"
        );
    }

    /// "rskim" stem: normal invocation, returns false.
    #[test]
    fn test_detect_argv0_for_rskim() {
        assert!(
            !detect_argv0_for("rskim"),
            "'rskim' must not trigger argv0 dispatch"
        );
    }

    /// "git": known non-meta subcommand, returns true.
    #[test]
    fn test_detect_argv0_for_git() {
        assert!(detect_argv0_for("git"), "'git' must trigger argv0 dispatch");
    }

    /// "cargo": known non-meta subcommand, returns true.
    #[test]
    fn test_detect_argv0_for_cargo() {
        assert!(
            detect_argv0_for("cargo"),
            "'cargo' must trigger argv0 dispatch"
        );
    }

    /// Unknown tool: returns false.
    #[test]
    fn test_detect_argv0_for_unknown_tool() {
        assert!(
            !detect_argv0_for("unknown_tool_xyz"),
            "unknown tool must not trigger argv0 dispatch"
        );
    }

    /// "init": meta subcommand, returns false.
    #[test]
    fn test_detect_argv0_for_init_meta() {
        assert!(
            !detect_argv0_for("init"),
            "'init' (meta) must not trigger argv0 dispatch"
        );
    }

    /// "stats": meta subcommand, returns false.
    #[test]
    fn test_detect_argv0_for_stats_meta() {
        assert!(
            !detect_argv0_for("stats"),
            "'stats' (meta) must not trigger argv0 dispatch"
        );
    }

    /// "heatmap": meta subcommand, returns false.
    #[test]
    fn test_detect_argv0_for_heatmap_meta() {
        assert!(
            !detect_argv0_for("heatmap"),
            "'heatmap' (meta) must not trigger argv0 dispatch"
        );
    }

    // ========================================================================
    // SKIM_SESSION_ID env var fallback tests
    // ========================================================================

    /// Empty string is rejected by is_safe_session_id.
    #[test]
    fn test_skim_session_id_empty_yields_none() {
        assert!(
            !analytics::is_safe_session_id(""),
            "empty session ID must be rejected by is_safe_session_id"
        );
    }

    /// SKIM_SESSION_ID with shell metacharacters yields None.
    #[test]
    fn test_skim_session_id_bad_chars_yields_none() {
        assert!(
            !analytics::is_safe_session_id("bad;chars"),
            "session ID with ';' must be rejected"
        );
        assert!(
            !analytics::is_safe_session_id("bad|pipe"),
            "session ID with '|' must be rejected"
        );
    }

    /// SKIM_SESSION_ID with 129+ chars yields None.
    #[test]
    fn test_skim_session_id_too_long_yields_none() {
        let long_id = "a".repeat(129);
        assert!(
            !analytics::is_safe_session_id(&long_id),
            "129-char session ID must be rejected"
        );
    }

    /// Valid SKIM_SESSION_ID is accepted.
    #[test]
    fn test_skim_session_id_valid_accepted() {
        let valid = "session-2024-01-15_abc123";
        assert!(
            analytics::is_safe_session_id(valid),
            "valid session ID must be accepted"
        );
    }

    // ========================================================================
    // filter_wrappers_from_path tests
    // ========================================================================

    /// Fast-path: PATH with no wrappers_dir substring returns None without allocation.
    #[test]
    fn test_filter_wrappers_fast_path_no_skim() {
        let wrappers = std::path::Path::new("/home/user/.skim/bin");
        let path = std::ffi::OsString::from("/usr/local/bin:/usr/bin:/bin");
        let result = filter_wrappers_from_path(&path, Some(wrappers));
        assert!(
            result.is_none(),
            "PATH without the wrappers_dir must return None (no filtering needed)"
        );
    }

    /// Fast-path passes through to full filter when a similar-looking path is
    /// present but does not match the exact wrappers directory — result is None.
    #[test]
    fn test_filter_wrappers_fast_path_skim_present_but_no_match() {
        // D6: the needle is derived from wrappers_dir, not a hardcoded ".skim".
        // A path containing a similar-looking segment is NOT filtered unless it
        // matches the exact wrappers_dir bytes.
        let wrappers = std::path::Path::new("/home/user/.skim/bin");
        let path = std::ffi::OsString::from("/usr/local/bin:/some/.skim-other/bin:/usr/bin");
        let result = filter_wrappers_from_path(&path, Some(wrappers));
        // The path does not contain "/home/user/.skim/bin", so nothing is filtered.
        assert!(
            result.is_none(),
            "PATH without exact wrappers_dir match must return None"
        );
    }

    /// KNOWN LIMITATION: filter_wrappers_from_path uses syntactic normalization
    /// only (component-level path collapsing), not filesystem canonicalization.
    ///
    /// If `~/.skim` is itself a symlink (e.g. `~/.skim -> /opt/skim-wrappers`),
    /// the syntactic comparison `normalized != wrappers_dir_canonical` will fail
    /// because the PATH entry carries the real path `/opt/skim-wrappers/bin` while
    /// `wrappers_dir_canonical` holds `~/.skim/bin` (syntactically normalised only).
    ///
    /// This means the recursion-prevention guard does NOT fire and `~/.skim/bin`
    /// effectively stays on PATH under the symlink alias — a skim wrapper invocation
    /// would recurse infinitely.
    ///
    /// Resolution requires `std::fs::canonicalize` on both sides of the comparison,
    /// which is an I/O call and cannot be pure/no-alloc. Tracked as a known limitation
    /// per PF-003. The test below documents the gap so the constraint is explicit.
    #[test]
    fn test_filter_wrappers_symlink_bypass_is_known_limitation() {
        // We cannot create real filesystem symlinks in a unit test reliably across
        // all platforms and CI environments.  Instead, this test documents the
        // known limitation by asserting the SYNTACTIC behaviour: a path entry that
        // resolves to the same filesystem location as `~/.skim/bin` but is spelled
        // differently (e.g. via a parent-directory symlink) will NOT be removed.
        //
        // Concretely: if $HOME/.skim is a symlink to /tmp/skim-wrappers, then
        // a PATH entry of `/tmp/skim-wrappers/bin` is NOT filtered out because
        // `skim_wrappers_dir()` returns `$HOME/.skim/bin`, and the syntactic
        // normalisation step cannot resolve that symlink.
        //
        // The safe escape hatch is SKIM_PASSTHROUGH=1, which bypasses all
        // handler logic and is documented in CLAUDE.md.
        //
        // If you are here to fix this: replace the syntactic `components().collect()`
        // canonicalization with `std::fs::canonicalize` on both sides and add
        // filesystem-level symlink tests using tempdir + std::os::unix::fs::symlink.
        //
        // This test intentionally has no assertions — its purpose is to be a
        // discoverable marker in the test suite for this limitation.
        let _note = "syntactic-only PATH filter: symlink bypass is a known limitation (PF-003)";
    }

    // ========================================================================
    // after_help drift guard
    // ========================================================================

    /// Every META_SUBCOMMAND (except `proxy`, which is cfg-gated and intentionally
    /// excluded from the user-facing help text) must appear in the `--help` after_help
    /// SUBCOMMANDS section.  This test fires whenever a meta subcommand is added to
    /// the registry without also listing it in the `SUBCOMMANDS:` block in `main.rs`.
    #[test]
    fn test_meta_subcommands_in_after_help() {
        let cmd = <Args as clap::CommandFactory>::command();
        let after_help = cmd
            .get_after_help()
            .expect("after_help must be set on Args")
            .to_string();
        for &name in cmd::META_SUBCOMMANDS {
            // `proxy` is #[cfg(feature = "proxy")]-gated and intentionally omitted
            // from the user-facing help text — it is an internal/advanced capability.
            if name == "proxy" {
                continue;
            }
            assert!(
                after_help.contains(name),
                "META_SUBCOMMANDS entry '{name}' is missing from the after_help \
                 SUBCOMMANDS section — add it to the SUBCOMMANDS list in main.rs"
            );
        }
    }
}
