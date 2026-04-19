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
                s.contains("skim test cargo"),
                "Expected skim test cargo in output, got: {s}"
            );
            assert!(
                s.contains("skim build clippy"),
                "Expected skim build clippy in output, got: {s}"
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
fn test_would_rewrite_gh_pr_list_json_rewrites() {
    let result = would_rewrite("gh pr list --json number");
    assert!(result.is_some(), "gh pr list --json should now rewrite");
    let rewritten = result.unwrap();
    assert!(
        rewritten.contains("skim infra gh pr list"),
        "Expected 'skim infra gh pr list' in output, got: {rewritten}"
    );
}

#[test]
fn test_would_rewrite_jest_rewrites() {
    assert_eq!(
        would_rewrite("jest src/"),
        Some("skim test jest src/".to_string()),
        "jest should rewrite to skim test jest"
    );
}

#[test]
fn test_would_rewrite_npx_jest_rewrites() {
    assert_eq!(
        would_rewrite("npx jest src/"),
        Some("skim test jest src/".to_string()),
        "npx jest should rewrite to skim test jest"
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
