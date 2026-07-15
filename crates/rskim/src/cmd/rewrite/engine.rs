//! Core rewrite algorithm — table matching and custom handlers.

use super::handlers::{try_rewrite_cat, try_rewrite_head, try_rewrite_tail};
use super::rules;
use super::types::RewriteResult;

/// Return true if any token in `middle` matches a skip-flag prefix.
///
/// Uses strict matching: `arg == flag` or `arg` starts with `flag=`.
/// Prevents loose `starts_with` from eating flags like `--staged` when
/// matching against `--stat`.  SEE: AD-RW-1.
fn should_skip_by_flag(middle: &[&str], skip_prefixes: &[&str]) -> bool {
    if skip_prefixes.is_empty() {
        return false;
    }
    middle.iter().any(|arg| {
        skip_prefixes.iter().any(|flag| {
            *arg == *flag
                || (arg.starts_with(flag) && arg.as_bytes().get(flag.len()) == Some(&b'='))
        })
    })
}

/// Attempt to rewrite a tokenized command. Returns `Some(RewriteResult)` on
/// match, `None` if no rewrite applies.
///
/// # Algorithm
///
/// Performs an env-var strip, toolchain strip, and separator split, then
/// walks the rule table once.  When a skip flag fires on a rule, iteration
/// continues (not returns) — but if a LATER rule then matches, the skipped
/// more-specific rule suppresses the rewrite (returns `None`).  This keeps
/// "specific rule said no" authoritative over broader catch-alls.
pub(super) fn try_rewrite(tokens: &[&str]) -> Option<RewriteResult> {
    if tokens.is_empty() {
        return None;
    }

    // Step 1: Strip leading env vars
    let env_split = strip_env_vars(tokens);
    let env_vars = &tokens[..env_split];
    let command_tokens = &tokens[env_split..];

    if command_tokens.is_empty() {
        return None;
    }

    // Step 1b: Honor SKIM_PASSTHROUGH=<truthy> as a command prefix.
    //
    // When an agent runs `SKIM_PASSTHROUGH=1 gh pr list`, the hook rewrites
    // its own env but the child inherits that env var. The env-prefix form
    // lets callers bypass rewriting for a single invocation without setting
    // or unsetting the global env, which is useful in scripts. SEE: AD-RW-14.
    if env_vars_contain_passthrough(env_vars) {
        return None;
    }

    // Step 2: Strip cargo toolchain prefix (+nightly etc.)
    let (toolchain, match_tokens) = strip_cargo_toolchain(command_tokens);

    // Step 3: Split at `--` separator
    let sep_pos = split_at_separator(&match_tokens);
    let before_sep = &match_tokens[..sep_pos];
    let separator_and_after = &match_tokens[sep_pos..];

    let mut skipped = false;

    for rule in rules::all_rules() {
        // --- Primary match path: strict prefix match (fast path) ---
        let strict_match = before_sep.len() >= rule.prefix.len()
            && before_sep[..rule.prefix.len()] == *rule.prefix;

        // --- Secondary match path: global-flag-aware match ---
        //
        // When a rule has `global_value_flags`, also try matching `prefix[0]`
        // at position 0, then skipping global flags in the remaining tokens to
        // find the subcommand.  This handles `kubectl -n ns get pods` where
        // global flags appear between the binary name and the subcommand.
        //
        // Only attempted when strict_match fails AND global_value_flags is non-empty,
        // so it adds zero overhead to rules that don't use it.
        let global_middle: Option<&[&str]>;
        let matched = if strict_match {
            global_middle = None;
            true
        } else if !rule.global_value_flags.is_empty()
            && !rule.prefix.is_empty()
            && !before_sep.is_empty()
            && before_sep[0] == rule.prefix[0]
        {
            // Skip global value flags starting from position 1 to find the subcommand.
            let skip = find_subcommand_in_tokens(&before_sep[1..], rule.global_value_flags);
            let sub_start = 1 + skip;
            let remaining = &before_sep[sub_start..];
            // Check that the remaining prefix elements (all except prefix[0])
            // match at the subcommand position.
            let suffix_prefix = &rule.prefix[1..];
            if remaining.len() >= suffix_prefix.len()
                && remaining[..suffix_prefix.len()] == *suffix_prefix
            {
                global_middle = Some(&remaining[suffix_prefix.len()..]);
                true
            } else {
                global_middle = None;
                false
            }
        } else {
            global_middle = None;
            false
        };

        if !matched {
            continue;
        }

        // `middle` is the tokens after the prefix match (used for skip checks).
        let middle: &[&str] = if strict_match {
            &before_sep[rule.prefix.len()..]
        } else {
            global_middle.unwrap_or(&[])
        };

        if should_skip_by_flag(middle, rule.skip_if_flag_prefix) {
            // Skip this rule but continue iterating so a catch-all rule can
            // still report its pipe_excluded value (AD-RW-2 asymmetry).
            skipped = true;
            continue;
        }

        // Guard: skip when the rule requires no `=`-containing middle tokens
        // but at least one is present.  This prevents `env LANG=C sort` from
        // being rewritten to `skim env LANG=C sort`, which breaks semantics
        // because the `VAR=val` tokens are env-var assignments for the child
        // command, not skim-processable output.
        if rule.skip_if_middle_contains_eq && middle.iter().any(|t| t.contains('=')) {
            skipped = true;
            continue;
        }

        // Fix 4: require_flag guard — at least one of the required flags must
        // be present somewhere after the prefix.  Used to distinguish
        // `psql -c "SQL"` (batch, safe to rewrite) from `psql -h host -d db`
        // (interactive, must not rewrite).
        if !rule.require_flag.is_empty() {
            let all_tokens_after_cmd = if strict_match {
                middle
            } else {
                // For global-flag-aware matches, search in the full set of
                // tokens after prefix[0] so flags before the subcommand are
                // also considered.
                &before_sep[1..]
            };
            let has_required = all_tokens_after_cmd
                .iter()
                .any(|tok| rule.require_flag.iter().any(|f| tok == f));
            if !has_required {
                skipped = true;
                continue;
            }
        }

        if skipped {
            // A more-specific rule was skipped by its flag — no rewrite.
            return None;
        }

        // Normal match — build rewrite output.
        // For global-flag matches, preserve the original token order so that
        // global flags remain in the output (e.g. `kubectl -n ns get pods`
        // → `skim kubectl -n ns get pods`).
        let output: Vec<String> = if strict_match {
            env_vars
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
                .collect()
        } else {
            // Global-flag match: emit rewrite_to[0] (the `skim` token) +
            // all original tokens (binary name + global flags + subcommand +
            // subcommand args) so that global flags are preserved in their
            // original positions.
            env_vars
                .iter()
                .map(|s| s.to_string())
                .chain(std::iter::once(rule.rewrite_to[0].to_string()))
                .chain(toolchain.map(String::from))
                // Emit the full original before_sep: binary name, global flags,
                // subcommand, and any remaining args.
                .chain(before_sep.iter().map(|s| s.to_string()))
                .chain(separator_and_after.iter().map(|s| s.to_string()))
                .collect()
        };

        return Some(RewriteResult {
            tokens: output,
            category: rule.category,
        });
    }

    // No rule matched — fall through to custom handlers for cat/head/tail.
    try_custom_handlers(env_vars, command_tokens)
}

/// Skip global flags in a token slice to find the subcommand index.
///
/// Returns the index (within `tokens`) of the first non-flag token.  Tokens
/// listed in `value_flags` are treated as value-consuming: the flag and the
/// immediately following token are both skipped.  Tokens that start with `--`
/// and contain `=` are treated as self-contained `--flag=value` tokens and
/// skipped without consuming an extra token.  All other flag tokens (start
/// with `-`) are skipped as boolean flags.
///
/// Used for global-flag-aware rewrite matching (Fix 3, rules with
/// `global_value_flags`).
fn find_subcommand_in_tokens(tokens: &[&str], value_flags: &[&str]) -> usize {
    let mut idx = 0;
    while idx < tokens.len() {
        let tok = tokens[idx];
        // Self-contained `--flag=value` form — skip, no extra token.
        if tok.starts_with("--") && tok.contains('=') {
            idx += 1;
            continue;
        }
        // Value-consuming flag — skip flag + following value token.
        if value_flags.contains(&tok) {
            idx += 2;
            continue;
        }
        // Boolean flag — skip without consuming a value.
        if tok.starts_with('-') {
            idx += 1;
            continue;
        }
        // Non-flag token: this is the subcommand position.
        return idx;
    }
    tokens.len()
}

/// Return `true` if any env-var token is `SKIM_PASSTHROUGH` with a truthy value.
///
/// Delegates to [`crate::cmd::check_passthrough_str`] for the truthy check
/// so the definition of "truthy" stays in one place.  SEE: AD-RW-14.
fn env_vars_contain_passthrough(env_vars: &[&str]) -> bool {
    env_vars.iter().any(|token| {
        token
            .strip_prefix("SKIM_PASSTHROUGH=")
            .map(crate::cmd::check_passthrough_str)
            .unwrap_or(false)
    })
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
        assert_eq!(result.tokens, vec!["skim", "cargo", "test"]);
    }

    #[test]
    fn test_cargo_test_with_trailing_args() {
        let result = try_rewrite(&["cargo", "test", "--", "--nocapture"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "cargo", "test", "--", "--nocapture"]
        );
    }

    #[test]
    fn test_cargo_nextest_run() {
        // "run" must be preserved so the dispatch layer receives "nextest run …" intact (FIX 2).
        let result = try_rewrite(&["cargo", "nextest", "run"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "cargo", "nextest", "run"]);
    }

    #[test]
    fn test_cargo_clippy() {
        let result = try_rewrite(&["cargo", "clippy"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "cargo", "clippy"]);
    }

    #[test]
    fn test_cargo_build() {
        let result = try_rewrite(&["cargo", "build"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "cargo", "build"]);
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
        assert_eq!(result.tokens, vec!["skim", "cargo", "test", "+nightly"]);
    }

    #[test]
    fn test_cargo_toolchain_with_env_var() {
        let result = try_rewrite(&["RUST_LOG=debug", "cargo", "+nightly", "test"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["RUST_LOG=debug", "skim", "cargo", "test", "+nightly"]
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
    // Strict flag matching (AD-RW-1 hygiene)
    // ========================================================================

    /// Regression: `--staged` must NOT be eaten by a `--stat` skip prefix.
    ///
    /// With the old loose `starts_with` check, `"--staged".starts_with("--stat")`
    /// returned `true`, silently blocking the AST-aware diff pipeline for staged
    /// changes. The strict-match fix (AD-RW-1) resolves this.
    #[test]
    fn test_staged_not_eaten_by_stat_prefix() {
        // After skip-list trim (AD-RW-4), `--stat` is no longer in the git diff
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

    /// Strict-match sweep: for each skip prefix in all rules, constructs a
    /// longer arg (`{skip}x`) with no `=` at the split point and asserts that
    /// `try_rewrite()` still matches the rule. A looser matcher (old behavior)
    /// would cause the rule to skip, hiding `--staged` and similar collisions.
    ///
    /// This exercises the real engine path rather than a local copy of the
    /// closure, so it will catch regressions in `try_table_match` directly.
    #[test]
    fn test_strict_skip_no_false_prefix_collisions() {
        for rule in super::super::rules::all_rules() {
            for &skip in rule.skip_if_flag_prefix {
                // Build a command: rule.prefix ++ [extended_arg]
                // where extended_arg = skip + "x" (e.g., "--stat" -> "--statx")
                let extended = format!("{skip}x");
                let mut tokens: Vec<&str> = rule.prefix.to_vec();
                tokens.push(&extended);

                let result = try_rewrite(&tokens);
                assert!(
                    result.is_some(),
                    "Rule {:?} with extended arg {:?} must rewrite — \
                     the engine's strict-match must not confuse {:?} with {:?}",
                    rule.prefix,
                    extended,
                    extended,
                    skip
                );
                let out = result.unwrap().tokens.join(" ");
                assert!(
                    out.contains(&extended),
                    "Rewritten output must preserve the extended arg {:?}: {}",
                    extended,
                    out
                );

                // And the exact `--flag=value` form should be skipped (returns None),
                // proving the engine *does* honor `=value` as a valid skip trigger.
                let with_value = format!("{skip}=somevalue");
                let mut tokens_eq: Vec<&str> = rule.prefix.to_vec();
                tokens_eq.push(&with_value);
                let result_eq = try_rewrite(&tokens_eq);
                assert!(
                    result_eq.is_none(),
                    "Rule {:?} with arg {:?} must be skipped by strict `=value` match",
                    rule.prefix,
                    with_value
                );
            }
        }
    }

    // ========================================================================
    // Glued short-flag behavior (regression-1 / AD-RW-1 side effect)
    // ========================================================================

    /// Strict-match fix (AD-RW-1) side effect: glued short flags like `-qverbose`
    /// do NOT match the skip prefix `-q` (strict match requires exact equality or
    /// `flag=value`).  This means `git fetch -qverbose` is NOT suppressed by the
    /// `-q` skip rule — it rewrites, passing `-qverbose` through to the skim
    /// wrapper unchanged.  This is intentional and correct: the skim wrapper
    /// receives the user's flag verbatim.
    #[test]
    fn test_strict_match_glued_short_flag_rewrites() {
        // `-q` is in the `git fetch` skip list.  A glued flag `-qverbose` must
        // NOT trigger the skip — the rule should still fire.
        let result = try_rewrite(&["git", "fetch", "-qverbose"]);
        assert!(
            result.is_some(),
            "git fetch -qverbose must rewrite: glued short flag must not match the -q skip prefix"
        );
        let rewritten = result.unwrap().tokens.join(" ");
        assert!(
            rewritten.contains("-qverbose"),
            "Glued flag must be preserved verbatim in output: {rewritten}"
        );

        // Sanity: the exact `-q` flag IS still skipped.
        let skipped = try_rewrite(&["git", "fetch", "-q"]);
        assert!(
            skipped.is_none(),
            "git fetch -q must still be skipped (exact match)"
        );
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

    // ========================================================================
    // Skipped-specific-rule suppression (preserved through the #317 collapse
    // of try_table_match_full into try_rewrite)
    // ========================================================================

    /// `grep -rn --count` matches the specific `grep -rn` rule, whose skip
    /// flag (`--count`) fires → skipped=true. When the catch-all `grep` rule
    /// is then reached, the skipped more-specific rule SUPPRESSES the rewrite:
    /// "specific rule said no" stays authoritative over broader catch-alls.
    #[test]
    fn test_skipped_specific_rule_suppresses_catch_all() {
        assert!(
            try_rewrite(&["grep", "-rn", "--count"]).is_none(),
            "grep -rn --count must not rewrite (skip flag on specific rule \
             suppresses the later catch-all)"
        );
    }

    /// `grep -rn pattern` has no skip flag → specific rule fires normally.
    #[test]
    fn test_grep_rn_no_skip_flag_rewrites() {
        assert!(
            try_rewrite(&["grep", "-rn", "pattern"]).is_some(),
            "grep -rn pattern must produce a rewrite"
        );
    }

    /// `grep --count` matches only the catch-all `grep` rule (no skip flag on
    /// the catch-all) → a rewrite IS produced.
    #[test]
    fn test_catch_all_grep_count_rewrites() {
        assert!(
            try_rewrite(&["grep", "--count"]).is_some(),
            "catch-all grep --count should produce a rewrite (no skip flag on catch-all)"
        );
    }

    // ========================================================================
    // env_vars_contain_passthrough unit tests (AD-RW-14)
    // ========================================================================

    #[test]
    fn test_env_vars_contain_passthrough_truthy() {
        assert!(
            env_vars_contain_passthrough(&["SKIM_PASSTHROUGH=1"]),
            "SKIM_PASSTHROUGH=1 must be detected as truthy"
        );
    }

    #[test]
    fn test_env_vars_contain_passthrough_falsy() {
        assert!(
            !env_vars_contain_passthrough(&["SKIM_PASSTHROUGH=0"]),
            "SKIM_PASSTHROUGH=0 must NOT be detected as truthy"
        );
    }

    #[test]
    fn test_env_vars_contain_passthrough_absent() {
        assert!(
            !env_vars_contain_passthrough(&["RUST_LOG=debug"]),
            "Absence of SKIM_PASSTHROUGH must return false"
        );
    }

    #[test]
    fn test_env_vars_contain_passthrough_mixed() {
        assert!(
            env_vars_contain_passthrough(&["RUST_LOG=debug", "SKIM_PASSTHROUGH=1"]),
            "SKIM_PASSTHROUGH=1 among other env vars must return true"
        );
    }
}
