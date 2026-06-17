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

use super::engine::try_rewrite;
use super::types::{
    CommandSegment, CompoundOp, CompoundSplitResult, QuoteState, RewriteCategory, RewriteResult,
};

// ---- Round-trip safety (#317) ----

/// Return `true` when `cmd` (after stripping trailing whitespace) contains an
/// **interior** newline that would make the command unsafe to rewrite.
///
/// Trailing newlines are benign — agent PreToolUse hooks often add a trailing
/// `\n` to the command string, which does not affect tokenization.  Interior
/// newlines (e.g., a multi-line commit message) indicate multi-line commands
/// that `split_whitespace` would flatten, corrupting the byte sequence.
///
/// Fix C (fix/rewrite-hook-falseneg): the hook layer previously called
/// `rewrite_would_corrupt` which checks `cmd.contains('\n')`, bailing even on
/// commands with only a trailing newline — commands from agent hooks that were
/// otherwise safely rewritable.  This function is the hook-layer guard: it
/// trims trailing whitespace first so trailing newlines pass through, then
/// delegates the full corruption check to [`rewrite_would_corrupt`].
///
/// Must be called with the raw hook-input command string.
pub(super) fn command_needs_passthrough(cmd: &str) -> bool {
    rewrite_would_corrupt(cmd.trim_end())
}

/// Return `true` when `cmd` contains shell syntax that the rewrite pipeline
/// cannot reconstruct byte-faithfully — every rewrite path MUST bail.
///
/// A rewrite that errors, changes semantics, or loses bytes is worse than no
/// rewrite: 72 sessions corrupted multi-line `git commit` heredocs before this
/// guard existed (#317 Addendum 5). Checks are deliberately substring-based
/// (even inside quotes): over-bailing only costs a missed optimization, while
/// under-bailing corrupts the user's command.
///
/// Triggers:
/// - any newline (tokenization flattens multi-line commands)
/// - heredoc `<<`
/// - command substitution `$(` / `${` or backticks
/// - unmatched quotes
/// - whitespace that does not survive split+rejoin (runs of spaces/tabs
///   inside quoted arguments)
/// - a recognized redirect followed by an unrecognized `>`-bearing token
///   (see [`redirect_order_hazard`])
/// - a recognized redirect token sitting inside quoted text (see
///   [`quoted_redirect_hazard`])
pub(super) fn rewrite_would_corrupt(cmd: &str) -> bool {
    if cmd.contains('\n')
        || cmd.contains('`')
        || cmd.contains("<<")
        || cmd.contains("$(")
        || cmd.contains("${")
        || cmd.contains("<(")
        || cmd.contains(">(")
    {
        return true;
    }
    if has_unmatched_quotes(cmd) {
        return true;
    }
    if redirect_order_hazard(cmd) {
        return true;
    }
    if quoted_redirect_hazard(cmd) {
        return true;
    }
    // Whitespace round-trip guard: tokenization must be lossless.
    let rejoined = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    rejoined != cmd.trim()
}

/// Return `true` when a recognized redirect token (`2>&1`, `>/dev/null`, …)
/// sits *inside quoted text* as a bare, whitespace-delimited token.
///
/// The compound rewriter tokenises each segment with `split_whitespace`, which
/// is quote-blind, so a quoted argument like `"msg 2>&1 here"` yields a bare
/// `2>&1` token. [`strip_segment_redirects`] then strips it and
/// [`splice_redirects_back`] re-appends it at segment end — silently deleting
/// text from the quoted argument AND injecting a real fd redirect the user
/// never wrote: `git commit -m "msg 2>&1 here" && true` would become
/// `skim git commit -m "msg here" 2>&1 && true`. Bail instead (#317:
/// byte-faithful or bail).
///
/// A redirect glued to its quote (`"2>&1`, no inner space) keeps the quote in
/// its token, so it is not recognized by [`is_single_redirect`] and never
/// stripped — those inputs correctly do not trip this guard. Deliberately
/// coarse: a lone `2>` token, or a quoted redirect in a non-compound command,
/// over-bails, which only costs a missed optimization.
fn quoted_redirect_hazard(cmd: &str) -> bool {
    let mut quote_state = QuoteState::None;
    let mut token = String::new();
    let mut token_in_quote = false;
    let mut chars = cmd.chars();

    let is_hazard = |tok: &str| is_single_redirect(tok) || tok == "2>";

    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            // Token boundary (matches split_whitespace). Whitespace inside a
            // quote does not change quote_state, but it still splits tokens.
            if token_in_quote && is_hazard(&token) {
                return true;
            }
            token.clear();
            token_in_quote = false;
            continue;
        }

        // Non-whitespace char belongs to the current token. Flag the token when
        // we are already inside a quote as the char is consumed.
        if quote_state != QuoteState::None {
            token_in_quote = true;
        }

        match quote_state {
            QuoteState::SingleQuote => {
                if ch == '\'' {
                    quote_state = QuoteState::None;
                }
            }
            QuoteState::DoubleQuote => {
                if ch == '\\' {
                    // Escaped char stays part of the token; consume it verbatim.
                    token.push(ch);
                    if let Some(next) = chars.next() {
                        token.push(next);
                    }
                    continue;
                } else if ch == '"' {
                    quote_state = QuoteState::None;
                }
            }
            QuoteState::None => {
                if ch == '\'' {
                    quote_state = QuoteState::SingleQuote;
                } else if ch == '"' {
                    quote_state = QuoteState::DoubleQuote;
                }
            }
        }
        token.push(ch);
    }

    // Trailing token (no terminating whitespace).
    token_in_quote && is_hazard(&token)
}

/// Return `true` when a recognized redirect token is followed anywhere by an
/// unrecognized `>`-bearing token.
///
/// [`strip_segment_redirects`] removes only the recognized forms and
/// [`splice_redirects_back`] re-appends them at segment end. An unrecognized
/// `>file` redirect stays in place, so a recognized redirect that originally
/// preceded it gets reordered PAST it — and redirect order is fd-routing
/// semantics: `2>&1 >log.txt` (stderr→terminal, stdout→log) is not
/// `>log.txt 2>&1` (both→log). Bail instead (#317: byte-faithful or bail).
///
/// Deliberately coarse and whole-command (over-bailing across segments, or on
/// quoted `>` characters in args, only costs a missed optimization).
fn redirect_order_hazard(cmd: &str) -> bool {
    let mut saw_recognized = false;
    for tok in cmd.split_whitespace() {
        if is_single_redirect(tok) || tok == "2>" {
            saw_recognized = true;
        } else if saw_recognized && tok.contains('>') {
            return true;
        }
    }
    false
}

/// Scan `cmd` with the same quote state machine as [`split_compound`],
/// returning `true` when a quote is left open at end of input.
fn has_unmatched_quotes(cmd: &str) -> bool {
    let mut quote_state = QuoteState::None;
    let mut chars = cmd.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote_state {
            QuoteState::SingleQuote => {
                if ch == '\'' {
                    quote_state = QuoteState::None;
                }
            }
            QuoteState::DoubleQuote => {
                if ch == '\\' {
                    chars.next(); // skip escaped char
                } else if ch == '"' {
                    quote_state = QuoteState::None;
                }
            }
            QuoteState::None => {
                if ch == '\'' {
                    quote_state = QuoteState::SingleQuote;
                } else if ch == '"' {
                    quote_state = QuoteState::DoubleQuote;
                }
            }
        }
    }
    quote_state != QuoteState::None
}

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
/// Returns the redirect tokens that were stripped so they can be re-spliced
/// via `splice_redirects_back` at emission time.  The `tokens` vec is mutated
/// in place.
///
/// # DESIGN NOTE (AD-RW-2)
///
/// Only appended/trailing redirects are handled.  Pre-command redirects
/// (`2>&1 foo`) are non-standard and out of scope per the plan.  The redirect
/// forms listed above cover the most common CI/agent patterns.
pub(super) fn strip_segment_redirects(tokens: &mut Vec<String>) -> Vec<String> {
    let mut stripped: Vec<String> = Vec::new();

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
        stripped.push(tok);
    }

    // Reverse to restore original order (we drained in reverse).
    stripped.reverse();

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
/// Exposed as `pub(super)` so `mod.rs` can call it directly, eliminating
/// duplicated inline loops.
pub(super) fn splice_redirects_back(tokens: &mut Vec<String>, redirects: &[String]) {
    for tok in redirects {
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
        if paren_depth == 0
            && let Some((op, advance)) = scan_operator(&chars, i, len)
        {
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

/// Return true if any segment has a trailing pipe operator.
pub(super) fn has_pipe_operator(segments: &[CommandSegment]) -> bool {
    segments
        .iter()
        .any(|s| s.trailing_operator == Some(CompoundOp::Pipe))
}

/// Attempt to rewrite a compound command expression.
///
/// For `&&`/`||`/`;`: tries `try_rewrite()` on each segment independently.
/// For `|`: NEVER rewrites (#317, user-approved): compressing a pipe
/// producer silently changes what downstream `grep`/`wc`/`head` consume —
/// the whole pipeline passes through untouched.
/// Returns `Some(RewriteResult)` if ANY segment was rewritten, `None` otherwise.
pub(super) fn try_rewrite_compound(segments: &[CommandSegment]) -> Option<RewriteResult> {
    if segments.is_empty() {
        return None;
    }

    if has_pipe_operator(segments) {
        return None;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // rewrite_would_corrupt (#317 round-trip safety)
    // ========================================================================

    /// The exact corruption class from #317 Addendum 5: a multi-line
    /// `git commit` message (heredoc-style) flattened by tokenization.
    /// 72 sessions / 180 failures before this guard.
    #[test]
    fn test_corrupt_guard_multiline_commit_bails() {
        let cmd = "git commit -m \"feat: subject line\n\nBody paragraph with detail.\n\"";
        assert!(rewrite_would_corrupt(cmd), "newlines must bail");
    }

    #[test]
    fn test_corrupt_guard_heredoc_bails() {
        assert!(rewrite_would_corrupt("git commit -F- <<'EOF'"));
        assert!(rewrite_would_corrupt("cat <<EOF"));
    }

    #[test]
    fn test_corrupt_guard_substitution_and_backticks_bail() {
        assert!(rewrite_would_corrupt("echo $(date)"));
        assert!(rewrite_would_corrupt("echo ${HOME}"));
        assert!(rewrite_would_corrupt("echo `date`"));
    }

    /// Process substitution `<(cmd)` / `>(cmd)` must bail.
    ///
    /// The compound rewriter does not handle process substitution — a future
    /// redirect-stripping change must not silently reorder around `<(` or `>(`.
    /// Bail is defense-in-depth; the tokens pass through byte-faithfully today
    /// because parens are not stripped, but the guard prevents silent breakage
    /// if redirect handling is ever extended.
    #[test]
    fn test_corrupt_guard_process_substitution_bails() {
        assert!(rewrite_would_corrupt("diff <(sort a.txt) <(sort b.txt)"));
        assert!(rewrite_would_corrupt("tee >(gzip > out.gz)"));
        assert!(rewrite_would_corrupt(
            "cargo test && diff <(sort a) <(sort b)"
        ));
    }

    #[test]
    fn test_corrupt_guard_unmatched_quote_bails() {
        assert!(rewrite_would_corrupt("git commit -m \"unterminated"));
        assert!(rewrite_would_corrupt("echo 'open"));
    }

    #[test]
    fn test_corrupt_guard_lossy_whitespace_bails() {
        // Double space inside a quoted argument does not survive
        // split_whitespace + join(" ").
        assert!(rewrite_would_corrupt("git commit -m \"two  spaces\""));
        assert!(rewrite_would_corrupt("grep \"a\tb\" file.txt"));
    }

    #[test]
    fn test_corrupt_guard_clean_commands_pass() {
        assert!(!rewrite_would_corrupt("git commit -m \"one-line message\""));
        assert!(!rewrite_would_corrupt("cargo test"));
        assert!(!rewrite_would_corrupt("grep -rn pattern src/"));
        assert!(!rewrite_would_corrupt("cargo test && cargo build"));
    }

    /// Redirect-order hazard: `2>&1 >log.txt` means stderr→terminal,
    /// stdout→log. Strip-and-append would reorder it to `>log.txt 2>&1`
    /// (both→log) — fd-routing corruption. Must bail.
    #[test]
    fn test_corrupt_guard_redirect_reorder_bails() {
        assert!(rewrite_would_corrupt(
            "cargo build 2>&1 >log.txt && cargo test"
        ));
        assert!(rewrite_would_corrupt(
            "cargo test 2>/dev/null >out && cargo build"
        ));
        assert!(rewrite_would_corrupt("cargo test 2>&1 >log.txt"));
        // Unrecognized-first, recognized, then another unrecognized.
        assert!(rewrite_would_corrupt("cmd >a 2>&1 >b"));
    }

    /// Safe redirect shapes still rewrite: recognized-only combinations keep
    /// their relative order through strip+append, and an unrecognized
    /// redirect BEFORE a recognized one is appended after it unchanged.
    #[test]
    fn test_corrupt_guard_safe_redirect_orders_pass() {
        assert!(!rewrite_would_corrupt("cargo test 2>&1"));
        assert!(!rewrite_would_corrupt("cargo test 2>&1 && cargo build"));
        assert!(!rewrite_would_corrupt("cargo test >log.txt 2>&1"));
        assert!(!rewrite_would_corrupt("cargo test 2>&1 >/dev/null"));
    }

    /// #322: a recognized redirect token sitting *inside* quoted text becomes a
    /// bare `2>&1` token after `split_whitespace`. The compound rewriter would
    /// strip it from the quoted argument and splice a real fd redirect onto the
    /// segment — corrupting the quoted prose AND changing fd routing. Must bail.
    #[test]
    fn test_corrupt_guard_quoted_redirect_bails() {
        assert!(rewrite_would_corrupt(
            "git commit -m \"msg 2>&1 here\" && true"
        ));
        assert!(rewrite_would_corrupt("echo \"log >/dev/null marker\" ; ls"));
        assert!(rewrite_would_corrupt(
            "printf \"a 2>/dev/null b\" && cargo test"
        ));
        // Single-quoted text trips the guard too.
        assert!(rewrite_would_corrupt(
            "git commit -m 'note &>/dev/null end' && true"
        ));
        // Over-bails even without a compound operator (safe — missed opt only).
        assert!(rewrite_would_corrupt("git commit -m \"msg 2>&1 here\""));
    }

    /// #322: a redirect glued to its quote (`"2>&1`, no inner space) keeps the
    /// quote in its token, so strip never recognizes it — those inputs must NOT
    /// over-bail. Real redirects outside quotes also keep rewriting.
    #[test]
    fn test_corrupt_guard_quoted_redirect_false_positives_pass() {
        assert!(!rewrite_would_corrupt("grep \"2>&1\" file.txt"));
        assert!(!rewrite_would_corrupt("grep \"2>&1 foo\" file.txt"));
        assert!(!rewrite_would_corrupt(
            "echo \"plain message\" && cargo test"
        ));
        assert!(!rewrite_would_corrupt("cargo test 2>&1 && cargo build"));
    }

    /// #322: pin the corruption itself — with the guard bypassed (split+rewrite
    /// directly), the quoted `2>&1` is stripped from the argument and re-spliced
    /// as a real redirect, proving the guard is load-bearing, not redundant.
    #[test]
    fn test_quoted_redirect_corruption_is_real_without_guard() {
        match split_compound("cargo test \"x 2>&1 y\" && cargo build") {
            CompoundSplitResult::Compound(segments) => {
                let joined = try_rewrite_compound(&segments)
                    .expect("rewrites without the guard")
                    .tokens
                    .join(" ");
                assert!(
                    !joined.contains("2>&1 y"),
                    "documents the quoted-redirect corruption the guard prevents: {joined}"
                );
            }
            other => panic!("Expected Compound, got {other:?}"),
        }
    }

    /// Pin the reorder defect itself: with the guard bypassed (calling
    /// split+rewrite directly), the hazard shape WOULD reorder — proving the
    /// guard is load-bearing, not redundant.
    #[test]
    fn test_redirect_reorder_defect_is_real_without_guard() {
        match split_compound("cargo build 2>&1 >log.txt && cargo test") {
            CompoundSplitResult::Compound(segments) => {
                let joined = try_rewrite_compound(&segments)
                    .expect("rewrites without the guard")
                    .tokens
                    .join(" ");
                let idx_merge = joined.find("2>&1").expect("2>&1 present");
                let idx_log = joined.find(">log.txt").expect(">log.txt present");
                assert!(
                    idx_merge > idx_log,
                    "documents the reorder the guard exists to prevent: {joined}"
                );
            }
            other => panic!("Expected Compound, got {other:?}"),
        }
    }

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
        assert!(joined.contains("skim cargo test"));
        assert!(joined.contains("&&"));
        assert!(joined.contains("skim cargo build"));
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
        assert!(joined.contains("skim cargo test"));
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

    /// #317 (user-approved): pipe expressions are NEVER rewritten — producer
    /// compression silently changes what the downstream consumer sees.
    #[test]
    fn test_compound_pipe_never_rewritten() {
        match split_compound("cargo test 2>&1 | head") {
            CompoundSplitResult::Compound(segments) => {
                assert!(
                    try_rewrite_compound(&segments).is_none(),
                    "pipe expressions must pass through untouched"
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

    // ========================================================================
    // Redirect stripping — all single-token and two-token forms (Task 6d)
    // ========================================================================

    /// Exercise every single-token redirect form that `is_single_redirect` recognises.
    ///
    /// Each form must be stripped (not appear in the matched tokens) but must be
    /// re-spliced back into the output at emission time.  We test stripping only
    /// here; re-splicing is covered by `test_compound_pipe_rewrite_preserves_redirect`.
    #[test]
    fn test_strip_segment_redirects_all_single_token_forms() {
        let forms = [
            "2>&1",
            ">&2",
            "1>&2",
            ">&1",
            ">/dev/null",
            "2>/dev/null",
            "&>/dev/null",
        ];
        for form in forms {
            let mut tokens: Vec<String> =
                vec!["cargo".to_string(), "test".to_string(), form.to_string()];
            let stripped = strip_segment_redirects(&mut tokens);
            assert_eq!(
                tokens,
                vec!["cargo", "test"],
                "redirect {form:?} must be stripped from token list"
            );
            assert_eq!(
                stripped,
                vec![form.to_string()],
                "stripped list must contain {form:?}"
            );
        }
    }

    /// The whitespace-separated two-token form `["2>", "/dev/null"]` must be
    /// stripped as a unit (both tokens removed together).
    #[test]
    fn test_strip_segment_redirects_two_token_form() {
        let mut tokens: Vec<String> = vec![
            "cargo".to_string(),
            "test".to_string(),
            "2>".to_string(),
            "/dev/null".to_string(),
        ];
        let stripped = strip_segment_redirects(&mut tokens);
        assert_eq!(
            tokens,
            vec!["cargo", "test"],
            "both tokens of the two-token form must be stripped"
        );
        assert_eq!(
            stripped,
            vec!["2>".to_string(), "/dev/null".to_string()],
            "stripped list must contain both two-token redirect tokens"
        );
    }

    /// `||` operator with a redirect on the left side: rewrite must preserve
    /// the redirect and the `||` consumer.
    #[test]
    fn test_compound_or_rewrite_preserves_redirect() {
        match split_compound("cargo test 2>&1 || echo failed") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                assert!(
                    result.is_some(),
                    "cargo test 2>&1 || echo failed must be rewritten"
                );
                let joined = result.unwrap().tokens.join(" ");
                assert!(
                    joined.contains("2>&1"),
                    "redirect must survive || rewrite: {joined}"
                );
                assert!(
                    joined.contains("|| echo failed"),
                    "|| consumer must be preserved: {joined}"
                );
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    /// `;` operator with a redirect: `cargo test 2>&1 ; echo done` must be
    /// rewritten with the redirect and `;` consumer preserved.
    #[test]
    fn test_compound_semicolon_rewrite_preserves_redirect() {
        match split_compound("cargo test 2>&1 ; echo done") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                assert!(
                    result.is_some(),
                    "cargo test 2>&1 ; echo done must be rewritten"
                );
                let joined = result.unwrap().tokens.join(" ");
                assert!(
                    joined.contains("2>&1"),
                    "redirect must survive ; rewrite: {joined}"
                );
                assert!(
                    joined.contains("; echo done") || joined.contains(";echo done"),
                    "; consumer must be preserved: {joined}"
                );
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    // ========================================================================
    // scan_operator regression — `>&N&&` must not confuse the `&&` scanner
    // (Task 6e)
    // ========================================================================

    /// `foo |& bar` — bash-specific "pipe stderr and stdout to next command".
    ///
    /// `scan_operator` parses `|&` as `Pipe` (the `|`) plus a stray `&` token
    /// on the next segment.  Since pipe segments are never rewritten
    /// (`has_pipe_operator` short-circuits to `None`), the whole expression
    /// passes through untouched, preserving shell semantics.  Pin this: the
    /// rewriter must not transform `foo |& bar` into anything.
    #[test]
    fn test_pipe_stderr_passthrough_untouched() {
        match split_compound("foo |& bar") {
            CompoundSplitResult::Compound(segments) => {
                assert!(
                    try_rewrite_compound(&segments).is_none(),
                    "|& expressions must pass through untouched (pipe short-circuit)"
                );
            }
            other => panic!("Expected Compound for foo |& bar, got {other:?}"),
        }
    }

    /// `foo >&1&& bar` — `>&1` immediately followed by `&&` (no space).
    ///
    /// The scan_operator guard `i > 0 && chars[i-1] == '>'` must prevent the
    /// first `&` of `>&1&&` (at the `&` in `1&&`) from being misidentified as
    /// the start of `&&`.  The command must split at the real `&&` boundary so
    /// both segments are seen.
    ///
    /// We validate this by checking that `split_compound` returns `Compound`
    /// (not `Single` or `Bail`) and that two segments are produced.
    #[test]
    fn test_scan_operator_redirect_before_and_and_no_space() {
        match split_compound("foo >&1&& bar") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(
                    segments.len(),
                    2,
                    "foo >&1&& bar must split into 2 segments: {segments:?}"
                );
                // First segment should contain `foo`; redirect stripped.
                assert!(
                    segments[0].tokens.contains(&"foo".to_string()),
                    "first segment must contain foo: {:?}",
                    segments[0].tokens
                );
                // Second segment should contain `bar`.
                assert!(
                    segments[1].tokens.contains(&"bar".to_string()),
                    "second segment must contain bar: {:?}",
                    segments[1].tokens
                );
            }
            CompoundSplitResult::Simple(_) => {
                panic!("foo >&1&& bar should split on && but got Simple")
            }
            CompoundSplitResult::Bail => {
                panic!("foo >&1&& bar should split on && but got Bail")
            }
        }
    }

    // ========================================================================
    // command_needs_passthrough — Fix C (fix/rewrite-hook-falseneg)
    // ========================================================================

    /// A trailing newline only (agent hooks add one) must NOT trigger passthrough.
    ///
    /// Fix C regression guard: `rewrite_would_corrupt` bails on ALL `\n`,
    /// including trailing ones added by agent hook infrastructure.  A command
    /// like `"cargo test\n"` is safe to rewrite after trimming.
    #[test]
    fn fix_c_trailing_newline_passes() {
        assert!(
            !command_needs_passthrough("cargo test\n"),
            "trailing newline must not force passthrough"
        );
        assert!(
            !command_needs_passthrough("grep -rn pattern src/\n"),
            "grep -rn with trailing newline must not force passthrough"
        );
        assert!(
            !command_needs_passthrough("cargo test\r\n"),
            "Windows-style trailing CRLF must not force passthrough"
        );
    }

    /// An interior newline (multi-line command body) MUST still trigger passthrough.
    ///
    /// This is the corruption case: `split_whitespace` flattens `\n` into a
    /// space, destroying the original byte sequence.
    #[test]
    fn fix_c_interior_newline_bails() {
        assert!(
            command_needs_passthrough(
                "git commit -m \"feat: subject\n\nBody paragraph.\""
            ),
            "interior newline must force passthrough"
        );
        assert!(
            command_needs_passthrough("echo first\necho second"),
            "two commands joined by interior newline must force passthrough"
        );
    }

    /// A clean command with no newline must pass through `command_needs_passthrough`
    /// unaffected — the wrapper must not introduce false positives.
    #[test]
    fn fix_c_clean_command_passes() {
        assert!(
            !command_needs_passthrough("cargo test"),
            "clean command must not need passthrough"
        );
        assert!(
            !command_needs_passthrough("cargo test && cargo build"),
            "compound clean command must not need passthrough"
        );
    }
}
