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
    /// When `true`, clone the full repository history (no `--depth 1`).
    ///
    /// Required for co-change analysis which needs complete git history.
    /// Defaults to `false` for backward compatibility.
    #[serde(default)]
    pub deep_clone: bool,
}

/// The set of language strings accepted in corpus.toml.
const VALID_LANGUAGES: &[&str] = &["Rust", "TypeScript", "Python", "Go", "Java"];

/// The set of language strings accepted in ast-corpus.toml.
///
/// Covers all 14 tree-sitter languages (the 3 serde-based languages — JSON, YAML,
/// TOML — are intentionally excluded because `rskim_core::Parser::new()` returns
/// `Err` for them).
pub const AST_VALID_LANGUAGES: &[&str] = &[
    "Rust",
    "TypeScript",
    "JavaScript",
    "Python",
    "Go",
    "Java",
    "C",
    "Cpp",
    "CSharp",
    "Ruby",
    "Sql",
    "Kotlin",
    "Swift",
    "Markdown",
];

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

/// Load and validate an AST corpus config from a TOML file.
///
/// Like [`load_corpus_config`] but validates against [`AST_VALID_LANGUAGES`]
/// (14 tree-sitter languages) and accepts `"HEAD"` as a valid commit reference
/// in addition to 40-character hex SHAs.
///
/// # Errors
///
/// Returns an error if the file cannot be read, is not valid TOML, or
/// contains invalid values (unsupported AST language, bad commit ref, non-https URL).
pub fn load_ast_corpus_config(path: &std::path::Path) -> anyhow::Result<CorpusConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading ast corpus config from {}", path.display()))?;

    let config: CorpusConfig =
        toml::from_str(&raw).with_context(|| "parsing ast corpus config TOML")?;

    for (i, repo) in config.repos.iter().enumerate() {
        validate_ast_repo(i, repo)?;
    }

    Ok(config)
}

fn validate_ast_repo(index: usize, repo: &RepoEntry) -> anyhow::Result<()> {
    // Validate URL must use HTTPS.
    if !repo.url.starts_with("https://") {
        bail!(
            "repos[{}]: url must start with 'https://', got: {}",
            index,
            repo.url
        );
    }

    // Validate commit: either "HEAD" or a 40-character lowercase hex SHA.
    let commit_ok = repo.commit == "HEAD"
        || (repo.commit.len() == 40 && repo.commit.chars().all(|c| c.is_ascii_hexdigit()));
    if !commit_ok {
        bail!(
            "repos[{}]: commit must be 'HEAD' or a 40-character hex SHA, got: {}",
            index,
            repo.commit
        );
    }

    // Validate language is one of the AST-supported target languages.
    if !AST_VALID_LANGUAGES.contains(&repo.language.as_str()) {
        bail!(
            "repos[{}]: unsupported AST language '{}'; valid options are: {}",
            index,
            repo.language,
            AST_VALID_LANGUAGES.join(", ")
        );
    }

    Ok(())
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

    // ── AST corpus config tests ──────────────────────────────────────────────

    #[test]
    fn ast_config_accepts_ast_languages() {
        let content = r#"
[[repos]]
url = "https://github.com/BurntSushi/ripgrep"
commit = "4649aa9700619f94cf9c66876e9549d83420e16c"
language = "Rust"

[[repos]]
url = "https://github.com/facebook/react"
commit = "HEAD"
language = "JavaScript"

[[repos]]
url = "https://github.com/dotnet/roslyn"
commit = "HEAD"
language = "CSharp"
"#;
        let (_dir, path) = write_temp_toml(content);
        let config = load_ast_corpus_config(&path).expect("valid AST config");
        assert_eq!(config.repos.len(), 3);
    }

    #[test]
    fn ast_config_accepts_head_commit() {
        let content = r#"
[[repos]]
url = "https://github.com/facebook/react"
commit = "HEAD"
language = "JavaScript"
"#;
        let (_dir, path) = write_temp_toml(content);
        let config = load_ast_corpus_config(&path).expect("HEAD should be accepted");
        assert_eq!(config.repos[0].commit, "HEAD");
    }

    #[test]
    fn ast_config_rejects_invalid_ast_language() {
        let content = r#"
[[repos]]
url = "https://github.com/example/repo"
commit = "HEAD"
language = "Haskell"
"#;
        let (_dir, path) = write_temp_toml(content);
        let err = load_ast_corpus_config(&path).expect_err("Haskell not in AST languages");
        assert!(err.to_string().contains("unsupported AST language"));
    }

    #[test]
    fn ast_config_rejects_serde_only_language() {
        // JSON/YAML/TOML are not tree-sitter languages — must be rejected
        let content = r#"
[[repos]]
url = "https://github.com/example/repo"
commit = "HEAD"
language = "Json"
"#;
        let (_dir, path) = write_temp_toml(content);
        let err = load_ast_corpus_config(&path).expect_err("JSON not in AST languages");
        assert!(err.to_string().contains("unsupported AST language"));
    }

    #[test]
    fn existing_config_unchanged_by_ast_additions() {
        // Verify that the original load_corpus_config still rejects Cpp (only valid in AST).
        let content = r#"
[[repos]]
url = "https://github.com/opencv/opencv"
commit = "4649aa9700619f94cf9c66876e9549d83420e16c"
language = "Cpp"
"#;
        let (_dir, path) = write_temp_toml(content);
        let err = load_corpus_config(&path).expect_err("Cpp not in lexical valid languages");
        assert!(err.to_string().contains("unsupported language"));
    }
}
