//! Utility helpers for the `skim agents` subcommand.

use std::path::Path;

/// Replace home directory prefix with ~ for display.
pub(super) fn tilde_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            return format!("~/{}", stripped.display());
        }
    }
    path.display().to_string()
}

/// Maximum directory traversal depth for recursive helpers.
pub(super) const MAX_TRAVERSAL_DEPTH: usize = 10;

/// Count files with a specific extension recursively in a directory.
pub(super) fn count_files_recursive(dir: &Path, extension: &str) -> usize {
    count_files_recursive_inner(dir, extension, 0)
}

fn count_files_recursive_inner(dir: &Path, extension: &str, depth: usize) -> usize {
    if depth >= MAX_TRAVERSAL_DEPTH {
        return 0;
    }
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_dir() {
                count += count_files_recursive_inner(&entry.path(), extension, depth + 1);
            } else if ft.is_file()
                && entry.path().extension().and_then(|e| e.to_str()) == Some(extension)
            {
                count += 1;
            }
        }
    }
    count
}

/// Count files (non-directories) directly in a directory.
pub(super) fn count_files_in_dir(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|ft| ft.is_file()))
                .count()
        })
        .unwrap_or(0)
}

/// Get human-readable size of a directory.
pub(super) fn dir_size_human(dir: &Path) -> String {
    let bytes = dir_size_bytes(dir);
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// Calculate total size of all files in a directory tree.
fn dir_size_bytes(dir: &Path) -> u64 {
    dir_size_bytes_inner(dir, 0)
}

fn dir_size_bytes_inner(dir: &Path, depth: usize) -> u64 {
    if depth >= MAX_TRAVERSAL_DEPTH {
        return 0;
    }
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_dir() {
                total += dir_size_bytes_inner(&entry.path(), depth + 1);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_tilde_path_with_home() {
        if let Some(home) = dirs::home_dir() {
            let path = home.join("some").join("path");
            let result = tilde_path(&path);
            assert!(
                result.starts_with("~/"),
                "expected ~/ prefix, got: {result}"
            );
            assert!(
                result.contains("some/path"),
                "expected path suffix, got: {result}"
            );
        }
    }

    #[test]
    fn test_tilde_path_without_home_prefix() {
        let path = PathBuf::from("/tmp/not-home/file");
        let result = tilde_path(&path);
        assert_eq!(result, "/tmp/not-home/file");
    }

    #[test]
    fn test_count_files_recursive_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(count_files_recursive(dir.path(), "jsonl"), 0);
    }

    #[test]
    fn test_count_files_recursive_with_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("b.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("c.txt"), "hello").unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("d.jsonl"), "{}").unwrap();
        assert_eq!(count_files_recursive(dir.path(), "jsonl"), 3);
    }

    #[test]
    fn test_dir_size_human_formats() {
        let dir = tempfile::TempDir::new().unwrap();
        let size = dir_size_human(dir.path());
        assert!(
            size.contains("bytes") || size.contains("KB"),
            "unexpected size format: {size}"
        );
    }
}
