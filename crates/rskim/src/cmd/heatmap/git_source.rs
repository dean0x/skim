//! Git data source for `skim heatmap` — the ONLY I/O file in this module.
//!
//! Uses `CommandRunner` to execute git commands and parses their output into
//! `CommitRecord` values via a simple state machine.

use std::time::Duration;

use crate::runner::{is_spawn_error, CommandRunner};

use super::types::{CommitRecord, FileChange, HeatmapConfig};

// ============================================================================
// Trait — for testability
// ============================================================================

/// Abstraction over git data sources.
///
/// The only implementation is [`CliGitSource`]; the trait enables unit-testing
/// the orchestration logic in `mod.rs` without spawning a real git process.
pub(crate) trait GitDataSource {
    fn fetch_commits(&self, config: &HeatmapConfig) -> anyhow::Result<Vec<CommitRecord>>;
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

    /// Return `true` when the cwd is inside a git repository.
    pub(crate) fn is_git_repo(&self) -> bool {
        match self
            .runner
            .run("git", &["rev-parse", "--is-inside-work-tree"])
        {
            Ok(out) => out.exit_code == Some(0),
            Err(_) => false,
        }
    }

    /// Return the repository root path.
    pub(crate) fn get_repo_root(&self) -> anyhow::Result<String> {
        let out = self
            .runner
            .run("git", &["rev-parse", "--show-toplevel"])
            .map_err(|e| {
                if is_spawn_error(&e) {
                    anyhow::anyhow!("git is not installed or not in PATH")
                } else {
                    e
                }
            })?;
        Ok(out.stdout.trim().to_string())
    }

    /// Return `true` when the repo is a shallow clone.
    pub(crate) fn detect_shallow_clone(&self) -> bool {
        match self
            .runner
            .run("git", &["rev-parse", "--is-shallow-repository"])
        {
            Ok(out) => out.stdout.trim() == "true",
            Err(_) => false,
        }
    }

    /// Fetch the Unix timestamp of the Nth-oldest commit within the last `n` commits.
    ///
    /// Returns `None` when the repo has fewer than `n` commits.
    pub(crate) fn fetch_commit_count_since(&self, n: usize) -> anyhow::Result<Option<u64>> {
        if n == 0 {
            return Ok(None);
        }
        let skip = format!("{}", n.saturating_sub(1));
        let n_str = format!("{n}");
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
        let ts: u64 = trimmed
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .unwrap_or(0);
        if ts == 0 {
            Ok(None)
        } else {
            Ok(Some(ts))
        }
    }

    /// Build the git log arg list from a config.
    fn build_git_log_args<'a>(
        &self,
        config: &HeatmapConfig,
        extra: &'a mut Vec<String>,
    ) -> Vec<&'a str> {
        // The base args that don't vary.
        extra.push("log".to_string());
        extra.push("--format=COMMIT:%H|%aN|%ad|%s".to_string());
        extra.push("--numstat".to_string());
        extra.push("--no-merges".to_string());
        extra.push("--date=unix".to_string());
        extra.push("-M".to_string());

        if let Some(since) = config.since {
            extra.push(format!("--since={since}"));
        }

        if let Some(ref path) = config.path {
            extra.push("--".to_string());
            extra.push(path.clone());
        }

        extra.iter().map(String::as_str).collect()
    }
}

impl GitDataSource for CliGitSource {
    fn fetch_commits(&self, config: &HeatmapConfig) -> anyhow::Result<Vec<CommitRecord>> {
        let mut owned_args: Vec<String> = Vec::new();
        let args = self.build_git_log_args(config, &mut owned_args);

        let output = self.runner.run("git", &args).map_err(|e| {
            if is_spawn_error(&e) {
                anyhow::anyhow!("git is not installed or not in PATH")
            } else {
                e
            }
        })?;

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
pub(crate) fn parse_git_log_output(raw: &str) -> anyhow::Result<Vec<CommitRecord>> {
    let mut commits: Vec<CommitRecord> = Vec::new();
    let mut current: Option<CommitRecord> = None;

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

            current = Some(CommitRecord {
                hash,
                author,
                timestamp,
                subject,
                files: Vec::new(),
            });
        } else if line.trim().is_empty() {
            // Blank lines separate commits — skip
            continue;
        } else {
            // Numstat line: additions\tdeletions\tpath
            // Binary files: - - path
            if let Some(record) = current.as_mut() {
                if let Some(file_change) = parse_numstat_line(line) {
                    record.files.push(file_change);
                }
            }
        }
    }

    // Flush the last commit
    if let Some(c) = current {
        commits.push(c);
    }

    Ok(commits)
}

/// Parse a single git numstat line into a `FileChange`.
///
/// Returns `None` for binary files (marked with `-` in both columns) or
/// malformed lines.
fn parse_numstat_line(line: &str) -> Option<FileChange> {
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

    Some(FileChange {
        path,
        additions,
        deletions,
    })
}

/// Resolve a git rename path like `{old => new}` or `dir/{old => new}/file`.
fn resolve_rename(raw: &str) -> String {
    // Find `{...}` brace pair
    if let (Some(open), Some(close)) = (raw.find('{'), raw.rfind('}')) {
        if open < close {
            let prefix = &raw[..open];
            let suffix = &raw[close + 1..];
            let inner = &raw[open + 1..close];

            // inner is "old => new"
            if let Some(arrow_pos) = inner.find(" => ") {
                let new_part = &inner[arrow_pos + 4..];
                // Reconstruct: prefix + new_part + suffix
                let resolved = format!("{prefix}{new_part}{suffix}");
                return resolved;
            }
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
        assert_eq!(result[0].timestamp, 1_700_000_000);
        assert_eq!(result[0].subject, "fix: something");
        assert!(result[0].files.is_empty());
    }

    #[test]
    fn test_parse_single_commit_with_files() {
        let input = "COMMIT:abc123|Alice|1700000000|chore: update\n5\t2\tsrc/main.rs\n3\t1\tlib/utils.rs\n\n";
        let result = parse_git_log_output(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].files.len(), 2);
        assert_eq!(result[0].files[0].path, "src/main.rs");
        assert_eq!(result[0].files[0].additions, 5);
        assert_eq!(result[0].files[0].deletions, 2);
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
        assert_eq!(result[0].files.len(), 1);
        assert_eq!(result[0].files[0].path, "real.rs");
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
        assert_eq!(result[0].subject, "feat: add foo|bar");
    }

    #[test]
    fn test_malformed_numstat_line_ignored() {
        let input = "COMMIT:abc|Alice|1000|msg\nnot-a-numstat-line\n\n";
        let result = parse_git_log_output(input).unwrap();
        // The malformed line should be silently ignored
        assert_eq!(result[0].files.len(), 0);
    }
}
