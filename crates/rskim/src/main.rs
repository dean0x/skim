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

mod cache;
mod tokens;

use clap::Parser;
use glob::glob;
use rayon::prelude::*;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use rskim_core::{
    transform_auto_with_config, transform_with_config, truncate_to_token_budget, Language, Mode,
    TransformConfig,
};

/// Maximum input size to prevent memory exhaustion (50MB)
const MAX_INPUT_SIZE: usize = 50 * 1024 * 1024;

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
    skim file.ts                       Read TypeScript with structure mode (cached)\n  \
    skim file.py --mode signatures     Extract Python signatures\n  \
    skim file.rs | bat -l rust         Skim Rust and highlight\n  \
    cat code.ts | skim - --lang=ts     Read from stdin (requires --language)\n  \
    skim - -l python < script.py       Short form language flag\n  \
    skim src/                          Process all files in directory recursively\n  \
    skim 'src/**/*.ts'                 Process all TypeScript files (glob pattern)\n  \
    skim '*.{js,ts}' --no-header       Process multiple files without headers\n  \
    skim . --jobs 8                    Process current directory with 8 threads\n  \
    skim file.ts --no-cache            Disable caching for pure transformation\n  \
    skim --clear-cache                 Clear all cached files\n\n\
For more info: https://github.com/dean0x/skim")]
struct Args {
    /// File, directory, or glob pattern to process (use '-' for stdin)
    #[arg(value_name = "FILE", required_unless_present = "clear_cache")]
    file: Option<String>,

    /// Transformation mode
    #[arg(short, long, value_enum, default_value = "structure")]
    #[arg(help = "Transformation mode: structure, signatures, types, full, or minimal")]
    mode: ModeArg,

    /// Override language detection (required for stdin, optional fallback otherwise)
    #[arg(short, long, value_enum)]
    #[arg(help = "Programming language: typescript, python, rust, go, java")]
    language: Option<LanguageArg>,

    /// Force parsing even if language unsupported
    #[arg(long)]
    force: bool,

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
}

/// Mode argument (clap value_enum wrapper)
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Structure,
    Signatures,
    Types,
    Full,
    Minimal,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Structure => Mode::Structure,
            ModeArg::Signatures => Mode::Signatures,
            ModeArg::Types => Mode::Types,
            ModeArg::Full => Mode::Full,
            ModeArg::Minimal => Mode::Minimal,
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
        }
    }
}

/// Build a TransformConfig from mode and optional max_lines
fn build_config(mode: Mode, max_lines: Option<usize>) -> TransformConfig {
    let mut config = TransformConfig::with_mode(mode);
    if let Some(n) = max_lines {
        config = config.with_max_lines(n);
    }
    config
}

/// Options for processing a single file
#[derive(Debug, Clone, Copy)]
struct ProcessOptions {
    /// Transformation mode
    mode: Mode,
    /// Explicit language override (None for auto-detection)
    explicit_lang: Option<Language>,
    /// Whether to use cache
    use_cache: bool,
    /// Whether to compute token statistics (for --show-stats)
    show_stats: bool,
    /// Maximum output lines (AST-aware truncation)
    max_lines: Option<usize>,
    /// Token budget for cascade mode
    token_budget: Option<usize>,
}

/// Result of processing a file
#[derive(Debug)]
struct ProcessResult {
    /// Transformed output
    output: String,
    /// Original token count (if computed)
    original_tokens: Option<usize>,
    /// Transformed token count (if computed)
    transformed_tokens: Option<usize>,
}

/// Report token statistics to stderr if token counts are available
fn report_token_stats(
    original_tokens: Option<usize>,
    transformed_tokens: Option<usize>,
    suffix: &str,
) {
    if let (Some(orig), Some(trans)) = (original_tokens, transformed_tokens) {
        let stats = tokens::TokenStats::new(orig, trans);
        eprintln!("\n[skim] {}{}", stats.format(), suffix);
    }
}

/// Check if path contains glob pattern characters
fn has_glob_pattern(path: &str) -> bool {
    path.contains('*') || path.contains('?') || path.contains('[')
}

/// Validate glob pattern to prevent path traversal attacks
fn validate_glob_pattern(pattern: &str) -> anyhow::Result<()> {
    // Reject absolute paths
    if pattern.starts_with('/') {
        anyhow::bail!(
            "Glob pattern must be relative (cannot start with '/')\n\
             Pattern: {}\n\
             Use relative paths like 'src/**/*.ts' instead of '/src/**/*.ts'",
            pattern
        );
    }

    // Reject patterns containing .. (parent directory traversal)
    if pattern.contains("..") {
        anyhow::bail!(
            "Glob pattern cannot contain '..' (parent directory traversal)\n\
             Pattern: {}\n\
             This prevents accessing files outside the current directory",
            pattern
        );
    }

    Ok(())
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
        match zero_hint {
            Some(hint) => anyhow::bail!("{flag_name} must be at least 1\n{hint}"),
            None => anyhow::bail!("{flag_name} must be at least 1"),
        }
    }
    if v > max {
        anyhow::bail!("{flag_name} value too high: {v} (maximum: {max})\n{max_reason}");
    }

    Ok(())
}

/// Count tokens, returning `usize::MAX` on failure (treats errors as over-budget).
///
/// Centralises the warning-on-error pattern used throughout the cascade logic.
fn count_tokens_or_max(text: &str) -> usize {
    tokens::count_tokens(text).unwrap_or_else(|e| {
        eprintln!("[skim] warning: token counting failed, treating as over-budget: {e}");
        usize::MAX
    })
}

/// Apply line-based truncation as a final fallback when all modes exceed the budget.
///
/// Emits a diagnostic to stderr and delegates to `truncate_to_token_budget`.
fn fallback_line_truncate(
    output: &str,
    language: Language,
    token_budget: usize,
    mode: Mode,
    known_token_count: Option<usize>,
) -> anyhow::Result<(String, Mode)> {
    eprintln!(
        "[skim] token budget: all modes exceeded budget, applying line truncation ({} mode)",
        mode.name(),
    );
    let truncated = truncate_to_token_budget(
        output,
        language,
        token_budget,
        count_tokens_or_max,
        known_token_count,
    )?;
    Ok((truncated, mode))
}

/// Cascade through transformation modes until output fits within `token_budget`.
///
/// Tries each mode from `starting_mode` through increasingly aggressive modes.
/// If no mode fits, applies line-based truncation as a final fallback.
/// Diagnostics are emitted to stderr only when escalating beyond the starting mode.
fn cascade_for_token_budget<F>(
    starting_mode: Mode,
    max_lines: Option<usize>,
    token_budget: usize,
    language: Language,
    transform_fn: F,
) -> anyhow::Result<(String, Mode)>
where
    F: Fn(&TransformConfig) -> anyhow::Result<Option<String>>,
{
    let cascade = starting_mode.cascade_from_here();
    let mut last_output = String::new();
    let mut last_mode = starting_mode;
    let mut last_token_count: Option<usize> = None;

    let mut config = build_config(starting_mode, max_lines);

    // Serde-based languages produce at most 2 distinct outputs regardless of mode:
    // - Full/Minimal: original source (passthrough)
    // - Structure/Signatures/Types: structure-extracted (all identical)
    // Short-circuit to avoid up to 3 redundant parse+transform cycles.
    if language.is_serde_based() {
        return cascade_serde(
            starting_mode,
            &mut config,
            token_budget,
            language,
            &transform_fn,
        );
    }

    for &mode in cascade {
        config.mode = mode;

        let Some(output) = transform_fn(&config)? else {
            continue;
        };

        let token_count = count_tokens_or_max(&output);

        if token_count <= token_budget {
            if mode != starting_mode {
                eprintln!(
                    "[skim] token budget: escalated from {} to {} mode ({} tokens)",
                    starting_mode.name(),
                    mode.name(),
                    token_count,
                );
            }
            return Ok((output, mode));
        }

        last_output = output;
        last_mode = mode;
        last_token_count = Some(token_count);
    }

    // Guard: no mode produced output (defensive; transform_fn currently always
    // returns Ok(Some(...)), but protects against future callers returning Ok(None)).
    if last_output.is_empty() {
        anyhow::bail!(
            "Token budget cascade: no transformation mode produced output. \
             Ensure the file is in a supported language or specify --language."
        );
    }

    fallback_line_truncate(
        &last_output,
        language,
        token_budget,
        last_mode,
        last_token_count,
    )
}

/// Serde-based cascade short-circuit for `cascade_for_token_budget`.
///
/// Serde languages (JSON, YAML, TOML) produce at most two distinct outputs:
/// passthrough (Full/Minimal) and structure-extracted (Structure/Signatures/Types).
/// This avoids up to 3 redundant parse+transform cycles in the generic cascade.
fn cascade_serde<F>(
    starting_mode: Mode,
    config: &mut TransformConfig,
    token_budget: usize,
    language: Language,
    transform_fn: &F,
) -> anyhow::Result<(String, Mode)>
where
    F: Fn(&TransformConfig) -> anyhow::Result<Option<String>>,
{
    // Try starting mode first
    let first_output = transform_fn(config)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Token budget cascade: no transformation mode produced output. \
                 Ensure the file is in a supported language or specify --language."
        )
    })?;

    let first_tokens = count_tokens_or_max(&first_output);
    if first_tokens <= token_budget {
        return Ok((first_output, starting_mode));
    }

    // If starting at Full/Minimal, try structure-extracted (the only other distinct output)
    if matches!(starting_mode, Mode::Full | Mode::Minimal) {
        config.mode = Mode::Structure;
        if let Some(extracted) = transform_fn(config)? {
            let extracted_tokens = count_tokens_or_max(&extracted);
            if extracted_tokens <= token_budget {
                eprintln!(
                    "[skim] token budget: escalated from {} to structure mode ({} tokens)",
                    starting_mode.name(),
                    extracted_tokens,
                );
                return Ok((extracted, Mode::Structure));
            }
            return fallback_line_truncate(
                &extracted,
                language,
                token_budget,
                Mode::Structure,
                Some(extracted_tokens),
            );
        }
    }

    // Starting mode was already Structure/Signatures/Types, or structure extraction
    // returned None (defensive). Fall back to line truncation on the first output.
    fallback_line_truncate(
        &first_output,
        language,
        token_budget,
        starting_mode,
        Some(first_tokens),
    )
}

/// Try to return a result from cache, handling token recount when needed.
///
/// Returns `Some(ProcessResult)` on cache hit, `None` on cache miss.
/// When stats are requested but the cached entry lacks token counts,
/// the original file is read to compute them on the fly.
fn try_cached_result(
    path: &Path,
    options: &ProcessOptions,
) -> anyhow::Result<Option<ProcessResult>> {
    if !options.use_cache {
        return Ok(None);
    }

    let Some(hit) = cache::read_cache(path, options.mode, options.max_lines, options.token_budget)
    else {
        return Ok(None);
    };

    // If stats are requested but the cache entry was written without them,
    // read the original file and count tokens for both source and output.
    let needs_recount = options.show_stats && hit.original_tokens.is_none();
    let (orig_tokens, trans_tokens) = if needs_recount {
        let contents = read_and_validate(path)?;
        match (
            tokens::count_tokens(&contents),
            tokens::count_tokens(&hit.content),
        ) {
            (Ok(orig), Ok(trans)) => (Some(orig), Some(trans)),
            _ => (None, None),
        }
    } else {
        (hit.original_tokens, hit.transformed_tokens)
    };

    Ok(Some(ProcessResult {
        output: hit.content,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
    }))
}

/// Read a file and validate it doesn't exceed the maximum input size.
fn read_and_validate(path: &Path) -> anyhow::Result<String> {
    let contents = fs::read_to_string(path)?;
    if contents.len() > MAX_INPUT_SIZE {
        anyhow::bail!(
            "File too large: {} bytes exceeds maximum of {} bytes ({}MB)",
            contents.len(),
            MAX_INPUT_SIZE,
            MAX_INPUT_SIZE / 1024 / 1024
        );
    }
    Ok(contents)
}

/// Transform file contents, trying auto-detection first and falling back to
/// `explicit_lang` when provided. Returns `(transformed_output, mode_used)`.
fn run_transform(
    contents: &str,
    path: &Path,
    options: &ProcessOptions,
) -> anyhow::Result<(String, Mode)> {
    let explicit_lang = options.explicit_lang;
    let transform_file = |config: &TransformConfig| -> anyhow::Result<Option<String>> {
        match transform_auto_with_config(contents, path, config) {
            Ok(output) => Ok(Some(output)),
            Err(e) => match explicit_lang {
                Some(language) => Ok(Some(transform_with_config(contents, language, config)?)),
                None => Err(e.into()),
            },
        }
    };

    if let Some(budget) = options.token_budget {
        let language = explicit_lang
            .or_else(|| rskim_core::detect_language_from_path(path))
            .unwrap_or(Language::TypeScript);

        cascade_for_token_budget(
            options.mode,
            options.max_lines,
            budget,
            language,
            transform_file,
        )
    } else {
        let config = build_config(options.mode, options.max_lines);
        let output = transform_file(&config)?.ok_or_else(|| {
            anyhow::anyhow!("Language detection failed and no --language specified")
        })?;
        Ok((output, options.mode))
    }
}

/// Process a single file and return transformed content with optional token statistics.
fn process_file(path: &Path, options: ProcessOptions) -> anyhow::Result<ProcessResult> {
    if let Some(result) = try_cached_result(path, &options)? {
        return Ok(result);
    }

    let contents = read_and_validate(path)?;
    let (result, mode_used) = run_transform(&contents, path, &options)?;

    let (orig_tokens, trans_tokens) = if options.show_stats {
        match (
            tokens::count_tokens(&contents),
            tokens::count_tokens(&result),
        ) {
            (Ok(orig), Ok(trans)) => (Some(orig), Some(trans)),
            _ => (None, None),
        }
    } else {
        (None, None)
    };

    if options.use_cache {
        let effective_mode = (mode_used != options.mode).then_some(mode_used);
        // Cache write failures are non-fatal; don't fail the transformation.
        let _ = cache::write_cache(&cache::CacheWriteParams {
            path,
            mode: options.mode,
            content: &result,
            original_tokens: orig_tokens,
            transformed_tokens: trans_tokens,
            max_lines: options.max_lines,
            token_budget: options.token_budget,
            effective_mode,
        });
    }

    Ok(ProcessResult {
        output: result,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
    })
}

/// Options for multi-file processing
#[derive(Debug, Clone, Copy)]
struct MultiFileOptions {
    process: ProcessOptions,
    no_header: bool,
    jobs: Option<usize>,
}

/// Process multiple files with parallel processing via rayon.
///
/// Used by both glob and directory inputs. Handles parallel execution,
/// error aggregation, and accumulated token statistics.
fn process_files(
    paths: Vec<PathBuf>,
    source_description: &str,
    options: MultiFileOptions,
) -> anyhow::Result<()> {
    if paths.is_empty() {
        anyhow::bail!("No files found: {}", source_description);
    }

    let process_options = options.process;

    let results: Vec<_> = if let Some(num_jobs) = options.jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_jobs)
            .build()?
            .install(|| {
                paths
                    .par_iter()
                    .map(|path| (path, process_file(path, process_options)))
                    .collect()
            })
    } else {
        paths
            .par_iter()
            .map(|path| (path, process_file(path, process_options)))
            .collect()
    };

    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    let mut success_count = 0;
    let mut error_count = 0;
    let mut total_original_tokens = 0usize;
    let mut total_transformed_tokens = 0usize;

    for (idx, (path, result)) in results.iter().enumerate() {
        match result {
            Ok(process_result) => {
                if !options.no_header && paths.len() > 1 {
                    if idx > 0 {
                        writeln!(writer)?;
                    }
                    writeln!(writer, "// === {} ===", path.display())?;
                }

                write!(writer, "{}", process_result.output)?;
                success_count += 1;

                if options.process.show_stats {
                    if let (Some(orig), Some(trans)) = (
                        process_result.original_tokens,
                        process_result.transformed_tokens,
                    ) {
                        total_original_tokens += orig;
                        total_transformed_tokens += trans;
                    }
                }
            }
            Err(e) => {
                eprintln!("Error processing {}: {}", path.display(), e);
                error_count += 1;
            }
        }
    }

    writer.flush()?;

    if success_count == 0 {
        anyhow::bail!("All {} file(s) failed to process", error_count);
    }

    if error_count > 0 {
        eprintln!(
            "\nProcessed {} file(s) successfully, {} failed",
            success_count, error_count
        );
    }

    if options.process.show_stats && total_original_tokens > 0 {
        let suffix = format!(" across {} file(s)", success_count);
        report_token_stats(
            Some(total_original_tokens),
            Some(total_transformed_tokens),
            &suffix,
        );
    }

    Ok(())
}

/// Process multiple files matched by glob pattern
fn process_glob(pattern: &str, options: MultiFileOptions) -> anyhow::Result<()> {
    validate_glob_pattern(pattern)?;

    let paths: Vec<_> = glob(pattern)?
        .filter_map(|entry| entry.ok())
        .filter(|p| {
            if !p.is_file() {
                return false;
            }
            // Reject symlinks to prevent access to files outside the working tree
            if let Ok(meta) = p.symlink_metadata() {
                if meta.file_type().is_symlink() {
                    eprintln!("Warning: Skipping symlink: {}", p.display());
                    return false;
                }
            }
            true
        })
        .collect();

    process_files(paths, &format!("pattern '{}'", pattern), options)
}

/// Collect all supported files from a directory recursively.
///
/// Walks the directory tree and filters for supported extensions
/// using `Language::from_path()`.
fn collect_files_from_directory(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    fn visit_dir(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            // Reject symlinks to prevent access to files outside the working tree
            let symlink_metadata = path.symlink_metadata()?;
            if symlink_metadata.file_type().is_symlink() {
                eprintln!("Warning: Skipping symlink: {}", path.display());
                continue;
            }

            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                visit_dir(&path, files)?;
            } else if metadata.is_file() && Language::from_path(&path).is_some() {
                files.push(path);
            }
        }

        Ok(())
    }

    visit_dir(dir, &mut files)?;

    files.sort();

    Ok(files)
}

/// Process all supported files in a directory recursively
fn process_directory(dir: &Path, options: MultiFileOptions) -> anyhow::Result<()> {
    let paths = collect_files_from_directory(dir)?;

    process_files(paths, &format!("directory '{}'", dir.display()), options)
}

/// Validate all numeric CLI flags (`--jobs`, `--max-lines`, `--tokens`)
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
        args.tokens,
        "--tokens",
        MAX_TOKEN_BUDGET,
        Some("Use --tokens 1 to get the minimum possible output."),
        "This exceeds any reasonable LLM context window.",
    )?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    validate_args(&args)?;

    if args.clear_cache {
        cache::clear_cache()?;
        println!("Cache cleared successfully");
        return Ok(());
    }

    let mode = Mode::from(args.mode);
    let explicit_lang = args.language.map(Language::from);
    let use_cache = !args.no_cache;
    let max_lines = args.max_lines;
    let token_budget = args.tokens;

    let file = args.file.expect("FILE is required (enforced by clap)");

    if file == "-" {
        let mut buffer = String::new();
        let bytes_read = io::stdin()
            .take(MAX_INPUT_SIZE as u64 + 1)
            .read_to_string(&mut buffer)?;

        if bytes_read > MAX_INPUT_SIZE {
            anyhow::bail!(
                "Input too large: {} bytes exceeds maximum of {} bytes ({}MB)",
                bytes_read,
                MAX_INPUT_SIZE,
                MAX_INPUT_SIZE / 1024 / 1024
            );
        }

        let language = explicit_lang.ok_or_else(|| {
            anyhow::anyhow!(
                "Language detection failed: reading from stdin requires --language flag\n\
                 Example: cat file.ts | skim - --language=typescript"
            )
        })?;

        let result = if let Some(budget) = token_budget {
            let (output, _mode_used) =
                cascade_for_token_budget(mode, max_lines, budget, language, |config| {
                    Ok(Some(transform_with_config(&buffer, language, config)?))
                })?;
            output
        } else {
            let config = build_config(mode, max_lines);
            transform_with_config(&buffer, language, &config)?
        };

        let stdout = io::stdout();
        let mut writer = BufWriter::new(stdout.lock());
        write!(writer, "{}", result)?;
        writer.flush()?;

        if args.show_stats {
            let orig = tokens::count_tokens(&buffer).ok();
            let trans = tokens::count_tokens(&result).ok();
            report_token_stats(orig, trans, "");
        }

        return Ok(());
    }

    let path = PathBuf::from(&file);

    let process_options = ProcessOptions {
        mode,
        explicit_lang,
        use_cache,
        show_stats: args.show_stats,
        max_lines,
        token_budget,
    };

    let multi_options = MultiFileOptions {
        process: process_options,
        no_header: args.no_header,
        jobs: args.jobs,
    };

    if path.is_dir() {
        return process_directory(&path, multi_options);
    }

    if has_glob_pattern(&file) {
        return process_glob(&file, multi_options);
    }

    let process_result = process_file(&path, process_options)?;

    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    write!(writer, "{}", process_result.output)?;
    writer.flush()?;

    if args.show_stats {
        report_token_stats(
            process_result.original_tokens,
            process_result.transformed_tokens,
            "",
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_glob_pattern() {
        assert!(has_glob_pattern("*.ts"));
        assert!(has_glob_pattern("src/**/*.js"));
        assert!(has_glob_pattern("file?.py"));
        assert!(has_glob_pattern("file[123].rs"));
        assert!(!has_glob_pattern("file.ts"));
        assert!(!has_glob_pattern("src/main.rs"));
    }

    // ========================================================================
    // cascade_for_token_budget unit tests (S7)
    // ========================================================================

    /// Mock transform: returns the first N words from source for the matching mode.
    fn mock_transform<'a>(
        source: &'a str,
        mode_sizes: &'a [(Mode, usize)],
    ) -> impl Fn(&TransformConfig) -> anyhow::Result<Option<String>> + 'a {
        move |config: &TransformConfig| {
            for &(mode, size) in mode_sizes {
                if config.mode == mode {
                    let words: Vec<&str> = source.split_whitespace().take(size).collect();
                    return Ok(Some(words.join(" ")));
                }
            }
            Ok(None)
        }
    }

    #[test]
    fn test_cascade_returns_first_mode_when_within_budget() {
        // Structure output = 3 tokens, budget = 10 → no escalation
        let source = "word1 word2 word3 word4 word5 word6 word7 word8 word9 word10";
        let mode_sizes = vec![
            (Mode::Structure, 3),
            (Mode::Signatures, 2),
            (Mode::Types, 1),
        ];
        let transform = mock_transform(source, &mode_sizes);

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, None, 10, Language::TypeScript, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Structure);
        assert_eq!(output, "word1 word2 word3");
    }

    #[test]
    fn test_cascade_escalates_to_more_aggressive_mode() {
        // Structure = 20 tokens (over budget), Signatures = 8 (within budget)
        let source = "a b c d e f g h i j k l m n o p q r s t";
        let mode_sizes = vec![
            (Mode::Structure, 20),
            (Mode::Signatures, 8),
            (Mode::Types, 3),
        ];
        let transform = mock_transform(source, &mode_sizes);

        let (_output, mode_used) =
            cascade_for_token_budget(Mode::Structure, None, 10, Language::TypeScript, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Signatures);
    }

    #[test]
    fn test_cascade_falls_through_to_line_truncation() {
        // All modes exceed budget → should hit line truncation fallback
        let source = "a b c d e f g h i j k l m n o p q r s t";
        let mode_sizes = vec![
            (Mode::Structure, 20),
            (Mode::Signatures, 15),
            (Mode::Types, 12),
        ];
        let transform = mock_transform(source, &mode_sizes);

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, None, 5, Language::TypeScript, transform)
                .unwrap();

        // Should use the most aggressive mode that produced output
        assert_eq!(mode_used, Mode::Types);
        // Output should contain truncation marker or be within budget
        let token_count = tokens::count_tokens(&output).unwrap_or(usize::MAX);
        assert!(
            token_count <= 5 || output.is_empty(),
            "Final output should be within budget or empty, got {} tokens: {:?}",
            token_count,
            output
        );
    }

    #[test]
    fn test_cascade_single_mode_types() {
        // Starting at Types → only one mode in cascade, must fit or truncate
        let source = "a b c d e f g h i j";
        let mode_sizes = vec![(Mode::Types, 5)];
        let transform = mock_transform(source, &mode_sizes);

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Types, None, 10, Language::TypeScript, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Types);
        assert_eq!(output, "a b c d e");
    }

    #[test]
    fn test_cascade_errors_when_no_mode_produces_output() {
        // All modes return None → should error
        let transform = |_config: &TransformConfig| -> anyhow::Result<Option<String>> { Ok(None) };

        let result =
            cascade_for_token_budget(Mode::Structure, None, 100, Language::TypeScript, transform);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no transformation mode produced output"),);
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
}
