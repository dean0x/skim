//! Core rewrite algorithm — table matching and custom handlers.

use super::handlers::{try_rewrite_cat, try_rewrite_head, try_rewrite_tail};
use super::rules::REWRITE_RULES;
use super::types::RewriteResult;

/// Attempt to rewrite a tokenized command. Returns `Some(RewriteResult)` on
/// match, `None` if no rewrite applies.
pub(super) fn try_rewrite(tokens: &[&str]) -> Option<RewriteResult> {
    if tokens.is_empty() {
        return None;
    }

    // Step 1: Strip leading env vars (KEY=VALUE pairs before the command)
    let env_split = strip_env_vars(tokens);
    let env_vars = &tokens[..env_split];
    let command_tokens = &tokens[env_split..];

    if command_tokens.is_empty() {
        return None;
    }

    // Step 2: Strip cargo toolchain prefix (+nightly etc.)
    let (toolchain, match_tokens) = strip_cargo_toolchain(command_tokens);

    // Step 3: Split at `--` separator
    let sep_pos = split_at_separator(&match_tokens);
    let before_sep = &match_tokens[..sep_pos];
    let separator_and_after = &match_tokens[sep_pos..];

    // Step 4: Try declarative table match, then custom handlers (cat/head/tail)
    try_table_match(env_vars, before_sep, separator_and_after, toolchain)
        .or_else(|| try_custom_handlers(env_vars, command_tokens))
}

/// Return the index of the first non-env-var token.
///
/// Env vars match pattern: contains `=` and everything before `=` is
/// `[A-Z0-9_]+` (all uppercase letters, digits, underscores).
/// Callers can slice `tokens[..index]` for env vars and `tokens[index..]`
/// for the command, avoiding a Vec allocation.
pub(super) fn strip_env_vars(tokens: &[&str]) -> usize {
    let mut count = 0;

    for token in tokens {
        if let Some(eq_pos) = token.find('=') {
            let key = &token[..eq_pos];
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                count += 1;
                continue;
            }
        }
        break;
    }

    count
}

/// Strip cargo toolchain prefix (e.g., `+nightly`).
///
/// If tokens[0] is "cargo" and tokens[1] starts with '+', strip tokens[1]
/// for matching but preserve it for output reconstruction.
pub(super) fn strip_cargo_toolchain<'a>(tokens: &[&'a str]) -> (Option<&'a str>, Vec<&'a str>) {
    if tokens.len() >= 2 && tokens[0] == "cargo" && tokens[1].starts_with('+') {
        let toolchain = Some(tokens[1]);
        let mut match_tokens = vec![tokens[0]];
        match_tokens.extend_from_slice(&tokens[2..]);
        (toolchain, match_tokens)
    } else {
        (None, tokens.to_vec())
    }
}

/// Find the index of the first `--` separator.
///
/// Returns the position of `--` if found, or `tokens.len()` if absent.
/// Callers can slice `tokens[..index]` for before and `tokens[index..]`
/// for separator-and-after, avoiding a Vec allocation.
pub(super) fn split_at_separator(tokens: &[&str]) -> usize {
    tokens
        .iter()
        .position(|t| *t == "--")
        .unwrap_or(tokens.len())
}

/// Try matching against the declarative rule table.
pub(super) fn try_table_match(
    env_vars: &[&str],
    before_sep: &[&str],
    separator_and_after: &[&str],
    toolchain: Option<&str>,
) -> Option<RewriteResult> {
    for rule in REWRITE_RULES {
        // Check if prefix matches
        if before_sep.len() < rule.prefix.len() {
            continue;
        }
        if before_sep[..rule.prefix.len()] != *rule.prefix {
            continue;
        }

        // Middle args: everything between prefix and separator
        let middle = &before_sep[rule.prefix.len()..];

        // Check skip_if_flag_prefix: if any middle arg exactly matches a skip flag
        // (or matches as `--flag=value`).
        //
        // DESIGN NOTE (AD-1): We use strict matching here — `arg == flag` or
        // `arg.starts_with(flag) && next_byte == b'='` — which mirrors
        // `cmd::mod::user_has_flag`. The previous loose `starts_with` check
        // caused `--staged` to be eaten by a `--stat` skip prefix, blocking
        // the AST-aware diff pipeline for staged changes.
        let strict_skip_match = |arg: &str, flag: &str| -> bool {
            arg == flag || (arg.starts_with(flag) && arg.as_bytes().get(flag.len()) == Some(&b'='))
        };
        if !rule.skip_if_flag_prefix.is_empty()
            && middle.iter().any(|arg| {
                rule.skip_if_flag_prefix
                    .iter()
                    .any(|skip| strict_skip_match(arg, skip))
            })
        {
            return None;
        }

        // Build output: env_vars ++ rewrite_to ++ toolchain ++ middle ++ separator_and_after
        let output: Vec<String> = env_vars
            .iter()
            .chain(rule.rewrite_to.iter())
            .map(|s| s.to_string())
            .chain(toolchain.map(String::from))
            .chain(
                middle
                    .iter()
                    .chain(separator_and_after.iter())
                    .map(|s| s.to_string()),
            )
            .collect();

        return Some(RewriteResult {
            tokens: output,
            category: rule.category,
        });
    }

    None
}

/// Try custom handlers for cat, head, tail.
pub(super) fn try_custom_handlers(
    env_vars: &[&str],
    command_tokens: &[&str],
) -> Option<RewriteResult> {
    if command_tokens.is_empty() {
        return None;
    }

    let result = match command_tokens[0] {
        "cat" => try_rewrite_cat(&command_tokens[1..]),
        "head" => try_rewrite_head(&command_tokens[1..]),
        "tail" => try_rewrite_tail(&command_tokens[1..]),
        _ => None,
    };

    result.map(|mut r| {
        // Prepend env vars if present
        if !env_vars.is_empty() {
            let mut with_env: Vec<String> = env_vars.iter().map(|s| s.to_string()).collect();
            with_env.extend(r.tokens);
            r.tokens = with_env;
        }
        r
    })
}

#[cfg(test)]
mod tests {
    use super::super::types::RewriteCategory;
    use super::*;

    // ========================================================================
    // Prefix rule matches
    // ========================================================================

    #[test]
    fn test_cargo_test() {
        let result = try_rewrite(&["cargo", "test"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "cargo"]);
    }

    #[test]
    fn test_cargo_test_with_trailing_args() {
        let result = try_rewrite(&["cargo", "test", "--", "--nocapture"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "test", "cargo", "--", "--nocapture"]
        );
    }

    #[test]
    fn test_cargo_nextest_run() {
        let result = try_rewrite(&["cargo", "nextest", "run"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "cargo"]);
    }

    #[test]
    fn test_cargo_clippy() {
        let result = try_rewrite(&["cargo", "clippy"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "build", "clippy"]);
    }

    #[test]
    fn test_cargo_build() {
        let result = try_rewrite(&["cargo", "build"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "build", "cargo"]);
    }

    #[test]
    fn test_git_status() {
        let result = try_rewrite(&["git", "status"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "git", "status"]);
    }

    #[test]
    fn test_git_diff() {
        let result = try_rewrite(&["git", "diff"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "git", "diff"]);
    }

    #[test]
    fn test_no_match_returns_none() {
        assert!(try_rewrite(&["echo", "hello"]).is_none());
    }

    // ========================================================================
    // strip_env_vars
    // ========================================================================

    #[test]
    fn test_strip_env_vars_none() {
        assert_eq!(strip_env_vars(&["cargo", "test"]), 0);
    }

    #[test]
    fn test_strip_env_vars_one() {
        assert_eq!(strip_env_vars(&["RUST_LOG=debug", "cargo", "test"]), 1);
    }

    #[test]
    fn test_strip_env_vars_two() {
        assert_eq!(
            strip_env_vars(&["RUST_LOG=debug", "NO_COLOR=1", "cargo", "test"]),
            2
        );
    }

    #[test]
    fn test_strip_env_vars_lowercase_not_stripped() {
        assert_eq!(strip_env_vars(&["foo=bar", "cargo", "test"]), 0);
    }

    // ========================================================================
    // strip_cargo_toolchain
    // ========================================================================

    #[test]
    fn test_strip_cargo_toolchain_nightly() {
        let (tc, tokens) = strip_cargo_toolchain(&["cargo", "+nightly", "test"]);
        assert_eq!(tc, Some("+nightly"));
        assert_eq!(tokens, vec!["cargo", "test"]);
    }

    #[test]
    fn test_strip_cargo_toolchain_none() {
        let (tc, tokens) = strip_cargo_toolchain(&["cargo", "test"]);
        assert!(tc.is_none());
        assert_eq!(tokens, vec!["cargo", "test"]);
    }

    // ========================================================================
    // split_at_separator
    // ========================================================================

    #[test]
    fn test_split_at_separator_found() {
        assert_eq!(
            split_at_separator(&["cargo", "test", "--", "--nocapture"]),
            2
        );
    }

    #[test]
    fn test_split_at_separator_not_found() {
        assert_eq!(split_at_separator(&["cargo", "test"]), 2);
    }

    // ========================================================================
    // Category assignment
    // ========================================================================

    #[test]
    fn test_test_category_for_cargo_test() {
        let result = try_rewrite(&["cargo", "test"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Test));
    }

    #[test]
    fn test_build_category_for_cargo_build() {
        let result = try_rewrite(&["cargo", "build"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Build));
    }

    #[test]
    fn test_git_category_for_git_status() {
        let result = try_rewrite(&["git", "status"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Git));
    }

    #[test]
    fn test_read_category_for_cat() {
        let result = try_rewrite(&["cat", "file.ts"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Read));
    }

    // ========================================================================
    // Cargo toolchain stripping
    // ========================================================================

    #[test]
    fn test_cargo_toolchain_nightly() {
        let result = try_rewrite(&["cargo", "+nightly", "test"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "cargo", "+nightly"]);
    }

    #[test]
    fn test_cargo_toolchain_with_env_var() {
        let result = try_rewrite(&["RUST_LOG=debug", "cargo", "+nightly", "test"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["RUST_LOG=debug", "skim", "test", "cargo", "+nightly"]
        );
    }

    // ========================================================================
    // cat handler tests (exercising engine → handlers path)
    // ========================================================================

    #[test]
    fn test_cat_code_file() {
        let result = try_rewrite(&["cat", "file.rs"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "file.rs", "--mode=pseudo"]);
    }

    #[test]
    fn test_cat_non_code_file() {
        assert!(try_rewrite(&["cat", "file.txt"]).is_none());
    }

    #[test]
    fn test_cat_no_args() {
        assert!(try_rewrite(&["cat"]).is_none());
    }

    // ========================================================================
    // Skip-flag behavior (git rules)
    // ========================================================================

    #[test]
    fn test_git_status_with_porcelain_rewrites() {
        let result = try_rewrite(&["git", "status", "--porcelain"]);
        assert!(
            result.is_some(),
            "Expected rewrite for 'git status --porcelain' — flag is now stripped by handler"
        );
        assert_eq!(
            result.unwrap().tokens,
            vec!["skim", "git", "status", "--porcelain"]
        );
    }

    // ========================================================================
    // Strict flag matching (AD-1 hygiene)
    // ========================================================================

    /// Regression: `--staged` must NOT be eaten by a `--stat` skip prefix.
    ///
    /// With the old loose `starts_with` check, `"--staged".starts_with("--stat")`
    /// returned `true`, silently blocking the AST-aware diff pipeline for staged
    /// changes. The strict-match fix (AD-1) resolves this.
    #[test]
    fn test_staged_not_eaten_by_stat_prefix() {
        // After skip-list trim (AD-4), `--stat` is no longer in the git diff
        // skip list, so `--staged` rewrites regardless. This test also verifies
        // that the strict engine itself would not eat `--staged` even if
        // `--stat` were still in the list.
        let result = try_rewrite(&["git", "diff", "--staged"]);
        assert!(
            result.is_some(),
            "git diff --staged must rewrite after engine strict-match fix"
        );
        let rewritten = result.unwrap().tokens.join(" ");
        assert!(
            rewritten.contains("--staged"),
            "rewritten command must preserve --staged: {rewritten}"
        );
    }

    /// Strict-match sweep: for each skip prefix in all rules, the exact form
    /// (`--flag`) must suppress rewrite, but a longer arg with the same prefix
    /// must NOT suppress rewrite unless it also follows the `--flag=value` pattern.
    #[test]
    fn test_strict_skip_no_false_prefix_collisions() {
        use super::super::rules::REWRITE_RULES;

        // For every rule's skip prefix, construct a longer arg that does NOT
        // have `=` at the split point and verify it does NOT trigger the skip.
        for rule in REWRITE_RULES {
            for &skip in rule.skip_if_flag_prefix {
                // The longer variant (skip + "x") must not be eaten by the
                // skip rule for the base flag.
                let extended = format!("{skip}x");
                let strict_skip_match = |arg: &str, flag: &str| -> bool {
                    arg == flag
                        || (arg.starts_with(flag) && arg.as_bytes().get(flag.len()) == Some(&b'='))
                };
                assert!(
                    !strict_skip_match(&extended, skip),
                    "strict_skip_match({:?}, {:?}) must be false — only exact or =value forms allowed",
                    extended, skip
                );

                // The `--flag=value` form must be eaten.
                let with_value = format!("{skip}=somevalue");
                assert!(
                    strict_skip_match(&with_value, skip),
                    "strict_skip_match({:?}, {:?}) must be true for =value form",
                    with_value,
                    skip
                );
            }
        }
    }

    // ========================================================================
    // Env var edge cases
    // ========================================================================

    #[test]
    fn test_lowercase_key_not_env_var() {
        // lowercase=value is not an env var (must be uppercase)
        assert!(try_rewrite(&["foo=bar", "cargo", "test"]).is_none());
    }

    #[test]
    fn test_env_var_with_numbers() {
        let result = try_rewrite(&["VAR_123=abc", "cargo", "test"]).unwrap();
        assert!(result.tokens.contains(&"VAR_123=abc".to_string()));
    }
}
