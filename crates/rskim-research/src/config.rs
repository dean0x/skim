//! Corpus configuration loader.
//!
//! Reads a TOML file describing the repos to clone and analyze.

use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;

/// Top-level corpus configuration parsed from TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct CorpusConfig {
    pub repos: Vec<RepoEntry>,
}

/// A single repository entry in the corpus config.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoEntry {
    pub url: String,
    pub commit: String,
    pub language: String,
}

/// The set of language strings accepted in corpus.toml.
const VALID_LANGUAGES: &[&str] = &["Rust", "TypeScript", "Python", "Go", "Java"];

/// Load and validate a corpus config from a TOML file.
///
/// # Errors
///
/// Returns an error if the file cannot be read, is not valid TOML, or
/// contains invalid values (unsupported language, bad commit SHA, non-https URL).
pub fn load_corpus_config(path: &Path) -> anyhow::Result<CorpusConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading corpus config from {}", path.display()))?;

    let config: CorpusConfig =
        toml::from_str(&raw).with_context(|| "parsing corpus config TOML")?;

    for (i, repo) in config.repos.iter().enumerate() {
        validate_repo(i, repo)?;
    }

    Ok(config)
}

fn validate_repo(index: usize, repo: &RepoEntry) -> anyhow::Result<()> {
    // Validate URL must use HTTPS.
    if !repo.url.starts_with("https://") {
        bail!(
            "repos[{}]: url must start with 'https://', got: {}",
            index,
            repo.url
        );
    }

    // Validate commit must be a 40-character lowercase hex string.
    if repo.commit.len() != 40 || !repo.commit.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "repos[{}]: commit must be a 40-character hex SHA, got: {}",
            index,
            repo.commit
        );
    }

    // Validate language is one of the supported target languages.
    if !VALID_LANGUAGES.contains(&repo.language.as_str()) {
        bail!(
            "repos[{}]: unsupported language '{}'; valid options are: {}",
            index,
            repo.language,
            VALID_LANGUAGES.join(", ")
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn write_temp_toml(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corpus.toml");
        std::fs::write(&path, content).expect("write");
        (dir, path)
    }

    #[test]
    fn valid_config_parses() {
        let content = r#"
[[repos]]
url = "https://github.com/BurntSushi/ripgrep"
commit = "4649aa9700619f94cf9c66876e9549d83420e16c"
language = "Rust"
"#;
        let (_dir, path) = write_temp_toml(content);
        let config = load_corpus_config(&path).expect("valid config");
        assert_eq!(config.repos.len(), 1);
        assert_eq!(config.repos[0].language, "Rust");
    }

    #[test]
    fn invalid_language_rejected() {
        let content = r#"
[[repos]]
url = "https://github.com/example/repo"
commit = "4649aa9700619f94cf9c66876e9549d83420e16c"
language = "Haskell"
"#;
        let (_dir, path) = write_temp_toml(content);
        let err = load_corpus_config(&path).expect_err("should fail");
        assert!(err.to_string().contains("unsupported language"));
    }

    #[test]
    fn missing_commit_fails() {
        // commit too short — only 10 chars
        let content = r#"
[[repos]]
url = "https://github.com/example/repo"
commit = "deadbeef00"
language = "Go"
"#;
        let (_dir, path) = write_temp_toml(content);
        let err = load_corpus_config(&path).expect_err("should fail");
        assert!(err.to_string().contains("40-character hex SHA"));
    }

    #[test]
    fn non_https_url_rejected() {
        let content = r#"
[[repos]]
url = "http://github.com/example/repo"
commit = "4649aa9700619f94cf9c66876e9549d83420e16c"
language = "Python"
"#;
        let (_dir, path) = write_temp_toml(content);
        let err = load_corpus_config(&path).expect_err("should fail");
        assert!(err.to_string().contains("https://"));
    }
}
