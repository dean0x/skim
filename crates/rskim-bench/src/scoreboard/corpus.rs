//! Scoreboard corpora: loading `corpora.toml` and materializing each corpus
//! as a pinned local clone (#203).
//!
//! `corpora.toml` uses the `rskim-research` corpus schema (`[[repos]]` with
//! `url` / `commit` / `language`) and its validator; this module adds the
//! scoreboard's own rules on top (lowercase SHAs, unique clone names).
//!
//! [`CorpusSource`] is the injection point between "which directory holds
//! this corpus" and "how it got there": [`GitCorpusSource`] clones or reuses a
//! pinned full-history checkout; tests use `FixtureCorpusSource` (under
//! `cfg(test)` / the `test-utils` feature) so they never touch the network.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use rskim_research::clone::{PinnedCloneState, verify_pinned_clone};

/// Default directory the scoreboard clones corpora into, relative to the
/// workspace root (under the gitignored `.bench-corpus/`, apart from the
/// `bench` / `cochange-validate` clones).
pub const DEFAULT_CORPUS_DIR: &str = ".bench-corpus/scoreboard";

/// One pinned corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusSpec {
    /// Clone directory and golden-file name: `extract_repo_name(url)`.
    pub name: String,
    /// `https://` clone URL.
    pub url: String,
    /// 40-character lowercase hex SHA.
    pub commit: String,
    /// `Rust | TypeScript | Python | Go | Java`.
    pub language: String,
}

/// Load and validate `corpora.toml`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed, fails
/// `rskim_research::config::load_corpus_config`'s validation (`https://`
/// URL, 40-hex SHA, supported language), lists no corpus, pins a SHA that is
/// not lowercase (clone reuse compares it with `git rev-parse` output), or
/// maps two URLs to the same clone name.
pub fn load_corpora(path: &Path) -> anyhow::Result<Vec<CorpusSpec>> {
    let config = rskim_research::config::load_corpus_config(path)?;
    anyhow::ensure!(
        !config.repos.is_empty(),
        "{} lists no corpus",
        path.display()
    );

    let mut names = BTreeSet::new();
    config
        .repos
        .into_iter()
        .map(|repo| {
            let name = rskim_research::clone::extract_repo_name(&repo.url)?;
            anyhow::ensure!(
                !repo.commit.bytes().any(|b| b.is_ascii_uppercase()),
                "{name}: commit {} must be lowercase hex",
                repo.commit
            );
            anyhow::ensure!(
                names.insert(name.clone()),
                "two corpora share the clone name {name:?}"
            );
            Ok(CorpusSpec {
                name,
                url: repo.url,
                commit: repo.commit,
                language: repo.language,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .with_context(|| format!("validating {}", path.display()))
}

/// Materializes a corpus as a local repository root.
pub trait CorpusSource {
    /// Make `spec` available locally and return its repository root.
    ///
    /// # Errors
    ///
    /// Returns an error if the corpus cannot be materialized; the scoreboard
    /// reports that as a harness error (exit 2).
    fn materialize(&self, spec: &CorpusSpec) -> anyhow::Result<PathBuf>;

    /// Re-check `root` after the queries ran, to prove skim wrote nothing
    /// into the corpus: anything but [`PinnedCloneState::Reusable`] means the
    /// corpus changed under the run.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be determined.
    fn verify_untouched(&self, spec: &CorpusSpec, root: &Path) -> anyhow::Result<PinnedCloneState> {
        verify_pinned_clone(root, &spec.commit)
    }
}

/// Production source: a full-history clone at `<corpus_dir>/<name>`, pinned
/// to the corpus commit, via
/// [`rskim_research::clone::ensure_pinned_history_clone`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCorpusSource {
    corpus_dir: PathBuf,
}

impl GitCorpusSource {
    /// Clone corpora under `corpus_dir` (e.g. [`DEFAULT_CORPUS_DIR`]).
    pub fn new(corpus_dir: impl Into<PathBuf>) -> Self {
        GitCorpusSource {
            corpus_dir: corpus_dir.into(),
        }
    }

    /// Where `spec` is cloned.
    pub fn dest(&self, spec: &CorpusSpec) -> PathBuf {
        self.corpus_dir.join(&spec.name)
    }
}

impl CorpusSource for GitCorpusSource {
    fn materialize(&self, spec: &CorpusSpec) -> anyhow::Result<PathBuf> {
        let dest = self.dest(spec);
        rskim_research::clone::ensure_pinned_history_clone(&spec.url, &spec.commit, &dest)
            .with_context(|| {
                format!(
                    "materializing corpus {} at {} into {}",
                    spec.name,
                    spec.commit,
                    dest.display()
                )
            })?;
        Ok(dest)
    }
}

/// MOCK: test source serving already-materialized local repositories by
/// corpus name (e.g. a [`crate::scoreboard::test_support::FixtureRepo`]),
/// with no git clone or network access.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone, Default)]
pub struct FixtureCorpusSource {
    roots: std::collections::BTreeMap<String, PathBuf>,
}

#[cfg(any(test, feature = "test-utils"))]
impl FixtureCorpusSource {
    /// Serve `root` for the corpus named `name`.
    pub fn with_corpus(mut self, name: &str, root: impl Into<PathBuf>) -> Self {
        self.roots.insert(name.to_string(), root.into());
        self
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl CorpusSource for FixtureCorpusSource {
    fn materialize(&self, spec: &CorpusSpec) -> anyhow::Result<PathBuf> {
        self.roots
            .get(&spec.name)
            .cloned()
            .with_context(|| format!("no fixture registered for corpus {:?}", spec.name))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use crate::scoreboard::test_support::FixtureRepo;

    fn checked_in_corpora() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scoreboard/corpora.toml")
    }

    fn write_corpora(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corpora.toml");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    fn repo(url: &str, commit: &str) -> String {
        format!("[[repos]]\nurl = \"{url}\"\ncommit = \"{commit}\"\nlanguage = \"Rust\"\n")
    }

    const SHA: &str = "4519153e5e461527f4bca45b042fff45c4ec6fb9";

    #[test]
    fn the_checked_in_corpora_are_the_four_pinned_repositories() {
        let specs = load_corpora(&checked_in_corpora()).unwrap();
        let got: Vec<(&str, &str, &str, &str)> = specs
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.url.as_str(),
                    s.commit.as_str(),
                    s.language.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "skim",
                    "https://github.com/dean0x/skim",
                    "b8a0a79463382347820f1c2572bde37b68e87c76",
                    "Rust"
                ),
                (
                    "ripgrep",
                    "https://github.com/BurntSushi/ripgrep",
                    "4519153e5e461527f4bca45b042fff45c4ec6fb9",
                    "Rust"
                ),
                (
                    "flask",
                    "https://github.com/pallets/flask",
                    "7374c85ddefc3f4b177a698ab9f0cbb6a5c0b392",
                    "Python"
                ),
                (
                    "zod",
                    "https://github.com/colinhacks/zod",
                    "bbc68f990c7e6a5e3f506c56fb04bd0279b9c9b5",
                    "TypeScript"
                ),
            ]
        );
    }

    #[test]
    fn invalid_corpora_are_rejected() {
        for body in [
            String::new(),
            repo("http://github.com/a/b", SHA),
            repo("https://github.com/a/b", "4519153"),
            repo("https://github.com/a/b", &SHA.to_uppercase()),
            format!(
                "{}{}",
                repo("https://github.com/a/b", SHA),
                repo("https://gitlab.com/c/b", SHA)
            ),
        ] {
            let (_dir, path) = write_corpora(&body);
            assert!(load_corpora(&path).is_err(), "{body}");
        }
    }

    #[test]
    fn git_source_clones_each_corpus_into_its_own_named_directory() {
        let source = GitCorpusSource::new("/tmp/corpora");
        let spec = CorpusSpec {
            name: "ripgrep".to_string(),
            url: "https://github.com/BurntSushi/ripgrep".to_string(),
            commit: SHA.to_string(),
            language: "Rust".to_string(),
        };
        assert_eq!(source.dest(&spec), PathBuf::from("/tmp/corpora/ripgrep"));
    }

    #[test]
    fn fixture_source_serves_registered_roots_and_verifies_them() {
        let fixture = FixtureRepo::new();
        fixture.write("src/lib.rs", "pub fn f() {}\n");
        let head = fixture.commit_all("init");
        let spec = CorpusSpec {
            name: "fixture".to_string(),
            url: "https://example.invalid/fixture".to_string(),
            commit: head,
            language: "Rust".to_string(),
        };
        let source = FixtureCorpusSource::default().with_corpus("fixture", fixture.root());

        let root = source.materialize(&spec).unwrap();
        assert_eq!(root, fixture.root());
        assert!(source.verify_untouched(&spec, &root).unwrap().is_reusable());

        fixture.write("written-by-skim.idx", "x");
        assert!(!source.verify_untouched(&spec, &root).unwrap().is_reusable());

        let unknown = CorpusSpec {
            name: "other".to_string(),
            ..spec
        };
        assert!(source.materialize(&unknown).is_err());
    }
}
