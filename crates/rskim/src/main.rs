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

use rskim_core::{transform, transform_auto, Language, Mode};

/// Maximum input size to prevent memory exhaustion (50MB)
const MAX_INPUT_SIZE: usize = 50 * 1024 * 1024;

/// Maximum number of parallel jobs (threads) to prevent resource exhaustion
const MAX_JOBS: usize = 128;

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
    #[arg(help = "Transformation mode: structure, signatures, types, or full")]
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
}

/// Mode argument (clap value_enum wrapper)
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Structure,
    Signatures,
    Types,
    Full,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Structure => Mode::Structure,
            ModeArg::Signatures => Mode::Signatures,
            ModeArg::Types => Mode::Types,
            ModeArg::Full => Mode::Full,
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
        }
    }
}

/// Options for processing a file (reduces function parameters)
#[derive(Debug, Clone, Copy)]
struct ProcessOptions {
    /// Transformation mode
    mode: Mode,
    /// Explicit language override (None for auto-detection)
    explicit_lang: Option<Language>,
    /// Whether to use cache
    use_cache: bool,
    /// Whether to include original content for token counting
    include_original: bool,
}

impl ProcessOptions {
    /// Create new processing options
    fn new(
        mode: Mode,
        explicit_lang: Option<Language>,
        use_cache: bool,
        include_original: bool,
    ) -> Self {
        Self {
            mode,
            explicit_lang,
            use_cache,
            include_original,
        }
    }
}

/// Result of processing a file (replaces tuple return)
#[derive(Debug)]
struct ProcessResult {
    /// Transformed output
    output: String,
    /// Original file content (if needed for token counting)
    #[allow(dead_code)]
    original: Option<String>,
    /// Original token count (if computed)
    original_tokens: Option<usize>,
    /// Transformed token count (if computed)
    transformed_tokens: Option<usize>,
}

impl ProcessResult {
    /// Create a new ProcessResult
    fn new(
        output: String,
        original: Option<String>,
        original_tokens: Option<usize>,
        transformed_tokens: Option<usize>,
    ) -> Self {
        Self {
            output,
            original,
            original_tokens,
            transformed_tokens,
        }
    }
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

/// Process a single file and return transformed content and optionally original content
fn process_file(path: &Path, options: ProcessOptions) -> anyhow::Result<ProcessResult> {
    // Try to read from cache if enabled
    let cached_result = if options.use_cache {
        cache::read_cache(path, options.mode)
    } else {
        None
    };

    // If we have cached result with token counts, return without reading file
    if let Some((ref content, orig_tokens, trans_tokens)) = cached_result {
        if !options.include_original && orig_tokens.is_some() && trans_tokens.is_some() {
            return Ok(ProcessResult::new(
                content.clone(),
                None,
                orig_tokens,
                trans_tokens,
            ));
        }
    }

    // Need to read the file (either for transformation or for token counting)
    let contents = fs::read_to_string(path)?;

    if contents.len() > MAX_INPUT_SIZE {
        anyhow::bail!(
            "File too large: {} bytes exceeds maximum of {} bytes ({}MB)",
            contents.len(),
            MAX_INPUT_SIZE,
            MAX_INPUT_SIZE / 1024 / 1024
        );
    }

    // If we have cached result, return it with original content
    if let Some((content, orig_tokens, trans_tokens)) = cached_result {
        return Ok(ProcessResult::new(
            content,
            Some(contents),
            orig_tokens,
            trans_tokens,
        ));
    }

    // Transform the file
    // ARCHITECTURE: Option B - Always try auto-detection first, use explicit_lang as fallback
    // This allows mixed-language directories while still supporting edge cases like .inc files
    let result = match transform_auto(&contents, path, options.mode) {
        Ok(output) => output,
        Err(e) => {
            // Auto-detection failed - use explicit language as fallback if provided
            if let Some(language) = options.explicit_lang {
                transform(&contents, language, options.mode)?
            } else {
                // No fallback available - propagate the auto-detection error
                return Err(e.into());
            }
        }
    };

    // Count tokens if stats are needed
    let (orig_tokens, trans_tokens) = if options.include_original {
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

    // Write to cache if enabled
    if options.use_cache {
        // Ignore cache write errors (don't fail transformation if caching fails)
        let _ = cache::write_cache(path, options.mode, &result, orig_tokens, trans_tokens);
    }

    let original = if options.include_original {
        Some(contents)
    } else {
        None
    };
    Ok(ProcessResult::new(
        result,
        original,
        orig_tokens,
        trans_tokens,
    ))
}

/// Options for multi-file processing
#[derive(Debug, Clone, Copy)]
struct MultiFileOptions {
    mode: Mode,
    explicit_lang: Option<Language>,
    no_header: bool,
    jobs: Option<usize>,
    use_cache: bool,
    show_stats: bool,
}

/// Process multiple files (with parallel processing)
///
/// ARCHITECTURE: Generic file processor used by both glob and directory inputs.
/// Handles parallel processing, error aggregation, and statistics.
fn process_files(
    paths: Vec<PathBuf>,
    source_description: &str,
    options: MultiFileOptions,
) -> anyhow::Result<()> {
    if paths.is_empty() {
        anyhow::bail!("No files found: {}", source_description);
    }

    // Create process options
    let process_options = ProcessOptions::new(
        options.mode,
        options.explicit_lang,
        options.use_cache,
        options.show_stats,
    );

    // Configure rayon thread pool if jobs specified
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
        // Use default rayon thread pool (number of CPUs)
        paths
            .par_iter()
            .map(|path| (path, process_file(path, process_options)))
            .collect()
    };

    // Write results to stdout (sequentially to maintain order)
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    let mut success_count = 0;
    let mut error_count = 0;
    let mut total_original_tokens = 0usize;
    let mut total_transformed_tokens = 0usize;

    for (idx, (path, result)) in results.iter().enumerate() {
        match result {
            Ok(process_result) => {
                // Add file separator header (unless disabled)
                if !options.no_header && paths.len() > 1 {
                    if idx > 0 {
                        writeln!(writer)?; // Blank line between files
                    }
                    writeln!(writer, "// === {} ===", path.display())?;
                }

                write!(writer, "{}", process_result.output)?;
                success_count += 1;

                // Accumulate token counts if show_stats is enabled (use cached counts)
                if options.show_stats {
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

    // Output token statistics if requested
    if options.show_stats && total_original_tokens > 0 {
        let suffix = format!(" across {} file(s)", success_count);
        report_token_stats(
            Some(total_original_tokens),
            Some(total_transformed_tokens),
            &suffix,
        );
    }

    Ok(())
}

/// Process multiple files matched by glob pattern (with parallel processing)
fn process_glob(
    pattern: &str,
    mode: Mode,
    explicit_lang: Option<Language>,
    no_header: bool,
    jobs: Option<usize>,
    use_cache: bool,
    show_stats: bool,
) -> anyhow::Result<()> {
    // Validate glob pattern for security
    validate_glob_pattern(pattern)?;

    let paths: Vec<_> = glob(pattern)?
        .filter_map(|entry| entry.ok())
        .filter(|p| {
            // Only process regular files, not directories
            if !p.is_file() {
                return false;
            }

            // Security: Reject symlinks to prevent access to sensitive files
            // A malicious user could create symlinks pointing to /etc/passwd, SSH keys, etc.
            if let Ok(metadata) = p.symlink_metadata() {
                if metadata.file_type().is_symlink() {
                    eprintln!("Warning: Skipping symlink: {}", p.display());
                    return false;
                }
            }

            true
        })
        .collect();

    let options = MultiFileOptions {
        mode,
        explicit_lang,
        no_header,
        jobs,
        use_cache,
        show_stats,
    };

    process_files(paths, &format!("pattern '{}'", pattern), options)
}

/// Collect all supported files from a directory recursively
///
/// ARCHITECTURE: Walks directory tree, filters for supported extensions.
/// Uses Language::from_path() for extension validation.
fn collect_files_from_directory(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    use std::fs;

    let mut files = Vec::new();

    fn visit_dir(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            // Security: Reject symlinks to prevent access to sensitive files
            // Use symlink_metadata() to check the link itself, not the target
            let symlink_metadata = path.symlink_metadata()?;
            if symlink_metadata.file_type().is_symlink() {
                eprintln!("Warning: Skipping symlink: {}", path.display());
                continue;
            }

            // Get regular metadata for is_dir/is_file checks
            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                // Recurse into subdirectories
                visit_dir(&path, files)?;
            } else if metadata.is_file() {
                // Check if file has supported extension
                if Language::from_path(&path).is_some() {
                    files.push(path);
                }
            }
        }

        Ok(())
    }

    visit_dir(dir, &mut files)?;

    // Sort for deterministic output
    files.sort();

    Ok(files)
}

/// Process all supported files in a directory recursively
fn process_directory(
    dir: &Path,
    mode: Mode,
    explicit_lang: Option<Language>,
    no_header: bool,
    jobs: Option<usize>,
    use_cache: bool,
    show_stats: bool,
) -> anyhow::Result<()> {
    let paths = collect_files_from_directory(dir)?;

    let options = MultiFileOptions {
        mode,
        explicit_lang,
        no_header,
        jobs,
        use_cache,
        show_stats,
    };

    process_files(paths, &format!("directory '{}'", dir.display()), options)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Validate --jobs parameter to prevent resource exhaustion
    if let Some(jobs) = args.jobs {
        if jobs == 0 {
            anyhow::bail!("--jobs must be at least 1");
        }
        if jobs > MAX_JOBS {
            anyhow::bail!(
                "--jobs value too high: {} (maximum: {})\n\
                 Using too many threads can exhaust system resources.\n\
                 Recommended: Use default (number of CPUs) or specify a moderate value.",
                jobs,
                MAX_JOBS
            );
        }
    }

    // Handle clear-cache command
    if args.clear_cache {
        cache::clear_cache()?;
        println!("Cache cleared successfully");
        return Ok(());
    }

    let mode = Mode::from(args.mode);
    let explicit_lang = args.language.map(Language::from);
    // Cache is enabled by default, disabled only if --no-cache is specified
    let use_cache = !args.no_cache;

    // File is required at this point (enforced by clap)
    let file = args.file.expect("FILE is required");

    // Handle stdin
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

        let result = transform(&buffer, language, mode)?;

        // Output transformed result
        let stdout = io::stdout();
        let mut writer = BufWriter::new(stdout.lock());
        write!(writer, "{}", result)?;
        writer.flush()?;

        // Output token statistics if requested
        if args.show_stats {
            if let (Ok(orig_tokens), Ok(trans_tokens)) =
                (tokens::count_tokens(&buffer), tokens::count_tokens(&result))
            {
                let stats = tokens::TokenStats::new(orig_tokens, trans_tokens);
                eprintln!("\n[skim] {}", stats.format());
            }
        }

        return Ok(());
    }

    // Check if input is a directory
    let path = PathBuf::from(&file);
    if path.is_dir() {
        return process_directory(
            &path,
            mode,
            explicit_lang,
            args.no_header,
            args.jobs,
            use_cache,
            args.show_stats,
        );
    }

    // Handle glob patterns
    if has_glob_pattern(&file) {
        return process_glob(
            &file,
            mode,
            explicit_lang,
            args.no_header,
            args.jobs,
            use_cache,
            args.show_stats,
        );
    }

    // Handle single file
    let options = ProcessOptions::new(mode, explicit_lang, use_cache, args.show_stats);
    let process_result = process_file(&path, options)?;

    // Output transformed result
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    write!(writer, "{}", process_result.output)?;
    writer.flush()?;

    // Output token statistics if requested (use cached counts)
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
}
