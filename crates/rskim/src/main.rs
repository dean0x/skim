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
    cat code.ts | skim - --lang=ts     Read from stdin with explicit language\n  \
    skim - -l python < script.py       Short form language flag\n  \
    skim 'src/**/*.ts'                 Process all TypeScript files (cached)\n  \
    skim '*.{js,ts}' --no-header       Process multiple files without headers\n  \
    skim file.ts --no-cache            Disable caching for pure transformation\n  \
    skim --clear-cache                 Clear all cached files\n\n\
For more info: https://github.com/dean0x/skim")]
struct Args {
    /// File to read (use '-' for stdin, supports glob patterns like '*.ts' or 'src/**/*.js')
    #[arg(value_name = "FILE", required_unless_present = "clear_cache")]
    file: Option<String>,

    /// Transformation mode
    #[arg(short, long, value_enum, default_value = "structure")]
    #[arg(help = "Transformation mode: structure, signatures, types, or full")]
    mode: ModeArg,

    /// Explicit language (required when reading from stdin, applies to all files when using globs)
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
    #[arg(short, long, help = "Number of parallel jobs for multi-file processing")]
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
        }
    }
}

/// Check if path contains glob pattern characters
fn has_glob_pattern(path: &str) -> bool {
    path.contains('*') || path.contains('?') || path.contains('[')
}

/// Process a single file and return transformed content and optionally original content
fn process_file(
    path: &Path,
    mode: Mode,
    explicit_lang: Option<Language>,
    use_cache: bool,
    include_original: bool,
) -> anyhow::Result<(String, Option<String>)> {
    // Try to read from cache if enabled
    let cached_result = if use_cache {
        cache::read_cache(path, mode)
    } else {
        None
    };

    // If we have cached result and don't need original, return early
    if let Some(ref cached) = cached_result {
        if !include_original {
            return Ok((cached.clone(), None));
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

    // If we have cached result, return it with original
    if let Some(cached) = cached_result {
        return Ok((cached, Some(contents)));
    }

    // Transform the file
    let result = match explicit_lang {
        Some(language) => transform(&contents, language, mode)?,
        None => transform_auto(&contents, path, mode)?,
    };

    // Write to cache if enabled
    if use_cache {
        // Ignore cache write errors (don't fail transformation if caching fails)
        let _ = cache::write_cache(path, mode, &result);
    }

    let original = if include_original { Some(contents) } else { None };
    Ok((result, original))
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
    let paths: Vec<_> = glob(pattern)?
        .filter_map(|entry| entry.ok())
        .filter(|p| p.is_file())
        .collect();

    if paths.is_empty() {
        anyhow::bail!("No files matched pattern: {}", pattern);
    }

    // Configure rayon thread pool if jobs specified
    let results: Vec<_> = if let Some(num_jobs) = jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_jobs)
            .build()?
            .install(|| {
                paths
                    .par_iter()
                    .map(|path| (path, process_file(path, mode, explicit_lang, use_cache, show_stats)))
                    .collect()
            })
    } else {
        // Use default rayon thread pool (number of CPUs)
        paths
            .par_iter()
            .map(|path| (path, process_file(path, mode, explicit_lang, use_cache, show_stats)))
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
            Ok((output, original)) => {
                // Add file separator header (unless disabled)
                if !no_header && paths.len() > 1 {
                    if idx > 0 {
                        writeln!(writer)?; // Blank line between files
                    }
                    writeln!(writer, "// === {} ===", path.display())?;
                }

                write!(writer, "{}", output)?;
                success_count += 1;

                // Accumulate token counts if show_stats is enabled
                if show_stats {
                    if let Some(orig) = original {
                        if let (Ok(orig_tokens), Ok(trans_tokens)) = (
                            tokens::count_tokens(orig),
                            tokens::count_tokens(output),
                        ) {
                            total_original_tokens += orig_tokens;
                            total_transformed_tokens += trans_tokens;
                        }
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
        anyhow::bail!(
            "All {} file(s) failed to process",
            error_count
        );
    }

    if error_count > 0 {
        eprintln!(
            "\nProcessed {} file(s) successfully, {} failed",
            success_count,
            error_count
        );
    }

    // Output token statistics if requested
    if show_stats && total_original_tokens > 0 {
        let stats = tokens::TokenStats::new(total_original_tokens, total_transformed_tokens);
        eprintln!("\n[skim] {} across {} file(s)", stats.format(), success_count);
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

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
            if let (Ok(orig_tokens), Ok(trans_tokens)) = (
                tokens::count_tokens(&buffer),
                tokens::count_tokens(&result),
            ) {
                let stats = tokens::TokenStats::new(orig_tokens, trans_tokens);
                eprintln!("\n[skim] {}", stats.format());
            }
        }

        return Ok(());
    }

    // Handle glob patterns
    if has_glob_pattern(&file) {
        return process_glob(&file, mode, explicit_lang, args.no_header, args.jobs, use_cache, args.show_stats);
    }

    // Handle single file
    let path = PathBuf::from(&file);
    let (result, original) = process_file(&path, mode, explicit_lang, use_cache, args.show_stats)?;

    // Output transformed result
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    write!(writer, "{}", result)?;
    writer.flush()?;

    // Output token statistics if requested
    if args.show_stats {
        if let Some(orig) = original {
            if let (Ok(orig_tokens), Ok(trans_tokens)) = (
                tokens::count_tokens(&orig),
                tokens::count_tokens(&result),
            ) {
                let stats = tokens::TokenStats::new(orig_tokens, trans_tokens);
                eprintln!("\n[skim] {}", stats.format());
            }
        }
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
