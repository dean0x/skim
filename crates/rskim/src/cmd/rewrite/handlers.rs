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

/// Select the skim mode for a `cat` rewrite over `files` (ADR-007).
///
/// - all declaration files → `--mode=structure` (preserves the full signal)
/// - all regular code files → `--mode=pseudo` (strips implementation noise)
/// - mixed → `None`: no single mode preserves both, so the rewrite bails (#317)
///
/// `head`/`tail` do not consult this: a line slice is served verbatim in
/// `--mode=full`, which is byte-exact for every file class (see
/// [`try_rewrite_head_tail`]).
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

/// POSIX default line count for `head`/`tail` when no count is given.
const POSIX_DEFAULT_LINE_COUNT: u64 = 10;

/// Parse an unsigned head/tail line-count token, rejecting every signed spelling.
///
/// `u64::from_str` accepts a leading `+`, so `"+5".parse::<u64>()` yields
/// `Ok(5)` — and `tail -n +5` means "from line 5 to EOF", the inverse of the
/// last-5-lines bound skim would emit. Dropping the sign would change the
/// command's meaning, so any token carrying one produces no rewrite at all
/// (#317: compress, never truncate). Negative counts bail for the same reason.
fn parse_unsigned_count(token: &str) -> Option<u64> {
    if token.starts_with('+') || token.starts_with('-') {
        return None;
    }
    token.parse::<u64>().ok()
}

/// Parse a line count from head/tail -N or -n N or -nN style arguments.
///
/// Returns `Some((count, files))` on success, `None` if no files found, an
/// unrecognized flag is encountered, or the count carries a sign (see
/// [`parse_unsigned_count`]) — the same bail the `-f`/`-c` flags take.
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
            count = Some(parse_unsigned_count(args[i])?);
        } else if let Some(rest) = arg.strip_prefix("-n") {
            // -nN form: rest is the count
            count = Some(parse_unsigned_count(rest)?);
        } else if arg.starts_with('-') && arg != "-" {
            // Check for -N (bare number) like -20
            let potential_num = &arg[1..];
            if let Some(n) = parse_unsigned_count(potential_num) {
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
/// A `head`/`tail` request is a *line slice*, not a structural query, so the
/// slice is served verbatim: the rewrite always emits `--mode=full` plus the
/// ADR-016 bound (`--max-lines` / `--last-lines`), which caps total output
/// lines with the elision marker occupying one of the N slots.
///
/// Three properties follow, all required by #317 (compress, never truncate):
///
/// - **`--mode=full` for every file class** — regular code and declaration
///   files alike. A transformed view rewrites the very bytes the slice is
///   supposed to reproduce (`pseudo` strips a trailing `;`, `structure` drops
///   implementation lines) and puts the elision count in transformed-line
///   space rather than source-line space. Because `full` is byte-exact for
///   every class, [`mode_for_files`] and its mixed-set bail are unnecessary
///   here; `cat` keeps them (ADR-007). Measured on a 1219-line file, `pseudo`
///   saved ~2% of bytes over `full` on such a slice — far less than the
///   fidelity it cost.
/// - **The bound is always emitted** — a countless `head file.rs` is a
///   ten-line request under POSIX, so the bound defaults to
///   [`POSIX_DEFAULT_LINE_COUNT`] rather than rendering the whole file.
/// - **Signed counts never reach here** — [`parse_line_count_and_files`]
///   bails on them.
fn try_rewrite_head_tail(args: &[&str], line_flag: &str) -> Option<RewriteResult> {
    let (count, files) = parse_line_count_and_files(args)?;

    if !files.iter().all(|f| is_code_file(f)) {
        return None;
    }

    let mut tokens: Vec<String> = vec!["skim".to_string()];
    tokens.extend(files.iter().map(|f| f.to_string()));
    tokens.push("--mode=full".to_string());
    tokens.push(line_flag.to_string());
    tokens.push(count.unwrap_or(POSIX_DEFAULT_LINE_COUNT).to_string());

    Some(RewriteResult {
        tokens,
        category: RewriteCategory::Read,
    })
}

/// Rewrite `head` command.
///
/// Rules:
/// - `head -20 file.ts` → `skim file.ts --mode=full --max-lines 20`
/// - `head -n 20 file.ts` → `skim file.ts --mode=full --max-lines 20`
/// - `head -n20 file.ts` → `skim file.ts --mode=full --max-lines 20`
/// - `head types.d.ts` → `skim types.d.ts --mode=full --max-lines 10`
/// - `head file.ts` → `skim file.ts --mode=full --max-lines 10` (POSIX default)
/// - `head -n +5 file.ts` → None (signed count)
/// - `head -20 data.csv` → None (not code file)
pub(super) fn try_rewrite_head(args: &[&str]) -> Option<RewriteResult> {
    try_rewrite_head_tail(args, "--max-lines")
}

/// Rewrite `tail` command.
///
/// Rules:
/// - `tail -20 file.rs` → `skim file.rs --mode=full --last-lines 20`
/// - `tail -n 20 file.rs` → `skim file.rs --mode=full --last-lines 20`
/// - `tail stubs/api.pyi` → `skim stubs/api.pyi --mode=full --last-lines 10`
/// - `tail file.rs` → `skim file.rs --mode=full --last-lines 10` (POSIX default)
/// - `tail -n +5 file.rs` → None (POSIX "from line 5 to EOF" — inverse meaning)
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

    /// E4.1: bare-number line count (`-20`) must produce a rewrite for markdown
    /// files — `CHANGELOG.md` is a common file pattern, and the slice is served
    /// verbatim like every other head/tail slice.
    #[test]
    fn test_head_bare_number_changelog_produces_rewrite() {
        let result = try_rewrite_head(&["-20", "CHANGELOG.md"])
            .expect("head -20 CHANGELOG.md must produce a rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--max-lines"),
            "rewrite must include --max-lines, got: {joined}"
        );
        assert!(
            joined.contains("20"),
            "rewrite must preserve the line count, got: {joined}"
        );
        assert!(
            joined.contains("--mode=full"),
            "markdown slice is served verbatim, got: {joined}"
        );
    }

    /// #322: the `is_code_file` gate runs BEFORE `is_declaration_file`, so the
    /// `.d.mts`/`.d.cts` terminal extensions must be known to
    /// `Language::from_extension` or the whole declaration path is unreachable
    /// for half the extensions workstream 5d promises.
    #[test]
    fn test_is_code_file_mts_cts() {
        assert!(is_code_file("api.mts"));
        assert!(is_code_file("api.cts"));
        assert!(is_code_file("types.d.mts"));
        assert!(is_code_file("dist/index.d.cts"));
    }

    #[test]
    fn test_cat_declaration_mts_cts_uses_structure_mode() {
        for path in ["src/api.d.mts", "dist/index.d.cts"] {
            let result = try_rewrite_cat(&[path]).unwrap_or_else(|| panic!("must rewrite {path}"));
            let joined = result.tokens.join(" ");
            assert!(
                joined.contains("--mode=structure"),
                "declaration file {path} needs structure mode: {joined}"
            );
            assert!(!joined.contains("pseudo"), "{joined}");
        }
    }

    #[test]
    fn test_head_tail_declaration_mts_cts_uses_full_mode() {
        let head = try_rewrite_head(&["-20", "src/api.d.mts"]).expect("must rewrite");
        assert!(head.tokens.join(" ").contains("--mode=full"));
        let tail = try_rewrite_tail(&["-20", "dist/index.d.cts"]).expect("must rewrite");
        assert!(tail.tokens.join(" ").contains("--mode=full"));
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

    // ========================================================================
    // (A) head/tail always rewrite to --mode=full (D5 parity fix)
    // ========================================================================

    /// RED at 167e73f: `head -20 f.rs` emits
    /// `SKIM_REWRITTEN_FROM=head skim f.rs --mode=pseudo --max-lines 20`.
    /// Fixed: head always uses `--mode=full`; pseudo strips trailing `;` from
    /// statements, corrupting verbatim slices the user asked for.
    #[test]
    fn test_head_short_flag_uses_full_mode() {
        let result = try_rewrite_head(&["-20", "f.rs"]).expect("must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "head uses full mode: {joined}"
        );
        assert!(
            joined.contains("--max-lines"),
            "must include --max-lines: {joined}"
        );
        assert!(joined.contains("20"), "must preserve count: {joined}");
        assert!(
            !joined.contains("--mode=pseudo"),
            "must NOT use pseudo: {joined}"
        );
    }

    /// RED at 167e73f: `head -n 20 f.rs` emits
    /// `SKIM_REWRITTEN_FROM=head skim f.rs --mode=pseudo --max-lines 20`.
    #[test]
    fn test_head_n_space_flag_uses_full_mode() {
        let result = try_rewrite_head(&["-n", "20", "f.rs"]).expect("must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "head -n uses full mode: {joined}"
        );
        assert!(
            joined.contains("--max-lines"),
            "must include --max-lines: {joined}"
        );
        assert!(
            !joined.contains("--mode=pseudo"),
            "must NOT use pseudo: {joined}"
        );
    }

    /// RED at 167e73f: `tail -5 f.rs` emits
    /// `SKIM_REWRITTEN_FROM=tail skim f.rs --mode=pseudo --last-lines 5`.
    #[test]
    fn test_tail_short_flag_uses_full_mode() {
        let result = try_rewrite_tail(&["-5", "f.rs"]).expect("must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "tail uses full mode: {joined}"
        );
        assert!(
            joined.contains("--last-lines"),
            "must include --last-lines: {joined}"
        );
        assert!(joined.contains("5"), "must preserve count: {joined}");
        assert!(
            !joined.contains("--mode=pseudo"),
            "must NOT use pseudo: {joined}"
        );
    }

    /// RED at 167e73f: `tail -n 5 f.rs` emits
    /// `SKIM_REWRITTEN_FROM=tail skim f.rs --mode=pseudo --last-lines 5`.
    #[test]
    fn test_tail_n_space_flag_uses_full_mode() {
        let result = try_rewrite_tail(&["-n", "5", "f.rs"]).expect("must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "tail -n uses full mode: {joined}"
        );
        assert!(
            joined.contains("--last-lines"),
            "must include --last-lines: {joined}"
        );
        assert!(
            !joined.contains("--mode=pseudo"),
            "must NOT use pseudo: {joined}"
        );
    }

    /// RED at 167e73f: `head -20 types.d.ts` emits
    /// `SKIM_REWRITTEN_FROM=head skim types.d.ts --mode=structure --max-lines 20`.
    /// Fixed: declaration files also use `--mode=full` for head/tail; structure
    /// drops implementation-bearing lines, breaking verbatim slice semantics.
    #[test]
    fn test_head_declaration_file_uses_full_mode() {
        let result = try_rewrite_head(&["-20", "types.d.ts"]).expect("must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "head on declaration file uses full mode: {joined}"
        );
        assert!(
            !joined.contains("--mode=structure"),
            "must NOT use structure: {joined}"
        );
    }

    /// RED at 167e73f: `tail -20 api.pyi` emits
    /// `SKIM_REWRITTEN_FROM=tail skim api.pyi --mode=structure --last-lines 20`.
    #[test]
    fn test_tail_declaration_pyi_uses_full_mode() {
        let result = try_rewrite_tail(&["-20", "api.pyi"]).expect("must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "tail on .pyi uses full mode: {joined}"
        );
        assert!(
            !joined.contains("--mode=structure"),
            "must NOT use structure: {joined}"
        );
    }

    // ========================================================================
    // (B) Bare head/tail apply POSIX default bound of 10
    // ========================================================================

    /// RED at 167e73f: bare `head f.rs` emits
    /// `SKIM_REWRITTEN_FROM=head skim f.rs --mode=pseudo` — no line bound.
    /// Rendering the entire file for a 10-line POSIX default request (ADR-016).
    /// Fixed: bare head rewrites to `--mode=full --max-lines 10`.
    #[test]
    fn test_head_bare_no_count_applies_posix_default_bound() {
        let result = try_rewrite_head(&["f.rs"]).expect("bare head must produce a rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "bare head uses full mode: {joined}"
        );
        assert!(
            joined.contains("--max-lines"),
            "bare head must include --max-lines bound: {joined}"
        );
        assert!(
            joined.contains("10"),
            "bare head must default to 10 lines: {joined}"
        );
    }

    /// RED at 167e73f: bare `tail f.rs` emits
    /// `SKIM_REWRITTEN_FROM=tail skim f.rs --mode=pseudo` — no line bound.
    /// Fixed: bare tail rewrites to `--mode=full --last-lines 10`.
    #[test]
    fn test_tail_bare_no_count_applies_posix_default_bound() {
        let result = try_rewrite_tail(&["f.rs"]).expect("bare tail must produce a rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=full"),
            "bare tail uses full mode: {joined}"
        );
        assert!(
            joined.contains("--last-lines"),
            "bare tail must include --last-lines bound: {joined}"
        );
        assert!(
            joined.contains("10"),
            "bare tail must default to 10 lines: {joined}"
        );
    }

    // ========================================================================
    // (C) Signed count → bail (mirrors tail -f / head -c bail pattern)
    // ========================================================================

    /// RED at 167e73f: `tail -n +5 f.rs` (POSIX "from line 5 to EOF") emits
    /// `SKIM_REWRITTEN_FROM=tail skim f.rs --mode=pseudo --last-lines 5`.
    /// Root cause: `"+5".parse::<u64>()` returns `Ok(5)` in Rust; the semantics
    /// are inverted — the user asked for all lines FROM line 5, not the LAST 5.
    /// Fixed: a leading `+` in the count makes `parse_line_count_and_files` return `None`.
    #[test]
    fn test_tail_signed_plus_n_space_bails() {
        assert!(
            parse_line_count_and_files(&["-n", "+5", "f.rs"]).is_none(),
            "tail -n +5: signed from-line count must bail (currently parses as 5)"
        );
    }

    /// RED at 167e73f: `tail -n+5 f.rs` (fused form, no space) emits
    /// `SKIM_REWRITTEN_FROM=tail skim f.rs --mode=pseudo --last-lines 5`.
    /// Same root cause: `"+5".parse::<u64>()` succeeds.
    #[test]
    fn test_tail_signed_plus_fused_bails() {
        assert!(
            parse_line_count_and_files(&["-n+5", "f.rs"]).is_none(),
            "tail -n+5 fused form: signed count must bail"
        );
    }

    /// GREEN at 167e73f: `tail --lines=+5 f.rs` already exits 1.
    /// It currently bails because `--lines=+5` is not recognised as a flag
    /// (the `potential_num` parse of `-lines=+5` fails).  The contract is the
    /// same regardless of reason: signed counts must produce no rewrite.
    #[test]
    fn test_tail_long_option_signed_plus_bails() {
        assert!(
            parse_line_count_and_files(&["--lines=+5", "f.rs"]).is_none(),
            "--lines=+5 must bail"
        );
    }

    /// GREEN at 167e73f: `head -n -5 f.rs` already exits 1.
    /// `"-5".parse::<u64>()` returns `Err` so `parse_line_count_and_files` returns `None`.
    /// Pins the bail contract for negative counts.
    #[test]
    fn test_head_signed_negative_count_bails() {
        assert!(
            parse_line_count_and_files(&["-n", "-5", "f.rs"]).is_none(),
            "head -n -5: negative count must bail"
        );
    }

    /// Completes the signed-count matrix: the fused negative form and the fused
    /// long option. A sign in any spelling must produce no rewrite.
    #[test]
    fn test_remaining_signed_count_spellings_bail() {
        for args in [["-n-5", "f.rs"], ["--lines=-5", "f.rs"], ["-+5", "f.rs"]] {
            assert!(
                parse_line_count_and_files(&args).is_none(),
                "signed count must bail: {args:?}"
            );
        }
    }

    // ========================================================================
    // Controls — GREEN today, must remain GREEN after the fix
    // ========================================================================

    /// GREEN at 167e73f: `cat f.rs` → `--mode=pseudo`.
    /// The fix changes head/tail only; cat is unchanged (ADR-007).
    #[test]
    fn test_cat_regular_code_control_still_pseudo() {
        let result = try_rewrite_cat(&["f.rs"]).expect("cat must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=pseudo"),
            "cat on regular code must stay pseudo after fix: {joined}"
        );
    }

    /// GREEN at 167e73f: `cat types.d.ts` → `--mode=structure`.
    /// The fix changes head/tail only; cat declaration handling is unchanged (ADR-007).
    #[test]
    fn test_cat_declaration_control_still_structure() {
        let result = try_rewrite_cat(&["types.d.ts"]).expect("cat must rewrite");
        let joined = result.tokens.join(" ");
        assert!(
            joined.contains("--mode=structure"),
            "cat on declaration file must stay structure after fix: {joined}"
        );
    }
}
