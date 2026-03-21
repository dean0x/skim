//! Multi-file processing: glob patterns, directory traversal, and parallel execution.
//!
//! Orchestrates [`crate::process::process_file`] over multiple inputs using rayon
//! for parallelism. Uses the `ignore` crate (from ripgrep) for directory walking,
//! which respects `.gitignore`, `.ignore`, and `.git/info/exclude` by default.

use globset::GlobBuilder;
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use rskim_core::Language;

use crate::process::{process_file, report_token_stats, ProcessOptions};

/// Options for multi-file processing
#[derive(Debug, Clone, Copy)]
pub(crate) struct MultiFileOptions {
    pub(crate) process: ProcessOptions,
    pub(crate) no_header: bool,
    pub(crate) jobs: Option<usize>,
    pub(crate) no_ignore: bool,
}

/// Glob metacharacters recognised by skim.
///
/// Used for detecting glob patterns in user input and for splitting the
/// static directory prefix from the glob suffix in [`glob_walk_root`].
/// Defined once here and re-used in `main.rs::looks_like_file_or_glob`.
pub(crate) const GLOB_METACHARACTERS: &[char] = &['*', '?', '[', '{'];

/// Check if path contains glob pattern characters
pub(crate) fn has_glob_pattern(path: &str) -> bool {
    path.contains(GLOB_METACHARACTERS)
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

    // Reject Windows drive letter paths (e.g., "C:\..." or "D:/...")
    if pattern.len() >= 3 {
        let bytes = pattern.as_bytes();
        if bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/')
        {
            anyhow::bail!(
                "Glob pattern must be relative (absolute Windows path not allowed)\n\
                 Pattern: {}\n\
                 Use relative paths like 'src/**/*.ts' instead",
                pattern
            );
        }
    }

    // Reject Windows UNC paths (e.g., "\\server\share")
    if pattern.starts_with("\\\\") {
        anyhow::bail!(
            "Glob pattern must be relative (UNC path not allowed)\n\
             Pattern: {}\n\
             Use relative paths like 'src/**/*.ts' instead",
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

/// Configure an `ignore::WalkBuilder` with gitignore/hidden-file settings.
///
/// When `no_ignore` is false (default), the walker respects `.gitignore`,
/// global gitignore, `.git/info/exclude`, `.ignore` files, and skips hidden
/// files/directories. When true, all ignore rules are disabled.
fn configure_walker(builder: &mut WalkBuilder, no_ignore: bool) {
    let respect_ignore = !no_ignore;
    builder
        .hidden(respect_ignore)
        .git_ignore(respect_ignore)
        .git_global(respect_ignore)
        .git_exclude(respect_ignore)
        .ignore(respect_ignore)
        .parents(respect_ignore)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));
}

/// Extract the static directory prefix and glob override pattern from a user
/// glob pattern.
///
/// The walker needs a root directory to start from and an override pattern
/// to filter files. We split on `/`, taking leading segments that contain no
/// glob metacharacters ([`GLOB_METACHARACTERS`]), and join them as the root.
/// The remainder becomes the override pattern.
///
/// # Examples
///
/// ```text
/// "src/**/*.ts"       -> ("src",       "**/*.ts")
/// "*.ts"              -> (".",         "*.ts")
/// "src/utils/**/*.ts" -> ("src/utils", "**/*.ts")
/// "**/*.ts"           -> (".",         "**/*.ts")
/// "src/*.rs"          -> ("src",       "*.rs")
/// ```
fn glob_walk_root(pattern: &str) -> (&str, &str) {
    let segments: Vec<&str> = pattern.split('/').collect();
    let mut static_count = 0;

    for segment in &segments {
        if segment.contains(GLOB_METACHARACTERS) {
            break;
        }
        static_count += 1;
    }

    if static_count == 0 {
        (".", pattern)
    } else if static_count == segments.len() {
        // All segments are static (no glob metacharacters). Treat the
        // entire pattern as a root with a match-everything glob. This is
        // defensive -- callers are expected to verify glob chars exist
        // before calling, but we must not panic on unexpected input.
        (pattern, "**")
    } else {
        // Find the byte offset where the glob portion starts
        let root_end: usize = segments[..static_count]
            .iter()
            .map(|s| s.len())
            .sum::<usize>()
            + static_count
            - 1; // account for the '/' separators between segments

        let root = &pattern[..root_end];
        let rest = &pattern[root_end + 1..]; // skip the '/' separator
        (root, rest)
    }
}

/// Format a hint about `--no-ignore` when gitignore filtering is active.
fn no_ignore_hint(no_ignore: bool) -> &'static str {
    if no_ignore {
        ""
    } else {
        "\nHint: Files may be excluded by .gitignore. Use --no-ignore to include all files."
    }
}

/// Process multiple files with parallel processing via rayon.
///
/// Used by both glob and directory inputs. Handles parallel execution,
/// error aggregation, and accumulated token statistics.
///
/// Precondition: `paths` must be non-empty. Callers should validate and
/// produce a descriptive error (with `--no-ignore` hint) before calling.
fn process_files(paths: Vec<PathBuf>, options: MultiFileOptions) -> anyhow::Result<()> {
    debug_assert!(
        !paths.is_empty(),
        "BUG: process_files called with empty paths"
    );
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
    let mut guardrail_count = 0usize;
    let mut total_original_tokens = 0usize;
    let mut total_transformed_tokens = 0usize;

    let show_headers = !options.no_header && paths.len() > 1;

    for (idx, (path, result)) in results.iter().enumerate() {
        match result {
            Ok(process_result) => {
                if show_headers {
                    if idx > 0 {
                        writeln!(writer)?;
                    }
                    writeln!(writer, "// === {} ===", path.display())?;
                }

                write!(writer, "{}", process_result.output)?;
                success_count += 1;

                if process_result.guardrail_triggered {
                    guardrail_count += 1;
                }

                if let (Some(orig), Some(trans)) = (
                    process_result.original_tokens,
                    process_result.transformed_tokens,
                ) {
                    total_original_tokens += orig;
                    total_transformed_tokens += trans;
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

    if guardrail_count > 0 {
        let total = success_count + error_count;
        eprintln!(
            "[skim:guardrail] triggered on {}/{} files",
            guardrail_count, total
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

/// Process multiple files matched by glob pattern.
///
/// Uses `ignore::WalkBuilder` for directory walking (respects `.gitignore`
/// and hidden file rules by default), then filters entries with a
/// `globset::GlobMatcher` to match the user's glob pattern. This ensures
/// gitignore rules are applied *before* glob matching, so gitignored files
/// are excluded even when the glob would otherwise match them.
pub(crate) fn process_glob(pattern: &str, options: MultiFileOptions) -> anyhow::Result<()> {
    validate_glob_pattern(pattern)?;

    let (walk_root, glob_pattern) = glob_walk_root(pattern);

    let glob = GlobBuilder::new(glob_pattern)
        .literal_separator(false)
        .build()
        .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {}", pattern, e))?;
    let matcher = glob.compile_matcher();

    let mut builder = WalkBuilder::new(walk_root);
    configure_walker(&mut builder, options.no_ignore);

    // SECURITY: Symlink traversal is prevented by `follow_links(false)` on the
    // walker (configured in `configure_walker`). Path traversal via `..` is
    // rejected by `validate_glob_pattern`. Together these make `canonicalize()`
    // unnecessary here, avoiding a syscall per file in the hot path.
    let paths: Vec<PathBuf> = builder
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|entry| {
            entry
                .path()
                .strip_prefix(walk_root)
                .ok()
                .is_some_and(|rel| matcher.is_match(rel))
        })
        .map(|entry| entry.into_path())
        .collect();

    if paths.is_empty() {
        anyhow::bail!(
            "No files found: pattern '{}'{}",
            pattern,
            no_ignore_hint(options.no_ignore)
        );
    }

    process_files(paths, options)
}

/// Collect all supported files from a directory recursively.
///
/// Uses `ignore::WalkBuilder` to walk the directory tree, respecting
/// `.gitignore` and hidden file rules. Filters for supported extensions
/// using `Language::from_path()`.
///
/// Walk errors (e.g. permission-denied on individual entries) are
/// intentionally dropped via `filter_map(|e| e.ok())`. A single
/// unreadable file should not abort traversal of an entire directory
/// tree -- this matches ripgrep/fd behavior.
fn collect_files_from_directory(dir: &Path, no_ignore: bool) -> Vec<PathBuf> {
    let mut builder = WalkBuilder::new(dir);
    configure_walker(&mut builder, no_ignore);

    builder
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|entry| Language::from_path(entry.path()).is_some())
        .map(|entry| entry.into_path())
        .collect()
}

/// Process all supported files in a directory recursively
pub(crate) fn process_directory(dir: &Path, options: MultiFileOptions) -> anyhow::Result<()> {
    let paths = collect_files_from_directory(dir, options.no_ignore);

    if paths.is_empty() {
        anyhow::bail!(
            "No files found: directory '{}'{}",
            dir.display(),
            no_ignore_hint(options.no_ignore)
        );
    }

    process_files(paths, options)
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
        assert!(has_glob_pattern("*.{js,ts}"));
        assert!(has_glob_pattern("src/{a,b}.ts"));
        assert!(!has_glob_pattern("file.ts"));
        assert!(!has_glob_pattern("src/main.rs"));
    }

    #[test]
    fn test_validate_glob_pattern_rejects_absolute_unix_paths() {
        let result = validate_glob_pattern("/etc/passwd");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("cannot start with '/'"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_rejects_absolute_path_with_glob() {
        let result = validate_glob_pattern("/src/**/*.ts");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("cannot start with '/'"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_rejects_parent_traversal() {
        let result = validate_glob_pattern("../secret/*.ts");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("parent directory traversal"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_rejects_embedded_parent_traversal() {
        let result = validate_glob_pattern("src/../../etc/passwd");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("parent directory traversal"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_rejects_windows_drive_paths() {
        let result = validate_glob_pattern("C:\\Users\\*.ts");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("absolute Windows path"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_rejects_windows_unc_paths() {
        let result = validate_glob_pattern("\\\\server\\share\\*.ts");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("UNC path"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_accepts_valid_relative_patterns() {
        assert!(validate_glob_pattern("src/**/*.ts").is_ok());
        assert!(validate_glob_pattern("*.rs").is_ok());
        assert!(validate_glob_pattern("tests/fixtures/*.py").is_ok());
        assert!(validate_glob_pattern("**/*.{js,ts}").is_ok());
    }

    #[test]
    fn test_validate_glob_pattern_accepts_tilde_prefix() {
        // Tilde is not expanded by the ignore crate (treated as literal),
        // so it is safe to allow as a relative pattern component.
        assert!(validate_glob_pattern("~/*.ts").is_ok());
    }

    // ========================================================================
    // glob_walk_root unit tests
    // ========================================================================

    #[test]
    fn test_glob_walk_root_with_prefix() {
        assert_eq!(glob_walk_root("src/**/*.ts"), ("src", "**/*.ts"));
    }

    #[test]
    fn test_glob_walk_root_no_prefix() {
        assert_eq!(glob_walk_root("*.ts"), (".", "*.ts"));
    }

    #[test]
    fn test_glob_walk_root_multi_segment_prefix() {
        assert_eq!(
            glob_walk_root("src/utils/**/*.ts"),
            ("src/utils", "**/*.ts")
        );
    }

    #[test]
    fn test_glob_walk_root_doublestar_start() {
        assert_eq!(glob_walk_root("**/*.ts"), (".", "**/*.ts"));
    }

    #[test]
    fn test_glob_walk_root_single_dir_star() {
        assert_eq!(glob_walk_root("src/*.rs"), ("src", "*.rs"));
    }

    #[test]
    fn test_glob_walk_root_brace_expansion() {
        assert_eq!(glob_walk_root("src/**/*.{js,ts}"), ("src", "**/*.{js,ts}"));
    }

    #[test]
    fn test_glob_walk_root_question_mark() {
        assert_eq!(glob_walk_root("src/file?.ts"), ("src", "file?.ts"));
    }

    #[test]
    fn test_glob_walk_root_bracket() {
        assert_eq!(glob_walk_root("src/file[123].ts"), ("src", "file[123].ts"));
    }

    #[test]
    fn test_glob_walk_root_no_glob_chars_defensive() {
        // Defensive: if no glob chars exist, treat the entire pattern as root
        // with a match-everything glob. This should not happen in practice
        // (callers check has_glob_pattern first), but must not panic.
        assert_eq!(glob_walk_root("src/file.ts"), ("src/file.ts", "**"));
    }
}
