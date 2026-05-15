//! Git data source for `skim heatmap` — the ONLY I/O file in this module.
//!
//! Uses `CommandRunner` to execute git commands and parses their output into
//! `CommitInfo` values via a simple state machine.

use std::time::Duration;

use crate::runner::{CommandRunner, is_spawn_error};

use super::types::{CommitInfo, FileChangeInfo, HeatmapConfig};

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

    /// Fetch commit records according to `config`.
    fn fetch_commits(&self, config: &HeatmapConfig) -> anyhow::Result<Vec<CommitInfo>>;
}

// ============================================================================
// CLI implementation
// ============================================================================

/// Concrete git data source that delegates to the `git` binary.
pub(crate) struct CliGitSource {
    runner: CommandRunner,
}

impl CliGitSource {
    pub(crate) fn new() -> Self {
        Self {
            runner: CommandRunner::new(Some(Duration::from_secs(120))),
        }
    }

    /// Build the git log arg list from a config.
    fn build_git_log_args(&self, config: &HeatmapConfig) -> Vec<String> {
        let mut args = vec![
            "log".to_string(),
            "--format=COMMIT:%H|%aN|%ad|%s".to_string(),
            "--numstat".to_string(),
            "--no-merges".to_string(),
            "--date=unix".to_string(),
            "-M".to_string(),
        ];

        if let Some(since) = config.since {
            args.push(format!("--since={since}"));
        }

        if let Some(ref path) = config.path {
            args.push("--".to_string());
            args.push(path.clone());
        }

        args
    }

    /// Resolve files changed between `base` and HEAD using three-dot diff.
    ///
    /// Uses `git diff --name-only <base>...HEAD` so that only commits reachable
    /// from HEAD but not from `base` are included.
    pub(crate) fn fetch_diff_files(&self, base: &str) -> anyhow::Result<Vec<String>> {
        let arg = format!("{base}...HEAD");
        let out = self
            .runner
            .run("git", &["diff", "--name-only", &arg])
            .map_err(git_not_found)?;

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
        let trimmed = out.stdout.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        // trimmed is non-empty; first line is the timestamp
        let ts: u64 = trimmed.lines().next().unwrap_or("").parse().unwrap_or(0);
        Ok((ts > 0).then_some(ts))
    }

    fn fetch_commits(&self, config: &HeatmapConfig) -> anyhow::Result<Vec<CommitInfo>> {
        let owned_args = self.build_git_log_args(config);
        let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();
        let output = self.runner.run("git", &args).map_err(git_not_found)?;
        parse_git_log_output(&output.stdout)
    }
}

// ============================================================================
// Parser
// ============================================================================

/// Parse the output of `git log --format=COMMIT:%H|%aN|%ad|%s --numstat`.
///
/// Exposed as `pub(crate)` so unit tests can exercise the parser on hardcoded
/// strings without spawning a real git process.
pub(crate) fn parse_git_log_output(raw: &str) -> anyhow::Result<Vec<CommitInfo>> {
    let mut commits: Vec<CommitInfo> = Vec::new();
    let mut current: Option<CommitInfo> = None;

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("COMMIT:") {
            // Flush the previous commit
            if let Some(c) = current.take() {
                commits.push(c);
            }
            // Parse: hash|author|timestamp|subject
            let parts: Vec<&str> = rest.splitn(4, '|').collect();
            if parts.len() < 4 {
                continue;
            }
            let hash = parts[0].trim().to_string();
            let author = parts[1].trim().to_string();
            let timestamp: u64 = parts[2].trim().parse().unwrap_or(0);
            let subject = parts[3].trim().to_string();

            current = Some(CommitInfo {
                hash,
                author,
                timestamp: i64::try_from(timestamp).unwrap_or(i64::MAX),
                message: subject,
                changed_files: Vec::new(),
            });
        } else if line.trim().is_empty() {
            // Blank lines separate commits — skip
            continue;
        } else {
            // Numstat line: additions\tdeletions\tpath
            // Binary files: - - path
            if let Some(record) = current.as_mut()
                && let Some(file_change) = parse_numstat_line(line)
            {
                record.changed_files.push(file_change);
            }
        }
    }

    // Flush the last commit
    if let Some(c) = current {
        commits.push(c);
    }

    Ok(commits)
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
mod tests {
    use super::*;

    fn make_log(entries: &[(&str, &str, u64, &str, &[(&str, &str, &str)])]) -> String {
        let mut out = String::new();
        for (hash, author, ts, subject, files) in entries {
            out.push_str(&format!("COMMIT:{hash}|{author}|{ts}|{subject}\n"));
            for (add, del, path) in *files {
                out.push_str(&format!("{add}\t{del}\t{path}\n"));
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn test_parse_empty_input() {
        let result = parse_git_log_output("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_single_commit_no_files() {
        let input = "COMMIT:abc123|Alice|1700000000|fix: something\n\n";
        let result = parse_git_log_output(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].hash, "abc123");
        assert_eq!(result[0].author, "Alice");
        assert_eq!(result[0].timestamp, 1_700_000_000i64);
        assert_eq!(result[0].message, "fix: something");
        assert!(result[0].changed_files.is_empty());
    }

    #[test]
    fn test_parse_single_commit_with_files() {
        let input = "COMMIT:abc123|Alice|1700000000|chore: update\n5\t2\tsrc/main.rs\n3\t1\tlib/utils.rs\n\n";
        let result = parse_git_log_output(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].changed_files.len(), 2);
        assert_eq!(result[0].changed_files[0].path, std::path::Path::new("src/main.rs"));
        assert_eq!(result[0].changed_files[0].additions, 5);
        assert_eq!(result[0].changed_files[0].deletions, 2);
    }

    #[test]
    fn test_parse_multiple_commits() {
        let input = make_log(&[
            (
                "hash1",
                "Alice",
                1_000,
                "first commit",
                &[("10", "0", "a.rs")],
            ),
            (
                "hash2",
                "Bob",
                2_000,
                "second commit",
                &[("5", "3", "b.rs")],
            ),
        ]);
        let result = parse_git_log_output(&input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].hash, "hash1");
        assert_eq!(result[1].hash, "hash2");
    }

    #[test]
    fn test_binary_files_skipped() {
        let input = "COMMIT:abc|Alice|1000|msg\n-\t-\tbinary.bin\n5\t2\treal.rs\n\n";
        let result = parse_git_log_output(input).unwrap();
        assert_eq!(result[0].changed_files.len(), 1);
        assert_eq!(result[0].changed_files[0].path, std::path::Path::new("real.rs"));
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

    #[test]
    fn test_parse_commit_subject_with_pipe() {
        // Subject may contain the separator — splitn(4) protects us
        let input = "COMMIT:abc|Alice|1000|feat: add foo|bar\n\n";
        let result = parse_git_log_output(input).unwrap();
        assert_eq!(result[0].message, "feat: add foo|bar");
    }

    #[test]
    fn test_malformed_numstat_line_ignored() {
        let input = "COMMIT:abc|Alice|1000|msg\nnot-a-numstat-line\n\n";
        let result = parse_git_log_output(input).unwrap();
        // The malformed line should be silently ignored
        assert_eq!(result[0].changed_files.len(), 0);
    }
}
