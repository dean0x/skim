//! Custom handlers for cat, head, and tail commands.
//!
//! These handlers are called when the declarative rule table doesn't match,
//! because cat/head/tail require argument inspection (file extension checks).

use super::types::{RewriteCategory, RewriteResult};

/// Check if a file path has a known code extension.
///
/// Extracts the extension from the path and checks against `Language::from_extension`.
/// Does NOT check if the file exists on disk — this is pure string analysis.
pub(super) fn is_code_file(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(rskim_core::Language::from_extension)
        .is_some()
}

/// Rewrite `cat` command.
///
/// Rules:
/// - `cat file.ts` → `skim file.ts --mode=pseudo`
/// - `cat -s file.ts` → `skim file.ts --mode=pseudo` (-s squeeze blanks: pseudo is better)
/// - `cat -n file.ts` → None (line numbers)
/// - `cat -b/-v/-e/-t/-A` → None (display flags)
/// - `cat file1.ts file2.py` → `skim file1.ts file2.py --mode=pseudo --no-header`
/// - `cat` (no file arg) → None
/// - `cat non-code.txt` → None
pub(super) fn try_rewrite_cat(args: &[&str]) -> Option<RewriteResult> {
    if args.is_empty() {
        return None;
    }

    let mut files: Vec<&str> = Vec::new();
    let mut has_unsupported_flag = false;

    for arg in args {
        if arg.starts_with('-') && *arg != "-" {
            // Allow -s (squeeze blank lines), reject everything else
            if *arg == "-s" {
                continue;
            }
            has_unsupported_flag = true;
            break;
        }
        files.push(arg);
    }

    if has_unsupported_flag || files.is_empty() {
        return None;
    }

    // All files must be code files
    if !files.iter().all(|f| is_code_file(f)) {
        return None;
    }

    let mut tokens: Vec<String> = vec!["skim".to_string()];
    tokens.extend(files.iter().map(|f| f.to_string()));
    tokens.push("--mode=pseudo".to_string());
    if files.len() > 1 {
        tokens.push("--no-header".to_string());
    }

    Some(RewriteResult {
        tokens,
        category: RewriteCategory::Read,
    })
}

/// Parse a line count from head/tail -N or -n N or -nN style arguments.
///
/// Returns `Some((count, files))` on success, `None` if no files found or
/// an unrecognized flag is encountered.
pub(super) fn parse_line_count_and_files<'a>(
    args: &[&'a str],
) -> Option<(Option<u64>, Vec<&'a str>)> {
    if args.is_empty() {
        return None;
    }

    let mut count: Option<u64> = None;
    let mut files: Vec<&'a str> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = args[i];

        if arg == "-n" {
            // -n N form: next arg is the count
            i += 1;
            if i >= args.len() {
                return None;
            }
            count = Some(args[i].parse::<u64>().ok()?);
        } else if let Some(rest) = arg.strip_prefix("-n") {
            // -nN form: rest is the count
            count = Some(rest.parse::<u64>().ok()?);
        } else if arg.starts_with('-') && arg != "-" {
            // Check for -N (bare number) like -20
            let potential_num = &arg[1..];
            if let Ok(n) = potential_num.parse::<u64>() {
                count = Some(n);
            } else {
                // Unknown flag
                return None;
            }
        } else {
            files.push(arg);
        }

        i += 1;
    }

    if files.is_empty() {
        return None;
    }

    Some((count, files))
}

/// Shared rewrite logic for head/tail commands.
///
/// Parses line count and file arguments, validates all files are code files,
/// and builds the skim command with the appropriate line-limit flag.
fn try_rewrite_head_tail(args: &[&str], line_flag: &str) -> Option<RewriteResult> {
    let (count, files) = parse_line_count_and_files(args)?;

    if !files.iter().all(|f| is_code_file(f)) {
        return None;
    }

    let mut tokens: Vec<String> = vec!["skim".to_string()];
    tokens.extend(files.iter().map(|f| f.to_string()));
    tokens.push("--mode=pseudo".to_string());
    if let Some(n) = count {
        tokens.push(line_flag.to_string());
        tokens.push(n.to_string());
    }

    Some(RewriteResult {
        tokens,
        category: RewriteCategory::Read,
    })
}

/// Rewrite `head` command.
///
/// Rules:
/// - `head -20 file.ts` → `skim file.ts --mode=pseudo --max-lines 20`
/// - `head -n 20 file.ts` → `skim file.ts --mode=pseudo --max-lines 20`
/// - `head -n20 file.ts` → `skim file.ts --mode=pseudo --max-lines 20`
/// - `head file.ts` → `skim file.ts --mode=pseudo`
/// - `head -20 data.csv` → None (not code file)
pub(super) fn try_rewrite_head(args: &[&str]) -> Option<RewriteResult> {
    try_rewrite_head_tail(args, "--max-lines")
}

/// Rewrite `tail` command.
///
/// Rules:
/// - `tail -20 file.rs` → `skim file.rs --mode=pseudo --last-lines 20`
/// - `tail -n 20 file.rs` → `skim file.rs --mode=pseudo --last-lines 20`
/// - `tail file.rs` → `skim file.rs --mode=pseudo`
/// - `tail -20 data.csv` → None (not code file)
pub(super) fn try_rewrite_tail(args: &[&str]) -> Option<RewriteResult> {
    try_rewrite_head_tail(args, "--last-lines")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // is_code_file
    // ========================================================================

    #[test]
    fn test_is_code_file_rs() {
        assert!(is_code_file("file.rs"));
    }

    #[test]
    fn test_is_code_file_ts() {
        assert!(is_code_file("src/main.ts"));
    }

    #[test]
    fn test_is_code_file_txt() {
        assert!(!is_code_file("file.txt"));
    }

    #[test]
    fn test_is_code_file_no_extension() {
        assert!(!is_code_file("Makefile"));
    }

    // ========================================================================
    // parse_line_count_and_files
    // ========================================================================

    #[test]
    fn test_parse_line_count_dash_n_space() {
        let result = parse_line_count_and_files(&["-n", "20", "file.ts"]);
        assert_eq!(result, Some((Some(20), vec!["file.ts"])));
    }

    #[test]
    fn test_parse_line_count_dash_n_no_space() {
        let result = parse_line_count_and_files(&["-n20", "file.ts"]);
        assert_eq!(result, Some((Some(20), vec!["file.ts"])));
    }

    #[test]
    fn test_parse_line_count_bare_number() {
        let result = parse_line_count_and_files(&["-20", "file.ts"]);
        assert_eq!(result, Some((Some(20), vec!["file.ts"])));
    }

    #[test]
    fn test_parse_line_count_no_count() {
        let result = parse_line_count_and_files(&["file.ts"]);
        assert_eq!(result, Some((None, vec!["file.ts"])));
    }

    #[test]
    fn test_parse_line_count_no_files() {
        let result = parse_line_count_and_files(&["-n", "20"]);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_line_count_empty() {
        let result = parse_line_count_and_files(&[]);
        assert!(result.is_none());
    }
}
