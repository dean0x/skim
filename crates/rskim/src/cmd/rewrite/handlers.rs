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

/// Check if a file path is a declaration file — a file that is ALL signal
/// and no implementation (#317).
///
/// `--mode=pseudo` strips a `.d.ts` file to nothing (the whole file is type
/// declarations) and `signatures` loses `.pyi` constants; `structure`
/// preserves both byte-for-byte (verified empirically). Uses full-filename
/// `ends_with` because `Path::extension()` only sees the final `.ts` of
/// `.d.ts`.
pub(super) fn is_declaration_file(path: &str) -> bool {
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
        || name.ends_with(".pyi")
}

/// Select the skim mode for a `cat`/`head`/`tail` rewrite over `files`.
///
/// - all declaration files → `--mode=structure` (preserves the full signal)
/// - all regular code files → `--mode=pseudo` (strips implementation noise)
/// - mixed → `None`: no single mode preserves both, so the rewrite bails (#317)
fn mode_for_files(files: &[&str]) -> Option<&'static str> {
    let declaration_count = files.iter().filter(|f| is_declaration_file(f)).count();
    if declaration_count == 0 {
        Some("--mode=pseudo")
    } else if declaration_count == files.len() {
        Some("--mode=structure")
    } else {
        None
    }
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

    let mode = mode_for_files(&files)?;

    let mut tokens: Vec<String> = vec!["skim".to_string()];
    tokens.extend(files.iter().map(|f| f.to_string()));
    tokens.push(mode.to_string());
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

    let mode = mode_for_files(&files)?;

    let mut tokens: Vec<String> = vec!["skim".to_string()];
    tokens.extend(files.iter().map(|f| f.to_string()));
    tokens.push(mode.to_string());
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
    // Declaration-file-aware mode (#317 — .d.ts gutting fix)
    // ========================================================================

    #[test]
    fn test_is_declaration_file() {
        assert!(is_declaration_file("types.d.ts"));
        assert!(is_declaration_file("src/lib/api.d.mts"));
        assert!(is_declaration_file("dist/index.d.cts"));
        assert!(is_declaration_file("stubs/requests.pyi"));
        assert!(!is_declaration_file("main.ts"));
        assert!(!is_declaration_file("module.py"));
        // Path::extension() would only see "ts" here — full-name check required.
        assert!(!is_declaration_file("d.ts.rs"));
    }

    #[test]
    fn test_cat_declaration_file_uses_structure_mode() {
        // --mode=pseudo strips a .d.ts to nothing; structure preserves it.
        let result = try_rewrite_cat(&["types.d.ts"]).expect("must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=structure"),
            "declaration files need structure mode: {joined}"
        );
        assert!(!joined.contains("pseudo"), "{joined}");
    }

    #[test]
    fn test_cat_pyi_uses_structure_mode() {
        let result = try_rewrite_cat(&["stubs/api.pyi"]).expect("must rewrite");
        assert!(result.tokens.join(" ").contains("--mode=structure"));
    }

    #[test]
    fn test_cat_regular_file_keeps_pseudo_mode() {
        let result = try_rewrite_cat(&["main.ts"]).expect("must rewrite");
        assert!(result.tokens.join(" ").contains("--mode=pseudo"));
    }

    #[test]
    fn test_cat_mixed_declaration_and_regular_bails() {
        // No single mode preserves both — the rewrite must bail (#317).
        assert!(try_rewrite_cat(&["types.d.ts", "main.ts"]).is_none());
    }

    #[test]
    fn test_head_tail_declaration_file_uses_structure_mode() {
        let head = try_rewrite_head(&["-20", "types.d.ts"]).expect("must rewrite");
        assert!(head.tokens.join(" ").contains("--mode=structure"));
        let tail = try_rewrite_tail(&["-20", "stubs/api.pyi"]).expect("must rewrite");
        assert!(tail.tokens.join(" ").contains("--mode=structure"));
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
