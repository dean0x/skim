//! Compound command splitting and rewriting (#45).
//!
//! Handles `&&`, `||`, `;`, `|` operators using a character-by-character
//! state machine that tracks quotes and paren depth.
//!
//! # Redirect stripping (AD-RW-2)
//!
//! Each segment may contain shell redirects (e.g., `2>&1`, `>/dev/null`).
//! These are stripped before passing tokens to the rule engine so that
//! `foo 2>&1` matches the same rule as `foo`.  Redirects are recorded and
//! spliced back into the emitted token stream at their original positions,
//! preserving shell semantics.
//!
//! SEE: AD-RW-2 — catch-all ls/grep + pipe exclusion design note.

use super::engine::{matches_catch_all_rule, try_rewrite};
use super::types::{
    CommandSegment, CompoundOp, CompoundSplitResult, QuoteState, RewriteCategory, RewriteResult,
};

// ---- Redirect stripping (AD-RW-2) ----

/// Strip shell redirect tokens from a segment's token list.
///
/// Recognized redirect forms (stripped):
/// - `2>&1`, `>&2`, `1>&2`, `>&1` — stderr/stdout merge
/// - `>/dev/null`, `2>/dev/null`, `&>/dev/null` — discard redirects
/// - Whitespace-separated two-token form: `["2>", "/dev/null"]`
///
/// NOT recognized (left in token list):
/// - `>file`, `2>file` — file redirects with arbitrary names (ambiguous)
/// - `| tee file` — pipe-based redirection
/// - heredocs (`<<`) — handled by bail logic
/// - Pre-command redirects (`2>&1 cmd`) — non-standard, out of scope
///
/// Returns the original `(index, token)` pairs that were stripped so they can
/// be re-spliced at emission time.  The `tokens` vec is mutated in place.
///
/// # DESIGN NOTE (AD-RW-2)
///
/// Only appended/trailing redirects are handled.  Pre-command redirects
/// (`2>&1 foo`) are non-standard and out of scope per the plan.  The redirect
/// forms listed above cover the most common CI/agent patterns.
pub(super) fn strip_segment_redirects(tokens: &mut Vec<String>) -> Vec<(usize, String)> {
    let mut stripped: Vec<(usize, String)> = Vec::new();

    // Two-pass: first collect indices to remove, then drain them.
    let mut remove_indices: Vec<usize> = Vec::new();

    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].as_str();

        // Single-token redirect forms.
        if is_single_redirect(tok) {
            remove_indices.push(i);
            i += 1;
            continue;
        }

        // Whitespace-separated two-token form: `2>` followed by `/dev/null`.
        if tok == "2>" && i + 1 < tokens.len() && tokens[i + 1] == "/dev/null" {
            remove_indices.push(i);
            remove_indices.push(i + 1);
            i += 2;
            continue;
        }

        i += 1;
    }

    // Drain in reverse order so indices stay valid.
    for &idx in remove_indices.iter().rev() {
        let tok = tokens.remove(idx);
        stripped.push((idx, tok));
    }

    // Re-sort by original index (ascending) since we iterated in reverse.
    stripped.sort_by_key(|(idx, _)| *idx);

    stripped
}

/// Returns `true` if `tok` is a single-token shell redirect that should be
/// stripped before rule matching.
fn is_single_redirect(tok: &str) -> bool {
    matches!(
        tok,
        "2>&1" | ">&2" | "1>&2" | ">&1" | ">/dev/null" | "2>/dev/null" | "&>/dev/null"
    )
}

/// Splice stripped redirects back into `tokens`.
///
/// Redirects are appended at the END of the token list.  Shell semantics for
/// trailing redirects are identical to mid-command placement (POSIX §2.7), and
/// appending avoids position-mismatch after the rule engine has rewritten the
/// token list (the original indices no longer map into the rewritten list).
///
/// Used at emission time to reconstruct the shell-semantics-equivalent command.
fn splice_redirects_back(tokens: &mut Vec<String>, redirects: &[(usize, String)]) {
    for (_idx, tok) in redirects {
        tokens.push(tok.clone());
    }
}

// ---- State machine helpers ----

/// Check whether position `i` is the start of a bail-triggering construct.
///
/// Bail triggers (evaluated only in `QuoteState::None`):
/// - backtick `` ` ``
/// - heredoc `<<`
/// - subshell `$(` or variable expansion `${`
///
/// Returns `true` when the caller should immediately return `Bail`.
fn check_bail(ch: char, chars: &[char], i: usize, len: usize) -> bool {
    if ch == '`' {
        return true;
    }
    if ch == '<' && i + 1 < len && chars[i + 1] == '<' {
        return true;
    }
    if ch == '$' && i + 1 < len && (chars[i + 1] == '(' || chars[i + 1] == '{') {
        return true;
    }
    false
}

/// Scan for a compound operator starting at position `i` (paren depth 0, unquoted).
///
/// Returns `Some((op, advance))` where `advance` is the number of char positions
/// to move past the operator, or `None` if no operator starts here.
///
/// The `&&` check includes a redirect guard: `>&1` patterns must not be mistaken
/// for `&&`.
fn scan_operator(chars: &[char], i: usize, len: usize) -> Option<(CompoundOp, usize)> {
    let ch = chars[i];

    if ch == '&' && i + 1 < len && chars[i + 1] == '&' {
        // Guard against >&N redirect patterns (e.g., 2>&1).
        if i > 0 && chars[i - 1] == '>' {
            return None;
        }
        return Some((CompoundOp::And, 2));
    }

    if ch == '|' && i + 1 < len && chars[i + 1] == '|' {
        return Some((CompoundOp::Or, 2));
    }

    // Single | must be checked after || to avoid misidentifying the first char.
    if ch == '|' {
        return Some((CompoundOp::Pipe, 1));
    }

    if ch == ';' {
        return Some((CompoundOp::Semicolon, 1));
    }

    None
}

/// Slice the current segment text from `input`, tokenise it, and push a
/// `CommandSegment` onto `segments`.  Does nothing when the slice is
/// all-whitespace (empty token list).
fn push_segment(
    input: &str,
    byte_offsets: &[usize],
    seg_end_char_idx: usize,
    current_start: usize,
    segments: &mut Vec<CommandSegment>,
    op: Option<CompoundOp>,
) {
    let seg_text = &input[current_start..byte_offsets[seg_end_char_idx]];
    let raw_tokens: Vec<String> = seg_text.split_whitespace().map(String::from).collect();
    if !raw_tokens.is_empty() {
        let mut tokens = raw_tokens;
        let stripped_redirects = strip_segment_redirects(&mut tokens);
        segments.push(CommandSegment {
            tokens,
            trailing_operator: op,
            stripped_redirects,
        });
    }
}

// ---- Public entry point ----

/// Split a shell command string at compound operators (`&&`, `||`, `;`, `|`).
///
/// Uses a character-by-character state machine tracking quotes and paren depth.
/// Only splits at operators when outside quotes and at paren depth 0.
///
/// Bail conditions (returns `Bail`): heredocs `<<`, subshells `$(`, backticks,
/// unmatched quotes at end of input.
pub(super) fn split_compound(input: &str) -> CompoundSplitResult {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();

    let mut segments: Vec<CommandSegment> = Vec::new();
    let mut current_start: usize = 0; // byte offset into input for current segment
    let mut quote_state = QuoteState::None;
    let mut paren_depth: usize = 0;
    let mut found_operator = false;
    let mut i: usize = 0;

    // Precompute byte offsets for each char index.
    let byte_offsets: Vec<usize> = {
        let mut offsets = Vec::with_capacity(len + 1);
        let mut bo = 0;
        for ch in &chars {
            offsets.push(bo);
            bo += ch.len_utf8();
        }
        offsets.push(bo); // sentinel for end-of-string
        offsets
    };

    while i < len {
        let ch = chars[i];

        // Handle quote state transitions (consume char and continue).
        match quote_state {
            QuoteState::SingleQuote => {
                if ch == '\'' {
                    quote_state = QuoteState::None;
                }
                i += 1;
                continue;
            }
            QuoteState::DoubleQuote => {
                if ch == '\\' && i + 1 < len {
                    i += 2; // skip escaped char (e.g., \")
                    continue;
                }
                if ch == '"' {
                    quote_state = QuoteState::None;
                }
                i += 1;
                continue;
            }
            QuoteState::None => {}
        }

        // Bail on heredocs, subshells, and backticks.
        if check_bail(ch, &chars, i, len) {
            return CompoundSplitResult::Bail;
        }

        // Enter quote mode.
        if ch == '\'' {
            quote_state = QuoteState::SingleQuote;
            i += 1;
            continue;
        }
        if ch == '"' {
            quote_state = QuoteState::DoubleQuote;
            i += 1;
            continue;
        }

        // Track parenthesis depth.
        if ch == '(' {
            paren_depth += 1;
            i += 1;
            continue;
        }
        if ch == ')' {
            paren_depth = paren_depth.saturating_sub(1);
            i += 1;
            continue;
        }

        // Only recognise operators at the top-level (paren depth 0).
        if paren_depth == 0 {
            if let Some((op, advance)) = scan_operator(&chars, i, len) {
                push_segment(
                    input,
                    &byte_offsets,
                    i,
                    current_start,
                    &mut segments,
                    Some(op),
                );
                found_operator = true;
                i += advance;
                current_start = byte_offsets[i.min(len)];
                continue;
            }
        }

        i += 1;
    }

    // Bail on unmatched quotes.
    if quote_state != QuoteState::None {
        return CompoundSplitResult::Bail;
    }

    if !found_operator {
        // No compound operators found — return as simple.
        let tokens: Vec<String> = input.split_whitespace().map(String::from).collect();
        return CompoundSplitResult::Simple(tokens);
    }

    // Push the final segment (after the last operator).
    let seg_text = &input[current_start..];
    let raw_tokens: Vec<String> = seg_text.split_whitespace().map(String::from).collect();
    if !raw_tokens.is_empty() {
        let mut tokens = raw_tokens;
        let stripped_redirects = strip_segment_redirects(&mut tokens);
        segments.push(CommandSegment {
            tokens,
            trailing_operator: None,
            stripped_redirects,
        });
    }

    CompoundSplitResult::Compound(segments)
}

// PIPE_EXCLUDED_SOURCES removed (AD-RW-2).
// The pipe-source exclusion is now handled via the `is_catch_all` flag on
// `RewriteRule` (types.rs).  `engine::matches_catch_all_rule` replaces the
// hard-coded list, so adding a new catch-all only requires a single edit in
// `rules.rs`.  See `try_rewrite_compound_pipe` and `mod.rs::classify_compound_pipe`.

/// Attempt to rewrite a compound command expression.
///
/// For `&&`/`||`/`;`: tries `try_rewrite()` on each segment independently.
/// For `|`: only rewrites the first segment (the output producer).
/// Returns `Some(RewriteResult)` if ANY segment was rewritten, `None` otherwise.
pub(super) fn try_rewrite_compound(segments: &[CommandSegment]) -> Option<RewriteResult> {
    if segments.is_empty() {
        return None;
    }

    // Check if this is a pipe expression (any segment has a Pipe operator)
    let has_pipe = segments
        .iter()
        .any(|s| s.trailing_operator == Some(CompoundOp::Pipe));

    if has_pipe {
        return try_rewrite_compound_pipe(segments);
    }

    // For &&/||/; — try rewriting each segment independently
    let mut any_rewritten = false;
    let mut first_category: Option<RewriteCategory> = None;
    let mut parts: Vec<String> = Vec::new();

    for seg in segments {
        let token_refs: Vec<&str> = seg.tokens.iter().map(|s| s.as_str()).collect();
        let rewrite = try_rewrite(&token_refs);

        let segment_text = match &rewrite {
            Some(r) => {
                any_rewritten = true;
                if first_category.is_none() {
                    first_category = Some(r.category);
                }
                // Splice redirects back at their original positions.
                let mut rewritten_tokens = r.tokens.clone();
                splice_redirects_back(&mut rewritten_tokens, &seg.stripped_redirects);
                rewritten_tokens.join(" ")
            }
            None => {
                // Not rewritten — restore full original form (tokens + redirects).
                let mut original_tokens = seg.tokens.clone();
                splice_redirects_back(&mut original_tokens, &seg.stripped_redirects);
                original_tokens.join(" ")
            }
        };

        parts.push(segment_text);

        // Add the operator between segments (not after the last one)
        if let Some(op) = seg.trailing_operator {
            parts.push(op.as_str().to_string());
        }
    }

    if !any_rewritten {
        return None;
    }

    Some(RewriteResult {
        tokens: parts,
        category: first_category.unwrap_or(RewriteCategory::Build),
    })
}

/// Rewrite a pipe expression. Only the first segment (output producer) is rewritten.
fn try_rewrite_compound_pipe(segments: &[CommandSegment]) -> Option<RewriteResult> {
    if segments.is_empty() {
        return None;
    }

    let first = &segments[0];

    let token_refs: Vec<&str> = first.tokens.iter().map(|s| s.as_str()).collect();

    // Do not rewrite catch-all rules on the pipe-source side (e.g. `ls | head`).
    // The `is_catch_all` flag on the matching rule drives this check instead of
    // the removed PIPE_EXCLUDED_SOURCES constant.  SEE: AD-RW-2.
    if matches_catch_all_rule(&token_refs) {
        return None;
    }

    let rewrite = try_rewrite(&token_refs)?;

    // Reconstruct: rewritten first segment | rest unchanged
    // Splice redirects back at their original positions for the first segment.
    let mut first_tokens = rewrite.tokens.clone();
    splice_redirects_back(&mut first_tokens, &first.stripped_redirects);

    let mut parts: Vec<String> = Vec::new();
    parts.push(first_tokens.join(" "));

    for (idx, seg) in segments.iter().enumerate() {
        if idx == 0 {
            // Already handled the first segment; add its operator
            if let Some(op) = seg.trailing_operator {
                parts.push(op.as_str().to_string());
            }
            continue;
        }
        // Restore redirects for non-rewritten segments.
        let mut seg_tokens = seg.tokens.clone();
        splice_redirects_back(&mut seg_tokens, &seg.stripped_redirects);
        parts.push(seg_tokens.join(" "));
        if let Some(op) = seg.trailing_operator {
            parts.push(op.as_str().to_string());
        }
    }

    Some(RewriteResult {
        tokens: parts,
        category: rewrite.category,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // split_compound state machine (#45)
    // ========================================================================

    #[test]
    fn test_split_compound_simple() {
        match split_compound("cargo test") {
            CompoundSplitResult::Simple(tokens) => {
                assert_eq!(tokens, vec!["cargo", "test"]);
            }
            other => panic!("Expected Simple, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_and_and() {
        match split_compound("cargo test && cargo build") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].tokens, vec!["cargo", "build"]);
                assert_eq!(segments[1].trailing_operator, None);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_or_or() {
        match split_compound("cargo test || echo fail") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::Or));
                assert_eq!(segments[1].tokens, vec!["echo", "fail"]);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_semicolon() {
        match split_compound("cargo test ; echo done") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::Semicolon));
                assert_eq!(segments[1].tokens, vec!["echo", "done"]);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_pipe() {
        match split_compound("cargo test | head") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::Pipe));
                assert_eq!(segments[1].tokens, vec!["head"]);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_mixed_operators() {
        match split_compound("cargo test && cargo build ; echo done") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 3);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].trailing_operator, Some(CompoundOp::Semicolon));
                assert_eq!(segments[2].trailing_operator, None);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_double_quoted_operators_not_split() {
        match split_compound(r#"echo "a && b" test"#) {
            CompoundSplitResult::Simple(tokens) => {
                assert!(tokens.contains(&r#""a"#.to_string()));
            }
            CompoundSplitResult::Compound(_) => panic!("Should not split inside double quotes"),
            CompoundSplitResult::Bail => panic!("Should not bail"),
        }
    }

    #[test]
    fn test_split_compound_single_quoted_operators_not_split() {
        match split_compound("echo 'a && b' test") {
            CompoundSplitResult::Simple(tokens) => {
                assert!(tokens.contains(&"'a".to_string()));
            }
            CompoundSplitResult::Compound(_) => panic!("Should not split inside single quotes"),
            CompoundSplitResult::Bail => panic!("Should not bail"),
        }
    }

    #[test]
    fn test_split_compound_heredoc_bails() {
        match split_compound("cat <<EOF && echo done") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for heredoc, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_subshell_bails() {
        match split_compound("$(command) && cargo test") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for subshell, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_backtick_bails() {
        match split_compound("`command` && cargo test") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for backtick, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_unmatched_quote_bails() {
        match split_compound("echo \"unclosed && cargo test") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for unmatched quote, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_redirect_2_ampersand_1_not_separator() {
        match split_compound("cargo test 2>&1") {
            CompoundSplitResult::Simple(tokens) => {
                assert_eq!(tokens, vec!["cargo", "test", "2>&1"]);
            }
            other => panic!("Expected Simple (redirect not separator), got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_and_and_no_spaces() {
        match split_compound("cargo test&&cargo build") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].tokens, vec!["cargo", "build"]);
                assert_eq!(segments[1].trailing_operator, None);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_escaped_double_quotes_not_split() {
        // The escaped quotes inside the double-quoted string don't end the string
        match split_compound(r#"echo "say \"hello\"" && cargo test"#) {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_variable_expansion_bails() {
        match split_compound("${CARGO:-cargo} test && echo done") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for variable expansion, got {:?}", other),
        }
    }

    // ========================================================================
    // Compound rewrite logic (#45)
    // ========================================================================

    #[test]
    fn test_compound_both_rewritten() {
        let segments = vec![
            CommandSegment {
                tokens: vec!["cargo".into(), "test".into()],
                trailing_operator: Some(CompoundOp::And),
                stripped_redirects: vec![],
            },
            CommandSegment {
                tokens: vec!["cargo".into(), "build".into()],
                trailing_operator: None,
                stripped_redirects: vec![],
            },
        ];
        let result = try_rewrite_compound(&segments).unwrap();
        let joined = result.tokens.join(" ");
        assert!(joined.contains("skim test cargo"));
        assert!(joined.contains("&&"));
        assert!(joined.contains("skim build cargo"));
    }

    #[test]
    fn test_compound_one_rewritten() {
        let segments = vec![
            CommandSegment {
                tokens: vec!["cargo".into(), "test".into()],
                trailing_operator: Some(CompoundOp::And),
                stripped_redirects: vec![],
            },
            CommandSegment {
                tokens: vec!["echo".into(), "done".into()],
                trailing_operator: None,
                stripped_redirects: vec![],
            },
        ];
        let result = try_rewrite_compound(&segments).unwrap();
        let joined = result.tokens.join(" ");
        assert!(joined.contains("skim test cargo"));
        assert!(joined.contains("echo done"));
    }

    #[test]
    fn test_compound_none_rewritten_returns_none() {
        let segments = vec![
            CommandSegment {
                tokens: vec!["echo".into(), "hello".into()],
                trailing_operator: Some(CompoundOp::And),
                stripped_redirects: vec![],
            },
            CommandSegment {
                tokens: vec!["echo".into(), "world".into()],
                trailing_operator: None,
                stripped_redirects: vec![],
            },
        ];
        assert!(try_rewrite_compound(&segments).is_none());
    }

    #[test]
    fn test_compound_empty_returns_none() {
        assert!(try_rewrite_compound(&[]).is_none());
    }

    /// `ls | head` must NOT be rewritten — `ls` is a catch-all rule and must not
    /// fire on the pipe-source side (AD-RW-2).
    #[test]
    fn test_pipe_catch_all_ls_not_rewritten() {
        match split_compound("ls | head") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                assert!(
                    result.is_none(),
                    "ls | head must not be rewritten (catch-all pipe-source exclusion): {result:?}"
                );
            }
            other => panic!("Expected Compound for ls | head, got {:?}", other),
        }
    }

    /// `grep foo file | head` must NOT be rewritten (catch-all pipe-source exclusion).
    #[test]
    fn test_pipe_catch_all_grep_not_rewritten() {
        match split_compound("grep foo file | head") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                assert!(
                    result.is_none(),
                    "grep | head must not be rewritten (catch-all pipe-source exclusion): {result:?}"
                );
            }
            other => panic!(
                "Expected Compound for grep foo file | head, got {:?}",
                other
            ),
        }
    }

    /// `cargo test 2>&1 | head` must be rewritten and preserve the redirect.
    #[test]
    fn test_compound_pipe_rewrite_preserves_redirect() {
        match split_compound("cargo test 2>&1 | head") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                assert!(result.is_some(), "cargo test 2>&1 | head must be rewritten");
                let joined = result.unwrap().tokens.join(" ");
                assert!(
                    joined.contains("2>&1"),
                    "Redirect must be preserved in rewritten pipe: {joined}"
                );
                assert!(
                    joined.contains("| head"),
                    "Pipe consumer must be preserved: {joined}"
                );
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    /// `cargo test 2>&1 && cargo build` must be rewritten and preserve the redirect.
    #[test]
    fn test_compound_and_rewrite_preserves_redirect() {
        match split_compound("cargo test 2>&1 && cargo build") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                assert!(
                    result.is_some(),
                    "cargo test 2>&1 && cargo build must be rewritten"
                );
                let joined = result.unwrap().tokens.join(" ");
                assert!(
                    joined.contains("2>&1"),
                    "Redirect must be preserved in rewritten compound: {joined}"
                );
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }
}
