//! File source abstraction for loading source files from the corpus.
//!
//! `GitCloneSource` clones repos with `git`; `FixtureSource` reads from
//! a local directory. Both implement `FileSource` for testing and production.

use std::path::{Path, PathBuf};

use anyhow::Context;
use rskim_core::Language;

use crate::config::RepoEntry;
use crate::types::SourceFile;

/// Maximum file size to accept (100 KiB).
const MAX_FILE_SIZE: u64 = 100 * 1024;

/// Number of bytes to inspect for null bytes (binary detection).
const BINARY_PROBE_BYTES: usize = 8192;

/// File extensions explicitly excluded (data formats, not code).
const EXCLUDED_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "toml", "md", "markdown"];

/// Target language file extensions accepted by the corpus.
const TARGET_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "py", "go", "java"];

/// Abstraction over file loading — enables testing without network access.
pub trait FileSource: Send + Sync {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>>;
}

/// Production file source that clones repos from GitHub.
pub struct GitCloneSource {
    pub corpus_dir: PathBuf,
}

impl FileSource for GitCloneSource {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        let repo_name = extract_repo_name(&repo.url)?;
        let dest = self.corpus_dir.join(&repo_name);

        // Clone if not already present.
        if !dest.exists() {
            clone_repo(&repo.url, &repo.commit, &dest)
                .with_context(|| format!("cloning {}", repo.url))?;
        }

        walk_and_load(&dest)
    }
}

fn extract_repo_name(url: &str) -> anyhow::Result<String> {
    url.rsplit('/')
        .next()
        .map(|s| s.trim_end_matches(".git").to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("cannot extract repo name from URL: {url}"))
}

fn clone_repo(url: &str, commit: &str, dest: &Path) -> anyhow::Result<()> {
    // Full clone so we can checkout a specific commit.
    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(dest)
        .status()
        .context("running git clone")?;

    if !status.success() {
        // Depth-1 clone may not include the pinned commit; try full clone.
        let status = std::process::Command::new("git")
            .args(["clone", url])
            .arg(dest)
            .status()
            .context("running full git clone")?;

        if !status.success() {
            anyhow::bail!("git clone failed for {url}");
        }

        // Checkout the pinned commit.
        let status = std::process::Command::new("git")
            .args(["-C", dest.to_str().unwrap_or("."), "checkout", commit])
            .status()
            .context("running git checkout")?;

        if !status.success() {
            anyhow::bail!("git checkout {commit} failed in {}", dest.display());
        }
    }

    Ok(())
}

fn walk_and_load(root: &Path) -> anyhow::Result<Vec<SourceFile>> {
    let mut files = Vec::new();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false) // include dot-files but .gitignore is respected
        .build();

    for entry in walker {
        let entry = entry.context("walking directory")?;
        if entry.file_type().map(|t| !t.is_file()).unwrap_or(true) {
            continue;
        }

        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Skip excluded extensions.
        if EXCLUDED_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        // Skip non-target extensions.
        if !TARGET_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        // Skip files that are too large.
        if entry
            .metadata()
            .map(|m| m.len() > MAX_FILE_SIZE)
            .unwrap_or(false)
        {
            continue;
        }

        // Detect language from extension.
        let language = match Language::from_extension(&ext) {
            Some(lang) => lang,
            None => continue,
        };

        // Read and validate content.
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        // Binary detection: look for null bytes in first BINARY_PROBE_BYTES.
        let probe_len = bytes.len().min(BINARY_PROBE_BYTES);
        if bytes[..probe_len].contains(&0u8) {
            continue;
        }

        // Require valid UTF-8.
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };

        files.push(SourceFile {
            path: path.to_path_buf(),
            language,
            content,
        });
    }

    Ok(files)
}

/// Test file source that reads from a fixture directory.
pub struct FixtureSource {
    pub fixture_dir: PathBuf,
}

impl FileSource for FixtureSource {
    fn fetch_files(&self, _repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        walk_and_load(&self.fixture_dir)
    }
}

/// Load all source files from a directory (public helper for the codegen step).
pub fn load_fixture_files(dir: &Path) -> anyhow::Result<Vec<SourceFile>> {
    walk_and_load(dir)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    fn dummy_repo() -> RepoEntry {
        crate::config::RepoEntry {
            url: "https://github.com/example/repo".to_string(),
            commit: "4649aa9700619f94cf9c66876e9549d83420e16c".to_string(),
            language: "Rust".to_string(),
        }
    }

    #[test]
    fn fixture_source_loads_rust_file() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let rust_file = files.iter().find(|f| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "sample_rust.rs")
                .unwrap_or(false)
        });
        assert!(rust_file.is_some(), "should find sample_rust.rs");
        assert_eq!(rust_file.unwrap().language, Language::Rust);
    }

    #[test]
    fn binary_file_is_skipped() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let bin_file = files.iter().find(|f| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "binary_file.bin")
                .unwrap_or(false)
        });
        // .bin has no target extension so it's excluded by extension filter
        assert!(bin_file.is_none(), "binary file should be skipped");
    }

    #[test]
    fn json_file_is_skipped() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let json_file = files.iter().find(|f| {
            f.path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "json")
                .unwrap_or(false)
        });
        assert!(json_file.is_none(), "json files should be excluded");
    }

    #[test]
    fn empty_file_is_included() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let empty = files.iter().find(|f| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "empty_file.rs")
                .unwrap_or(false)
        });
        assert!(empty.is_some(), "empty Rust file should be included");
        assert_eq!(empty.unwrap().content, "");
    }

    #[test]
    fn fixture_source_is_trait_object_compatible() {
        let source: Box<dyn FileSource> = Box::new(FixtureSource {
            fixture_dir: fixtures_dir(),
        });
        // Just verifying it compiles as a trait object.
        let _ = source.fetch_files(&dummy_repo());
    }
}
