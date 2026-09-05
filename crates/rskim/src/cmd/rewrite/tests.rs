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
fn test_classify_pipe_pager_not_rewritten() {
    // #317 (user-approved): pipes are not rewritten when the consumer is a
    // pager or any tool other than bare `cat` (AD-RW-2 exception). Compressing
    // the producer changes what the downstream consumer sees.
    assert_eq!(
        classify_command("git show HEAD | less"),
        CommandClassification::Unhandled,
        "pipe to a pager must NOT be rewritten"
    );
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

/// #317: non-bare-cat pipe expressions pass through untouched — redirects
/// included, because the ORIGINAL command runs unchanged.  The sole exception
/// is the bare `| cat` shape (AD-RW-2); `| head` is not that shape.
#[test]
fn test_classify_compound_pipe_head_is_unhandled() {
    assert_eq!(
        classify_command("cargo test 2>&1 | head"),
        CommandClassification::Unhandled,
        "pipe to head must not be rewritten (only bare `| cat` is the AD-RW-2 exception)"
    );
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
    // In a compound `gh issue view 93 -q .body | jq .`, the full compound
    // returns None because:
    //   (a) the gh segment skips (the -q skip fires — see test_gh_issue_view_q_skips
    //       which isolates that behavior), AND
    //   (b) the `jq` segment is unhandled.
    // Either reason alone would produce None; both apply here.
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

// --- `--` end-of-options separator: steering flags after `--` must not skip ---

#[test]
fn test_gh_api_steering_after_separator_still_rewrites() {
    // A steering flag that appears AFTER `--` must not trigger the skip-list.
    // The engine splits at `--` and checks skip flags only in `before_sep`
    // (the tokens before the separator); post-`--` tokens are passed through
    // verbatim as arguments to the child command.
    //
    // This mirrors `test_user_steers_output_steering_after_separator_ignored`
    // in shared.rs, pinning that both layers agree on `--` semantics (PF-007:
    // assert the discriminating outcome — rewrite fires — not merely is_some).
    let result = would_rewrite("gh api repos/o/r -- --json");
    assert!(
        result.is_some(),
        "gh api repos/o/r -- --json must rewrite: --json after -- is not a steering flag"
    );
    let rewritten = result.unwrap();
    assert!(
        rewritten.starts_with("skim gh api"),
        "rewritten command must start with 'skim gh api', got: {rewritten}"
    );
    assert!(
        rewritten.contains("--json"),
        "rewritten command must preserve --json as a positional arg, got: {rewritten}"
    );
}

// ============================================================================
// require_flags_for_tool — D5 wrapper-surface gate (dispatch_for_wrapper)
// ============================================================================

/// `psql` must declare the require-flags `["-c", "--command"]`.
///
/// Pinned so that removing or renaming the psql `require_flag` entry in the
/// rule table is caught here before it silently breaks the D5 wrapper gate.
#[test]
fn test_require_flags_for_tool_psql_returns_expected_flags() {
    let flags = require_flags_for_tool("psql")
        .expect("psql must have require_flags (interactive-session guard)");
    assert!(
        flags.contains(&"-c"),
        "psql require_flags must include '-c'; got: {flags:?}"
    );
    assert!(
        flags.contains(&"--command"),
        "psql require_flags must include '--command'; got: {flags:?}"
    );
}

/// `mysql` must declare the require-flags `["-e", "--execute"]`.
///
/// Drift-guard: keeps the wrapper D5 gate in sync with the rewrite rule table.
#[test]
fn test_require_flags_for_tool_mysql_returns_expected_flags() {
    let flags = require_flags_for_tool("mysql")
        .expect("mysql must have require_flags (interactive-session guard)");
    assert!(
        flags.contains(&"-e"),
        "mysql require_flags must include '-e'; got: {flags:?}"
    );
    assert!(
        flags.contains(&"--execute"),
        "mysql require_flags must include '--execute'; got: {flags:?}"
    );
}

/// `git` has no `require_flag` entries — `require_flags_for_tool` must return
/// `None` so the D5 gate is a no-op for git and does not block normal dispatch.
#[test]
fn test_require_flags_for_tool_git_returns_none() {
    assert!(
        require_flags_for_tool("git").is_none(),
        "git must have no require_flags (D5 gate must be a no-op for git)"
    );
}

/// Pins the exact return value of [`require_flags_for_tool`] for psql and mysql,
/// verifies [`interactive_tool_for`] for all DB tools, and asserts that the shared
/// [`arg_matches_flag`] predicate accepts each rule-table flag in both short and
/// long (`--flag=value`) forms.
///
/// The first two blocks are the strictest of the three psql/mysql tests: the
/// `contains`-based tests above are strictly weaker, so this equality assertion
/// is the real regression pin.
///
/// The `arg_matches_flag` block is the cross-source drift guard: both the wrapper
/// gate and the rewrite engine route through `arg_matches_flag`, so verifying it
/// here against the flags returned by `require_flags_for_tool` confirms that both
/// surfaces agree for the same tool and argument — and that the shared predicate
/// handles both short-flag and `--flag=value` forms correctly.
#[test]
fn test_require_flags_exact_values_and_arg_matches_flag_cross_source_pin() {
    // psql: rule table declares &["-c", "--command"]
    let psql = require_flags_for_tool("psql").expect("psql must have require_flags");
    let mut psql_sorted = psql.clone();
    psql_sorted.sort_unstable();
    let mut expected_psql = vec!["-c", "--command"];
    expected_psql.sort_unstable();
    assert_eq!(
        psql_sorted, expected_psql,
        "psql require_flags must exactly match the rule-table declaration"
    );
    // psql is also interactive (has a require_flag set)
    assert!(
        interactive_tool_for("psql"),
        "interactive_tool_for(\"psql\") must be true — D5 wrapper gate must guard it"
    );

    // mysql: rule table declares &["-e", "--execute"]
    let mysql = require_flags_for_tool("mysql").expect("mysql must have require_flags");
    let mut mysql_sorted = mysql.clone();
    mysql_sorted.sort_unstable();
    let mut expected_mysql = vec!["-e", "--execute"];
    expected_mysql.sort_unstable();
    assert_eq!(
        mysql_sorted, expected_mysql,
        "mysql require_flags must exactly match the rule-table declaration"
    );
    assert!(
        interactive_tool_for("mysql"),
        "interactive_tool_for(\"mysql\") must be true — D5 wrapper gate must guard it"
    );

    // sqlite3: no require_flags (None) — always interactive (any invocation
    // can open an interactive REPL); `interactive_tool_for` must be true.
    assert!(
        require_flags_for_tool("sqlite3").is_none(),
        "sqlite3 must have no require_flags (any invocation can be interactive)"
    );
    assert!(
        interactive_tool_for("sqlite3"),
        "interactive_tool_for(\"sqlite3\") must be true — no safe non-interactive flag exists"
    );

    // Non-interactive tools: git and cargo must NOT be in the interactive set.
    assert!(
        !interactive_tool_for("git"),
        "interactive_tool_for(\"git\") must be false — git is never interactive"
    );
    assert!(
        !interactive_tool_for("cargo"),
        "interactive_tool_for(\"cargo\") must be false — cargo is never interactive"
    );

    // Cross-source drift guard: `arg_matches_flag` — the shared predicate used
    // by both the wrapper gate and the rewrite engine — must recognise each flag
    // returned by `require_flags_for_tool` in both its short form and its
    // `--flag=value` long form.  This pins the agreement between surfaces: a
    // change that breaks `arg_matches_flag` for a rule-table flag would cause
    // both the D5 wrapper gate and the rewrite engine to disagree, and this
    // assertion catches it before the mismatch reaches a wrapper invocation.
    for flag in &psql_sorted {
        assert!(
            arg_matches_flag(flag, flag),
            "arg_matches_flag must accept psql's own flag '{flag}' as a self-match"
        );
        if flag.starts_with("--") {
            let long_form = format!("{flag}=SELECT 1");
            assert!(
                arg_matches_flag(&long_form, flag),
                "arg_matches_flag must accept psql's long flag '{flag}' with '=value' suffix"
            );
        }
    }
    for flag in &mysql_sorted {
        assert!(
            arg_matches_flag(flag, flag),
            "arg_matches_flag must accept mysql's own flag '{flag}' as a self-match"
        );
        if flag.starts_with("--") {
            let long_form = format!("{flag}=SELECT 1");
            assert!(
                arg_matches_flag(&long_form, flag),
                "arg_matches_flag must accept mysql's long flag '{flag}' with '=value' suffix"
            );
        }
    }
}

// ========================================================================
// AD-RW-2 reversal: classify_command agrees with try_rewrite_compound
// for the `| cat` pipeline shape (RED before fix, GREEN after).
// ========================================================================

/// Helper: call the same engine path the hook uses, returning Some(rewritten)
/// or None.  Mirrors the hook's `run_hook_mode` compound dispatch verbatim so
/// the drift-guard test drives a code path independent of `classify_command`.
fn engine_rewrite(shape: &str) -> Option<String> {
    use super::compound::{split_compound, try_rewrite_compound};
    use super::engine::try_rewrite;
    use super::types::CompoundSplitResult;

    let has_operator =
        shape.contains("&&") || shape.contains("||") || shape.contains(';') || shape.contains('|');
    if !has_operator {
        let tokens: Vec<&str> = shape.split_whitespace().collect();
        return try_rewrite(&tokens).map(|r| r.tokens.join(" "));
    }
    match split_compound(shape) {
        CompoundSplitResult::Bail => None,
        CompoundSplitResult::Simple(simple_tokens) => {
            let refs: Vec<&str> = simple_tokens.iter().map(|s| s.as_str()).collect();
            try_rewrite(&refs).map(|r| r.tokens.join(" "))
        }
        CompoundSplitResult::Compound(segments) => {
            try_rewrite_compound(&segments).map(|r| r.tokens.join(" "))
        }
    }
}

/// `git log -n 3 | cat` must classify as `Rewritten` after AD-RW-2 reversal.
///
/// Current (before fix): `classify_command` returns `Unhandled` because
/// `classify_compound_pipe` did not consult `is_bare_cat_pipeline`.
/// Engine side (`skim rewrite "git log -n 3 | cat"`): exit 0, stdout =
/// `skim git log -n 3 | cat`.
/// Classify side (reasoned from code): after fix, `classify_compound_pipe` calls
/// `is_bare_cat_pipeline` → true → `classify_segment_fine("git log -n 3")` →
/// `Rewritten(["skim", "git", "log", "-n", "3"])` → `Rewritten("skim git log -n 3 | cat")`.
///
/// RED before fix; GREEN after.
#[test]
fn test_classify_pipe_bare_cat_git_log_rewritten() {
    assert!(
        matches!(
            classify_command("git log -n 3"),
            CommandClassification::Rewritten(_)
        ),
        "sanity: plain `git log -n 3` must be Rewritten"
    );
    assert_eq!(
        classify_command("git log -n 3 | cat"),
        CommandClassification::Rewritten("skim git log -n 3 | cat".to_string()),
        "`git log -n 3 | cat` must be Rewritten with source rewritten and `| cat` preserved"
    );
}

/// `cat README.md | cat` must classify as `Rewritten` after AD-RW-2 reversal.
///
/// Current (before fix): `classify_command` returns `Unhandled`.
/// Engine side (`skim rewrite "cat README.md | cat"`): exit 0, stdout =
/// `SKIM_REWRITTEN_FROM=cat skim README.md --mode=pseudo | cat`.
/// Classify side (reasoned from code): `classify_segment_fine("cat README.md")` →
/// `Rewritten(["SKIM_REWRITTEN_FROM=cat", "skim", "README.md", "--mode=pseudo"])` →
/// `Rewritten("SKIM_REWRITTEN_FROM=cat skim README.md --mode=pseudo | cat")`.
///
/// RED before fix; GREEN after.
#[test]
fn test_classify_pipe_bare_cat_file_read_rewritten() {
    assert!(
        matches!(
            classify_command("cat README.md"),
            CommandClassification::Rewritten(_)
        ),
        "sanity: plain `cat README.md` must be Rewritten"
    );
    assert_eq!(
        classify_command("cat README.md | cat"),
        CommandClassification::Rewritten(
            "SKIM_REWRITTEN_FROM=cat skim README.md --mode=pseudo | cat".to_string()
        ),
        "`cat README.md | cat` must be Rewritten with source rewritten as standalone form"
    );
}

/// `grep -rn foo src | cat` must classify as `Rewritten` after AD-RW-2 reversal.
///
/// Current (before fix): `classify_command` returns `Unhandled`.
/// Engine side (`skim rewrite "grep -rn foo src | cat"`): exit 0, stdout =
/// `skim grep -rn foo src | cat`.
/// Classify side (reasoned from code): `classify_segment_fine("grep -rn foo src")` →
/// `Rewritten(["skim", "grep", "-rn", "foo", "src"])` →
/// `Rewritten("skim grep -rn foo src | cat")`.
///
/// RED before fix; GREEN after.
#[test]
fn test_classify_pipe_bare_cat_grep_rewritten() {
    assert!(
        matches!(
            classify_command("grep -rn foo src"),
            CommandClassification::Rewritten(_)
        ),
        "sanity: plain `grep -rn foo src` must be Rewritten"
    );
    assert_eq!(
        classify_command("grep -rn foo src | cat"),
        CommandClassification::Rewritten("skim grep -rn foo src | cat".to_string()),
        "`grep -rn foo src | cat` must be Rewritten with source rewritten as standalone form"
    );
}

// ========================================================================
// Controls: shapes that must remain Unhandled (before AND after fix).
// ========================================================================

/// `git log | cat > out.txt` — stdout redirect on cat segment → not bare cat.
///
/// Current: `Unhandled`. Must stay `Unhandled` after fix.
/// Engine side: exit 1 (not rewritten — Rule S arms `command_needs_exact_bytes`).
#[test]
fn test_classify_pipe_cat_redirect_stays_unhandled() {
    assert_eq!(
        classify_command("git log | cat > out.txt"),
        CommandClassification::Unhandled,
        "`git log | cat > out.txt` must stay Unhandled (stdout redirect on cat)"
    );
}

/// `git log | cat | tee f` — three pipeline stages → not the 2-stage shape.
///
/// Current: `Unhandled`. Must stay `Unhandled` after fix.
/// Engine side: exit 1 (not rewritten — Rule T arms `command_needs_exact_bytes`).
#[test]
fn test_classify_pipe_cat_tee_stays_unhandled() {
    assert_eq!(
        classify_command("git log | cat | tee f"),
        CommandClassification::Unhandled,
        "`git log | cat | tee f` must stay Unhandled (three stages, not bare `| cat`)"
    );
}

/// `git log | cat -n` — cat has an argument → not bare cat.
///
/// Current: `Unhandled`. Must stay `Unhandled` after fix.
/// Engine side: exit 1 (not rewritten — consumer tokens are ["cat", "-n"]).
#[test]
fn test_classify_pipe_cat_n_stays_unhandled() {
    assert_eq!(
        classify_command("git log | cat -n"),
        CommandClassification::Unhandled,
        "`git log | cat -n` must stay Unhandled (cat has arguments)"
    );
}

/// `git log | less` — consumer is `less`, not `cat`.
///
/// Current: `Unhandled`. Must stay `Unhandled` after fix.
/// Engine side: exit 1 (not rewritten).
#[test]
fn test_classify_pipe_less_stays_unhandled() {
    assert_eq!(
        classify_command("git log | less"),
        CommandClassification::Unhandled,
        "`git log | less` must stay Unhandled (pager, not bare cat)"
    );
}

/// `ls | head` — consumer is `head`, not `cat`.
///
/// Current: `Unhandled`. Must stay `Unhandled` after fix.
/// Engine side: exit 1 (not rewritten).
#[test]
fn test_classify_pipe_head_stays_unhandled() {
    assert_eq!(
        classify_command("ls | head"),
        CommandClassification::Unhandled,
        "`ls | head` must stay Unhandled (head is not bare cat)"
    );
}

// ========================================================================
// Drift guard: classify_command agrees with the hook's engine path
// (RED for the three `| cat` shapes before fix; GREEN after).
// ========================================================================

/// For each shape, `classify_command` returns `Rewritten` IF AND ONLY IF the
/// hook's engine path (the same `try_rewrite` / `try_rewrite_compound` the
/// `run_hook_mode` dispatch calls) returns `Some`.
///
/// This test is the single place that catches a future drift between the two
/// surfaces.  Adding a new rewritable pipe shape to the engine without updating
/// `classify_compound_pipe` (or vice-versa) will break this test first.
///
/// Current engine verdicts (observed via `skim rewrite "<shape>"`):
/// - `git log -n 3 | cat` → exit 0 (rewrites)          classify: currently Unhandled (bug)
/// - `cat README.md | cat` → exit 0 (rewrites)          classify: currently Unhandled (bug)
/// - `grep -rn foo src | cat` → exit 0 (rewrites)       classify: currently Unhandled (bug)
/// - `git log | cat > out.txt` → exit 1 (no rewrite)    classify: Unhandled ✓
/// - `git log | cat | tee f` → exit 1 (no rewrite)      classify: Unhandled ✓
/// - `git log | cat -n` → exit 1 (no rewrite)           classify: Unhandled ✓
/// - `git log | less` → exit 1 (no rewrite)             classify: Unhandled ✓
/// - `ls | head` → exit 1 (no rewrite)                  classify: Unhandled ✓
/// - `git status` → exit 0 (skim git status)             classify: Rewritten ✓
/// - `cat file.rs` → exit 0 (skim ... --mode=pseudo)    classify: Rewritten ✓
///
/// RED for the first three shapes before fix; GREEN for all ten after.
#[test]
fn test_classify_vs_engine_drift_guard() {
    // (shape, expected: classify_command returns Rewritten iff engine returns Some)
    let shapes = [
        "git log -n 3 | cat",
        "cat README.md | cat",
        "grep -rn foo src | cat",
        "git log | cat > out.txt",
        "git log | cat | tee f",
        "git log | cat -n",
        "git log | less",
        "ls | head",
        "git status",
        "cat file.rs",
    ];

    for shape in shapes {
        let classify_rewritten =
            matches!(classify_command(shape), CommandClassification::Rewritten(_));
        let engine_rewrites = engine_rewrite(shape).is_some();
        assert_eq!(
            classify_rewritten, engine_rewrites,
            "classify_command / engine disagree on '{shape}': \
             classify={classify_rewritten}, engine={engine_rewrites}"
        );
    }
}
