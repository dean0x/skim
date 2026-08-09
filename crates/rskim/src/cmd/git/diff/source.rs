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

        if (flag == "-C" || flag == "--work-tree")
            && let Some(val) = global_flags.get(i + 1)
        {
            return Some(PathBuf::from(val));
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
    let runner = CommandRunner::new();
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
    let output = runner.run("git", &arg_refs)?;
    if output.exit_code != Some(0) {
        anyhow::bail!("git show {ref_spec} failed: {}", output.stderr.trim());
    }
    Ok(output.stdout)
}

/// Flags that consume the NEXT token as a space-separated value.
///
/// When scanning for the positional ref in `git show`/`git diff` args, these
/// flags must be skipped along with their value argument — otherwise
/// `git show -U 5 HEAD` would return `"5"` instead of `"HEAD"`.
///
/// This is the exact class described by PF-008: a short-flag matcher that
/// fails to account for the following value token.
///
/// The `--flag=value` form is handled by the `starts_with('-')` guard in the
/// caller — `--unified=5` starts with `-`, so it is already skipped.  Only
/// the space-separated form needs special treatment here.
const VALUE_FLAGS: &[&str] = &[
    "-U",
    "--unified",               // context lines count
    "-L",                      // line range (e.g. -L 1,10:file); `:` form already filtered
    "-S",                      // pickaxe string
    "-G",                      // pickaxe regex
    "--output",                // write output to a file
    "--diff-filter",           // diff filter string
    "--diff-merges",           // merge diff format
    "--format",                // output format (also --pretty)
    "--pretty",                // alias for --format
    "--ignore-matching-lines", // ignore lines matching regex
];

/// Extract the first positional ref from `git show` args.
///
/// Scans args for the first non-flag, non-refspec argument that appears before
/// the POSIX `--` pathspec separator. Returns `"HEAD"` when no explicit ref is
/// found (bare `git show` with no ref argument).
///
/// A ref is distinguished from a flag by not starting with `-` and from a
/// refspec by not containing `:`. Range-form args (e.g. `A..B`) are included —
/// `get_file_source` resolves ranges via `extract_range_right` before calling
/// this function, so the is_show path is only reached for single-ref forms.
///
/// Flags listed in [`VALUE_FLAGS`] consume the next token as their value and
/// both tokens are skipped — preventing a numeric value like `5` in
/// `git show -U 5 HEAD` from being mistaken for a ref.
fn extract_show_ref(args: &[String]) -> String {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            break;
        }
        // Skip flags that take a space-separated value: consume both the flag
        // and the following token (PF-008: short-flag cluster / value confusion).
        if VALUE_FLAGS.contains(&arg.as_str()) {
            i += 2; // skip flag AND its value
            continue;
        }
        if !arg.starts_with('-') && !arg.contains(':') {
            return arg.clone();
        }
        i += 1;
    }
    "HEAD".to_string()
}

/// Extract positional (non-flag, non-range, non-refspec) arguments before `--`.
///
/// Used by `get_file_source` to detect the two-ref `git diff A B` form.
/// Excludes args starting with `-`, containing `..` (range), or containing `:`
/// (refspec like `HEAD:path`).
fn positional_refs(args: &[String]) -> Vec<&str> {
    let mut result = Vec::new();
    for arg in args {
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') && !arg.contains("..") && !arg.contains(':') {
            result.push(arg.as_str());
        }
    }
    result
}

/// Resolve the file source content for AST parsing.
///
/// Resolution order:
/// 1. `--cached` / `--staged`: use `git show :path`
/// 2. Commit range (`A..B` or `A...B`): use `git show B:path`
/// 3. `git show <ref>` (`is_show: true`): use `git show <ref>:path`
/// 4. Two-ref diff (`git diff A B`): use `git show B:path`, falling back to the
///    working tree when `B` does not resolve as a revision (it was a `--`-less
///    pathspec, as in `git diff HEAD src/foo.ts`)
/// 5. Unstaged working tree (`git diff <single-ref>` or bare `git diff`):
///    read from disk (respecting `-C` / `--work-tree`)
///
/// The `is_show` flag must be passed explicitly by the caller — this function
/// never re-sniffs argv to infer the invocation context, which would recreate
/// the exact class of bug this design addresses (#467).
pub(super) fn get_file_source(
    path: &str,
    global_flags: &[String],
    args: &[String],
    is_show: bool,
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

    // For `git show <ref>`, read the blob from git at the given ref.
    // `git diff <single-ref>` legitimately diffs against the working tree —
    // reading from disk is correct there; only `git show` needs this path.
    // The caller passes `is_show` explicitly; we never re-sniff argv.
    if is_show {
        let ref_spec = extract_show_ref(args);
        return git_show(global_flags, &format!("{ref_spec}:{path}"));
    }

    // Space-separated two-ref diff: `git diff A B` — B is the new-side ref.
    // A single positional ref (`git diff A`) legitimately diffs against the
    // working tree, so reading from disk is correct there.
    //
    // `git diff A B` and `git diff <ref> <pathspec>` (the `--`-less form, e.g.
    // `git diff HEAD src/foo.ts`) are syntactically identical here — git itself
    // disambiguates by trying to resolve each argument as a revision.  We do the
    // same, cheaply: if the blob fetch fails, `refs[1]` was a pathspec and not a
    // ref, so fall through to the working-tree read, which is the correct source
    // for that form.  A wrong source cannot produce corrupt breadcrumbs either
    // way — `source_matches_diff` in `render.rs` rejects a mismatched revision
    // and routes to raw hunks.
    let refs = positional_refs(args);
    if refs.len() >= 2 {
        let new_ref = refs[1];
        match git_show(global_flags, &format!("{new_ref}:{path}")) {
            Ok(source) => return Ok(source),
            Err(e) => {
                crate::debug_log!(
                    "[skim] git diff AST: {new_ref:?} is not a revision ({e}); \
                     treating it as a pathspec and reading {path} from the working tree"
                );
            }
        }
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

        let result = get_file_source("test.txt", &global_flags, &args, false);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap(), "hello world");
    }

    /// `git diff <ref> <path>` without `--` must still resolve to the working
    /// tree.
    ///
    /// `positional_refs` cannot tell `git diff A B` (two revisions) from
    /// `git diff HEAD src/foo.ts` (revision + `--`-less pathspec) — both are two
    /// bare positionals.  Treating the pathspec as a ref makes `git show
    /// test.txt:test.txt` fail; without the working-tree fallback the whole AST
    /// render is abandoned and a stderr line is printed for a perfectly ordinary
    /// command.
    #[test]
    fn test_get_file_source_ref_plus_pathspec_falls_back_to_working_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let global_flags: Vec<String> =
            vec!["-C".into(), dir.path().to_string_lossy().into_owned()];
        // Mirrors `git diff HEAD test.txt` — two positionals, second is a path.
        let args: Vec<String> = vec!["HEAD".into(), "test.txt".into()];

        let result = get_file_source("test.txt", &global_flags, &args, false);
        assert!(
            result.is_ok(),
            "a `--`-less pathspec must fall back to the working tree, got: {result:?}"
        );
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

    // ========================================================================
    // extract_show_ref unit tests (PF-008: space-separated flag value)
    // ========================================================================

    #[test]
    fn test_extract_show_ref_bare_ref() {
        // `git show abc123` — no flags, just a ref
        let args: Vec<String> = vec!["abc123".into()];
        assert_eq!(extract_show_ref(&args), "abc123");
    }

    #[test]
    fn test_extract_show_ref_no_args_returns_head() {
        // `git show` with no args — bare invocation defaults to HEAD
        let args: Vec<String> = vec![];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_flags_only_returns_head() {
        // `git show --stat` — a flag but no ref
        let args: Vec<String> = vec!["--stat".into()];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_skips_unified_short_and_value() {
        // `git show -U 5 HEAD` — the `5` is -U's value, not a ref (PF-008)
        let args: Vec<String> = vec!["-U".into(), "5".into(), "HEAD".into()];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_skips_unified_long_and_value() {
        // `git show --unified 5 HEAD`
        let args: Vec<String> = vec!["--unified".into(), "5".into(), "HEAD".into()];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_unified_equals_form_already_works() {
        // `git show --unified=5 HEAD` — starts with `-`, already skipped by guard
        let args: Vec<String> = vec!["--unified=5".into(), "HEAD".into()];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_skips_output_flag_and_value() {
        // `git show --output /tmp/out.patch HEAD`
        let args: Vec<String> = vec!["--output".into(), "/tmp/out.patch".into(), "HEAD".into()];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_skips_pickaxe_s_and_value() {
        // `git show -S "needle" abc123`
        let args: Vec<String> = vec!["-S".into(), "needle".into(), "abc123".into()];
        assert_eq!(extract_show_ref(&args), "abc123");
    }

    #[test]
    fn test_extract_show_ref_skips_pickaxe_g_and_value() {
        // `git show -G "regex" abc123`
        let args: Vec<String> = vec!["-G".into(), "regex".into(), "abc123".into()];
        assert_eq!(extract_show_ref(&args), "abc123");
    }

    #[test]
    fn test_extract_show_ref_stops_at_double_dash() {
        // `git show -- path/to/file` — everything after `--` is a pathspec
        let args: Vec<String> = vec!["--".into(), "path/to/file".into()];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_refspec_with_colon_skipped() {
        // `git show HEAD:src/main.rs` — contains `:`, treated as refspec not ref
        let args: Vec<String> = vec!["HEAD:src/main.rs".into()];
        assert_eq!(extract_show_ref(&args), "HEAD");
    }

    #[test]
    fn test_extract_show_ref_multiple_value_flags() {
        // `git show -U 3 --format oneline abc123`
        let args: Vec<String> = vec![
            "-U".into(),
            "3".into(),
            "--format".into(),
            "oneline".into(),
            "abc123".into(),
        ];
        assert_eq!(extract_show_ref(&args), "abc123");
    }

    #[test]
    fn test_get_file_source_rejects_null_byte() {
        let global_flags: Vec<String> = vec![];
        let args: Vec<String> = vec![];
        let result = get_file_source("foo\0bar", &global_flags, &args, false);
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

        let result = get_file_source("../secret.txt", &global_flags, &args, false);
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
