//! Multi-file processing: glob patterns, directory traversal, and parallel execution.
//!
//! Orchestrates [`crate::process::process_file`] over multiple inputs using rayon
//! for parallelism.

use glob::glob;
use rayon::prelude::*;
use std::fs;
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
}

/// Check if path contains glob pattern characters
pub(crate) fn has_glob_pattern(path: &str) -> bool {
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
pub(crate) fn process_glob(pattern: &str, options: MultiFileOptions) -> anyhow::Result<()> {
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
pub(crate) fn process_directory(dir: &Path, options: MultiFileOptions) -> anyhow::Result<()> {
    let paths = collect_files_from_directory(dir)?;

    process_files(paths, &format!("directory '{}'", dir.display()), options)
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
        // Tilde is not expanded by the glob crate (treated as literal),
        // so it is safe to allow as a relative pattern component.
        assert!(validate_glob_pattern("~/*.ts").is_ok());
    }
}
