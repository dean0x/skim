//! The scoreboard oracle's own file universe and coverage universe (#203).
//!
//! Mirrors the CLI walker (`crates/rskim/src/cmd/search/walk.rs`) and the
//! index producer (`crates/rskim/src/cmd/search/index.rs`) closely enough that
//! a policy change on either side shows up as a `universe.delta`, while
//! staying an independent re-implementation: nothing here imports
//! `rskim_core::Language` or any other skim code. Each rule cites the CLI
//! logic it mirrors.
//!
//! # Rules
//!
//! - **Candidates** — `git ls-files -z` (tracked, ADR-008's union) ∪
//!   untracked-not-ignored files (`git ls-files -z --others
//!   --exclude-standard`) with no hidden (`.`-prefixed) path component, the
//!   walker's `.hidden(true)` (`walk.rs:1157`). On a verified-clean clone the
//!   untracked half is empty.
//! - **Regular files only** — `symlink_metadata`; symlinks, gitlinks and
//!   other non-files are dropped (`walk.rs:418-432`).
//! - **Extension allow-list** — the oracle's own copy of
//!   `Language::from_extension` (`crates/rskim-core/src/types.rs:55-80`),
//!   shared with its `--lang` map
//!   ([`crate::scoreboard::oracle::is_indexable_extension`]),
//!   case-sensitive, extension only.
//! - **Size** — at most [`MAX_FILE_BYTES`], checked at walk time (`walk.rs:359`).
//! - **Encoding** — strict UTF-8 (producer phase).
//! - **Minified gate** — producer phase, skipped for json / yaml / yml /
//!   toml (`index.rs:880-881`); see `is_minified`.
//!
//! Git runs under a [`GitIsolation`]: the same isolated `HOME` the skim
//! subprocess gets, with `GIT_CONFIG_NOSYSTEM=1`, so global excludes cannot
//! diverge from skim's `git_global(true)`.
//!
//! # Known divergences (untracked files only; empty on a clean clone)
//!
//! - skim's walker also honours `.ignore` files (`ignore(true)`); `git
//!   ls-files --exclude-standard` does not.
//! - An untracked nested repository is one directory entry to `git ls-files`
//!   (dropped here as a non-regular file) but is descended into by the walker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

use crate::scoreboard::oracle::is_indexable_extension;

/// Files larger than this are skipped at walk time (`walk.rs::MAX_FILE_BYTES`).
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// skim's index file cap (`IndexConfig::DEFAULT_MAX_FILES`,
/// `crates/rskim/src/cmd/search/types.rs:453`).
pub const MAX_INDEXED_FILES: usize = 50_000;

/// Bytes probed for a NUL byte by the coverage universe's binary check.
const BINARY_PROBE_BYTES: usize = 8192;

// Minified gate — copies of `walk.rs`'s MINIFY_* constants.
const MINIFY_PROBE_BYTES: usize = 8192;
const MINIFY_MIN_BYTES: usize = 8 * MINIFY_PROBE_BYTES; // 65_536
const MINIFY_AVG_LINE_BYTES: usize = 500;

/// Extensions of skim's serde-based languages (JSON / YAML / TOML), which are
/// exempt from the minified gate (`Language::is_serde_based`).
const SERDE_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "toml"];

/// Timeout for each `git ls-files` call (seconds).
const GIT_TIMEOUT_SECS: u64 = 120;

/// Environment variables removed from every isolated git (or skim)
/// subprocess: anything that could point git at another repository or load
/// configuration from outside the isolated `HOME`.
const ISOLATION_REMOVED_ENV: &[&str] = &[
    "XDG_CONFIG_HOME",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

// ============================================================================
// Git isolation
// ============================================================================

/// The isolated git environment shared by the oracle and the skim subprocess:
/// `HOME` points at a scoreboard-owned directory, `GIT_CONFIG_NOSYSTEM=1`,
/// and `ISOLATION_REMOVED_ENV` is cleared.
///
/// Apply the same instance to the skim command (see [`GitIsolation::apply`])
/// so both sides resolve global excludes from the same empty `HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIsolation {
    home: PathBuf,
}

impl GitIsolation {
    /// Isolate git under `home`.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        GitIsolation { home: home.into() }
    }

    /// The isolated `HOME`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Set `HOME` and `GIT_CONFIG_NOSYSTEM=1` on `cmd` and remove every
    /// variable in `ISOLATION_REMOVED_ENV`.
    pub fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.home).env("GIT_CONFIG_NOSYSTEM", "1");
        for var in ISOLATION_REMOVED_ENV {
            cmd.env_remove(var);
        }
    }
}

// ============================================================================
// Skip reasons
// ============================================================================

/// The skim pipeline phase a skip belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipPhase {
    /// Decided without reading content (walker / tracked-union classify).
    /// skim never persists these, so they are INFO only.
    Walk,
    /// Decided after reading content (`index.rs` producer). skim persists
    /// these in the manifest and reports them in `--stats --json`
    /// `skipped_by_reason`.
    Producer,
}

/// Why a candidate is outside the oracle's indexed universe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// The path is empty, absolute, or has a `..` component (defense in
    /// depth: git never emits one).
    UnsafePath,
    /// The path bytes are not UTF-8.
    NonUtf8Path,
    /// Symlink, directory (gitlink / nested repository), or other non-file.
    NotRegularFile,
    /// The path could not be stat-ed or read (e.g. deleted from the worktree).
    Unreadable,
    /// Extension outside the indexable set.
    UnsupportedExtension,
    /// Larger than [`MAX_FILE_BYTES`] at walk time.
    TooLarge,
    /// Content is not strict UTF-8.
    NonUtf8,
    /// Content fails the minified gate.
    Minified,
}

impl SkipReason {
    /// Stable label. The producer-phase labels equal skim's
    /// `PersistedSkipReason::label()` (`crates/rskim/src/cmd/search/types.rs:633-637`)
    /// so the persisted breakdowns compare key for key. The walk-phase size
    /// skip is `too_large_at_walk`, distinct from skim's persisted
    /// `too_large` (a file that grew between walk and read).
    pub fn label(self) -> &'static str {
        match self {
            SkipReason::UnsafePath => "unsafe_path",
            SkipReason::NonUtf8Path => "non_utf8_path",
            SkipReason::NotRegularFile => "not_regular_file",
            SkipReason::Unreadable => "unreadable",
            SkipReason::UnsupportedExtension => "unsupported_extension",
            SkipReason::TooLarge => "too_large_at_walk",
            SkipReason::NonUtf8 => "non_utf8",
            SkipReason::Minified => "minified",
        }
    }

    /// The phase this reason belongs to.
    pub fn phase(self) -> SkipPhase {
        match self {
            SkipReason::NonUtf8 | SkipReason::Minified => SkipPhase::Producer,
            SkipReason::UnsafePath
            | SkipReason::NonUtf8Path
            | SkipReason::NotRegularFile
            | SkipReason::Unreadable
            | SkipReason::UnsupportedExtension
            | SkipReason::TooLarge => SkipPhase::Walk,
        }
    }
}

/// One candidate outside the indexed universe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    /// Repo-relative path (lossily decoded for [`SkipReason::NonUtf8Path`]).
    pub path: String,
    pub reason: SkipReason,
}

/// Coverage of the tracked text files by the indexed universe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Tracked text files that are in the indexed universe.
    pub indexed_tracked: usize,
    /// Tracked regular files of at most [`MAX_FILE_BYTES`] with no NUL byte
    /// in their first 8 KiB.
    pub tracked_text: usize,
}

impl Coverage {
    /// `indexed_tracked / tracked_text`; `1.0` for a repository with no
    /// tracked text.
    pub fn ratio(&self) -> f64 {
        if self.tracked_text == 0 {
            return 1.0;
        }
        self.indexed_tracked as f64 / self.tracked_text as f64
    }
}

// ============================================================================
// Universe
// ============================================================================

/// The oracle's indexed universe of one corpus (paths and their text), its
/// skip breakdown, and its coverage universe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Universe {
    indexed: BTreeMap<String, String>,
    tracked_indexed: BTreeSet<String>,
    skipped: Vec<SkippedFile>,
    tracked_text: BTreeSet<String>,
    unindexed_text: BTreeMap<String, String>,
}

/// Where a candidate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Tracked,
    Untracked,
}

impl Universe {
    /// Compute the universe of the repository whose toplevel is `root`.
    ///
    /// # Errors
    ///
    /// Returns an error if `root` is not the toplevel of a git repository, or
    /// if `git ls-files` fails to spawn, times out, or exits non-zero.
    pub fn compute(root: &Path, git: &GitIsolation) -> anyhow::Result<Self> {
        if !root.join(".git").exists() {
            anyhow::bail!("{} is not a git repository root (no .git)", root.display());
        }
        let tracked: BTreeSet<Vec<u8>> = ls_files(root, git, &[])?.into_iter().collect();
        let untracked: BTreeSet<Vec<u8>> =
            ls_files(root, git, &["--others", "--exclude-standard"])?
                .into_iter()
                .filter(|p| !tracked.contains(p) && !has_hidden_component(p))
                .collect();

        let mut universe = Universe::default();
        for raw in tracked {
            universe.classify(root, raw, Origin::Tracked);
        }
        for raw in untracked {
            universe.classify(root, raw, Origin::Untracked);
        }
        universe.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(universe)
    }

    /// Classify one candidate into exactly one of: indexed, or skipped with
    /// a reason. Tracked text files also enter the coverage universe.
    fn classify(&mut self, root: &Path, raw: Vec<u8>, origin: Origin) {
        let path = match String::from_utf8(raw) {
            Ok(p) => p,
            Err(e) => {
                let lossy = String::from_utf8_lossy(e.as_bytes()).into_owned();
                return self.skip(lossy, SkipReason::NonUtf8Path);
            }
        };
        if !is_safe_relative(&path) {
            return self.skip(path, SkipReason::UnsafePath);
        }

        let abs = root.join(&path);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(_) => return self.skip(path, SkipReason::Unreadable),
        };
        if !meta.is_file() {
            return self.skip(path, SkipReason::NotRegularFile);
        }

        let tracked = origin == Origin::Tracked;
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let indexable = is_indexable_extension(ext);
        let fits = meta.len() <= MAX_FILE_BYTES;

        // Read only what the indexed or coverage universe needs.
        let bytes = if fits && (indexable || tracked) {
            match std::fs::read(&abs) {
                Ok(b) => Some(b),
                Err(_) => return self.skip(path, SkipReason::Unreadable),
            }
        } else {
            None
        };
        let is_tracked_text = tracked && bytes.as_deref().is_some_and(|b| !has_nul_probe(b));
        if is_tracked_text {
            self.tracked_text.insert(path.clone());
        }

        if !indexable {
            self.remember_unindexed(&path, bytes, is_tracked_text);
            return self.skip(path, SkipReason::UnsupportedExtension);
        }
        let Some(bytes) = bytes else {
            return self.skip(path, SkipReason::TooLarge);
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                self.remember_unindexed(&path, Some(e.into_bytes()), is_tracked_text);
                return self.skip(path, SkipReason::NonUtf8);
            }
        };
        if !SERDE_EXTENSIONS.contains(&ext) && is_minified(&text) {
            self.remember_unindexed(&path, Some(text.into_bytes()), is_tracked_text);
            return self.skip(path, SkipReason::Minified);
        }

        if tracked {
            self.tracked_indexed.insert(path.clone());
        }
        self.indexed.insert(path, text);
    }

    fn skip(&mut self, path: String, reason: SkipReason) {
        self.skipped.push(SkippedFile { path, reason });
    }

    /// Keep the (lossily decoded) text of a tracked text file that is not
    /// indexed, for `unindexed_hits`.
    fn remember_unindexed(&mut self, path: &str, bytes: Option<Vec<u8>>, is_tracked_text: bool) {
        if let (true, Some(bytes)) = (is_tracked_text, bytes) {
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            };
            self.unindexed_text.insert(path.to_string(), text);
        }
    }

    /// Number of indexed files.
    pub fn len(&self) -> usize {
        self.indexed.len()
    }

    /// Whether no file is indexed.
    pub fn is_empty(&self) -> bool {
        self.indexed.is_empty()
    }

    /// Whether `path` is in the indexed universe.
    pub fn contains(&self, path: &str) -> bool {
        self.indexed.contains_key(path)
    }

    /// Text of an indexed file.
    pub fn text(&self, path: &str) -> Option<&str> {
        self.indexed.get(path).map(String::as_str)
    }

    /// Indexed paths, byte-wise sorted.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.indexed.keys().map(String::as_str)
    }

    /// `(path, text)` for every indexed file, byte-wise sorted — the input
    /// [`crate::scoreboard::oracle::ground_truth`] expects.
    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.indexed.iter().map(|(p, t)| (p.as_str(), t.as_str()))
    }

    /// Every skipped candidate, sorted by path.
    pub fn skipped(&self) -> &[SkippedFile] {
        &self.skipped
    }

    /// Per-reason counts over every skip (INFO).
    pub fn skipped_by_reason(&self) -> BTreeMap<String, u64> {
        count_reasons(self.skipped.iter())
    }

    /// Per-reason counts over producer-phase skips only, zero counts omitted:
    /// the same shape as skim's `--stats --json` `skipped_by_reason`, for a
    /// direct equality check.
    pub fn persisted_skipped_by_reason(&self) -> BTreeMap<String, u64> {
        count_reasons(self.producer_skips())
    }

    /// Producer-phase skips: the ones skim persists.
    fn producer_skips(&self) -> impl Iterator<Item = &SkippedFile> {
        self.skipped
            .iter()
            .filter(|s| s.reason.phase() == SkipPhase::Producer)
    }

    /// Files skim's walk accepts — indexed plus producer-phase skips — which
    /// is what skim's [`MAX_INDEXED_FILES`] cap counts (the cap is applied to
    /// walk entries, before content is read).
    pub fn walk_accepted_count(&self) -> usize {
        self.indexed.len() + self.producer_skips().count()
    }

    /// Require the corpus to fit under skim's [`MAX_INDEXED_FILES`] cap, so
    /// the cap never silently truncates what the scoreboard compares.
    ///
    /// # Errors
    ///
    /// Returns an error naming the count when it exceeds the cap.
    pub fn check_file_cap(&self) -> anyhow::Result<()> {
        self.check_file_cap_at(MAX_INDEXED_FILES)
    }

    /// [`Universe::check_file_cap`] against an explicit `cap`.
    ///
    /// # Errors
    ///
    /// Returns an error when [`Universe::walk_accepted_count`] exceeds `cap`.
    pub fn check_file_cap_at(&self, cap: usize) -> anyhow::Result<()> {
        let n = self.walk_accepted_count();
        if n > cap {
            anyhow::bail!("corpus has {n} walk-accepted files, over skim's {cap}-file index cap");
        }
        Ok(())
    }

    /// Coverage of the tracked text files (untracked files never count).
    pub fn coverage(&self) -> Coverage {
        Coverage {
            indexed_tracked: self.tracked_indexed.len(),
            tracked_text: self.tracked_text.len(),
        }
    }

    /// `(path, text)` of every tracked text file outside the indexed
    /// universe, byte-wise sorted; text is lossily decoded. Ground-truth hits
    /// among these are a query's `unindexed_hits` (INFO).
    pub fn unindexed_text_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.unindexed_text
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
    }
}

fn count_reasons<'a>(skips: impl Iterator<Item = &'a SkippedFile>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for s in skips {
        *counts.entry(s.reason.label().to_string()).or_insert(0) += 1;
    }
    counts
}

// ============================================================================
// Helpers
// ============================================================================

/// Two-signal minified gate plus the whole-file average — a port of
/// `walk.rs::minified_metric` (`walk.rs:1230-1266`, AD-395-1). Minified iff
/// all three hold: at least 64 KiB; at most one newline in the first 8 KiB;
/// whole-file average line length over 500 bytes. The third condition keeps
/// "blob-then-code" files (one huge first line, many normal lines after) in
/// the index.
fn is_minified(content: &str) -> bool {
    if content.len() < MINIFY_MIN_BYTES {
        return false;
    }
    let probe = &content.as_bytes()[..content.len().min(MINIFY_PROBE_BYTES)];
    if probe.iter().filter(|&&b| b == b'\n').count() > 1 {
        return false;
    }
    let newlines = content.bytes().filter(|&b| b == b'\n').count();
    content.len() / (newlines + 1) > MINIFY_AVG_LINE_BYTES
}

/// Whether the first 8 KiB contain a NUL byte (the coverage universe's
/// binary probe).
fn has_nul_probe(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_PROBE_BYTES)].contains(&0)
}

/// A non-empty relative path made only of normal components — the oracle's
/// own containment rule, standing in for skim's `is_repo_relative_safe` and
/// `Language::from_path`'s `..` rejection (`types.rs:155-169`).
fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Whether any `/`-separated component starts with `.`.
fn has_hidden_component(path: &[u8]) -> bool {
    path.split(|&b| b == b'/')
        .any(|seg| seg.first() == Some(&b'.'))
}

/// `git -C <root> ls-files -z <extra>` under `git`'s isolation, with
/// repository discovery fenced at `root`'s parent so a non-repository `root`
/// can never borrow an enclosing checkout. Returns the raw path bytes.
fn ls_files(root: &Path, git: &GitIsolation, extra: &[&str]) -> anyhow::Result<Vec<Vec<u8>>> {
    let canonical = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalizing {}", root.display()))?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&canonical)
        .args(["ls-files", "-z"])
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git.apply(&mut cmd);
    if let Some(parent) = canonical.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let label = format!("git ls-files {} (scoreboard universe)", extra.join(" "));
    let out = rskim_research::clone::git_output_with_timeout(cmd, &label, GIT_TIMEOUT_SECS)?;
    if !out.status.success() {
        anyhow::bail!(
            "{label} failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use crate::scoreboard::test_support::FixtureRepo;

    fn universe(repo: &FixtureRepo) -> Universe {
        Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap()
    }

    fn reason_of(u: &Universe, path: &str) -> Option<SkipReason> {
        u.skipped()
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.reason)
    }

    /// `len` bytes of `x` whose only `newlines` newlines sit at the very end,
    /// so the 8 KiB probe is newline-free and the average line length is
    /// `len / (newlines + 1)`.
    fn one_long_line(len: usize, newlines: usize) -> String {
        let mut s = "x".repeat(len - newlines);
        s.push_str(&"\n".repeat(newlines));
        s
    }

    // --- minified gate (pure) -------------------------------------------------

    #[test]
    fn minified_needs_at_least_64_kib() {
        assert!(!is_minified(&one_long_line(65_535, 0)));
        assert!(is_minified(&one_long_line(65_536, 0)));
    }

    #[test]
    fn minified_needs_at_most_one_newline_in_the_first_8_kib() {
        let mut one = "a\n".to_string();
        one.push_str(&"x".repeat(70_000));
        assert!(is_minified(&one));

        let mut two = "a\nb\n".to_string();
        two.push_str(&"x".repeat(70_000));
        assert!(!is_minified(&two));
    }

    #[test]
    fn minified_needs_whole_file_average_line_over_500_bytes() {
        // 66_000 bytes: 131 newlines → 132 lines → average exactly 500 (kept);
        // 130 newlines → 131 lines → average 503 (minified).
        assert!(!is_minified(&one_long_line(66_000, 131)));
        assert!(is_minified(&one_long_line(66_000, 130)));
    }

    #[test]
    fn blob_then_code_is_not_minified() {
        // First 8 KiB is one long line, but thousands of short lines follow.
        let mut s = "x".repeat(9_000);
        s.push('\n');
        for i in 0..8_000 {
            s.push_str(&format!("let v{i} = {i};\n"));
        }
        assert!(s.len() >= 65_536);
        assert!(!is_minified(&s));
    }

    #[test]
    fn safe_relative_paths_only() {
        assert!(is_safe_relative("src/main.rs"));
        assert!(!is_safe_relative("../escape.rs"));
        assert!(!is_safe_relative("a/../../b.rs"));
        assert!(!is_safe_relative("/etc/passwd"));
        assert!(!is_safe_relative(""));
    }

    // --- universe rules (AC 4) ------------------------------------------------

    #[test]
    fn tracked_source_files_are_indexed_with_their_text() {
        let repo = FixtureRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.write("README.md", "# hi\n");
        repo.commit_all("init");

        let u = universe(&repo);
        assert_eq!(
            u.paths().collect::<Vec<_>>(),
            vec!["README.md", "src/main.rs"]
        );
        assert_eq!(u.text("src/main.rs"), Some("fn main() {}\n"));
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn extension_allow_list_is_case_sensitive() {
        let repo = FixtureRepo::new();
        repo.write("notes.txt", "text\n");
        repo.write("UPPER.RS", "fn f() {}\n");
        repo.write("Makefile", "all:\n");
        repo.commit_all("init");

        let u = universe(&repo);
        assert!(u.is_empty());
        for p in ["notes.txt", "UPPER.RS", "Makefile"] {
            assert_eq!(
                reason_of(&u, p),
                Some(SkipReason::UnsupportedExtension),
                "{p}"
            );
        }
    }

    #[test]
    fn minified_bundle_is_skipped_but_blob_then_code_is_indexed() {
        let repo = FixtureRepo::new();
        repo.write("dist/bundle.js", one_long_line(70_000, 0));
        let mut blob_then_code = "x".repeat(9_000);
        blob_then_code.push('\n');
        for i in 0..8_000 {
            blob_then_code.push_str(&format!("let v{i} = {i};\n"));
        }
        repo.write("src/generated.js", &blob_then_code);
        repo.commit_all("init");

        let u = universe(&repo);
        assert_eq!(reason_of(&u, "dist/bundle.js"), Some(SkipReason::Minified));
        assert!(u.contains("src/generated.js"));
    }

    #[test]
    fn data_formats_are_exempt_from_the_minified_gate() {
        let repo = FixtureRepo::new();
        for p in ["a.json", "b.yaml", "c.yml", "d.toml"] {
            repo.write(p, one_long_line(70_000, 0));
        }
        repo.commit_all("init");

        let u = universe(&repo);
        for p in ["a.json", "b.yaml", "c.yml", "d.toml"] {
            assert!(u.contains(p), "{p}");
        }
    }

    #[test]
    fn files_over_5_mib_are_a_walk_phase_skip() {
        let repo = FixtureRepo::new();
        let limit = usize::try_from(MAX_FILE_BYTES).unwrap();
        // Multi-line, so the at-limit file is not also a minified candidate.
        let at_limit = "a\n".repeat(limit / 2);
        assert_eq!(at_limit.len(), limit);
        repo.write("at_limit.rs", &at_limit);
        repo.write("over_limit.rs", format!("{at_limit}a"));
        repo.commit_all("init");

        let u = universe(&repo);
        assert!(u.contains("at_limit.rs"));
        assert_eq!(reason_of(&u, "over_limit.rs"), Some(SkipReason::TooLarge));
        assert_eq!(SkipReason::TooLarge.phase(), SkipPhase::Walk);
        assert!(
            u.persisted_skipped_by_reason().is_empty(),
            "skim never persists a walk-phase size skip"
        );
    }

    #[test]
    fn non_utf8_files_are_a_producer_phase_skip() {
        let repo = FixtureRepo::new();
        repo.write("latin1.rs", [b'/', b'/', b' ', 0xE9, b'\n']);
        repo.commit_all("init");

        let u = universe(&repo);
        assert_eq!(reason_of(&u, "latin1.rs"), Some(SkipReason::NonUtf8));
        assert_eq!(SkipReason::NonUtf8.phase(), SkipPhase::Producer);
    }

    #[cfg(unix)]
    #[test]
    fn tracked_symlinks_are_dropped() {
        let repo = FixtureRepo::new();
        repo.write("real.rs", "fn real() {}\n");
        std::os::unix::fs::symlink("real.rs", repo.root().join("link.rs")).unwrap();
        repo.commit_all("init");
        assert!(
            repo.git(&["ls-files"]).contains("link.rs"),
            "fixture: symlink is tracked"
        );

        let u = universe(&repo);
        assert!(u.contains("real.rs"));
        assert!(!u.contains("link.rs"));
        assert_eq!(reason_of(&u, "link.rs"), Some(SkipReason::NotRegularFile));
    }

    #[test]
    fn tracked_but_ignored_files_are_indexed_and_ignored_untracked_are_not() {
        let repo = FixtureRepo::new();
        repo.write(".gitignore", "generated/\n");
        repo.write("generated/tracked.rs", "fn kept() {}\n");
        repo.commit_forced(&[".gitignore", "generated/tracked.rs"], "init");
        repo.write("generated/untracked.rs", "fn dropped() {}\n");

        let u = universe(&repo);
        assert!(
            u.contains("generated/tracked.rs"),
            "ADR-008 union keeps tracked files"
        );
        assert!(!u.contains("generated/untracked.rs"));
    }

    #[test]
    fn untracked_files_count_unless_under_a_hidden_component() {
        let repo = FixtureRepo::new();
        repo.write(".github/ci.yml", "on: push\n");
        repo.commit_all("init");
        repo.write("src/new.rs", "fn new() {}\n");
        repo.write(".scratch/notes.md", "# local\n");
        repo.write("src/.cache/tmp.rs", "fn tmp() {}\n");

        let u = universe(&repo);
        assert!(u.contains(".github/ci.yml"), "hidden but tracked stays in");
        assert!(u.contains("src/new.rs"));
        assert!(!u.contains(".scratch/notes.md"));
        assert!(!u.contains("src/.cache/tmp.rs"));
    }

    #[test]
    fn a_tracked_file_missing_from_the_worktree_is_not_indexed() {
        let repo = FixtureRepo::new();
        repo.write("gone.rs", "fn gone() {}\n");
        repo.commit_all("init");
        std::fs::remove_file(repo.root().join("gone.rs")).unwrap();

        let u = universe(&repo);
        assert!(!u.contains("gone.rs"));
        assert_eq!(reason_of(&u, "gone.rs"), Some(SkipReason::Unreadable));
    }

    #[test]
    fn global_excludes_come_only_from_the_isolated_home() {
        let repo = FixtureRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.commit_all("init");
        repo.write("src/untracked.rs", "fn u() {}\n");

        let excluding_home = tempfile::tempdir().unwrap();
        let ignore = excluding_home.path().join(".config").join("git");
        std::fs::create_dir_all(&ignore).unwrap();
        std::fs::write(ignore.join("ignore"), "untracked.rs\n").unwrap();

        let excluded =
            Universe::compute(repo.root(), &GitIsolation::new(excluding_home.path())).unwrap();
        let clean = universe(&repo);
        assert!(!excluded.contains("src/untracked.rs"));
        assert!(clean.contains("src/untracked.rs"));
    }

    #[test]
    fn a_directory_that_is_not_a_repository_root_is_an_error() {
        let repo = FixtureRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.commit_all("init");

        let nested = repo.root().join("src");
        assert!(Universe::compute(&nested, &GitIsolation::new(repo.home())).is_err());
        let plain = tempfile::tempdir().unwrap();
        assert!(Universe::compute(plain.path(), &GitIsolation::new(repo.home())).is_err());
    }

    // --- breakdowns, cap, coverage --------------------------------------------

    #[test]
    fn persisted_breakdown_matches_skims_producer_phase_reasons_only() {
        let repo = FixtureRepo::new();
        repo.write("bundle.js", one_long_line(70_000, 0));
        repo.write("latin1.rs", [0xE9, b'\n']);
        repo.write("notes.txt", "text\n");
        repo.commit_all("init");

        let u = universe(&repo);
        let persisted = u.persisted_skipped_by_reason();
        assert_eq!(
            persisted,
            BTreeMap::from([("minified".to_string(), 1), ("non_utf8".to_string(), 1)])
        );
        let full = u.skipped_by_reason();
        assert_eq!(full.get("unsupported_extension"), Some(&1));
        assert_eq!(full.get("minified"), Some(&1));
    }

    #[test]
    fn file_cap_counts_every_walk_accepted_file() {
        let repo = FixtureRepo::new();
        repo.write("a.rs", "fn a() {}\n");
        repo.write("b.rs", "fn b() {}\n");
        repo.write("c.js", one_long_line(70_000, 0)); // walk-accepted, producer-skipped
        repo.write("d.txt", "not a candidate\n");
        repo.commit_all("init");

        let u = universe(&repo);
        assert_eq!(u.walk_accepted_count(), 3);
        assert!(u.check_file_cap_at(3).is_ok());
        assert!(u.check_file_cap_at(2).is_err());
        assert!(u.check_file_cap().is_ok());
    }

    #[test]
    fn coverage_is_indexed_tracked_files_over_tracked_text_files() {
        let repo = FixtureRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.write("LICENSE", "MIT\n");
        repo.write("logo.png", [0x89, b'P', b'N', b'G', 0, 0, 1]);
        repo.write("bundle.js", one_long_line(70_000, 0));
        repo.commit_all("init");
        repo.write("untracked.rs", "fn u() {}\n");

        let u = universe(&repo);
        let cov = u.coverage();
        // Tracked text: main.rs, LICENSE, bundle.js (logo.png has a NUL).
        assert_eq!(cov.tracked_text, 3);
        assert_eq!(cov.indexed_tracked, 1, "untracked files are not coverage");
        assert!((cov.ratio() - 1.0 / 3.0).abs() < 1e-9);

        let unindexed: Vec<&str> = u.unindexed_text_files().map(|(p, _)| p).collect();
        assert_eq!(unindexed, vec!["LICENSE", "bundle.js"]);
        assert_eq!(
            u.unindexed_text_files()
                .find(|(p, _)| *p == "LICENSE")
                .map(|(_, t)| t),
            Some("MIT\n")
        );
    }

    #[test]
    fn coverage_of_an_empty_repository_is_complete() {
        let repo = FixtureRepo::new();
        repo.commit_all("empty");
        let cov = universe(&repo).coverage();
        assert_eq!((cov.indexed_tracked, cov.tracked_text), (0, 0));
        assert!((cov.ratio() - 1.0).abs() < 1e-9);
    }
}
