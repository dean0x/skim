//! Git data source for `skim heatmap` — the ONLY I/O file in this module.
//!
//! Uses `CommandRunner` to execute git commands and parses their output into
//! `CommitInfo` values via a simple state machine.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::cmd::is_repo_relative_safe;
use crate::runner::{CommandOutput, CommandRunner, MAX_OUTPUT_BYTES, is_spawn_error};

use super::metrics::without_quotes;
use super::note;
use super::types::{CommitInfo, FileChangeInfo, HeatmapConfig};

/// `git log` record header: hash, author timestamp, author name and subject,
/// separated by NUL bytes.
///
/// The hash and the timestamp come first, so no content of an author name or
/// a subject (a `|`, for example) can move them.
const LOG_FORMAT: &str = "--format=COMMIT:%H%x00%ad%x00%aN%x00%s";

/// The start of each record header line in [`LOG_FORMAT`] output.
const RECORD_PREFIX: &str = "COMMIT:";

/// Map a `CommandRunner` error to a friendly "git not installed" message when appropriate.
fn git_not_found(e: anyhow::Error) -> anyhow::Error {
    if is_spawn_error(&e) {
        anyhow::anyhow!("git is not installed or not in PATH")
    } else {
        e
    }
}

// ============================================================================
// Trait — for testability
// ============================================================================

/// Abstraction over git data sources.
///
/// The only implementation is [`CliGitSource`]; the trait enables unit-testing
/// the orchestration logic in `mod.rs` without spawning a real git process.
///
/// Heatmap-specific git data source — wraps CLI `git` binary via `CommandRunner`.
///
/// Infra methods (`is_git_repo`, `get_repo_root`, `detect_shallow_clone`,
/// `fetch_commit_count_since`) are included so `run_with_source` can accept a
/// single `&dyn GitDataSource` instead of splitting into a concrete `&CliGitSource`
/// for infrastructure and a trait object for data fetch.
///
/// No method passes the repository root as a pathspec or a revision. It goes
/// after `-C` only. The log and count methods pass `--path` after `--` with
/// `--literal-pathspecs`, so a folder matches its files without glob or
/// pathspec magic.
pub(crate) trait GitDataSource {
    /// Return `true` when the cwd is inside a git repository.
    fn is_git_repo(&self) -> bool;

    /// Return the repository root path.
    fn get_repo_root(&self) -> anyhow::Result<String>;

    /// Return `true` when the repo is a shallow clone.
    fn detect_shallow_clone(&self) -> bool;

    /// Fetch the Unix timestamp of the Nth-oldest commit within the last `n` commits.
    ///
    /// Returns `None` when the repo has fewer than `n` commits.
    fn fetch_commit_count_since(&self, n: usize) -> anyhow::Result<Option<u64>>;

    /// Fetch commits according to `config`, with the count of skipped records.
    ///
    /// When the git output stops at the runner's output limit, the last
    /// record is dropped and [`ParsedLog::cut_oldest`] is set. Any other
    /// failed `git log` is an error with git's message, but a branch with no
    /// commits gives an empty history.
    fn fetch_commits(&self, config: &HeatmapConfig) -> anyhow::Result<ParsedLog>;

    /// List the complete HEAD tree, with keys relative to `repo_root`.
    /// Git quotes the keys in the same form as in the log and count output.
    fn list_tracked_files(&self, repo_root: &str) -> anyhow::Result<HeadTree>;

    /// Count text lines in HEAD. A scope is literal and relative to the cwd.
    fn count_head_lines(&self, repo_root: &str, path: Option<&str>) -> anyhow::Result<HeadCounts>;

    /// Return the path of the cwd below the repository root
    /// (`git rev-parse --show-prefix`): empty at the root, else it ends in `/`.
    fn show_prefix(&self) -> anyhow::Result<String>;
}

// ============================================================================
// CLI implementation
// ============================================================================

/// Concrete git data source that delegates to the `git` binary.
pub(crate) struct CliGitSource {
    runner: CommandRunner,
}

/// Build the `git log` arg list for a window start and an optional `--path`.
///
/// Disable signature output so each record contains only the expected rows.
/// Keep paths root-relative so they match the HEAD tree and line counts.
///
/// `--literal-pathspecs` goes before the subcommand, so git reads the path
/// after `--` as a literal path: a folder still matches the files below it,
/// but a glob or a pathspec magic prefix matches only that literal name. It is
/// the same as `GIT_LITERAL_PATHSPECS=1`; it is an argument here because the
/// run reads stdout through `CommandRunner::run_stdout_degrade`, which takes
/// no environment.
fn build_git_log_args(since: Option<u64>, path: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "-c".to_string(),
        "log.showSignature=false".to_string(),
        "-c".to_string(),
        "diff.relative=false".to_string(),
        "--literal-pathspecs".to_string(),
        "log".to_string(),
        LOG_FORMAT.to_string(),
        "--raw".to_string(),
        "--numstat".to_string(),
        "--no-merges".to_string(),
        "--date=unix".to_string(),
        "-M".to_string(),
    ];

    if let Some(since) = since.filter(|&epoch| epoch != 0) {
        // An explicit epoch keeps Git from guessing the date of a bare number.
        args.push(format!("--since=@{since} +0000"));
    }

    if let Some(path) = path {
        args.push("--".to_string());
        args.push(path.to_string());
    }

    args
}

/// Build the `git diff` arg list for the files changed between `base` and HEAD.
///
/// Keep paths root-relative so the targets match the history paths.
///
/// `--end-of-options` makes git read the next argument as a revision, so the
/// `--diff` value reaches git as a value only. The revision must never go
/// after `--`: git reads anything there as a path.
fn build_diff_args(base: &str) -> Vec<String> {
    vec![
        "-c".to_string(),
        "diff.relative=false".to_string(),
        "diff".to_string(),
        "--name-only".to_string(),
        "--end-of-options".to_string(),
        format!("{base}...HEAD"),
    ]
}

impl CliGitSource {
    pub(crate) fn new() -> Self {
        Self {
            runner: CommandRunner::new(),
        }
    }

    /// Heatmap does not stop when stderr is closed.
    fn capture_git(&self, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let (out, cut) = self.runner.run_stdout_degrade("git", args)?;
        if cut {
            note(format_args!(
                "{}",
                crate::output::fidelity::output_cap_marker(
                    "git",
                    out.stdout.len(),
                    MAX_OUTPUT_BYTES
                )
            ));
        }
        Ok(out)
    }

    /// Resolve files changed between `base` and HEAD using three-dot diff.
    ///
    /// Uses `git diff --name-only --end-of-options <base>...HEAD` so that only
    /// commits reachable from HEAD but not from `base` are included.
    pub(crate) fn fetch_diff_files(&self, base: &str) -> anyhow::Result<Vec<String>> {
        let owned_args = build_diff_args(base);
        let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();
        let out = self.capture_git(&args).map_err(git_not_found)?;

        if out.exit_code != Some(0) {
            let stderr = out.stderr.trim();
            if stderr.contains("unknown revision") || stderr.contains("bad revision") {
                anyhow::bail!("base branch '{base}' not found");
            }
            anyhow::bail!("git diff failed: {stderr}");
        }

        Ok(out
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.strip_prefix("./").unwrap_or(l).to_string())
            .collect())
    }

    /// `true` when HEAD names no commit: the branch has no commits yet.
    ///
    /// `git rev-parse --verify --quiet` exits 1 for a name that does not
    /// resolve. Any other result, a spawn error included, gives `false`, so
    /// a failed `git log` is then reported, not hidden as an empty history.
    fn head_is_unborn(&self) -> bool {
        matches!(
            self.capture_git(&["rev-parse", "--verify", "--quiet", "HEAD"]),
            Ok(out) if out.exit_code == Some(1)
        )
    }
}

impl GitDataSource for CliGitSource {
    fn is_git_repo(&self) -> bool {
        match self.capture_git(&["rev-parse", "--is-inside-work-tree"]) {
            Ok(out) => out.exit_code == Some(0),
            Err(_) => false,
        }
    }

    fn get_repo_root(&self) -> anyhow::Result<String> {
        let out = self
            .capture_git(&["rev-parse", "--show-toplevel"])
            .map_err(git_not_found)?;
        rev_parse_path(&out, "--show-toplevel")
    }

    fn detect_shallow_clone(&self) -> bool {
        match self.capture_git(&["rev-parse", "--is-shallow-repository"]) {
            Ok(out) => out.stdout.trim() == "true",
            Err(_) => false,
        }
    }

    fn fetch_commit_count_since(&self, n: usize) -> anyhow::Result<Option<u64>> {
        if n == 0 {
            return Ok(None);
        }
        let skip = n.saturating_sub(1).to_string();
        let n_str = n.to_string();
        // The first line must be a timestamp, not a signature report.
        let out = self.capture_git(&[
            "-c",
            "log.showSignature=false",
            "log",
            "--format=%ad",
            "--date=unix",
            "-n",
            &n_str,
            "--skip",
            &skip,
        ])?;
        // The first line is the timestamp. A line that is not a valid
        // timestamp gives `None`, never a zero timestamp.
        let ts = out
            .stdout
            .trim()
            .lines()
            .next()
            .and_then(parse_unix_timestamp)
            .and_then(|t| u64::try_from(t).ok());
        Ok(ts.filter(|&t| t > 0))
    }

    fn fetch_commits(&self, config: &HeatmapConfig) -> anyhow::Result<ParsedLog> {
        let owned_args = build_git_log_args(config.since, config.path.as_deref());
        let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();
        let (output, cut) = self
            .runner
            .run_stdout_degrade("git", &args)
            .map_err(git_not_found)?;
        parsed_log_from(&output, cut, || self.head_is_unborn())
    }

    fn list_tracked_files(&self, repo_root: &str) -> anyhow::Result<HeadTree> {
        let (out, cut) = self
            .runner
            .run_stdout_degrade("git", &tracked_files_args(repo_root))?;
        tracked_files_from(&out, cut)
    }

    fn count_head_lines(&self, repo_root: &str, path: Option<&str>) -> anyhow::Result<HeadCounts> {
        let (out, cut) = self
            .runner
            .run_stdout_degrade("git", &head_line_count_args(repo_root, path))?;
        head_line_counts_from(&out, cut)
    }

    fn show_prefix(&self) -> anyhow::Result<String> {
        let out = self
            .capture_git(&["rev-parse", "--show-prefix"])
            .map_err(git_not_found)?;
        rev_parse_path(&out, "--show-prefix")
    }
}

/// The arg list that lists the paths of the HEAD tree of `repo_root`.
///
/// The paths come from the committed tree, so git does not read the index
/// and no index setting of the repository applies. A branch with no commits
/// makes git fail, and the caller then uses its fallback. `--full-tree` lists
/// the whole tree for any cwd.
fn tracked_files_args(repo_root: &str) -> [&str; 7] {
    [
        "-C",
        repo_root,
        "ls-tree",
        "-r",
        "-l",
        "--full-tree",
        "HEAD",
    ]
}

/// Scope a count from the cwd, but always return root-relative keys.
fn head_line_count_args<'a>(repo_root: &'a str, path: Option<&'a str>) -> Vec<&'a str> {
    let mut args = Vec::with_capacity(12);
    match path {
        Some(_) => args.push("--literal-pathspecs"),
        None => args.extend(["-C", repo_root]),
    }
    args.extend([
        "grep",
        "-I",
        "-c",
        "--full-name",
        "--no-color",
        "--no-recurse-submodules",
        "-e",
        "",
        "HEAD",
        "--",
    ]);
    args.extend(path);
    args
}

/// Map a `git log` run to its records.
///
/// `cut` is the runner's flag for stdout that stopped at its output limit.
/// Then the last record can be cut anywhere, so it is dropped, and
/// [`ParsedLog::cut_oldest`] gets the oldest author timestamp that is kept.
/// A cut before the end of the first record is an error, because no full
/// record is left.
///
/// With no cut, exit 0 parses all of stdout. Any other exit status is an
/// error with git's stderr, but when `head_unborn` reports a branch with no
/// commits the result is an empty history. `head_unborn` runs only then.
fn parsed_log_from(
    out: &CommandOutput,
    cut: bool,
    head_unborn: impl FnOnce() -> bool,
) -> anyhow::Result<ParsedLog> {
    if cut {
        let mut parsed = parse_git_log_output(without_last_record(&out.stdout));
        let oldest = parsed.commits.iter().map(|c| c.timestamp).min();
        let Some(oldest) = oldest else {
            anyhow::bail!(
                "git log output stopped at the {} MiB output limit before the end of the first commit record",
                MAX_OUTPUT_BYTES / (1024 * 1024)
            );
        };
        parsed.cut_oldest = Some(oldest);
        return Ok(parsed);
    }
    if out.exit_code == Some(0) {
        return Ok(parse_git_log_output(&out.stdout));
    }
    if head_unborn() {
        return Ok(ParsedLog::default());
    }
    let status = match out.exit_code {
        Some(code) => format!("exit status {code}"),
        None => "a signal".to_string(),
    };
    let stderr = out.stderr.trim();
    if stderr.is_empty() {
        anyhow::bail!("git log ended with {status}");
    }
    anyhow::bail!("git log ended with {status}: {stderr}")
}

/// The text before the last line that starts a record, so that a record
/// the output limit may have cut is not parsed. With one record or none,
/// nothing is left.
fn without_last_record(raw: &str) -> &str {
    let mut last_start = 0;
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        if line.starts_with(RECORD_PREFIX) {
            last_start = offset;
        }
        offset += line.len();
    }
    &raw[..last_start]
}

pub(crate) type HeadTree = HashMap<String, HeadEntry>;
pub(crate) type HeadCounts = Vec<(String, u64)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeadEntry {
    /// The listing supplies zero for an empty file. Git grep supplies text counts.
    /// After the count call, `None` means binary or outside the requested scope.
    Regular(Option<u64>),
    Other,
}

/// Reject a partial listing. A missing row could otherwise look like a deletion.
fn tracked_files_from(out: &CommandOutput, cut: bool) -> anyhow::Result<HeadTree> {
    anyhow::ensure!(
        !cut && out.exit_code == Some(0),
        "incomplete git ls-tree output"
    );
    let mut entries = HashMap::new();
    for line in out.stdout.lines() {
        let (path, entry) =
            parse_tree_line(line).ok_or_else(|| anyhow::anyhow!("invalid git ls-tree row"))?;
        anyhow::ensure!(
            entries.insert(path.to_owned(), entry).is_none(),
            "duplicate git ls-tree path"
        );
    }
    Ok(entries)
}

fn parse_tree_line(line: &str) -> Option<(&str, HeadEntry)> {
    let (metadata, path) = line.split_once('\t')?;
    let mut fields = metadata.split_ascii_whitespace();
    let (mode, kind, oid, size) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    let valid_size = (kind == "commit" && size == "-") || size.bytes().all(|b| b.is_ascii_digit());
    if fields.next().is_some()
        || mode.len() != 6
        || !mode.bytes().all(|b| (b'0'..=b'7').contains(&b))
        || !matches!(kind, "blob" | "commit")
        || !is_object_id(oid)
        || !valid_size
        || path.is_empty()
        || path.contains(['\t', '\0'])
        || !disk_path(path).is_some_and(|p| is_repo_relative_safe(&p))
    {
        return None;
    }
    let entry = match (mode, kind) {
        ("100644" | "100755", "blob") => HeadEntry::Regular((size == "0").then_some(0)),
        _ => HeadEntry::Other,
    };
    Some((path, entry))
}

/// No text matches gives an empty result, not a failed count.
fn head_line_counts_from(out: &CommandOutput, cut: bool) -> anyhow::Result<HeadCounts> {
    anyhow::ensure!(!cut, "incomplete git grep output");
    match out.exit_code {
        Some(0) => out
            .stdout
            .lines()
            .map(|line| {
                parse_count_line(line).ok_or_else(|| anyhow::anyhow!("invalid git grep row"))
            })
            .collect(),
        Some(1) if out.stdout.is_empty() => Ok(Vec::new()),
        _ => anyhow::bail!("git grep did not return line counts"),
    }
}

fn parse_count_line(line: &str) -> Option<(String, u64)> {
    // Preserve Git quotes. A colon in the name is not the count separator.
    let (path, count) = line.strip_prefix("HEAD:")?.rsplit_once(':')?;
    if path.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((path.to_owned(), count.parse().ok()?))
}

/// Keep each path byte except Git's line terminator.
fn rev_parse_path(out: &CommandOutput, flag: &str) -> anyhow::Result<String> {
    if out.exit_code != Some(0) {
        anyhow::bail!("git rev-parse {flag} failed: {}", out.stderr.trim());
    }
    Ok(out
        .stdout
        .strip_suffix('\n')
        .unwrap_or(&out.stdout)
        .to_string())
}

// ============================================================================
// Parser
// ============================================================================

/// Commits parsed from `git log` output, with the count of skipped records.
#[derive(Debug, Default)]
pub(crate) struct ParsedLog {
    /// Well-formed commits, in `git log` order.
    pub(crate) commits: Vec<CommitInfo>,
    /// Count of `COMMIT:` records skipped for a missing field, a bad hash or a
    /// bad timestamp. Their numstat lines are skipped with them.
    pub(crate) malformed: usize,
    /// Records skipped because raw and numstat rows did not pair.
    pub(crate) unpaired: usize,
    /// File rows dropped because their resolved path did not decode to a safe path.
    pub(crate) unsafe_paths: usize,
    /// `Some(ts)` when the git output stopped at the runner's output limit:
    /// the last record was dropped, and `ts` is the author timestamp of the
    /// oldest commit kept. `None` for a full output.
    pub(crate) cut_oldest: Option<i64>,
}

/// Parse `git log` with [`LOG_FORMAT`], `--raw` and `--numstat`.
/// The runner bounds the input, and each record uses only aliases from newer records.
#[must_use]
pub(super) fn parse_git_log_output(raw: &str) -> ParsedLog {
    let mut parsed = ParsedLog::default();
    let mut aliases = HashMap::new();
    let mut rows = RecordRows::default();
    let mut current = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix(RECORD_PREFIX) {
            if let Some(commit) = current.take() {
                rows.finish(commit, &mut aliases, &mut parsed);
            }
            rows.clear();
            current = parse_commit_header(rest);
            if current.is_none() {
                parsed.malformed += 1;
            }
        } else if current.is_some() && !line.is_empty() {
            rows.push(line);
        }
    }
    if let Some(commit) = current {
        rows.finish(commit, &mut aliases, &mut parsed);
    }
    parsed
}

#[derive(Default)]
struct RecordRows<'a> {
    raw: Vec<RawRow<'a>>,
    stats: Vec<NumstatRow<'a>>,
    invalid: bool,
}

impl<'a> RecordRows<'a> {
    fn clear(&mut self) {
        self.raw.clear();
        self.stats.clear();
        self.invalid = false;
    }

    fn push(&mut self, line: &'a str) {
        if line.starts_with(':') {
            if let Some(row) = parse_raw_row(line) {
                self.raw.push(row);
                return;
            }
        } else if let Some(row) = parse_numstat_line(line) {
            self.stats.push(row);
            return;
        }
        self.invalid = true;
    }

    fn finish(
        &self,
        mut commit: CommitInfo,
        aliases: &mut HashMap<&'a str, &'a str>,
        parsed: &mut ParsedLog,
    ) {
        let pairs = self.raw.iter().zip(&self.stats);
        if self.invalid
            || self.raw.len() != self.stats.len()
            || pairs
                .clone()
                .any(|(raw, stat)| raw.old.is_none() && raw.path != stat.path)
        {
            parsed.unpaired += 1;
            return;
        }

        let mut indices: HashMap<&str, usize> = HashMap::new();
        let mut renames = Vec::new();
        for (raw, stat) in pairs {
            let path = aliases.get(raw.path).copied().unwrap_or(raw.path);
            if !disk_path(path).is_some_and(|rel| is_repo_relative_safe(&rel)) {
                parsed.unsafe_paths += 1;
                continue;
            }
            if let Some(old) = raw.old {
                renames.push((old, path));
            }
            if let Some((add, del)) = stat.lines {
                if raw.old.is_some() && add == 0 && del == 0 {
                    continue;
                }
                add_file_change(&mut commit.changed_files, &mut indices, path, add, del);
            }
        }

        // A newly created path has no prior history under that name. Apply all
        // changes to the alias table only after every row has used its old state.
        for raw in &self.raw {
            if raw.added || raw.old.is_some() {
                aliases.remove(raw.path);
            }
        }
        aliases.extend(renames);
        parsed.commits.push(commit);
    }
}

fn add_file_change<'a>(
    files: &mut Vec<FileChangeInfo>,
    indices: &mut HashMap<&'a str, usize>,
    path: &'a str,
    additions: u64,
    deletions: u64,
) {
    let next = files.len();
    let index = *indices.entry(path).or_insert(next);
    if let Some(file) = files.get_mut(index) {
        file.additions = file.additions.saturating_add(additions);
        file.deletions = file.deletions.saturating_add(deletions);
    } else {
        // Keep Git's spelling for joins with cards, HEAD paths and diff paths.
        files.push(FileChangeInfo {
            path: PathBuf::from(path),
            additions,
            deletions,
        });
    }
}

/// Parse the part of a record header after `COMMIT:`.
///
/// Returns `None` when a field is missing, the hash is not 40 or 64 ASCII hex
/// characters, or the timestamp is not ASCII digits that fit in `i64`.
fn parse_commit_header(rest: &str) -> Option<CommitInfo> {
    let mut fields = rest.splitn(4, '\0');
    let hash = fields.next()?;
    let timestamp = fields.next()?;
    let author = fields.next()?;
    let subject = fields.next()?;

    if !is_object_id(hash) {
        return None;
    }
    let timestamp = parse_unix_timestamp(timestamp)?;

    Some(CommitInfo {
        hash: hash.to_string(),
        author: author.trim().to_string(),
        timestamp,
        message: subject.trim().to_string(),
        changed_files: Vec::new(),
    })
}

/// `true` for a full SHA-1 (40) or SHA-256 (64) object name in ASCII hex.
fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse a `--date=unix` timestamp: ASCII digits only, and it fits in `i64`.
fn parse_unix_timestamp(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

struct RawRow<'a> {
    path: &'a str,
    old: Option<&'a str>,
    added: bool,
}

fn parse_raw_row(line: &str) -> Option<RawRow<'_>> {
    let (metadata, paths) = line.split_once('\t')?;
    let mut fields = metadata.split(' ');
    for _ in 0..4 {
        fields.next().filter(|field| !field.is_empty())?;
    }
    let status = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let (old, path) = match status {
        "A" | "D" | "M" | "T" | "U" | "X" | "B" => (None, paths),
        _ => {
            let (kind, score) = status.split_at_checked(1)?;
            if score.is_empty() || !score.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            match kind {
                "M" => (None, paths),
                "C" | "R" => {
                    let (source, path) = paths.split_once('\t')?;
                    if source.is_empty() {
                        return None;
                    }
                    ((kind == "R").then_some(source), path)
                }
                _ => return None,
            }
        }
    };
    if path.is_empty() || path.contains('\t') {
        return None;
    }
    Some(RawRow {
        path,
        old,
        added: status == "A",
    })
}

/// Binary rows have no line counts but still take part in ordinal pairing.
struct NumstatRow<'a> {
    path: &'a str,
    lines: Option<(u64, u64)>,
}

fn parse_numstat_line(line: &str) -> Option<NumstatRow<'_>> {
    let (add, rest) = line.split_once('\t')?;
    let (del, path) = rest.split_once('\t')?;
    let lines = match (add, del) {
        ("-", "-") => None,
        (a, d)
            if a.bytes().all(|b| b.is_ascii_digit()) && d.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some((a.parse().ok()?, d.parse().ok()?))
        }
        _ => return None,
    };
    (!path.is_empty()).then_some(NumstatRow { path, lines })
}

/// Decode a path from Git output for a check on disk or against path rules.
/// Keep the quoted form as the shared key for Git log, diff and HEAD paths.
pub(super) fn disk_path(path: &str) -> Option<PathBuf> {
    let (inner, quoted) = without_quotes(path);
    if !quoted {
        return Some(PathBuf::from(path));
    }
    path_from_bytes(c_unescaped(inner)?)
}

/// The bytes of the text inside git's quotes, or `None` for an escape that
/// git does not write. Each pass reads at least one byte, so the loop ends
/// after at most `inner.len()` passes.
fn c_unescaped(inner: &str) -> Option<Vec<u8>> {
    let mut rest = inner.bytes();
    let mut out = Vec::with_capacity(inner.len());
    while let Some(b) = rest.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let byte = match rest.next()? {
            b'a' => 0x07,
            b'b' => 0x08,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 0x0b,
            b'f' => 0x0c,
            b'r' => b'\r',
            b'"' => b'"',
            b'\\' => b'\\',
            // Three octal digits, the first 0-3, so the value fits in a byte.
            first @ b'0'..=b'3' => {
                let mut value = first - b'0';
                for _ in 0..2 {
                    let digit = rest.next().filter(|d| (b'0'..=b'7').contains(d))?;
                    value = value * 8 + (digit - b'0');
                }
                value
            }
            _ => return None,
        };
        out.push(byte);
    }
    Some(out)
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(std::ffi::OsString::from_vec(bytes).into())
}

/// Off Unix a path is text: bytes that are not UTF-8 name no file.
#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(bytes).ok().map(Into::into)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    /// `(add_count, del_count, path)` triplet for a file changed in a commit.
    type FileEntry<'a> = (&'a str, &'a str, &'a str);
    /// `(hash, author, timestamp, subject, files)` row for `make_log`.
    type LogEntry<'a> = (&'a str, &'a str, u64, &'a str, &'a [FileEntry<'a>]);

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// One record header in the NUL-separated `LOG_FORMAT` layout.
    fn header(hash: &str, ts: &str, author: &str, subject: &str) -> String {
        format!("COMMIT:{hash}\0{ts}\0{author}\0{subject}\n")
    }

    fn make_log(entries: &[LogEntry<'_>]) -> String {
        let mut out = String::new();
        for (hash, author, ts, subject, files) in entries {
            out.push_str(&header(hash, &ts.to_string(), author, subject));
            for (_, _, path) in *files {
                out.push_str(&format!(":100644 100644 1234 5678 M\t{path}\n"));
            }
            for (add, del, path) in *files {
                out.push_str(&format!("{add}\t{del}\t{path}\n"));
            }
            out.push('\n');
        }
        out
    }

    fn paths(commit: &CommitInfo) -> Vec<&Path> {
        commit
            .changed_files
            .iter()
            .map(|f| f.path.as_path())
            .collect()
    }

    /// A good record (one file `a.rs`), then `bad` with its own numstat line
    /// `bad.rs`, then a good record (one file `b.rs`).
    fn log_with_bad_record_between(bad: &str) -> String {
        format!(
            "{}:100644 100644 1234 5678 M\ta.rs\n1\t0\ta.rs\n\n{bad}1\t0\tbad.rs\n\n{}:100644 100644 1234 5678 M\tb.rs\n1\t0\tb.rs\n\n",
            header(HASH_A, "1000", "Alice", "first"),
            header(HASH_B, "2000", "Bob", "second"),
        )
    }

    /// Assert that only the two good records of `log_with_bad_record_between`
    /// survive, each with only its own file.
    fn assert_only_good_records(parsed: &ParsedLog) {
        assert_eq!(parsed.commits.len(), 2);
        assert_eq!(parsed.commits[0].hash, HASH_A);
        assert_eq!(paths(&parsed.commits[0]), vec![Path::new("a.rs")]);
        assert_eq!(parsed.commits[1].hash, HASH_B);
        assert_eq!(paths(&parsed.commits[1]), vec![Path::new("b.rs")]);
    }

    fn output(stdout: &str, exit_code: Option<i32>) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: "fatal: test\n".to_string(),
            exit_code,
            duration: Duration::ZERO,
        }
    }

    // -----------------------------------------------------------------------
    // git log record parsing
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_empty_input() {
        let parsed = parse_git_log_output("");
        assert!(parsed.commits.is_empty());
        assert_eq!(parsed.malformed, 0);
    }

    #[test]
    fn test_parse_single_commit_no_files() {
        let input = format!(
            "{}\n",
            header(HASH_A, "1700000000", "Alice", "fix: something")
        );
        let parsed = parse_git_log_output(&input);
        assert_eq!(parsed.malformed, 0);
        assert_eq!(parsed.commits.len(), 1);
        assert_eq!(parsed.commits[0].hash, HASH_A);
        assert_eq!(parsed.commits[0].author, "Alice");
        assert_eq!(parsed.commits[0].timestamp, 1_700_000_000i64);
        assert_eq!(parsed.commits[0].message, "fix: something");
        assert!(parsed.commits[0].changed_files.is_empty());
    }

    #[test]
    fn test_parse_single_commit_with_files() {
        let input = format!(
            "{}:100644 100644 1234 5678 M\tsrc/main.rs\n:100644 100644 1234 5678 M\tlib/utils.rs\n5\t2\tsrc/main.rs\n3\t1\tlib/utils.rs\n\n",
            header(HASH_A, "1700000000", "Alice", "chore: update")
        );
        let parsed = parse_git_log_output(&input);
        assert_eq!(parsed.commits.len(), 1);
        assert_eq!(
            paths(&parsed.commits[0]),
            vec![Path::new("src/main.rs"), Path::new("lib/utils.rs")]
        );
        assert_eq!(parsed.commits[0].changed_files[0].additions, 5);
        assert_eq!(parsed.commits[0].changed_files[0].deletions, 2);
    }

    #[test]
    fn test_parse_multiple_commits() {
        let input = make_log(&[
            (
                HASH_A,
                "Alice",
                1_000,
                "first commit",
                &[("10", "0", "a.rs")],
            ),
            (HASH_B, "Bob", 2_000, "second commit", &[("5", "3", "b.rs")]),
        ]);
        let parsed = parse_git_log_output(&input);
        assert_eq!(parsed.malformed, 0);
        assert_eq!(parsed.commits.len(), 2);
        assert_eq!(parsed.commits[0].hash, HASH_A);
        assert_eq!(parsed.commits[1].hash, HASH_B);
    }

    #[test]
    fn test_parse_author_with_pipe_keeps_timestamp_and_subject() {
        let input = format!(
            "{}:100644 100644 1234 5678 M\ta.rs\n1\t0\ta.rs\n\n",
            header(HASH_A, "1700000123", "Ann | Ops", "feat: add foo|bar")
        );
        let parsed = parse_git_log_output(&input);
        assert_eq!(parsed.malformed, 0);
        assert_eq!(parsed.commits.len(), 1);
        assert_eq!(parsed.commits[0].author, "Ann | Ops");
        assert_eq!(parsed.commits[0].timestamp, 1_700_000_123i64);
        assert_eq!(parsed.commits[0].message, "feat: add foo|bar");
    }

    #[test]
    fn test_parse_accepts_sha256_hash() {
        let hash = "c".repeat(64);
        let input = format!("{}\n", header(&hash, "1000", "Alice", "msg"));
        let parsed = parse_git_log_output(&input);
        assert_eq!(parsed.malformed, 0);
        assert_eq!(parsed.commits.len(), 1);
        assert_eq!(parsed.commits[0].hash, hash);
    }

    #[test]
    fn test_parse_skips_bad_hash_records_with_their_numstat_lines() {
        let not_hex = "g".repeat(40);
        let too_long = "a".repeat(41);
        for bad_hash in ["abc123", not_hex.as_str(), too_long.as_str(), ""] {
            let bad = header(bad_hash, "1500", "Eve", "bad");
            let parsed = parse_git_log_output(&log_with_bad_record_between(&bad));
            assert_eq!(parsed.malformed, 1, "hash {bad_hash:?}");
            assert_only_good_records(&parsed);
        }
    }

    #[test]
    fn test_parse_skips_non_digit_timestamp_records_with_their_numstat_lines() {
        for bad_ts in ["", "17e9", "-1500", "+1500", " 1500", "1500 ", "abc"] {
            let bad = header(&"d".repeat(40), bad_ts, "Eve", "bad");
            let parsed = parse_git_log_output(&log_with_bad_record_between(&bad));
            assert_eq!(parsed.malformed, 1, "timestamp {bad_ts:?}");
            assert_only_good_records(&parsed);
        }
    }

    #[test]
    fn test_parse_skips_timestamp_that_does_not_fit_i64() {
        let bad = header(&"d".repeat(40), "9223372036854775808", "Eve", "bad");
        let parsed = parse_git_log_output(&log_with_bad_record_between(&bad));
        assert_eq!(parsed.malformed, 1);
        assert_only_good_records(&parsed);
    }

    #[test]
    fn test_parse_skips_record_with_missing_field() {
        let bad = format!("COMMIT:{}\x001500\0Eve\n", "d".repeat(40));
        let parsed = parse_git_log_output(&log_with_bad_record_between(&bad));
        assert_eq!(parsed.malformed, 1);
        assert_only_good_records(&parsed);
    }

    #[test]
    fn test_parse_counts_each_malformed_record() {
        let input = format!(
            "{}1\t0\tx.rs\n\n{}1\t0\ty.rs\n\n",
            header("zz", "1000", "Eve", "bad hash"),
            header(HASH_A, "soon", "Eve", "bad timestamp"),
        );
        let parsed = parse_git_log_output(&input);
        assert!(parsed.commits.is_empty());
        assert_eq!(parsed.malformed, 2);
    }

    #[test]
    fn test_binary_files_skipped() {
        let input = format!(
            "{}:100644 100644 1234 5678 M\tbinary.bin\n:100644 100644 1234 5678 M\treal.rs\n-\t-\tbinary.bin\n5\t2\treal.rs\n\n",
            header(HASH_A, "1000", "Alice", "msg")
        );
        let parsed = parse_git_log_output(&input);
        assert_eq!(paths(&parsed.commits[0]), vec![Path::new("real.rs")]);
    }

    #[test]
    fn test_malformed_numstat_line_skips_record() {
        let input = format!(
            "{}not-a-numstat-line\n\n",
            header(HASH_A, "1000", "Alice", "msg")
        );
        let parsed = parse_git_log_output(&input);
        assert!(parsed.commits.is_empty());
        assert_eq!(parsed.malformed, 0);
        assert_eq!(parsed.unpaired, 1);
    }

    #[test]
    fn test_rename_merges_final_paths_before_metrics() {
        let input = format!(
            "{}:100644 100644 1234 5678 R100\told.rs\tnew.rs\n0\t0\told.rs => new.rs\n\n{}:100644 100644 1234 5678 M\told.rs\n:100644 100644 1234 5678 M\tnew.rs\n1\t2\told.rs\n3\t4\tnew.rs\n",
            header(HASH_A, "2000", "Alice", "move"),
            header(HASH_B, "1000", "Alice", "edit"),
        );
        let parsed = parse_git_log_output(&input);
        assert_eq!(paths(&parsed.commits[1]), [Path::new("new.rs")]);
        assert_eq!(parsed.commits[1].changed_files[0].additions, 4);
        assert_eq!(parsed.commits[1].changed_files[0].deletions, 6);
    }

    #[test]
    fn test_rename_follows_only_the_new_file_after_path_reuse() {
        let input = format!(
            "{}:100644 100644 5678 5678 R100\told.rs\tnew.rs\n0\t0\told.rs => new.rs\n\n\
             {}:000000 100644 0000 5678 A\told.rs\n1\t0\told.rs\n\n\
             {}:100644 000000 1234 0000 D\told.rs\n0\t1\told.rs\n\n\
             {}:000000 100644 0000 1234 A\told.rs\n1\t0\told.rs\n",
            header(HASH_A, "4000", "Alice", "rename second file"),
            header(HASH_B, "3000", "Alice", "add second file"),
            header(
                "cccccccccccccccccccccccccccccccccccccccc",
                "2000",
                "Alice",
                "delete first file"
            ),
            header(
                "dddddddddddddddddddddddddddddddddddddddd",
                "1000",
                "Alice",
                "add first file"
            ),
        );

        let parsed = parse_git_log_output(&input);
        assert_eq!(
            parsed.commits.iter().map(paths).collect::<Vec<_>>(),
            [
                vec![],
                vec![Path::new("new.rs")],
                vec![Path::new("old.rs")],
                vec![Path::new("old.rs")]
            ]
        );
    }

    #[test]
    fn test_rename_cycle_resolves_to_one_path_in_one_pass() {
        let input = format!(
            "{}:100644 100644 1234 5678 R90\tb.rs\ta.rs\n1\t0\tb.rs => a.rs\n\n\
             {}:100644 100644 1234 5678 R90\ta.rs\tb.rs\n1\t0\ta.rs => b.rs\n\n\
             {}:100644 100644 1234 5678 M\ta.rs\n1\t0\ta.rs\n",
            header(HASH_A, "3000", "Alice", "move back"),
            header(HASH_B, "2000", "Alice", "move out"),
            header(
                "cccccccccccccccccccccccccccccccccccccccc",
                "1000",
                "Alice",
                "edit"
            ),
        );

        let parsed = parse_git_log_output(&input);
        assert_eq!(parsed.commits.len(), 3);
        for commit in &parsed.commits {
            assert_eq!(paths(commit), [Path::new("a.rs")]);
        }
    }

    #[test]
    fn test_log_skips_unpaired_records_without_installing_aliases() {
        let fresh = header(HASH_A, "2000", "Alice", "bad");
        let prior = header(HASH_B, "1000", "Alice", "good")
            + ":000000 100644 0000 1234 A\told.rs\n1\t0\told.rs\n";
        for bad in [
            ":100644 100644 1234 5678 R100\told.rs\tnew.rs\n:100644 100644 1234 5678 M\tx.rs\n0\t0\told.rs => new.rs\n",
            ":100644 100644 1234 5678 R100\told.rs\tnew.rs\n:100644 100644 1234 5678 M\tx.rs\n0\t0\told.rs => new.rs\n1\t0\ty.rs\n",
            ":100644 100644 1234 5678 M\tx.rs\n+1\t0\tx.rs\n",
            ":100644 100644 1234 5678 M\tx.rs\n-\t0\tx.rs\n",
        ] {
            let parsed = parse_git_log_output(&format!("{fresh}{bad}\n{prior}"));
            assert_eq!(parsed.unpaired, 1, "{bad}");
            assert_eq!(parsed.malformed, 0, "{bad}");
            assert_eq!(parsed.commits.len(), 1, "{bad}");
            assert_eq!(paths(&parsed.commits[0]), [Path::new("old.rs")]);
        }
    }

    #[test]
    fn test_log_drops_paths_that_are_unsafe_after_decode() {
        let input = make_log(&[(
            HASH_A,
            "Alice",
            1000,
            "edit",
            &[
                ("1", "0", "/absolute.rs"),
                ("1", "0", r#""src/\056\056/escape.rs""#),
                ("1", "0", "safe.rs"),
            ],
        )]);
        let parsed = parse_git_log_output(&input);
        assert_eq!(paths(&parsed.commits[0]), [Path::new("safe.rs")]);
        assert_eq!(parsed.unsafe_paths, 2);
    }

    #[test]
    fn test_binary_rename_keeps_alias_but_has_no_file_change() {
        let input = format!(
            "{}:100644 100644 1234 5678 R100\told.rs\tnew.rs\n-\t-\told.rs => new.rs\n{}:100644 100644 1234 5678 M\told.rs\n1\t0\told.rs\n",
            header(HASH_A, "2000", "Alice", "move"),
            header(HASH_B, "1000", "Alice", "edit"),
        );
        let parsed = parse_git_log_output(&input);
        assert_eq!(parsed.unpaired, 0);
        assert!(parsed.commits[0].changed_files.is_empty());
        assert_eq!(paths(&parsed.commits[1]), [Path::new("new.rs")]);
    }

    // -----------------------------------------------------------------------
    // git argument lists
    // -----------------------------------------------------------------------

    #[test]
    fn test_git_log_args_use_nul_record_format() {
        let args = build_git_log_args(Some(1_700_000_000), Some("src/"));
        assert_eq!(
            args,
            vec![
                "-c",
                "log.showSignature=false",
                "-c",
                "diff.relative=false",
                "--literal-pathspecs",
                "log",
                "--format=COMMIT:%H%x00%ad%x00%aN%x00%s",
                "--raw",
                "--numstat",
                "--no-merges",
                "--date=unix",
                "-M",
                "--since=@1700000000 +0000",
                "--",
                "src/",
            ]
        );
    }

    #[test]
    fn test_git_log_args_omit_since_for_epoch_zero() {
        let args = build_git_log_args(Some(0), None);
        assert!(
            !args.iter().any(|arg| arg.starts_with("--since")),
            "args: {args:?}"
        );
    }

    /// The `--path` value is the last argument, after `--`, and the literal
    /// path flag goes before the subcommand, where git reads it.
    #[test]
    fn test_git_log_args_read_the_path_value_as_a_literal_path() {
        let args = build_git_log_args(None, Some("*"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--literal-pathspecs", "log"]),
            "args: {args:?}"
        );
        assert_eq!(args[args.len() - 2..], ["--", "*"], "args: {args:?}");
        assert_eq!(
            args.iter().filter(|a| a.as_str() == "--").count(),
            1,
            "args: {args:?}"
        );
    }

    #[test]
    fn test_diff_args_put_end_of_options_before_revision() {
        assert_eq!(
            build_diff_args("main"),
            vec![
                "-c",
                "diff.relative=false",
                "diff",
                "--name-only",
                "--end-of-options",
                "main...HEAD",
            ]
        );
        let args = build_diff_args("-x");
        let end = args.iter().position(|a| a == "--end-of-options").unwrap();
        let rev = args.iter().position(|a| a == "-x...HEAD").unwrap();
        assert!(end < rev, "args: {args:?}");
        assert!(!args.iter().any(|a| a == "--"), "args: {args:?}");
    }

    /// Each flag value reaches git as one argument: the `--diff` value after
    /// `--end-of-options`, the window start with its UTC offset, and
    /// the `--path` value after `--`.
    #[test]
    fn test_flag_values_reach_git_as_one_argument_each() {
        for base in ["-x", "--x", "-x=y", "x y"] {
            let rev = format!("{base}...HEAD");
            assert_eq!(
                build_diff_args(base),
                vec![
                    "-c",
                    "diff.relative=false",
                    "diff",
                    "--name-only",
                    "--end-of-options",
                    rev.as_str(),
                ],
                "--diff {base:?}"
            );
        }
        let log = build_git_log_args(Some(42), Some("-x"));
        assert_eq!(
            log.iter().filter(|a| a.contains("42")).collect::<Vec<_>>(),
            vec!["--since=@42 +0000"],
            "args: {log:?}"
        );
        assert_eq!(log[log.len() - 2..], ["--", "-x"], "args: {log:?}");
    }

    // -----------------------------------------------------------------------
    // git log run outcome
    // -----------------------------------------------------------------------

    /// Two full records, newest first: `HASH_A` at 2000 with `a.rs`, then
    /// `HASH_B` at 1000 with `b.rs`.
    fn two_records() -> String {
        format!(
            "{}:100644 100644 1234 5678 M\ta.rs\n1\t0\ta.rs\n\n{}:100644 100644 1234 5678 M\tb.rs\n1\t0\tb.rs\n\n",
            header(HASH_A, "2000", "Alice", "second"),
            header(HASH_B, "1000", "Bob", "first"),
        )
    }

    /// Assert the two records of `two_records`, each with only its own file.
    fn assert_two_records(parsed: &ParsedLog) {
        assert_eq!(parsed.commits.len(), 2);
        assert_eq!(parsed.commits[0].hash, HASH_A);
        assert_eq!(paths(&parsed.commits[0]), vec![Path::new("a.rs")]);
        assert_eq!(parsed.commits[1].hash, HASH_B);
        assert_eq!(paths(&parsed.commits[1]), vec![Path::new("b.rs")]);
        assert_eq!(parsed.malformed, 0);
    }

    fn head_check_not_expected() -> bool {
        panic!("the HEAD check must run only after a failed git log")
    }

    #[test]
    fn test_log_exit_zero_keeps_every_record() {
        let out = output(&two_records(), Some(0));
        let parsed = parsed_log_from(&out, false, head_check_not_expected).unwrap();
        assert_two_records(&parsed);
        assert_eq!(parsed.cut_oldest, None);
    }

    #[test]
    fn test_log_cut_at_output_limit_drops_the_last_record() {
        // The cut is in the middle of a numstat path of the third record.
        let raw = format!(
            "{}1\t0\tsrc/foo.rs\n2\t0\tsrc/fo",
            two_records() + &header(&"c".repeat(40), "500", "Cy", "third")
        );
        for exit_code in [None, Some(141), Some(0)] {
            let out = output(&raw, exit_code);
            let parsed = parsed_log_from(&out, true, head_check_not_expected).unwrap();
            assert_two_records(&parsed);
            assert_eq!(parsed.cut_oldest, Some(1000), "exit {exit_code:?}");
        }
    }

    #[test]
    fn test_log_cut_in_raw_block_drops_whole_record() {
        let raw = two_records()
            + &header(&"c".repeat(40), "500", "Cy", "third")
            + ":100644 100644 1234 5678 R100\told.rs\tnew.rs\n:100644 100";
        let parsed = parsed_log_from(&output(&raw, None), true, head_check_not_expected).unwrap();
        assert_two_records(&parsed);
        assert_eq!(parsed.unpaired, 0);
        assert_eq!(parsed.cut_oldest, Some(1000));
    }

    #[test]
    fn test_log_cut_in_a_record_header_counts_no_malformed_record() {
        let raw = two_records() + "COMMIT:cccccccc";
        let parsed = parsed_log_from(&output(&raw, None), true, head_check_not_expected).unwrap();
        assert_two_records(&parsed);
        assert_eq!(parsed.cut_oldest, Some(1000));
    }

    #[test]
    fn test_log_cut_before_the_end_of_the_first_record_is_error() {
        let raw = header(HASH_A, "2000", "Alice", "only") + "1\t0\tsrc/fo";
        let msg = parsed_log_from(&output(&raw, None), true, head_check_not_expected)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("64 MiB output limit"), "got: {msg}");
    }

    #[test]
    fn test_log_nonzero_exit_is_error_with_git_stderr() {
        let out = output(&two_records(), Some(128));
        let msg = parsed_log_from(&out, false, || false)
            .unwrap_err()
            .to_string();
        assert_eq!(msg, "git log ended with exit status 128: fatal: test");
    }

    #[test]
    fn test_log_signal_exit_without_cut_is_error() {
        let out = output(&two_records(), None);
        let msg = parsed_log_from(&out, false, || false)
            .unwrap_err()
            .to_string();
        assert_eq!(msg, "git log ended with a signal: fatal: test");
    }

    #[test]
    fn test_log_failure_on_a_branch_with_no_commits_is_empty_history() {
        let out = output("", Some(128));
        let parsed = parsed_log_from(&out, false, || true).unwrap();
        assert!(parsed.commits.is_empty());
        assert_eq!(parsed.malformed, 0);
        assert_eq!(parsed.cut_oldest, None);
    }

    // -----------------------------------------------------------------------
    // Tracked files and show-prefix
    // -----------------------------------------------------------------------

    #[test]
    fn test_tracked_files_args_read_the_head_tree() {
        assert_eq!(
            tracked_files_args("/repo").as_slice(),
            ["-C", "/repo", "ls-tree", "-r", "-l", "--full-tree", "HEAD"]
        );
    }

    #[test]
    fn test_head_count_args_pass_a_literal_scope_without_submodule_recursion() {
        assert_eq!(
            head_line_count_args("/repo", Some(":(exclude)src")),
            [
                "--literal-pathspecs",
                "grep",
                "-I",
                "-c",
                "--full-name",
                "--no-color",
                "--no-recurse-submodules",
                "-e",
                "",
                "HEAD",
                "--",
                ":(exclude)src",
            ]
        );
    }

    const HEAD_LISTING: &str = concat!(
        "100644 blob 0123456789012345678901234567890123456789      14\ta.rs\n",
        "100755 blob 0123456789012345678901234567890123456789       1\tbin.rs\n",
        "120000 blob 0123456789012345678901234567890123456789       5\tlink.rs\n",
        "160000 commit 0123456789012345678901234567890123456789       -\tsubmodule\n",
    );

    #[test]
    fn test_head_listing_keeps_gitlink_and_padded_size_rows() {
        let entries = tracked_files_from(&output(HEAD_LISTING, Some(0)), false).unwrap();
        assert_eq!(
            entries,
            HashMap::from([
                ("a.rs".into(), HeadEntry::Regular(None)),
                ("bin.rs".into(), HeadEntry::Regular(None)),
                ("link.rs".into(), HeadEntry::Other),
                ("submodule".into(), HeadEntry::Other),
            ])
        );
    }

    #[test]
    fn test_head_listing_rejects_a_partial_result() {
        for (code, cut) in [(Some(128), false), (None, false), (Some(0), true)] {
            assert!(tracked_files_from(&output(HEAD_LISTING, code), cut).is_err());
        }
        for bad_row in [
            "invalid row",
            "100644 blob 0123456789012345678901234567890123456789 -\tbad.rs",
            "100644 blob 0123456789012345678901234567890123456789 1\t../bad.rs",
            "100644 blob 0123456789012345678901234567890123456789 1 extra\tbad.rs",
        ] {
            let rows = format!("{HEAD_LISTING}{bad_row}\n");
            assert!(tracked_files_from(&output(&rows, Some(0)), false).is_err());
        }
    }

    #[test]
    fn test_head_counts_keep_quoted_paths_and_colons() {
        let raw = concat!("HEAD:src/a:b.rs:3\n", "HEAD:\"src/na\\303\\257ve.rs\":11\n",);
        assert_eq!(
            head_line_counts_from(&output(raw, Some(0)), false).unwrap(),
            [
                ("src/a:b.rs".into(), 3),
                (r#""src/na\303\257ve.rs""#.into(), 11)
            ]
        );
    }

    #[test]
    fn test_head_counts_accept_only_complete_output_or_an_empty_exit_one() {
        assert!(
            head_line_counts_from(&output("", Some(1)), false)
                .unwrap()
                .is_empty()
        );
        for (stdout, code, cut) in [
            ("HEAD:ok.rs:2\nother:a.rs:9\n", Some(0), false),
            ("HEAD:ok.rs:2\nHEAD:a.rs:bad\n", Some(0), false),
            ("HEAD:ok.rs:2\nHEAD::7\n", Some(0), false),
            ("HEAD:a.rs:1\n", Some(1), false),
            ("HEAD:a.rs:1\n", Some(0), true),
            ("", Some(128), false),
            ("", None, false),
        ] {
            assert!(head_line_counts_from(&output(stdout, code), cut).is_err());
        }
    }

    #[test]
    fn test_show_prefix_output_keeps_trailing_slash() {
        assert_eq!(
            rev_parse_path(&output("src/cmd/\n", Some(0)), "--show-prefix").unwrap(),
            "src/cmd/"
        );
        assert_eq!(
            rev_parse_path(&output("\n", Some(0)), "--show-prefix").unwrap(),
            ""
        );
    }

    #[test]
    fn test_show_prefix_nonzero_exit_is_error() {
        let msg = rev_parse_path(&output("", Some(128)), "--show-prefix")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("--show-prefix failed"), "got: {msg}");
    }

    /// Run `git -C <dir> <args>` with a fixed identity; panics on failure.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Write each of `files` (relative to `root`) and stage it.
    fn add_files(root: &Path, files: &[&str]) {
        for rel in files {
            let file = root.join(rel);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, "x\n").unwrap();
            git_in(root, &["add", rel]);
        }
    }

    /// Run the heatmap `git log` in `root` with an optional `--path` value.
    fn log_in(root: &Path, path: Option<&str>) -> ParsedLog {
        let mut log_args = vec!["-C".to_string(), root.to_str().unwrap().to_string()];
        log_args.extend(build_git_log_args(None, path));
        let log_args: Vec<&str> = log_args.iter().map(String::as_str).collect();
        let log = CommandRunner::new().run("git", &log_args).unwrap();
        assert_eq!(log.exit_code, Some(0), "git log: {}", log.stderr);
        parse_git_log_output(&log.stdout)
    }

    /// A folder value matches the files below it; a glob or a pathspec
    /// magic value matches only a path with that literal name.
    #[test]
    fn test_git_log_path_matches_a_folder_and_not_a_glob() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_in(root, &["init", "-q"]);
        add_files(root, &["src/sub/a.rs"]);
        git_in(root, &["commit", "-q", "-m", "one"]);
        add_files(root, &["top.rs"]);
        git_in(root, &["commit", "-q", "-m", "two"]);

        for (path, want) in [
            ("src", 1),
            ("src/", 1),
            ("src/sub", 1),
            ("*.rs", 0),
            (":/", 0),
            (":(exclude)src", 0),
        ] {
            let parsed = log_in(root, Some(path));
            assert_eq!(parsed.commits.len(), want, "--path {path:?}");
        }
    }

    #[test]
    fn test_tracked_files_come_from_the_head_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_in(root, &["init", "-q"]);
        add_files(root, &["a.rs"]);
        git_in(root, &["commit", "-q", "-m", "one"]);
        // Staged only: not in the HEAD tree.
        add_files(root, &["b.rs"]);

        let tracked = CliGitSource::new()
            .list_tracked_files(root.to_str().unwrap())
            .unwrap();
        assert_eq!(
            tracked,
            HashMap::from([("a.rs".into(), HeadEntry::Regular(None))])
        );
    }

    #[test]
    fn test_tracked_files_on_a_branch_with_no_commits_give_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_in(root, &["init", "-q"]);
        add_files(root, &["a.rs"]);

        assert!(
            CliGitSource::new()
                .list_tracked_files(root.to_str().unwrap())
                .is_err()
        );
    }

    /// The tracked-file list and the numstat paths must use one encoding, or a
    /// non-ASCII crate folder never matches its own files.
    #[test]
    fn test_tracked_files_and_numstat_paths_share_one_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_in(root, &["init", "-q"]);
        // The default, set here so that a global setting cannot hide a
        // difference between the two encodings.
        git_in(root, &["config", "core.quotePath", "true"]);
        add_files(
            root,
            &["crates/naïve/Cargo.toml", "crates/naïve/src/lib.rs"],
        );
        git_in(root, &["commit", "-q", "-m", "init"]);

        let root_arg = root.to_str().unwrap();
        let source = CliGitSource::new();
        let mut tracked: Vec<_> = source
            .list_tracked_files(root_arg)
            .unwrap()
            .into_keys()
            .collect();
        let mut counted: Vec<_> = source
            .count_head_lines(root_arg, None)
            .unwrap()
            .into_iter()
            .map(|(path, count)| {
                assert_eq!(count, 1, "{path}");
                path
            })
            .collect();

        let parsed = log_in(root, None);
        assert_eq!(parsed.commits.len(), 1);
        let mut numstat: Vec<String> = parsed.commits[0]
            .changed_files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();

        tracked.sort();
        numstat.sort();
        counted.sort();
        assert_eq!(tracked.len(), 2, "tracked: {tracked:?}");
        assert_eq!(numstat, tracked);
        assert_eq!(counted, tracked);
    }

    #[test]
    fn test_head_counts_use_root_relative_names_from_a_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let subfolder = root.join("src");
        git_in(root, &["init", "-q"]);
        add_files(root, &["top.rs", "src/a.rs", "src/deep/b.rs"]);
        git_in(root, &["commit", "-qm", "init"]);
        std::fs::write(root.join("src/deep/b.rs"), "dirty\nmore\n").unwrap();

        for (scope, expected) in [
            (
                None,
                "HEAD:src/a.rs:1\nHEAD:src/deep/b.rs:1\nHEAD:top.rs:1\n",
            ),
            (Some("deep"), "HEAD:src/deep/b.rs:1\n"),
        ] {
            let stdout = git_in(
                &subfolder,
                &head_line_count_args(root.to_str().unwrap(), scope),
            );
            assert_eq!(stdout, expected);
        }
    }
}
