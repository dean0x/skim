//! Single-file processing pipeline.
//!
//! Handles reading, transforming, caching, and outputting a single file or
//! stdin stream. Multi-file orchestration lives in [`crate::multi`].

use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use rskim_core::{
    detect_language_from_path, transform_auto_with_config, transform_with_config, Language, Mode,
    TransformConfig,
};

use crate::{cache, cascade, tokens};

/// Maximum input size to prevent memory exhaustion (50MB)
const MAX_INPUT_SIZE: usize = 50 * 1024 * 1024;

/// Options for processing a single file
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessOptions {
    /// Transformation mode
    pub(crate) mode: Mode,
    /// Explicit language override (None for auto-detection)
    pub(crate) explicit_lang: Option<Language>,
    /// Whether to use cache
    pub(crate) use_cache: bool,
    /// Whether to compute token statistics (for --show-stats)
    pub(crate) show_stats: bool,
    /// Maximum output lines (AST-aware truncation)
    pub(crate) max_lines: Option<usize>,
    /// Last N lines truncation (mutually exclusive with max_lines)
    pub(crate) last_lines: Option<usize>,
    /// Token budget for cascade mode
    pub(crate) token_budget: Option<usize>,
}

/// Result of processing a file
#[derive(Debug)]
#[must_use]
pub(crate) struct ProcessResult {
    /// Transformed output
    pub(crate) output: String,
    /// Original token count (if computed)
    pub(crate) original_tokens: Option<usize>,
    /// Transformed token count (if computed)
    pub(crate) transformed_tokens: Option<usize>,
    /// Whether the output guardrail was triggered (compressed > raw)
    pub(crate) guardrail_triggered: bool,
}

/// Count tokens for both original and transformed text, returning `(None, None)` on failure.
///
/// Centralises the paired token-counting pattern used across the processing pipeline.
fn count_token_pair(original: &str, transformed: &str) -> (Option<usize>, Option<usize>) {
    match (
        tokens::count_tokens(original),
        tokens::count_tokens(transformed),
    ) {
        (Ok(orig), Ok(trans)) => (Some(orig), Some(trans)),
        _ => (None, None),
    }
}

/// Report token statistics to stderr if token counts are available
pub(crate) fn report_token_stats(
    original_tokens: Option<usize>,
    transformed_tokens: Option<usize>,
    suffix: &str,
) {
    if let (Some(orig), Some(trans)) = (original_tokens, transformed_tokens) {
        let stats = tokens::TokenStats::new(orig, trans);
        eprintln!("\n[skim] {}{}", stats.format(), suffix);
    }
}

/// Write a single-input result to stdout and optionally report token stats to stderr.
///
/// Used by both `process_stdin` and the single-file path in `main()`.
/// Multi-file paths use their own output logic in `process_files()`.
pub(crate) fn write_result_and_stats(
    result: &ProcessResult,
    show_stats: bool,
) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    write!(writer, "{}", result.output)?;
    writer.flush()?;

    if show_stats {
        report_token_stats(result.original_tokens, result.transformed_tokens, "");
    }

    Ok(())
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

    let Some(hit) = cache::read_cache(
        path,
        options.mode,
        options.max_lines,
        options.last_lines,
        options.token_budget,
    ) else {
        return Ok(None);
    };

    // If stats are requested but the cache entry was written without them,
    // read the original file and count tokens for both source and output.
    let needs_recount = options.show_stats && hit.original_tokens.is_none();
    let (orig_tokens, trans_tokens) = if needs_recount {
        let contents = read_and_validate(path)?;
        count_token_pair(&contents, &hit.content)
    } else {
        (hit.original_tokens, hit.transformed_tokens)
    };

    Ok(Some(ProcessResult {
        output: hit.content,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
        guardrail_triggered: false,
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
        // Try auto-detection first; fall back to explicit language if provided.
        let auto_result = transform_auto_with_config(contents, path, config);
        if let Ok(output) = auto_result {
            return Ok(Some(output));
        }
        let Some(language) = explicit_lang else {
            return Err(auto_result.unwrap_err().into());
        };
        Ok(Some(transform_with_config(contents, language, config)?))
    };

    match options.token_budget {
        Some(budget) => {
            let language = explicit_lang
                .or_else(|| detect_language_from_path(path))
                .unwrap_or_else(|| {
                    eprintln!(
                        "[skim] warning: language detection failed for '{}', defaulting to TypeScript",
                        path.display(),
                    );
                    Language::TypeScript
                });

            cascade::cascade_for_token_budget(
                options.mode,
                options.max_lines,
                options.last_lines,
                budget,
                language,
                transform_file,
            )
        }
        None => {
            let config = cascade::build_config(options.mode, options.max_lines, options.last_lines);
            let output = transform_file(&config)?.ok_or_else(|| {
                anyhow::anyhow!("Language detection failed and no --language specified")
            })?;
            Ok((output, options.mode))
        }
    }
}

/// Process stdin input and return transformed content with optional token statistics.
///
/// Reads from stdin with a size limit, resolves the language from `--language` or
/// `--filename`, transforms the source (with optional token-budget cascade), and
/// computes token stats when `show_stats` is enabled.
pub(crate) fn process_stdin(
    options: ProcessOptions,
    filename_hint: Option<&str>,
) -> anyhow::Result<ProcessResult> {
    let mut buffer = String::with_capacity(64 * 1024);
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

    let filename_lang = filename_hint.and_then(|f| Language::from_path(Path::new(f)));

    let language = options.explicit_lang.or(filename_lang).ok_or_else(|| {
        if let Some(fname) = filename_hint {
            anyhow::anyhow!(
                "Language detection failed: unrecognized filename '{}'\n\
                 Supported extensions: .ts, .tsx, .js, .jsx, .py, .rs, .go, .java, .c, .h, .cpp, .hpp, .cxx, .cc, .md, .json, .yaml, .yml, .toml\n\
                 Hint: use --language to specify the language explicitly\n\
                 Example: cat file | skim - --language=typescript",
                fname
            )
        } else {
            anyhow::anyhow!(
                "Language detection failed: reading from stdin requires --language or --filename\n\
                 Example: cat file.ts | skim - --language=typescript\n\
                 Example: git show HEAD:main.rs | skim - --filename=main.rs"
            )
        }
    })?;

    let transformed = match options.token_budget {
        Some(budget) => {
            let (output, _mode) = cascade::cascade_for_token_budget(
                options.mode,
                options.max_lines,
                options.last_lines,
                budget,
                language,
                |config| Ok(Some(transform_with_config(&buffer, language, config)?)),
            )?;
            output
        }
        None => {
            let config = cascade::build_config(options.mode, options.max_lines, options.last_lines);
            transform_with_config(&buffer, language, &config)?
        }
    };

    // Apply output guardrail: if compressed output is larger than raw, emit raw instead.
    // Same protection as process_file; token counting happens after so stats reflect
    // the final output.
    let (final_output, guardrail_triggered) =
        if options.mode != Mode::Full && options.token_budget.is_none() {
            let outcome = crate::output::guardrail::apply_to_stderr(buffer.clone(), transformed)?;
            let triggered = outcome.was_triggered();
            (outcome.into_output(), triggered)
        } else {
            (transformed, false)
        };

    let (orig_tokens, trans_tokens) = if options.show_stats {
        count_token_pair(&buffer, &final_output)
    } else {
        (None, None)
    };

    Ok(ProcessResult {
        output: final_output,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
        guardrail_triggered,
    })
}

/// Process a single file and return transformed content with optional token statistics.
pub(crate) fn process_file(path: &Path, options: ProcessOptions) -> anyhow::Result<ProcessResult> {
    if let Some(result) = try_cached_result(path, &options)? {
        return Ok(result);
    }

    let contents = read_and_validate(path)?;
    let (result, mode_used) = run_transform(&contents, path, &options)?;

    // Apply output guardrail: if compressed output is larger than raw, emit raw instead.
    // Token counting happens AFTER this decision so stats reflect the final output.
    let (final_output, guardrail_triggered) =
        if options.mode != Mode::Full && options.token_budget.is_none() {
            let outcome = crate::output::guardrail::apply_to_stderr(contents.clone(), result)?;
            let triggered = outcome.was_triggered();
            (outcome.into_output(), triggered)
        } else {
            (result, false)
        };

    let (orig_tokens, trans_tokens) = if options.show_stats {
        count_token_pair(&contents, &final_output)
    } else {
        (None, None)
    };

    // Cache the transform result (post-guardrail). Cache write failures are
    // non-fatal; don't fail the transformation.
    if options.use_cache {
        let effective_mode = (mode_used != options.mode).then_some(mode_used);
        let _ = cache::write_cache(&cache::CacheWriteParams {
            path,
            mode: options.mode,
            content: &final_output,
            original_tokens: orig_tokens,
            transformed_tokens: trans_tokens,
            max_lines: options.max_lines,
            last_lines: options.last_lines,
            token_budget: options.token_budget,
            effective_mode,
        });
    }

    Ok(ProcessResult {
        output: final_output,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
        guardrail_triggered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // count_token_pair tests
    // ========================================================================

    #[test]
    fn count_token_pair_returns_some_for_valid_input() {
        let (orig, trans) = count_token_pair("hello world", "hello");
        assert!(orig.is_some(), "original tokens should be Some");
        assert!(trans.is_some(), "transformed tokens should be Some");
        assert!(
            orig.unwrap() > trans.unwrap(),
            "original should have more tokens than transformed"
        );
    }

    #[test]
    fn count_token_pair_returns_some_for_empty_strings() {
        let (orig, trans) = count_token_pair("", "");
        assert_eq!(orig, Some(0));
        assert_eq!(trans, Some(0));
    }

    #[test]
    fn count_token_pair_original_equals_transformed_for_identical_input() {
        let text = "fn main() { println!(\"hello\"); }";
        let (orig, trans) = count_token_pair(text, text);
        assert_eq!(orig, trans);
    }

    // ========================================================================
    // report_token_stats tests
    // ========================================================================

    #[test]
    fn report_token_stats_does_not_panic_with_none_values() {
        // Should be a no-op when tokens are None
        report_token_stats(None, None, "");
        report_token_stats(Some(100), None, "");
        report_token_stats(None, Some(50), "");
    }

    #[test]
    fn report_token_stats_does_not_panic_with_valid_values() {
        // Should write to stderr without panicking
        report_token_stats(Some(1000), Some(200), " (test)");
    }

    // ========================================================================
    // read_and_validate tests
    // ========================================================================

    #[test]
    fn read_and_validate_rejects_nonexistent_file() {
        let result = read_and_validate(Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
    }

    // ========================================================================
    // ProcessResult #[must_use] compile-time guard
    // ========================================================================

    #[test]
    fn process_result_fields_accessible() {
        let result = ProcessResult {
            output: "test".to_string(),
            original_tokens: Some(10),
            transformed_tokens: Some(5),
            guardrail_triggered: false,
        };
        assert_eq!(result.output, "test");
        assert_eq!(result.original_tokens, Some(10));
        assert_eq!(result.transformed_tokens, Some(5));
        assert!(!result.guardrail_triggered);
    }
}
