//! Git data source for `skim heatmap` — the ONLY I/O file in this module.
//!
//! Uses `CommandRunner` to execute git commands and parses their output into
//! `CommitInfo` values via a simple state machine.

use crate::runner::{CommandOutput, CommandRunner, is_spawn_error};

use super::types::{CommitInfo, FileChangeInfo, HeatmapConfig};

/// `git log` record header: hash, author timestamp, author name and subject,
/// separated by NUL bytes.
///
/// The hash and the timestamp come first, so no content of an author name or
/// a subject (a `|`, for example) can move them.
const LOG_FORMAT: &str = "--format=COMMIT:%H%x00%ad%x00%aN%x00%s";

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
/// No method takes a path from the user. A future method that does must put
/// the path after `--` and run git with `GIT_LITERAL_PATHSPECS=1`
/// (`CommandRunner::run_with_env`).
///
/// Long-term, this trait will be replaced by `rskim_search::TemporalSource` +
/// `GixSource` once the heatmap pipeline migrates from CLI git to the gix library.
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
    fn fetch_commits(&self, config: &HeatmapConfig) -> anyhow::Result<ParsedLog>;

    /// List the paths in the index of the repository at `repo_root`, relative
    /// to that root.
    ///
    /// The paths use the same encoding as the `--numstat` paths of
    /// [`fetch_commits`](Self::fetch_commits): git C-quotes unusual names in
    /// both. `Ok(None)` means the list is not available; the caller uses its
    /// fallback and the run continues.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "package-level modules are the first caller")
    )]
    fn list_tracked_files(&self, repo_root: &str) -> anyhow::Result<Option<Vec<String>>>;

    /// Return the path of the cwd below the repository root
    /// (`git rev-parse --show-prefix`): empty at the root, else it ends in `/`.
    #[expect(dead_code, reason = "the file card is the first caller")]
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
fn build_git_log_args(since: Option<u64>, path: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "log".to_string(),
        LOG_FORMAT.to_string(),
        "--numstat".to_string(),
        "--no-merges".to_string(),
        "--date=unix".to_string(),
        "-M".to_string(),
    ];

    if let Some(since) = since {
        args.push(format!("--since={since}"));
    }

    if let Some(path) = path {
        args.push("--".to_string());
        args.push(path.to_string());
    }

    args
}

/// Build the `git diff` arg list for the files changed between `base` and HEAD.
///
/// `--end-of-options` makes git read the next argument as a revision, so the
/// `--diff` value reaches git as a value only. The revision must never go
/// after `--`: git reads anything there as a path.
fn build_diff_args(base: &str) -> Vec<String> {
    vec![
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

    /// Resolve files changed between `base` and HEAD using three-dot diff.
    ///
    /// Uses `git diff --name-only --end-of-options <base>...HEAD` so that only
    /// commits reachable from HEAD but not from `base` are included.
    pub(crate) fn fetch_diff_files(&self, base: &str) -> anyhow::Result<Vec<String>> {
        let owned_args = build_diff_args(base);
        let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();
        let out = self.runner.run("git", &args).map_err(git_not_found)?;

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
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(|l| l.strip_prefix("./").unwrap_or(l).to_string())
            .collect())
    }
}

impl GitDataSource for CliGitSource {
    fn is_git_repo(&self) -> bool {
        match self
            .runner
            .run("git", &["rev-parse", "--is-inside-work-tree"])
        {
            Ok(out) => out.exit_code == Some(0),
            Err(_) => false,
        }
    }

    fn get_repo_root(&self) -> anyhow::Result<String> {
        let out = self
            .runner
            .run("git", &["rev-parse", "--show-toplevel"])
            .map_err(git_not_found)?;
        Ok(out.stdout.trim().to_string())
    }

    fn detect_shallow_clone(&self) -> bool {
        match self
            .runner
            .run("git", &["rev-parse", "--is-shallow-repository"])
        {
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
        let out = self.runner.run(
            "git",
            &[
                "log",
                "--format=%ad",
                "--date=unix",
                "-n",
                &n_str,
                "--skip",
                &skip,
            ],
        )?;
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
        let output = self.runner.run("git", &args).map_err(git_not_found)?;
        Ok(parse_git_log_output(&output.stdout))
    }

    fn list_tracked_files(&self, repo_root: &str) -> anyhow::Result<Option<Vec<String>>> {
        // No `-z`: the plain output C-quotes unusual paths exactly as
        // `--numstat` does, so both lists use one path encoding for any
        // `core.quotePath` setting.
        let result = self.runner.run("git", &["-C", repo_root, "ls-files"]);
        Ok(tracked_files_from(result))
    }

    fn show_prefix(&self) -> anyhow::Result<String> {
        let out = self
            .runner
            .run("git", &["rev-parse", "--show-prefix"])
            .map_err(git_not_found)?;
        prefix_from(&out)
    }
}

/// Map a `git ls-files` run to its path list.
///
/// A spawn error or any exit status other than 0 gives `None` (with one
/// debug-gated note), and the caller uses its fallback. A run cut at the
/// runner's output cap does not report exit 0, so a partial list is never
/// used as a full one.
// rustc versions differ on whether this is live while its trait method has
// no caller; remove this `allow` with the `expect` on `list_tracked_files`.
#[cfg_attr(not(test), allow(dead_code))]
fn tracked_files_from(result: anyhow::Result<CommandOutput>) -> Option<Vec<String>> {
    match result {
        Ok(out) if out.exit_code == Some(0) => Some(
            out.stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        Ok(out) => {
            crate::debug_log!(
                "[skim:heatmap] git ls-files exit status {:?}; modules use the top-folder rule",
                out.exit_code
            );
            None
        }
        Err(e) => {
            crate::debug_log!(
                "[skim:heatmap] git ls-files did not run ({e}); modules use the top-folder rule"
            );
            None
        }
    }
}

/// Read the output of `git rev-parse --show-prefix`.
// rustc versions differ on whether this is live while its trait method has
// no caller; remove this `allow` with the `expect` on `show_prefix`.
#[cfg_attr(not(test), allow(dead_code))]
fn prefix_from(out: &CommandOutput) -> anyhow::Result<String> {
    if out.exit_code != Some(0) {
        anyhow::bail!("git rev-parse --show-prefix failed: {}", out.stderr.trim());
    }
    // Remove only the line end: the prefix itself ends in `/`.
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
}

/// Parse the output of `git log` with [`LOG_FORMAT`] and `--numstat`.
///
/// Exposed as `pub(crate)` so unit tests can exercise the parser on hardcoded
/// strings without spawning a real git process.
#[must_use]
pub(crate) fn parse_git_log_output(raw: &str) -> ParsedLog {
    let mut commits: Vec<CommitInfo> = Vec::new();
    let mut malformed = 0usize;
    let mut current: Option<CommitInfo> = None;

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("COMMIT:") {
            // Flush the previous commit
            if let Some(c) = current.take() {
                commits.push(c);
            }
            // A bad record leaves `current` as `None`, so its numstat lines
            // attach to no commit.
            current = parse_commit_header(rest);
            if current.is_none() {
                malformed += 1;
            }
        } else if line.trim().is_empty() {
            // Blank lines separate commits — skip
            continue;
        } else if let Some(record) = current.as_mut()
            && let Some(file_change) = parse_numstat_line(line)
        {
            // Numstat line: additions\tdeletions\tpath (binary files: - - path)
            record.changed_files.push(file_change);
        }
    }

    // Flush the last commit
    if let Some(c) = current {
        commits.push(c);
    }

    ParsedLog { commits, malformed }
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

    if !is_commit_hash(hash) {
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
fn is_commit_hash(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse a `--date=unix` timestamp: ASCII digits only, and it fits in `i64`.
fn parse_unix_timestamp(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Parse a single git numstat line into a `FileChangeInfo`.
///
/// Returns `None` for binary files (marked with `-` in both columns) or
/// malformed lines.
fn parse_numstat_line(line: &str) -> Option<FileChangeInfo> {
    let parts: Vec<&str> = line.splitn(3, '\t').collect();
    if parts.len() < 3 {
        return None;
    }

    let additions_str = parts[0].trim();
    let deletions_str = parts[1].trim();
    let raw_path = parts[2].trim();

    // Skip binary files
    if additions_str == "-" && deletions_str == "-" {
        return None;
    }

    let additions: u64 = additions_str.parse().unwrap_or(0);
    let deletions: u64 = deletions_str.parse().unwrap_or(0);

    // Resolve renames: `{old => new}` or `dir/{old => new}/rest`
    let path = resolve_rename(raw_path);

    Some(FileChangeInfo {
        path: std::path::PathBuf::from(path),
        additions,
        deletions,
    })
}

/// Resolve a git rename path like `{old => new}` or `dir/{old => new}/file`.
fn resolve_rename(raw: &str) -> String {
    // Find `{...}` brace pair
    if let (Some(open), Some(close)) = (raw.find('{'), raw.rfind('}'))
        && open < close
    {
        let prefix = &raw[..open];
        let suffix = &raw[close + 1..];
        let inner = &raw[open + 1..close];

        // inner is "old => new"
        if let Some(arrow_pos) = inner.find(" => ") {
            let new_part = &inner[arrow_pos + 4..];
            return format!("{prefix}{new_part}{suffix}");
        }
    }
    raw.to_string()
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
            "{}1\t0\ta.rs\n\n{bad}1\t0\tbad.rs\n\n{}1\t0\tb.rs\n\n",
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
            "{}5\t2\tsrc/main.rs\n3\t1\tlib/utils.rs\n\n",
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
            "{}1\t0\ta.rs\n\n",
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
            "{}-\t-\tbinary.bin\n5\t2\treal.rs\n\n",
            header(HASH_A, "1000", "Alice", "msg")
        );
        let parsed = parse_git_log_output(&input);
        assert_eq!(paths(&parsed.commits[0]), vec![Path::new("real.rs")]);
    }

    #[test]
    fn test_malformed_numstat_line_ignored() {
        let input = format!(
            "{}not-a-numstat-line\n\n",
            header(HASH_A, "1000", "Alice", "msg")
        );
        let parsed = parse_git_log_output(&input);
        // The malformed line should be silently ignored
        assert_eq!(parsed.commits[0].changed_files.len(), 0);
        assert_eq!(parsed.malformed, 0);
    }

    #[test]
    fn test_rename_resolution_simple() {
        assert_eq!(resolve_rename("{old.rs => new.rs}"), "new.rs");
    }

    #[test]
    fn test_rename_resolution_with_prefix() {
        assert_eq!(resolve_rename("src/{old.rs => new.rs}"), "src/new.rs");
    }

    #[test]
    fn test_rename_resolution_with_suffix() {
        assert_eq!(
            resolve_rename("src/{old => new}/main.rs"),
            "src/new/main.rs"
        );
    }

    #[test]
    fn test_no_rename_passthrough() {
        assert_eq!(resolve_rename("src/main.rs"), "src/main.rs");
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
                "log",
                "--format=COMMIT:%H%x00%ad%x00%aN%x00%s",
                "--numstat",
                "--no-merges",
                "--date=unix",
                "-M",
                "--since=1700000000",
                "--",
                "src/",
            ]
        );
    }

    #[test]
    fn test_diff_args_put_end_of_options_before_revision() {
        assert_eq!(
            build_diff_args("main"),
            vec!["diff", "--name-only", "--end-of-options", "main...HEAD"]
        );
        let args = build_diff_args("-x");
        let end = args.iter().position(|a| a == "--end-of-options").unwrap();
        let rev = args.iter().position(|a| a == "-x...HEAD").unwrap();
        assert!(end < rev, "args: {args:?}");
        assert!(!args.iter().any(|a| a == "--"), "args: {args:?}");
    }

    // -----------------------------------------------------------------------
    // Tracked files and show-prefix
    // -----------------------------------------------------------------------

    #[test]
    fn test_tracked_files_nonzero_exit_gives_none() {
        assert_eq!(tracked_files_from(Ok(output("a.rs\n", Some(128)))), None);
    }

    #[test]
    fn test_tracked_files_signal_exit_gives_none() {
        // A run cut at the output cap ends with no exit code; its partial
        // list must not be used.
        assert_eq!(tracked_files_from(Ok(output("a.rs\n", None))), None);
    }

    #[test]
    fn test_tracked_files_spawn_error_gives_none() {
        let err = anyhow::Error::new(crate::runner::RunnerError::SpawnFailed {
            program: "git".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        });
        assert_eq!(tracked_files_from(Err(err)), None);
    }

    #[test]
    fn test_show_prefix_output_keeps_trailing_slash() {
        assert_eq!(
            prefix_from(&output("src/cmd/\n", Some(0))).unwrap(),
            "src/cmd/"
        );
        assert_eq!(prefix_from(&output("\n", Some(0))).unwrap(), "");
    }

    #[test]
    fn test_show_prefix_nonzero_exit_is_error() {
        let msg = prefix_from(&output("", Some(128))).unwrap_err().to_string();
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
        for rel in ["crates/naïve/Cargo.toml", "crates/naïve/src/lib.rs"] {
            let file = root.join(rel);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, "x\n").unwrap();
        }
        git_in(root, &["add", "."]);
        git_in(root, &["commit", "-q", "-m", "init"]);
        let root_str = root.to_str().unwrap();

        let mut tracked = CliGitSource::new()
            .list_tracked_files(root_str)
            .unwrap()
            .expect("ls-files ran in a git repo");

        let mut log_args = vec!["-C".to_string(), root_str.to_string()];
        log_args.extend(build_git_log_args(None, None));
        let log_args: Vec<&str> = log_args.iter().map(String::as_str).collect();
        let log = CommandRunner::new().run("git", &log_args).unwrap();
        let parsed = parse_git_log_output(&log.stdout);
        assert_eq!(parsed.commits.len(), 1);
        let mut numstat: Vec<String> = parsed.commits[0]
            .changed_files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();

        tracked.sort();
        numstat.sort();
        assert_eq!(tracked.len(), 2, "tracked: {tracked:?}");
        assert_eq!(numstat, tracked);
    }
}
