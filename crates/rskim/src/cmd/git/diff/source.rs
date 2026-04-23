//! File source resolution for AST parsing in the diff pipeline.

use std::path::PathBuf;

use crate::cmd::user_has_flag;
use crate::runner::CommandRunner;

/// Resolve the working tree root from global flags.
///
/// Checks for `-C <path>`, `--work-tree <path>`, or `--work-tree=<path>`.
/// Returns `None` if no path override is present.
pub(super) fn resolve_work_tree(global_flags: &[String]) -> Option<PathBuf> {
    let mut i = 0;
    while i < global_flags.len() {
        let flag = &global_flags[i];

        if flag == "-C" || flag == "--work-tree" {
            if let Some(val) = global_flags.get(i + 1) {
                return Some(PathBuf::from(val));
            }
        }

        if let Some(val) = flag.strip_prefix("--work-tree=") {
            return Some(PathBuf::from(val));
        }

        i += 1;
    }
    None
}

/// Extract the right-hand side of a range separator (`..` or `...`).
///
/// Returns `"HEAD"` when the right side is empty (e.g., `"main.."`).
pub(super) fn extract_range_right(arg: &str, separator: &str) -> Option<String> {
    let pos = arg.find(separator)?;
    let right = &arg[pos + separator.len()..];
    Some(if right.is_empty() {
        "HEAD".to_string()
    } else {
        right.to_string()
    })
}

/// Run `git show <ref_spec>` and return stdout, or bail on failure.
pub(super) fn git_show(global_flags: &[String], ref_spec: &str) -> anyhow::Result<String> {
    // Guard against argument injection: a ref_spec starting with `-` could be
    // interpreted as a flag by `git show`.
    if ref_spec.starts_with('-') {
        anyhow::bail!("invalid ref spec: {ref_spec:?} (must not start with '-')");
    }
    // SECURITY: `-c` values within `global_flags` are passed to git as-is. This is
    // intentional: skim trusts its CLI caller. Agent hooks must not forward
    // untrusted `-c` values from external sources.
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["show".to_string(), ref_spec.to_string()]);
    let runner = CommandRunner::new(Some(crate::cmd::DEFAULT_CMD_TIMEOUT));
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
    let output = runner.run("git", &arg_refs)?;
    if output.exit_code != Some(0) {
        anyhow::bail!("git show {ref_spec} failed: {}", output.stderr.trim());
    }
    Ok(output.stdout)
}

/// Resolve the file source content for AST parsing.
///
/// - Unstaged (working tree): read from disk (respecting `-C` / `--work-tree`)
/// - `--cached` / `--staged`: use `git show :path`
/// - Commit range (`A..B` or `A B`): use `git show B:path`
pub(super) fn get_file_source(
    path: &str,
    global_flags: &[String],
    args: &[String],
) -> anyhow::Result<String> {
    // Reject null bytes — they could truncate the ref spec passed to git.
    if path.contains('\0') {
        anyhow::bail!("invalid diff path: contains null byte");
    }

    if user_has_flag(args, &["--cached", "--staged"]) {
        return git_show(global_flags, &format!(":{path}"));
    }

    // Check for commit range in args (e.g., "HEAD~2..HEAD" or "A...B").
    // Try three-dot first so `find("..")` doesn't accidentally match at the
    // wrong position inside a `...` range.
    let range_commit = args
        .iter()
        .find_map(|a| extract_range_right(a, "...").or_else(|| extract_range_right(a, "..")));

    if let Some(commit) = range_commit {
        return git_show(global_flags, &format!("{commit}:{path}"));
    }

    // Default: read from working tree (disk).
    // When `-C` or `--work-tree` is set, prepend that path to the file path.
    let root = resolve_work_tree(global_flags);
    let disk_path = match &root {
        Some(r) => r.join(path),
        None => PathBuf::from(path),
    };

    // Path-traversal guard: canonicalize and verify the resolved path stays
    // within the work-tree root (or CWD when no explicit root is set).
    let canonical = disk_path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("failed to resolve {}: {e}", disk_path.display()))?;
    let base = match &root {
        Some(r) => r
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("failed to resolve work tree {}: {e}", r.display()))?,
        None => std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to get current directory: {e}"))?,
    };
    if !canonical.starts_with(&base) {
        anyhow::bail!(
            "path traversal detected: {} escapes work tree {}",
            canonical.display(),
            base.display()
        );
    }

    std::fs::read_to_string(&canonical)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", canonical.display()))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Path resolution with -C flag tests (Gap 4)
    // ========================================================================

    #[test]
    fn test_resolve_work_tree_with_c_flag() {
        let flags: Vec<String> = vec!["-C".into(), "/other/repo".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, Some(PathBuf::from("/other/repo")));
    }

    #[test]
    fn test_resolve_work_tree_with_work_tree_flag() {
        let flags: Vec<String> = vec!["--work-tree".into(), "/other/repo".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, Some(PathBuf::from("/other/repo")));
    }

    #[test]
    fn test_resolve_work_tree_with_work_tree_equals() {
        let flags: Vec<String> = vec!["--work-tree=/other/repo".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, Some(PathBuf::from("/other/repo")));
    }

    #[test]
    fn test_resolve_work_tree_none() {
        let flags: Vec<String> = vec!["--no-pager".into()];
        let result = resolve_work_tree(&flags);
        assert_eq!(result, None);
    }

    #[test]
    fn test_get_file_source_with_c_flag_path() {
        // Create a temp dir with a file
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let global_flags: Vec<String> =
            vec!["-C".into(), dir.path().to_string_lossy().into_owned()];
        let args: Vec<String> = vec![];

        let result = get_file_source("test.txt", &global_flags, &args);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "hello world");
    }

    // ========================================================================
    // extract_range_right unit tests (Issue source:35:testing)
    // ========================================================================

    #[test]
    fn test_extract_range_right_two_dot_with_content() {
        let result = extract_range_right("main..feature", "..");
        assert_eq!(result, Some("feature".to_string()));
    }

    #[test]
    fn test_extract_range_right_two_dot_empty_right_returns_head() {
        // "main.." has an empty right side — should default to "HEAD"
        let result = extract_range_right("main..", "..");
        assert_eq!(result, Some("HEAD".to_string()));
    }

    #[test]
    fn test_extract_range_right_no_separator_returns_none() {
        let result = extract_range_right("main", "..");
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_range_right_three_dot_with_content() {
        let result = extract_range_right("main...feature", "...");
        assert_eq!(result, Some("feature".to_string()));
    }

    #[test]
    fn test_extract_range_right_three_dot_empty_right_returns_head() {
        let result = extract_range_right("main...", "...");
        assert_eq!(result, Some("HEAD".to_string()));
    }

    // ========================================================================
    // Security guard unit tests (Issue source:49:testing)
    // ========================================================================

    #[test]
    fn test_git_show_rejects_dash_ref_spec() {
        // A ref_spec beginning with '-' must be rejected to prevent flag injection.
        let global_flags: Vec<String> = vec![];
        let result = git_show(&global_flags, "-malicious-flag");
        assert!(result.is_err(), "expected Err for dash-prefixed ref_spec");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("must not start with '-'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_get_file_source_rejects_null_byte() {
        let global_flags: Vec<String> = vec![];
        let args: Vec<String> = vec![];
        let result = get_file_source("foo\0bar", &global_flags, &args);
        assert!(result.is_err(), "expected Err for path with null byte");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("null byte"), "unexpected error: {msg}");
    }

    #[test]
    fn test_get_file_source_rejects_path_traversal() {
        // Construct a temp dir and attempt to read a file outside it using `../`.
        let dir = tempfile::TempDir::new().unwrap();
        // Write a sentinel file one level above so the path can be canonicalized.
        let outside = dir.path().parent().unwrap().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();

        let global_flags: Vec<String> =
            vec!["-C".into(), dir.path().to_string_lossy().into_owned()];
        let args: Vec<String> = vec![];

        let result = get_file_source("../secret.txt", &global_flags, &args);
        assert!(result.is_err(), "expected Err for path traversal attempt");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("path traversal") || msg.contains("escapes work tree"),
            "unexpected error: {msg}"
        );

        // Cleanup the file we wrote outside the temp dir.
        let _ = std::fs::remove_file(&outside);
    }
}
