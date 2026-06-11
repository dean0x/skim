use super::*;

// ========================================================================
// classify_command() — tri-state API tests (AD-RW-2)
// ========================================================================

#[test]
fn test_classify_simple_rewritten() {
    assert_eq!(
        classify_command("git show HEAD"),
        CommandClassification::Rewritten("skim git show HEAD".to_string()),
        "git show HEAD must be classified as Rewritten"
    );
}

#[test]
fn test_classify_simple_already_compact() {
    assert_eq!(
        classify_command("git worktree list"),
        CommandClassification::AlreadyCompact,
        "git worktree list must be classified as AlreadyCompact"
    );
}

#[test]
fn test_classify_simple_unhandled() {
    assert_eq!(
        classify_command("echo hello"),
        CommandClassification::Unhandled,
        "echo hello is not rewritable or acknowledged"
    );
}

#[test]
fn test_classify_compound_all_rewritten() {
    let result = classify_command("cargo test && cargo clippy");
    match result {
        CommandClassification::Rewritten(s) => {
            assert!(
                s.contains("skim cargo test"),
                "Expected skim cargo test in output, got: {s}"
            );
            assert!(
                s.contains("skim cargo clippy"),
                "Expected skim cargo clippy in output, got: {s}"
            );
            assert!(s.contains("&&"), "Expected && operator in output, got: {s}");
        }
        other => panic!("Expected Rewritten, got {other:?}"),
    }
}

#[test]
fn test_classify_compound_mixed_rewritten_ack() {
    let result = classify_command("git worktree list && git show HEAD");
    match result {
        CommandClassification::Rewritten(s) => {
            assert!(
                s.contains("git worktree list"),
                "AlreadyCompact segment must pass through unchanged: {s}"
            );
            assert!(
                s.contains("skim git show HEAD"),
                "Rewritten segment must be rewritten: {s}"
            );
        }
        other => panic!("Expected Rewritten for mixed ack+rewritten, got {other:?}"),
    }
}

#[test]
fn test_classify_compound_all_ack() {
    let result = classify_command("git worktree list && git worktree list");
    assert_eq!(
        result,
        CommandClassification::AlreadyCompact,
        "All-ack compound must be AlreadyCompact"
    );
}

#[test]
fn test_classify_compound_any_nomatch() {
    let result = classify_command("git worktree list && echo done");
    assert_eq!(
        result,
        CommandClassification::Unhandled,
        "Any NoMatch segment in compound must make the whole thing Unhandled"
    );
}

#[test]
fn test_classify_pipe_first_segment_rewritten() {
    let result = classify_command("git show HEAD | less");
    match result {
        CommandClassification::Rewritten(s) => {
            assert!(
                s.contains("skim git show HEAD"),
                "First pipe segment must be rewritten: {s}"
            );
            assert!(s.contains("| less"), "Pipe consumer must be preserved: {s}");
        }
        other => panic!("Expected Rewritten for pipe with rewritable first seg, got {other:?}"),
    }
}

#[test]
fn test_classify_pipe_first_segment_ack() {
    let result = classify_command("git worktree list | wc -l");
    assert_eq!(
        result,
        CommandClassification::AlreadyCompact,
        "Pipe with AlreadyCompact first segment must be AlreadyCompact"
    );
}

/// Stripped redirects must survive classify_compound reconstruction (Issue #2 / AD-RW-2).
///
/// `cargo test 2>&1 && cargo build` — the `2>&1` is stripped before rule matching
/// and must be spliced back into the rewritten compound string so it is not
/// silently dropped from the discover suggestion.
#[test]
fn test_classify_compound_preserves_stripped_redirects() {
    let result = classify_command("cargo test 2>&1 && cargo build");
    match result {
        CommandClassification::Rewritten(s) => {
            assert!(
                s.contains("2>&1"),
                "Stripped redirect must be preserved in rewritten compound: {s}"
            );
        }
        other => panic!("Expected Rewritten, got {other:?}"),
    }
}

/// Stripped redirects must survive classify_compound_pipe reconstruction (Issue #2).
///
/// `cargo test 2>&1 | head` — the `2>&1` is stripped before rule matching
/// and must be spliced back into the rewritten pipe command.
#[test]
fn test_classify_compound_pipe_preserves_stripped_redirects() {
    let result = classify_command("cargo test 2>&1 | head");
    match result {
        CommandClassification::Rewritten(s) => {
            assert!(
                s.contains("2>&1"),
                "Stripped redirect must be preserved in rewritten pipe: {s}"
            );
            assert!(s.contains("| head"), "Pipe consumer must be preserved: {s}");
        }
        other => panic!("Expected Rewritten, got {other:?}"),
    }
}

#[test]
fn test_classify_already_skim_returns_unhandled() {
    assert_eq!(
        classify_command("skim git show HEAD"),
        CommandClassification::Unhandled,
        "Already-skim commands must return Unhandled"
    );
}

#[test]
fn test_classify_empty_returns_unhandled() {
    assert_eq!(
        classify_command(""),
        CommandClassification::Unhandled,
        "Empty input must return Unhandled"
    );
    assert_eq!(
        classify_command("   "),
        CommandClassification::Unhandled,
        "Whitespace-only input must return Unhandled"
    );
}

// ========================================================================
// would_rewrite() API tests
// ========================================================================

#[test]
fn test_would_rewrite_git_status_with_s() {
    assert_eq!(
        would_rewrite("git status -s"),
        Some("skim git status -s".to_string()),
        "git status -s should rewrite (handler strips -s)"
    );
}

#[test]
fn test_would_rewrite_git_log_oneline() {
    let result = would_rewrite("git log --oneline -5");
    assert!(
        result.is_some(),
        "git log --oneline -5 should rewrite (handler strips --oneline)"
    );
    let rewritten = result.unwrap();
    assert!(
        rewritten.starts_with("skim git log"),
        "Expected 'skim git log ...' prefix, got: {rewritten}"
    );
}

#[test]
fn test_would_rewrite_already_skim_returns_none() {
    assert_eq!(
        would_rewrite("skim git status"),
        None,
        "Already-skim commands must not be rewritten"
    );
}

#[test]
fn test_would_rewrite_empty_returns_none() {
    assert_eq!(would_rewrite(""), None, "Empty input must return None");
    assert_eq!(
        would_rewrite("   "),
        None,
        "Whitespace-only input must return None"
    );
}

#[test]
fn test_would_rewrite_non_rewritable_returns_none() {
    assert_eq!(
        would_rewrite("python3 -c 'print(1)'"),
        None,
        "python3 -c is not a rewritable pattern"
    );
}

/// `git diff --stat` now rewrites (--stat removed from skip list per AD-RW-4).
/// The diff handler detects --stat via user_has_flag and calls run_passthrough,
/// so the user sees byte-identical git output.
#[test]
fn test_would_rewrite_git_diff_stat_rewrites() {
    let result = would_rewrite("git diff --stat");
    assert_eq!(
        result,
        Some("skim git diff --stat".to_string()),
        "git diff --stat must rewrite after AD-RW-4 skip-list trim"
    );
}

#[test]
fn test_would_rewrite_gh_pr_list_json_skips() {
    // --json is now in the skip-list for all gh list/view commands: passthrough.
    assert_eq!(
        would_rewrite("gh pr list --json number"),
        None,
        "gh pr list --json must skip rewrite (output-steering flag)"
    );
}

#[test]
fn test_would_rewrite_jest_rewrites() {
    assert_eq!(
        would_rewrite("jest src/"),
        Some("skim jest src/".to_string()),
        "jest should rewrite to skim jest"
    );
}

#[test]
fn test_would_rewrite_npx_jest_rewrites() {
    assert_eq!(
        would_rewrite("npx jest src/"),
        Some("skim jest src/".to_string()),
        "npx jest should rewrite to skim jest"
    );
}

/// Regression test for mixed-compound semantics (regression-2 / AD-RW-2).
///
/// `would_rewrite` wraps `classify_command`, which returns `Unhandled` when
/// ANY segment of a compound command has no match.  A compound like
/// `"cargo test && echo done"` has one rewritable segment (`cargo test`) and
/// one unhandled segment (`echo done`), so `classify_command` returns
/// `Unhandled` and `would_rewrite` returns `None`.
///
/// This is intentional: `would_rewrite` is a conservative API — `None` means
/// "the full compound cannot be cleanly rewritten".  Callers that need
/// per-segment resolution should use `classify_command` directly.
#[test]
fn test_would_rewrite_mixed_compound_returns_none() {
    // One rewritable segment + one unhandled segment → None.
    assert_eq!(
        would_rewrite("cargo test && echo done"),
        None,
        "Mixed compound with an unhandled segment must return None"
    );
    // Sanity: pure-rewritable compound still returns Some.
    assert!(
        would_rewrite("cargo test && cargo clippy").is_some(),
        "All-rewritable compound must return Some"
    );
}

// ========================================================================
// has_compound_operators() — byte-scanner edge cases
// ========================================================================

#[test]
fn test_has_compound_operators_empty() {
    assert!(!has_compound_operators(""), "empty string has no operators");
}

#[test]
fn test_has_compound_operators_single_char_no_op() {
    assert!(!has_compound_operators("a"), "single non-op char");
    assert!(!has_compound_operators("x"), "single non-op char x");
}

#[test]
fn test_has_compound_operators_pipe() {
    assert!(has_compound_operators("git log | less"), "| is an operator");
    assert!(has_compound_operators("|"), "bare | is an operator");
}

#[test]
fn test_has_compound_operators_semicolon() {
    assert!(has_compound_operators("echo a; echo b"), "; is an operator");
    assert!(has_compound_operators(";"), "bare ; is an operator");
}

#[test]
fn test_has_compound_operators_double_ampersand() {
    assert!(
        has_compound_operators("cargo test && cargo clippy"),
        "&& is an operator"
    );
    assert!(has_compound_operators("&&"), "bare && is an operator");
}

#[test]
fn test_has_compound_operators_single_ampersand_is_not_compound() {
    // A lone `&` (background job) is intentionally NOT treated as a
    // compound operator by this scanner; only `&&` triggers it.
    assert!(
        !has_compound_operators("cargo test &"),
        "trailing single & is not a compound operator"
    );
    assert!(
        !has_compound_operators("&"),
        "bare single & is not a compound operator"
    );
}

#[test]
fn test_has_compound_operators_double_pipe() {
    // `||` starts with `|` which is immediately detected as an operator.
    assert!(
        has_compound_operators("cmd1 || cmd2"),
        "|| contains | which is an operator"
    );
}

#[test]
fn test_has_compound_operators_pipe_ampersand_combo() {
    // `|&` starts with `|` — detected on the first byte.
    assert!(
        has_compound_operators("cmd |& tee out.txt"),
        "|& starts with | which is an operator"
    );
}

#[test]
fn test_has_compound_operators_lookahead_at_end() {
    // `bytes.get(i + 1) == Some(&b'&')` must return false (not panic)
    // when the trailing byte is a lone `&` at end-of-string.
    assert!(
        !has_compound_operators("cmd &"),
        "trailing lone & without a second & is not an operator"
    );
    // But trailing `&&` is valid.
    assert!(
        has_compound_operators("cmd &&"),
        "trailing && is a compound operator"
    );
}

#[test]
fn test_has_compound_operators_plain_command() {
    assert!(
        !has_compound_operators("git status"),
        "plain command has no compound operator"
    );
    assert!(
        !has_compound_operators("cargo test --lib"),
        "cargo test with flags has no compound operator"
    );
}

// ========================================================================
// collect_input_tokens() — edge-case coverage (AD-RW-13)
// ========================================================================

/// Helper: invoke collect_input_tokens with a set of &str positional args.
fn tokens_from(args: &[&str]) -> Option<Vec<String>> {
    collect_input_tokens(args).expect("collect_input_tokens must not error")
}

/// Empty positional args list with no stdin → returns None.
///
/// Note: this test is only meaningful when stdin is not a pipe (i.e. when
/// running interactively).  In CI, stdin is typically not a TTY so the
/// function reads stdin; passing an empty slice here avoids that branch.
/// The test verifies the `tokens.is_empty()` guard inside the function.
#[test]
fn test_collect_input_tokens_empty_slice_is_none() {
    // An all-whitespace single arg produces no tokens → None.
    assert_eq!(
        tokens_from(&["   "]),
        None,
        "all-whitespace single arg must return None"
    );
}

/// Convert a `&[&str]` literal into `Vec<String>` for assertion comparisons.
fn sv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// Single multi-word quoted arg tokenizes the same as equivalent multi-arg form.
///
/// Regression for the AD-RW-13 fix: `skim rewrite 'prettier --check src/'`
/// (shell passes one arg) must tokenize identically to
/// `skim rewrite prettier --check src/` (three separate args).
#[test]
fn test_collect_input_tokens_single_quoted_equals_multi_arg() {
    let single = tokens_from(&["prettier --check src/"]);
    let multi = tokens_from(&["prettier", "--check", "src/"]);
    assert_eq!(
        single, multi,
        "single-quoted arg must produce same tokens as multi-arg form"
    );
    assert_eq!(
        single,
        Some(sv(&["prettier", "--check", "src/"])),
        "expected 3 tokens"
    );
}

/// Tab characters inside a single arg are treated as whitespace (split_whitespace).
#[test]
fn test_collect_input_tokens_tab_as_whitespace() {
    let result = tokens_from(&["cargo\ttest"]);
    assert_eq!(
        result,
        Some(sv(&["cargo", "test"])),
        "tab must be treated as whitespace"
    );
}

/// Multiple consecutive spaces inside a single arg collapse to one split boundary.
#[test]
fn test_collect_input_tokens_consecutive_spaces() {
    let result = tokens_from(&["cargo  test  --release"]);
    assert_eq!(
        result,
        Some(sv(&["cargo", "test", "--release"])),
        "consecutive spaces must collapse to single boundaries"
    );
}

/// Mixed quoted + bare args: flat_map over all positional args.
///
/// `skim rewrite 'cargo test' --extra` produces positional args
/// `["cargo test", "--extra"]`, which should flat_map to
/// `["cargo", "test", "--extra"]`.
#[test]
fn test_collect_input_tokens_mixed_quoted_and_bare() {
    let result = tokens_from(&["cargo test", "--extra"]);
    assert_eq!(
        result,
        Some(sv(&["cargo", "test", "--extra"])),
        "mixed quoted + bare args must flat_map to unified token list"
    );
}

/// Empty string arg inside a multi-arg slice contributes no tokens.
#[test]
fn test_collect_input_tokens_empty_string_arg_ignored() {
    // ["", "cargo", "test"] → the empty arg contributes nothing.
    let result = tokens_from(&["", "cargo", "test"]);
    assert_eq!(
        result,
        Some(sv(&["cargo", "test"])),
        "empty string arg must contribute no tokens"
    );
}

/// Single non-empty arg with no spaces produces a single-token result.
#[test]
fn test_collect_input_tokens_single_word() {
    let result = tokens_from(&["pytest"]);
    assert_eq!(
        result,
        Some(sv(&["pytest"])),
        "single word must produce single token"
    );
}

/// All-whitespace multi-arg slice produces None.
#[test]
fn test_collect_input_tokens_all_whitespace_multi() {
    let result = tokens_from(&[" ", "\t", "  "]);
    assert_eq!(
        result, None,
        "all-whitespace multi-arg must return None (no tokens)"
    );
}

// ========================================================================
// skip-flag tests: gh list commands (--jq, --template, --web)
// ========================================================================

#[test]
fn test_gh_pr_list_jq_skips() {
    assert_eq!(
        would_rewrite("gh pr list --jq '.[0]'"),
        None,
        "gh pr list --jq must skip rewrite (user-defined transform)"
    );
}

#[test]
fn test_gh_pr_list_template_skips() {
    assert_eq!(
        would_rewrite("gh pr list --template '{{.title}}'"),
        None,
        "gh pr list --template must skip rewrite (user-defined transform)"
    );
}

#[test]
fn test_gh_pr_list_web_skips() {
    assert_eq!(
        would_rewrite("gh pr list --web"),
        None,
        "gh pr list --web must skip rewrite (opens browser)"
    );
}

#[test]
fn test_gh_issue_list_jq_skips() {
    assert_eq!(
        would_rewrite("gh issue list --jq '.[]'"),
        None,
        "gh issue list --jq must skip rewrite (user-defined transform)"
    );
}

#[test]
fn test_gh_issue_list_template_skips() {
    assert_eq!(
        would_rewrite("gh issue list --template '{{.title}}'"),
        None,
        "gh issue list --template must skip rewrite (user-defined transform)"
    );
}

#[test]
fn test_gh_issue_list_web_skips() {
    assert_eq!(
        would_rewrite("gh issue list --web"),
        None,
        "gh issue list --web must skip rewrite (opens browser, no stdout)"
    );
}

#[test]
fn test_gh_run_list_jq_skips() {
    assert_eq!(
        would_rewrite("gh run list --jq '.[]'"),
        None,
        "gh run list --jq must skip rewrite (user-defined transform)"
    );
}

#[test]
fn test_gh_run_list_template_skips() {
    assert_eq!(
        would_rewrite("gh run list --template '{{.name}}'"),
        None,
        "gh run list --template must skip rewrite (user-defined transform)"
    );
}

/// `gh run list` does NOT support `--web` (verified via `gh run list --help`).
/// Unlike `gh pr list` and `gh issue list` which open a browser tab with `--web`,
/// `gh run list` does not recognise this flag, so it passes through as a regular
/// argument and the rule still fires.
#[test]
fn test_gh_run_list_web_still_rewrites() {
    let result = would_rewrite("gh run list --web");
    assert!(
        result.is_some(),
        "gh run list --web must rewrite: --web is not a valid flag for gh run list"
    );
}

#[test]
fn test_gh_release_list_jq_skips() {
    assert_eq!(
        would_rewrite("gh release list --jq '.[]'"),
        None,
        "gh release list --jq must skip rewrite (user-defined transform)"
    );
}

#[test]
fn test_gh_release_list_template_skips() {
    assert_eq!(
        would_rewrite("gh release list --template '{{.name}}'"),
        None,
        "gh release list --template must skip rewrite (user-defined transform)"
    );
}

/// `gh release list` does NOT support `--web` (verified via `gh release list --help`).
/// Since `--web` is not a recognized flag, it passes through as a regular argument
/// and the rule still fires.
#[test]
fn test_gh_release_list_web_still_rewrites() {
    let result = would_rewrite("gh release list --web");
    assert!(
        result.is_some(),
        "gh release list --web must rewrite: --web is not a valid flag for gh release list"
    );
}

// ========================================================================
// SKIM_PASSTHROUGH env prefix tests
// ========================================================================

#[test]
fn test_passthrough_env_prefix_skips_rewrite() {
    assert_eq!(
        would_rewrite("SKIM_PASSTHROUGH=1 gh pr list"),
        None,
        "SKIM_PASSTHROUGH=1 as env prefix must suppress rewrite"
    );
}

#[test]
fn test_passthrough_env_prefix_true_skips() {
    assert_eq!(
        would_rewrite("SKIM_PASSTHROUGH=true gh pr list"),
        None,
        "SKIM_PASSTHROUGH=true as env prefix must suppress rewrite"
    );
}

#[test]
fn test_passthrough_env_prefix_yes_skips() {
    assert_eq!(
        would_rewrite("SKIM_PASSTHROUGH=yes cargo test"),
        None,
        "SKIM_PASSTHROUGH=yes as env prefix must suppress rewrite"
    );
}

#[test]
fn test_passthrough_env_prefix_zero_still_rewrites() {
    let result = would_rewrite("SKIM_PASSTHROUGH=0 gh pr list");
    assert!(
        result.is_some(),
        "SKIM_PASSTHROUGH=0 must not suppress rewrite (falsy value)"
    );
}

#[test]
fn test_passthrough_env_mixed_with_others_skips() {
    assert_eq!(
        would_rewrite("RUST_LOG=debug SKIM_PASSTHROUGH=1 gh pr list"),
        None,
        "SKIM_PASSTHROUGH=1 among other env vars must still suppress rewrite"
    );
}

#[test]
fn test_non_passthrough_env_still_rewrites() {
    let result = would_rewrite("RUST_LOG=debug gh pr list");
    assert!(
        result.is_some(),
        "Unrelated env var must not suppress rewrite"
    );
}

// ========================================================================
// env command VAR=val guard (issue batch-b)
// ========================================================================

/// `env LANG=C sort file.txt` must NOT be rewritten: the `LANG=C` token is a
/// per-invocation env-var assignment passed to `sort`, not printenv output.
/// Rewriting to `skim env LANG=C sort file.txt` would execute printenv instead
/// of setting LANG and running sort.
#[test]
fn test_env_var_assignment_arg_skips_rewrite() {
    assert_eq!(
        would_rewrite("env LANG=C sort file.txt"),
        None,
        "env LANG=C sort file.txt must not be rewritten — VAR=val arg signals command invocation"
    );
}

/// Multiple VAR=val args also skip rewriting.
#[test]
fn test_env_multiple_var_assignment_args_skip_rewrite() {
    assert_eq!(
        would_rewrite("env LANG=C LC_ALL=C sort file.txt"),
        None,
        "env with multiple VAR=val args must not be rewritten"
    );
}

/// Bare `env` (no args — print all env vars) still rewrites normally.
#[test]
fn test_bare_env_still_rewrites() {
    assert_eq!(
        would_rewrite("env"),
        Some("skim env".to_string()),
        "bare env must still rewrite — no VAR=val arg present"
    );
}

/// `env -i CMD` (with only flag args, no VAR=val) still rewrites.
/// Note: `-i` is in skip_if_flag_prefix, so this should return None via the
/// existing flag-skip path — not the new eq-guard path.
#[test]
fn test_env_minus_i_skips_via_flag_guard() {
    assert_eq!(
        would_rewrite("env -i bash"),
        None,
        "env -i must not be rewritten — -i is in skip_if_flag_prefix"
    );
}

// ========================================================================
// gh output-steering skip tests (Part 1A)
// Tests the hook-path skip-list for short aliases and --json.
// ========================================================================

// --- gh issue view ---

#[test]
fn test_gh_issue_view_q_skips() {
    // Reported repro: gh issue view 93 -q .body must not be rewritten.
    assert_eq!(
        would_rewrite("gh issue view 93 -q .body"),
        None,
        "gh issue view -q must skip rewrite (short alias for --jq)"
    );
}

#[test]
fn test_gh_issue_view_t_skips() {
    assert_eq!(
        would_rewrite("gh issue view 93 -t {{.body}}"),
        None,
        "gh issue view -t must skip rewrite (short alias for --template)"
    );
}

#[test]
fn test_gh_issue_view_w_skips() {
    assert_eq!(
        would_rewrite("gh issue view 93 -w"),
        None,
        "gh issue view -w must skip rewrite (short alias for --web)"
    );
}

#[test]
fn test_gh_issue_view_json_skips() {
    assert_eq!(
        would_rewrite("gh issue view 93 --json number,title,body"),
        None,
        "gh issue view --json must skip rewrite (output-steering flag)"
    );
}

// --- gh pr view ---

#[test]
fn test_gh_pr_view_q_skips() {
    assert_eq!(
        would_rewrite("gh pr view 15 -q .body"),
        None,
        "gh pr view -q must skip rewrite (short alias for --jq)"
    );
}

// --- gh run list ---

#[test]
fn test_gh_run_list_json_skips() {
    assert_eq!(
        would_rewrite("gh run list --json status"),
        None,
        "gh run list --json must skip rewrite (output-steering flag)"
    );
}

// --- gh release list ---

#[test]
fn test_gh_release_list_json_skips() {
    assert_eq!(
        would_rewrite("gh release list --json tagName"),
        None,
        "gh release list --json must skip rewrite (output-steering flag)"
    );
}

// --- gh pr checks ---

#[test]
fn test_gh_pr_checks_json_skips() {
    assert_eq!(
        would_rewrite("gh pr checks 15 --json state"),
        None,
        "gh pr checks --json must skip rewrite (output-steering flag)"
    );
}

// --- gh api ---

#[test]
fn test_gh_api_q_skips() {
    assert_eq!(
        would_rewrite("gh api repos/o/r -q .name"),
        None,
        "gh api -q must skip rewrite (short alias for --jq)"
    );
}

#[test]
fn test_gh_api_t_skips() {
    assert_eq!(
        would_rewrite("gh api repos/o/r -t {{.name}}"),
        None,
        "gh api -t must skip rewrite (short alias for --template)"
    );
}

// --- guards: must still rewrite ---

#[test]
fn test_gh_api_json_still_rewrites() {
    // gh api has no --json flag (responses are always JSON), so --json is NOT
    // in the api skip-list. An invocation like `gh api ... --json x` would be
    // an unrecognized flag that gh itself would reject, but the rewrite engine
    // must still fire (it doesn't validate flag semantics).
    let result = would_rewrite("gh api repos/o/r --json x");
    assert!(
        result.is_some(),
        "gh api --json must still rewrite: --json is not in the api skip-list"
    );
}

#[test]
fn test_gh_run_list_w_workflow_still_rewrites() {
    // On gh run list, -w means --workflow (a filter), NOT --web.
    // gh run list has no --web, so -w is NOT in its skip-list.
    let result = would_rewrite("gh run list -w ci.yml");
    assert!(
        result.is_some(),
        "gh run list -w must still rewrite: -w means --workflow on run list, not --web"
    );
}

#[test]
fn test_gh_release_list_w_still_rewrites() {
    // gh release list has no --web support, so -w is not in its skip-list.
    let result = would_rewrite("gh release list -w");
    assert!(
        result.is_some(),
        "gh release list -w must still rewrite: --web is not supported by release list"
    );
}

// --- compound / pipe: unrewritten gh segment ---

#[test]
fn test_gh_issue_view_q_in_pipe_left_unrewritten() {
    // In a compound `gh issue view 93 -q .body | jq .`, the gh segment must
    // not be rewritten (the -q skip fires), so the full compound returns None
    // (mixed rewrite + unhandled).
    let result = would_rewrite("gh issue view 93 -q .body | jq .");
    assert!(
        result.is_none(),
        "compound with skipped gh segment and unhandled jq must return None"
    );
}

#[test]
fn test_gh_issue_view_json_in_and_chain_unrewritten() {
    // `x && gh issue view 93 --json y`: gh segment skips, x is unhandled.
    let result = would_rewrite("x && gh issue view 93 --json y");
    assert!(
        result.is_none(),
        "compound with skipped gh segment and unhandled x must return None"
    );
}
