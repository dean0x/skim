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

    // Known subcommand check — subcommands always take priority.
    // Use `skim ./name` or a full path to read a file that shares a subcommand name.
    if cmd::is_known_subcommand(positional) {
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
    init                                     Initialize skim configuration\n  \
    learn                                    Detect CLI error patterns\n  \
    rewrite <COMMAND>...                     Rewrite commands into skim equivalents\n  \
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

    /// Override language detection (required for stdin unless --filename is given)
    #[arg(short, long, alias = "lang", value_enum)]
    #[arg(
        help = "Programming language: typescript, javascript, python, rust, go, java, c, cpp, csharp, ruby, sql, kotlin, swift, markdown, json, yaml, toml (or use --filename for auto-detection from stdin)"
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

/// Pure PATH filter: removes all entries that match `~/.skim/bin` from `path`.
///
/// Returns `Some(filtered)` when at least one entry was removed, `None` when
/// the wrappers directory cannot be determined or the path is unchanged.
///
/// Extracted as a pure function (no `set_var`) so it can be unit-tested
/// directly without touching the process environment.
fn filter_wrappers_from_path(path: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
    // Fast-path: if the raw PATH string contains no ".skim" substring, the
    // wrappers directory cannot be present.  Skip the expensive
    // split-normalize-filter-join entirely — this is the common case when
    // `skim init --wrappers` has not been run.
    if !path.as_encoded_bytes().windows(5).any(|w| w == b".skim") {
        return None;
    }

    let wrappers_dir = cmd::skim_wrappers_dir()?;
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
    if let Some(new_path) = filter_wrappers_from_path(&path) {
        // SAFETY: THREADS_SPAWNED is false (asserted above), so no other
        // thread can be reading the environment concurrently.
        unsafe {
            std::env::set_var("PATH", &new_path);
        }
    }
}

// ============================================================================
// D2b (#370): stdout-is-regular-file predicate for wrapper passthrough
// ============================================================================

/// Testable seam: return `true` when the `io::Result<Metadata>` describes a
/// regular file. Used by [`stdout_is_regular_file`] so the check can be unit-
/// tested with synthetic metadata without touching real file descriptors.
#[cfg(unix)]
fn is_regular_file_stdout(meta: std::io::Result<std::fs::Metadata>) -> bool {
    meta.map(|m| m.is_file()).unwrap_or(false)
}

/// Return `true` when the process's stdout (fd 1) is a regular file.
///
/// When a PATH-wrapper invocation is `~/.skim/bin/gh api … > out.json`, the
/// shell sets fd 1 → the file BEFORE exec-ing the wrapper. No `>` appears in
/// argv, so the rewrite-engine guard (D2-A) cannot catch it. Detecting the fd
/// directly and running the real tool with inherited stdio ensures raw bytes
/// reach the file unmodified (#370, #317). Pipes/ttys are not regular files
/// and fall through to normal compression (skim's core use case).
///
/// Non-Unix: always returns `false` (compression proceeds normally).
#[cfg(unix)]
fn stdout_is_regular_file() -> bool {
    use std::mem::ManuallyDrop;
    use std::os::unix::io::FromRawFd;
    // Borrow fd 1 via a ManuallyDrop wrapper so the destructor never closes it.
    // SAFETY: fd 1 is always valid at this point in main(); ManuallyDrop
    // guarantees the File is never dropped (fd not closed).
    let f = unsafe { ManuallyDrop::new(std::fs::File::from_raw_fd(1)) };
    is_regular_file_stdout(f.metadata())
}

#[cfg(not(unix))]
fn stdout_is_regular_file() -> bool {
    false
}

fn main() -> ExitCode {
    // Strip ~/.skim/bin from PATH FIRST — before any thread is spawned.
    // This prevents infinite recursion when invoked as a symlink (PF-003).
    strip_skim_wrappers_from_path();

    // Initialise debug flag from SKIM_DEBUG env var once, before any threads
    // are spawned. After this call, is_debug_enabled() is a pure atomic load.
    debug::init_debug_from_env();

    // Extract --debug before routing so it applies to all subcommands.
    if std::env::args().any(|a| a == "--debug") {
        debug::force_enable_debug();
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
        // D2b (#370): when stdout is a regular file the shell has already
        // redirected fd 1 to the file before exec-ing us. Run the real tool
        // with inherited stdio so its raw bytes reach the file unmodified (#317).
        // Pipes/ttys are not regular files → still compress (normal path).
        if stdout_is_regular_file() {
            Ok(cmd::run_inherited_passthrough(&name, &args))
        } else {
            cmd::dispatch(&name, &args, &analytics)
        }
    } else {
        match resolve_invocation() {
            Invocation::FileOperation => run_file_operation(&analytics).map(|()| ExitCode::SUCCESS),
            Invocation::Subcommand { name, args } => cmd::dispatch(&name, &args, &analytics),
        }
    };

    let exit_code = match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
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
fn process_single_arg(
    file: &str,
    args: &Args,
    analytics: &analytics::AnalyticsConfig,
    process_options: process::ProcessOptions,
    multi_options: multi::MultiFileOptions,
) -> anyhow::Result<()> {
    // Capture cwd on the main thread before any background threads are spawned.
    // (std::env::current_dir is not safe to call from background threads in general.)
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    let mode_str = format!("{:?}", Mode::from(args.mode)).to_lowercase();

    if file == "-" {
        let result = process::process_stdin(process_options, args.filename.as_deref())?;
        process::write_result_and_stats(&result, args.show_stats)?;
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
    process::write_result_and_stats(&result, args.show_stats)?;
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
    // D2b (#370): is_regular_file_stdout unit tests
    // ========================================================================

    #[cfg(unix)]
    #[test]
    fn test_is_regular_file_stdout_true_for_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let meta = tmp.as_file().metadata();
        assert!(
            is_regular_file_stdout(meta),
            "metadata from a regular file must return true"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_is_regular_file_stdout_false_for_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = std::fs::metadata(tmp.path());
        assert!(
            !is_regular_file_stdout(meta),
            "metadata from a directory must return false"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_is_regular_file_stdout_false_for_err() {
        let meta: std::io::Result<std::fs::Metadata> =
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(
            !is_regular_file_stdout(meta),
            "Err metadata must return false (defensive)"
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

        // Call the real extracted function — no manual replication.
        let result = filter_wrappers_from_path(&path_str)
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
        let other = std::path::PathBuf::from("/usr/local/bin");
        let other2 = std::path::PathBuf::from("/usr/bin");

        let input_paths = vec![other.clone(), other2.clone()];
        let path_str = std::env::join_paths(&input_paths).unwrap();

        // filter_wrappers_from_path returns None when nothing was removed.
        let result = filter_wrappers_from_path(&path_str);
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

        let result = filter_wrappers_from_path(&path_str)
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

        let result = filter_wrappers_from_path(&path_str)
            .expect("wrappers dir present — filter must return Some");
        let filtered: Vec<_> = std::env::split_paths(&result).collect();

        assert_eq!(
            filtered.len(),
            1,
            "both duplicate wrappers entries must be removed"
        );
        assert_eq!(filtered[0], other, "only /usr/bin must remain");
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

    /// Fast-path: PATH with no ".skim" substring returns None without allocation.
    #[test]
    fn test_filter_wrappers_fast_path_no_skim() {
        let path = std::ffi::OsString::from("/usr/local/bin:/usr/bin:/bin");
        let result = filter_wrappers_from_path(&path);
        assert!(
            result.is_none(),
            "PATH with no '.skim' must return None immediately (fast-path)"
        );
    }

    /// Fast-path passes through to full filter when ".skim" is present but
    /// does not match the wrappers directory — result may be None (unchanged).
    #[test]
    fn test_filter_wrappers_fast_path_skim_present_but_no_match() {
        // A path containing ".skim" in a different position should not cause
        // a panic; it falls through to the full filter which returns None
        // when nothing was removed.
        let path = std::ffi::OsString::from("/usr/local/bin:/some/.skim-other/bin:/usr/bin");
        // We can't assert the exact value because it depends on skim_wrappers_dir(),
        // but we can assert no panic occurs and the function is callable.
        let _ = filter_wrappers_from_path(&path);
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
}
