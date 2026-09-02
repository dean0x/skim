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
/// - stdout (fd 1) or both streams redirected to a file (see
///   [`stdout_redirected_to_file`]): a visibility bail — the command
///   reconstructs fine but rewriting changes what reaches the file (#370)
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
    if stdout_redirected_to_file(cmd) {
        return true;
    }
    // Whitespace round-trip guard: tokenization must be lossless.
    let rejoined = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    rejoined != cmd.trim()
}

/// Return `true` when `cmd` redirects **stdout (fd 1) or both streams** to a
/// FILE. Such a command must never be rewritten: skim would interpose and the
/// file would capture skim's compressed summary instead of the tool's raw bytes
/// (#370). Redirect sibling of the pipe exclusion (AD-RW-2).
///
/// This is a *visibility* bail, not a byte-faithfulness one — the command
/// reconstructs fine; rewriting changes what reaches the file. Quote-aware: a
/// `>` inside single or double quotes is ignored (no over-bail). Maximally
/// strict: catches spaced/glued/append/fd-prefixed forms, `&>file`, `>&FILE`,
/// and glued-middle `foo>out`. Skips `2>`/`2>>` (stderr — stdout still reaches
/// the agent) and fd-dups (`>&1`, `>&2`, `>&-`, `1>&2`) whose source fd does not
/// itself point at a file.
///
/// CHECK ORDER (source-fd before target) is load-bearing.
///
/// # fd-2 tracking: `cmd 2>f >&2` (case 8)
///
/// `2>f` looks stderr-only and `>&2` looks like a harmless fd-dup, so a scanner
/// that judges each token in isolation sees no stdout→file redirect at all — yet
/// the pair routes fd 1 onto `f`. Running the rewrite this scan used to permit
/// for `git log -n 5 2>f >&2` put 623 compressed bytes into a file where raw git
/// wrote 10716 (measured on this branch). The scan therefore carries one bit of
/// state, `fd2_is_file`, updated left-to-right so ORDER is honoured: `>&2 2>f`
/// (dup first, redirect after) leaves fd 1 on the original stderr and correctly
/// does not bail.
///
/// This is the deliberate fix for case 8, rather than moving the check to an
/// `fstat` on the explicit-subcommand path. An fd-1 `fstat` gate on
/// `Invocation::Subcommand` would also fire for `skim git log > out.txt` — a
/// command where the user typed `skim` themselves — and silently serve raw,
/// overriding an explicit request. That path cannot distinguish a user-authored
/// `skim …` from a hook-injected one, so ground truth there would defeat intent.
/// Ground truth belongs on the surface where skim was never asked for (the
/// wrapper); syntax belongs on the surface that can see the redirect.
fn stdout_redirected_to_file(cmd: &str) -> bool {
    // Byte-indexed scanner: avoids a Vec<char> heap allocation on the rewrite
    // hot path. All operator characters (`>`, `'`, `"`, `\`, `2`, `&`, `-`,
    // ASCII digits, space) are single-byte ASCII; multibyte UTF-8 bytes (≥ 0x80)
    // can never equal them, so they fall through the `i += 1` arm unchanged.
    //
    // Quote state uses plain bools (not QuoteState<>) because the escape rule
    // `!in_single && b == b'\\'` maps directly to bash semantics in a single
    // expression, and the two-bool invariant (at most one true at a time) is
    // maintained by the `!in_double` / `!in_single` guards on every transition.
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    // Does fd 2 currently point at a file? Set by `2>FILE` / `2>>FILE`, cleared
    // by `2>&N`. Read when fd 1 is dup'd from fd 2 (`>&2`, `1>&2`).
    let mut fd2_is_file = false;

    while i < len {
        let ch = bytes[i];

        // Backslash escapes the next char when outside single quotes (bash
        // semantics). This handles two cases:
        //   Unquoted:      `\'` → next `'` is literal, never toggles in_single.
        //   Double-quoted: `\"` → next `"` is literal, never toggles in_double.
        //   Single-quoted: `\` is literal (no escape inside `'` in bash).
        // Without this, a balanced `\' ... \'` pair would trick the scanner into
        // treating the region as single-quoted and miss a real `>` between them
        // (avoids PF-004 false-negative / re-opens #370).
        if !in_single && ch == b'\\' {
            i += 2;
            continue;
        }

        // Quote state transitions (only while not inside the other quote type).
        if ch == b'\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if ch == b'"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }

        // Inside any open quote: `>` here is not a redirect operator.
        if in_single || in_double {
            i += 1;
            continue;
        }

        if ch != b'>' {
            i += 1;
            continue;
        }

        // Unquoted `>` at position i.
        //
        // Determine source-fd from the character immediately before.
        // Stderr-only (`2>`) iff the previous char is a STANDALONE '2'
        // (preceded by whitespace or start-of-string).
        let is_stderr_only =
            i > 0 && bytes[i - 1] == b'2' && (i < 2 || bytes[i - 2].is_ascii_whitespace());

        if is_stderr_only {
            // Skip this `>` and an optional second `>` (the `2>>` append form).
            i += 1;
            if i < len && bytes[i] == b'>' {
                i += 1;
            }
            // Record where fd 2 lands. `2>&N` is an fd-dup (fd 2 follows another
            // fd, which is not a file here — a file-bound fd 1 would already have
            // bailed); anything else, including a bare trailing `2>`, is a file
            // target. A later `>&2` then routes stdout into that file (case 8).
            let mut t = i;
            while t < len && bytes[t] == b' ' {
                t += 1;
            }
            fd2_is_file = !(t < len && bytes[t] == b'&');
            continue;
        }

        // Stdout (or both streams) is the source — examine the target.
        let mut j = i + 1;
        // Consume optional second `>` (append form `>>`).
        if j < len && bytes[j] == b'>' {
            j += 1;
        }
        // Skip spaces between `>` and target.
        while j < len && bytes[j] == b' ' {
            j += 1;
        }
        // fd-dup: `>&<digits>` or `>&-` — NOT a file redirect; skip.
        // bash treats `>&word` as fd-dup ONLY when `word` is entirely ASCII
        // digits or exactly `-`; `>&2x` redirects both streams to file `2x` and
        // must bail. Scan to the end of the token and verify every byte.
        if j < len && bytes[j] == b'&' {
            let k = j + 1;
            if k < len && (bytes[k].is_ascii_digit() || bytes[k] == b'-') {
                // Advance m to the first whitespace or end-of-string after k.
                let mut m = k + 1;
                while m < len && !bytes[m].is_ascii_whitespace() {
                    m += 1;
                }
                // The whole post-& target must be either exactly `-` or all digits.
                let is_fd_dup = (bytes[k] == b'-' && m == k + 1)
                    || bytes[k..m].iter().all(|b| b.is_ascii_digit());
                if is_fd_dup {
                    // `>&2` / `1>&2` points fd 1 wherever fd 2 currently points.
                    // If a preceding `2>FILE` put fd 2 on a file, stdout now
                    // lands in that file — bail (case 8).
                    if fd2_is_file && bytes[k] == b'2' && m == k + 1 {
                        return true;
                    }
                    i = m;
                    continue;
                }
            }
        }
        // Anything else is a file target — bail.
        return true;
    }

    false
}

// ---- Byte-exact destination detection (cross-surface fidelity parity) ----

/// Pipe consumers that persist or digest the EXACT bytes of their stdin.
///
/// Membership means "compressing the producer corrupts this consumer's result",
/// not merely "this consumer reads bytes". `cat`, `head`, `grep`, `less`, `jq`
/// and friends are deliberately ABSENT: they render the stream for a reader, and
/// compressing what an agent is about to read is skim's entire purpose.
///
/// The set only needs to cover consumers that persist WITHOUT a `>` redirect —
/// `| gzip > out.gz` is already caught by [`stdout_redirected_to_file`] via its
/// `>`. That keeps the list small and reviewable.
///
/// HEURISTIC, and honest about it: this is a denylist, so an unlisted persisting
/// consumer still gets compressed bytes. A denylist was chosen over an allowlist
/// of safe readers because the allowlist's failure mode — serving raw for every
/// unlisted consumer — would silently kill compression for `| grep`, `| wc`,
/// `| head` and every other everyday pipeline, which is the outcome this work
/// explicitly rejects.
const BYTE_EXACT_PIPE_CONSUMERS: &[&str] = &[
    // Write the stream verbatim to a destination of their own.
    "tee",
    "dd",
    "sponge", // Archive or re-encode the stream verbatim.
    "gzip",
    "bzip2",
    "xz",
    "zstd",
    "base64",
    "tar",
    "openssl",
    // Digest the exact bytes — any substitution changes the answer.
    "cksum",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "sha512sum",
    "shasum",
];

/// Return `true` when `cmd`'s stdout destination requires the tool's exact
/// bytes, so the wrapper surface must serve raw even though `fstat` alone would
/// choose to compress.
///
/// This is the rewrite engine acting as the authority on the one thing it can
/// see and `fstat` cannot: **pipeline shape**. `| cat` and `| tee out.txt` are
/// the same FIFO to the wrapper; only a look at the far end distinguishes them.
///
/// Three rules, in cost order:
/// - **S — capture/plumbing**: `$(…)`, backticks and process substitution make
///   the shell consume stdout as a value or wire it to another command's fd.
///   (`${…}` is parameter expansion, not capture, and is deliberately excluded.)
/// - **R — redirect**: stdout or both streams land on a file or named FIFO,
///   including the `2>f … >&2` fd-dup shape.
/// - **T — byte-exact pipe consumer**: the pipeline's next stage persists or
///   digests the exact bytes ([`BYTE_EXACT_PIPE_CONSUMERS`]).
///
/// Anything else — plain `| cat`, `| grep`, `| head`, a TTY — returns `false`
/// and keeps compressing.
pub(super) fn command_needs_exact_bytes(cmd: &str) -> bool {
    // Rule S: the shell captures stdout as a value or plumbs it into an fd.
    if is_capture_shape(cmd) {
        return true;
    }
    // Rule R: stdout (or both streams) lands on a file or named FIFO.
    if stdout_redirected_to_file(cmd) {
        return true;
    }
    // Rule T: the downstream pipe stage needs the bytes verbatim.
    pipe_consumer_needs_exact_bytes(cmd)
}

/// Rule S: the shell consumes stdout as a value (`$(…)`, backticks) or wires it
/// into another command's fd (`<(…)`, `>(…)`).
///
/// (`${…}` is parameter expansion, not capture, and is deliberately excluded.)
///
/// Also the authoritative "tokenisation cannot be trusted here" predicate:
/// these shapes nest a whole command inside one whitespace-delimited token, so
/// [`command_heads`] refuses to guess at head names for them.
fn is_capture_shape(cmd: &str) -> bool {
    cmd.contains("$(") || cmd.contains('`') || cmd.contains("<(") || cmd.contains(">(")
}

/// Upper bound on the number of distinct command heads recorded for one
/// command.
///
/// Every loop and resource gets an explicit bound. Real commands name a handful
/// of tools; a pathological one-liner must not be able to make the hook write an
/// unbounded number of marker files. Exceeding it reports *unknown* rather than
/// a truncated set — see [`command_heads`].
const MAX_COMMAND_HEADS: usize = 16;

/// Commands that exec *another* command given as their argument.
///
/// For these the segment head names the launcher, not the tool whose stdout is
/// actually captured: `timeout 60 git log | tee f` has head `timeout`, and
/// marking `timeout` would leave `git` unmarked — a byte loss. Each takes its
/// own options with its own arity (`timeout 60 …`, `nice -n 5 …`), so the real
/// tool cannot be recovered by skipping a fixed number of tokens. They report
/// *unknown* instead and fall back to the wildcard.
///
/// `sudo` and `command` are absent deliberately: [`tokens_head`] already steps
/// over them to reach the real tool.
const EXEC_PREFIXES: &[&str] = &[
    "env", "nice", "ionice", "chrt", "nohup", "setsid", "timeout", "stdbuf", "unbuffer", "xargs",
    "time", "doas", "watch", "script",
];

/// The basenames of every command head in `cmd`, deduplicated and capped at
/// [`MAX_COMMAND_HEADS`].
///
/// This is the *scope* of [`command_needs_exact_bytes`]: the set of tools whose
/// wrapper invocations belong to this command. The hook records the verdict
/// under these names so a marker set for `git log | tee f` cannot change what a
/// concurrent, hook-less, or later `cargo`/`grep`/`ls` wrapper invocation does.
///
/// An **empty** result means "the tool set is not knowable from this text".
/// Callers must treat that as *all tools*, never as *no tools*: erring wide
/// costs compression, erring narrow costs bytes.
///
/// **Partial knowledge is not knowledge.** If any segment's head cannot be
/// resolved to a plain tool name — a capture shape, a `Bail` shape, an
/// [`EXEC_PREFIXES`] launcher, an unrepresentable name (`sudo -u bob git` heads
/// on `-u`), or more heads than [`MAX_COMMAND_HEADS`] — the whole command
/// reports unknown. Returning the heads it *could* read would leave the
/// unreadable stage's tool unmarked, and an unmarked byte-exact stage is
/// exactly the byte loss this marker exists to prevent.
pub(super) fn command_heads(cmd: &str) -> Vec<String> {
    // A capture shape hides a command inside a single whitespace token
    // (`out=$(git log)` tokenises to `out=$(git`, `log`, …), so `tokens_head`
    // would confidently return the wrong name. Report "unknown" instead.
    if is_capture_shape(cmd) {
        return Vec::new();
    }

    let segments = match split_compound(cmd) {
        CompoundSplitResult::Simple(tokens) => vec![tokens],
        CompoundSplitResult::Compound(segments) => segments.into_iter().map(|s| s.tokens).collect(),
        // Unsupported shell syntax — the token stream is not trustworthy.
        CompoundSplitResult::Bail => return Vec::new(),
    };

    let mut heads: Vec<String> = Vec::new();
    for tokens in &segments {
        // An empty segment is not a command (`git log;` trails one).
        if tokens.is_empty() {
            continue;
        }
        let Some(head) = tokens_head(tokens).filter(|h| {
            crate::cmd::session_sidecar::is_safe_marker_tool(h) && !EXEC_PREFIXES.contains(h)
        }) else {
            return Vec::new();
        };
        if heads.iter().any(|h| h == head) {
            continue;
        }
        if heads.len() == MAX_COMMAND_HEADS {
            return Vec::new();
        }
        heads.push(head.to_string());
    }

    heads
}

/// Return `true` when any pipe stage of `cmd` feeds a [`BYTE_EXACT_PIPE_CONSUMERS`]
/// command.
fn pipe_consumer_needs_exact_bytes(cmd: &str) -> bool {
    let CompoundSplitResult::Compound(segments) = split_compound(cmd) else {
        // Simple (no operator) has no consumer; Bail shapes are already caught
        // by rule S, which runs first.
        return false;
    };
    segments.windows(2).any(|pair| {
        pair[0].trailing_operator == Some(CompoundOp::Pipe)
            && segment_head(&pair[1]).is_some_and(|head| BYTE_EXACT_PIPE_CONSUMERS.contains(&head))
    })
}

/// Extract a segment's command name: the first token that is neither a leading
/// `VAR=VAL` assignment nor a privilege/dispatch prefix, reduced to its basename
/// so `/usr/bin/tee` matches `tee`.
fn segment_head(seg: &CommandSegment) -> Option<&str> {
    tokens_head(&seg.tokens)
}

/// [`segment_head`] over a bare token slice, for the `Simple` (no compound
/// operator) split result which carries tokens rather than segments.
fn tokens_head(tokens: &[String]) -> Option<&str> {
    tokens
        .iter()
        .map(String::as_str)
        .find(|t| !t.contains('=') && *t != "sudo" && *t != "command")
        .map(|t| t.rsplit('/').next().unwrap_or(t))
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

/// Rebuild the command text from a split segment list: each segment's tokens,
/// the redirects [`strip_segment_redirects`] lifted out of it, and the operator
/// that follows it.
///
/// [`try_rewrite_compound`] is handed segments alone, but the destination
/// predicates ([`command_needs_exact_bytes`]) read *syntax*, which only exists
/// in the joined form. Runs of whitespace are not preserved — commands whose
/// tokenisation is lossy are already refused upstream by
/// [`rewrite_would_corrupt`].
fn rejoin_segments(segments: &[CommandSegment]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in segments {
        parts.extend(seg.tokens.iter().map(String::as_str));
        parts.extend(seg.stripped_redirects.iter().map(String::as_str));
        if let Some(op) = seg.trailing_operator {
            parts.push(op.as_str());
        }
    }
    parts.join(" ")
}

/// Return `true` for the one pipeline shape AD-RW-2 permits: `<source> | cat`,
/// with bare `cat` as the sole consumer.
///
/// `| cat` is a **reader render** — an agent defeating a pager, not persisting
/// bytes — and compressing what an agent is about to read is skim's entire
/// purpose. So this shape rewrites while every other pipeline still bails: only
/// the far end of the pipe separates `| cat` from `| tee out.txt`, and reading
/// it wrong costs the user data. The hook's [`command_needs_exact_bytes`]
/// verdict for such a command is `false`, so no force-raw marker is set — which
/// is consistent, because the explicit-subcommand path the rewrite emits
/// compresses into the FIFO on purpose.
///
/// The shape is deliberately exact — exactly two segments joined by a single
/// `|`, the source carrying no redirects, the consumer being the single token
/// `cat` with no arguments and no operator of its own. `cat -n`, `cat -`,
/// `cat file`, a third stage, and any interleaved `&&`/`||`/`;` all fall
/// outside it.
///
/// [`command_needs_exact_bytes`] over the rejoined text is then the safety
/// gate, and it is what keeps `… | cat > f` (rule R, whose `>`/target tokens
/// stay inside the consumer segment) and `… | cat | tee f` (rule T) raw.
fn is_bare_cat_pipeline(segments: &[CommandSegment]) -> bool {
    let [source, consumer] = segments else {
        return false;
    };
    if source.trailing_operator != Some(CompoundOp::Pipe) || !source.stripped_redirects.is_empty() {
        return false;
    }
    if consumer.trailing_operator.is_some()
        || !consumer.stripped_redirects.is_empty()
        || consumer.tokens != ["cat"]
    {
        return false;
    }
    !command_needs_exact_bytes(&rejoin_segments(segments))
}

/// Attempt to rewrite a compound command expression.
///
/// For `&&`/`||`/`;`: tries `try_rewrite()` on each segment independently.
/// For `|`: bails (#317, user-approved) — compressing a pipe producer silently
/// changes what downstream `grep`/`wc`/`head` consume, so the whole pipeline
/// passes through untouched. The single exception is
/// [`is_bare_cat_pipeline`]: `<source> | cat` rewrites its source, because
/// bare `cat` renders the stream for a reader rather than consuming its bytes.
/// Returns `Some(RewriteResult)` if ANY segment was rewritten, `None` otherwise.
pub(super) fn try_rewrite_compound(segments: &[CommandSegment]) -> Option<RewriteResult> {
    if segments.is_empty() {
        return None;
    }

    if has_pipe_operator(segments) && !is_bare_cat_pipeline(segments) {
        return None;
    }

    // For &&/||/; (and the bare-`| cat` shape) — try rewriting each segment
    // independently. `try_rewrite` declines bare `cat`, so the consumer stage
    // of a `| cat` pipeline is re-emitted verbatim.
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

    /// Safe redirect shapes still rewrite: stderr-only or fd-dup forms do not
    /// affect stdout and must not trigger the bail guard.
    #[test]
    fn test_corrupt_guard_safe_redirect_orders_pass() {
        assert!(!rewrite_would_corrupt("cargo test 2>&1"));
        assert!(!rewrite_would_corrupt("cargo test 2>&1 && cargo build"));
        // NOTE: `cargo test >log.txt 2>&1` and `cargo test 2>&1 >/dev/null`
        // were removed from this test — they redirect stdout to a file and now
        // correctly bail via stdout_redirected_to_file (#370). See
        // test_corrupt_guard_stdout_to_file_bails below.
    }

    /// D2 (#370): stdout-to-file redirect shapes bail via `stdout_redirected_to_file`.
    ///
    /// BAIL matrix: all forms that route stdout (fd 1) or both streams to a
    /// file — including spaced, glued, append, fd-prefixed, `>&file`, and
    /// compound varieties. Skips stderr-only (`2>`) and fd-dups (`>&1`/`>&2`).
    #[test]
    fn test_corrupt_guard_stdout_to_file_bails() {
        // Previously in test_corrupt_guard_safe_redirect_orders_pass (moved here #370):
        assert!(
            rewrite_would_corrupt("cargo test >log.txt 2>&1"),
            ">log.txt redirects stdout — must bail"
        );
        assert!(
            rewrite_would_corrupt("cargo test 2>&1 >/dev/null"),
            ">/dev/null after 2>&1 redirects stdout — must bail"
        );

        // Full bail matrix:
        assert!(
            rewrite_would_corrupt("gh api repos/o/r/x > out.json"),
            "> file"
        );
        assert!(
            rewrite_would_corrupt("cargo test >log.txt"),
            ">file (glued)"
        );
        assert!(rewrite_would_corrupt("cargo test >> log.txt"), ">> append");
        assert!(rewrite_would_corrupt("cargo test 1> f"), "1> explicit");
        assert!(rewrite_would_corrupt("cmd &> f"), "&> both streams");
        assert!(rewrite_would_corrupt("cmd &>> f"), "&>> append both");
        assert!(
            rewrite_would_corrupt("cargo test > /dev/null"),
            "> /dev/null"
        );
        assert!(rewrite_would_corrupt("curl https://x >&file"), ">&file");
        assert!(rewrite_would_corrupt("echo foo>out"), "glued foo>out");
        assert!(rewrite_would_corrupt("a > f && b"), "compound with > f");

        // Security fix (Issue 1a): backslash-escaped single quote outside quotes
        // (`\'`) must NOT open a quoting context. A `>` between two `\'` pairs was
        // previously invisible to the scanner, letting skim corrupt the redirect
        // target (avoids PF-004 false-negative).
        assert!(
            rewrite_would_corrupt(r"grep x\' file > out z\'z"),
            r"backslash-escaped \' must not hide a stdout redirect"
        );

        // Security fix (Issue 1b): `>&<digit>filename` is a redirect to a file
        // whose name starts with a digit, NOT an fd-dup. Only `>&<all-digits>`
        // (e.g. `>&2`) or `>&-` are fd-dups; `>&2x` redirects both streams to
        // file `2x` (avoids PF-004 false-negative).
        assert!(
            rewrite_would_corrupt("cmd >&2x"),
            ">&2x redirects both streams to file 2x, not an fd-dup"
        );

        // Compound: stderr-append (`2>>`) skipped, then stdout redirect must bail.
        assert!(
            rewrite_would_corrupt("cmd 2>>a >b"),
            "2>>a >b — skip 2>> then bail on >b"
        );

        // Non-standalone `2` before `>` — the digit is part of a longer token so
        // it is NOT the stderr source prefix; `>` is a stdout redirect.
        assert!(
            rewrite_would_corrupt("cmd foo2>out"),
            "foo2>out — 2 not standalone, so > is a stdout redirect"
        );
    }

    /// No-bail companion for stdout_redirected_to_file: stderr-only forms and
    /// fd-dups must NOT trigger the guard.
    #[test]
    fn test_corrupt_guard_stdout_to_file_no_bail() {
        assert!(!rewrite_would_corrupt("cargo test 2> f"), "2> stderr only");
        assert!(
            !rewrite_would_corrupt("cargo test 2>>log"),
            "2>> stderr append"
        );
        assert!(!rewrite_would_corrupt("cargo test 2>&1"), "2>&1 fd-dup");
        assert!(!rewrite_would_corrupt("cargo test >&1"), ">&1 fd-dup");
        assert!(!rewrite_would_corrupt("cargo test >&2"), ">&2 fd-dup");
        assert!(!rewrite_would_corrupt("cargo test 1>&2"), "1>&2 fd-dup");
        assert!(!rewrite_would_corrupt("cmd >&-"), ">&- close-fd dup");
        assert!(!rewrite_would_corrupt("cargo test | jq ."), "pipe — no >");
        // quoted `>` inside double quotes must not over-bail
        assert!(
            !rewrite_would_corrupt(r#"git commit -m "x > y""#),
            "quoted >"
        );
    }

    // ========================================================================
    // Case 8: `2>f >&2` — fd 1 dup'd from a file-bound fd 2
    // ========================================================================

    /// The measured data-loss case. `2>f` then `>&2` routes stdout onto `f`,
    /// but each token in isolation looks harmless (stderr-only, then fd-dup).
    /// Before this guard the engine emitted `skim git log -n 5 2>f >&2`, which
    /// wrote 623 compressed bytes where raw git wrote 10716.
    #[test]
    fn test_corrupt_guard_stderr_file_then_dup_to_stdout_bails() {
        assert!(
            rewrite_would_corrupt("git log -n 5 2>f >&2"),
            "2>f then >&2 puts stdout in f — must bail"
        );
        assert!(
            rewrite_would_corrupt("git log -n 5 2>/tmp/x.txt >&2"),
            "absolute path target"
        );
        assert!(
            rewrite_would_corrupt("cargo test 2> log.txt >&2"),
            "spaced 2> form"
        );
        assert!(
            rewrite_would_corrupt("cargo test 2>>log.txt >&2"),
            "2>> append then dup"
        );
        assert!(
            rewrite_would_corrupt("cargo test 2>f 1>&2"),
            "explicit 1>&2 dup form"
        );
    }

    /// ORDER is load-bearing: dup FIRST, then redirect fd 2, leaves fd 1 on the
    /// original stderr — no stdout→file redirect exists, so the fd-2 tracking
    /// must NOT fire.
    ///
    /// Asserted against `stdout_redirected_to_file` directly, not
    /// `rewrite_would_corrupt`: the outer guard bails on this shape anyway via
    /// the deliberately coarse `redirect_order_hazard` (a recognized `>&2`
    /// followed by an unrecognized `>`-bearing `2>f`), which would mask whether
    /// the fd-2 state machine got the ordering right.
    #[test]
    fn test_stdout_redirect_scan_respects_dup_before_stderr_redirect() {
        assert!(
            !stdout_redirected_to_file("cargo test >&2 2>f"),
            ">&2 before 2>f — fd 1 follows the ORIGINAL stderr, no stdout->file"
        );
        // Sanity: the reverse order DOES route stdout into the file.
        assert!(
            stdout_redirected_to_file("cargo test 2>f >&2"),
            "2>f before >&2 — fd 1 lands on f"
        );
    }

    /// `2>&1` points fd 2 at fd 1 (not a file), so a later `>&2` is a no-op dup
    /// and must not bail — otherwise the extremely common `cmd 2>&1` shapes
    /// would stop being rewritten.
    #[test]
    fn test_corrupt_guard_fd2_dup_does_not_arm_the_guard() {
        assert!(
            !rewrite_would_corrupt("cargo test 2>&1 >&2"),
            "2>&1 leaves fd 2 off-file; >&2 must stay a harmless dup"
        );
        assert!(!rewrite_would_corrupt("cargo test >&2"), "bare >&2");
        assert!(!rewrite_would_corrupt("cargo test 1>&2"), "bare 1>&2");
    }

    // ========================================================================
    // command_needs_exact_bytes — the rewrite surface's verdict for the wrapper
    // ========================================================================

    /// Rule T: a pipe consumer that persists or digests exact bytes.
    #[test]
    fn test_needs_exact_bytes_byte_exact_pipe_consumers() {
        assert!(command_needs_exact_bytes("git log -n 5 | tee out.txt"));
        assert!(command_needs_exact_bytes("git log -n 5 | tee -a out.txt"));
        assert!(command_needs_exact_bytes("git log | sha256sum"));
        assert!(command_needs_exact_bytes("git log | dd of=out.bin"));
        assert!(command_needs_exact_bytes("git log | base64"));
        // Basename reduction: an absolute path to the same tool still matches.
        assert!(command_needs_exact_bytes("git log | /usr/bin/tee out.txt"));
        // Leading env assignments and `sudo` do not hide the consumer.
        assert!(command_needs_exact_bytes("git log | LC_ALL=C tee out.txt"));
        assert!(command_needs_exact_bytes("git log | sudo tee /etc/x"));
    }

    /// **The case that must not regress.** Readers keep compressing — this is
    /// skim's core value, and a blanket "any pipe → raw" rule was rejected
    /// precisely because it would destroy it.
    #[test]
    fn test_needs_exact_bytes_reader_pipes_still_compress() {
        assert!(!command_needs_exact_bytes("git log -n 5 | cat"));
        assert!(!command_needs_exact_bytes("git log -n 5 | head -20"));
        assert!(!command_needs_exact_bytes("git log | grep fix"));
        assert!(!command_needs_exact_bytes("git log | wc -l"));
        assert!(!command_needs_exact_bytes("git log | less"));
        assert!(!command_needs_exact_bytes("git log -n 5"));
        assert!(!command_needs_exact_bytes("cargo test && cargo build"));
    }

    /// Rule S: the shell consumes stdout as a value or plumbs it into an fd.
    #[test]
    fn test_needs_exact_bytes_capture_and_process_substitution() {
        assert!(command_needs_exact_bytes("out=$(git log -n 5)"));
        assert!(command_needs_exact_bytes("echo `git log -n 5`"));
        assert!(command_needs_exact_bytes("diff <(git log) <(git log -n 1)"));
        assert!(command_needs_exact_bytes("tee >(gzip > out.gz)"));
        // `${VAR}` is parameter expansion, not capture — must NOT arm the rule.
        assert!(!command_needs_exact_bytes("git log -n ${N}"));
    }

    /// Rule R: file and named-FIFO redirects, including the case-8 shape.
    #[test]
    fn test_needs_exact_bytes_redirects() {
        assert!(command_needs_exact_bytes("git log -n 5 > out.txt"));
        assert!(command_needs_exact_bytes("git log -n 5 >> out.txt"));
        assert!(command_needs_exact_bytes("git log -n 5 > /dev/null"));
        assert!(command_needs_exact_bytes("git log -n 5 > myfifo"));
        assert!(command_needs_exact_bytes("git log -n 5 2>f >&2"));
        // A `>` inside a pipeline stage still counts (`| gzip > f.gz`).
        assert!(command_needs_exact_bytes("git log | gzip > out.gz"));
        // stderr-only redirects leave stdout alone.
        assert!(!command_needs_exact_bytes("git log -n 5 2> err.txt"));
    }

    // ========================================================================
    // command_heads — the SCOPE of that verdict
    // ========================================================================

    /// Every stage of a pipeline is named, so one hook invocation covers every
    /// wrapper invocation it will produce.
    #[test]
    fn test_command_heads_names_every_pipeline_stage() {
        assert_eq!(command_heads("git log -n 5 | tee out.txt"), ["git", "tee"]);
        assert_eq!(command_heads("cargo test && git log"), ["cargo", "git"]);
        assert_eq!(command_heads("git log -n 5"), ["git"]);
    }

    /// The same normalisation `segment_head` already applies for rule T:
    /// basename reduction, and leading `VAR=VAL` / `sudo` / `command` skipped.
    #[test]
    fn test_command_heads_normalises_like_rule_t() {
        assert_eq!(
            command_heads("git log | /usr/bin/tee out.txt"),
            ["git", "tee"]
        );
        assert_eq!(
            command_heads("git log | LC_ALL=C tee out.txt"),
            ["git", "tee"]
        );
        assert_eq!(command_heads("git log | sudo tee /etc/x"), ["git", "tee"]);
    }

    /// Repeats collapse, and the list is bounded — a pathological one-liner must
    /// not make the hook write an unbounded number of marker files. Overflowing
    /// the bound reports *unknown* (wildcard), never a truncated set: a
    /// truncated set would leave the dropped tools unmarked.
    #[test]
    fn test_command_heads_dedupes_and_is_bounded() {
        assert_eq!(command_heads("git log | git cat-file --batch"), ["git"]);

        let at_bound: String = (0..MAX_COMMAND_HEADS)
            .map(|i| format!("tool{i} x"))
            .collect::<Vec<_>>()
            .join(" && ");
        assert_eq!(command_heads(&at_bound).len(), MAX_COMMAND_HEADS);

        let over_bound: String = (0..MAX_COMMAND_HEADS + 1)
            .map(|i| format!("tool{i} x"))
            .collect::<Vec<_>>()
            .join(" && ");
        assert!(
            command_heads(&over_bound).is_empty(),
            "overflowing the bound must report unknown, not a truncated set"
        );
    }

    /// **A launcher is not the tool whose stdout is captured.**
    ///
    /// `timeout 60 git log | tee f` heads on `timeout`; marking `timeout` would
    /// leave `git` unmarked and the tee would capture compressed bytes. These
    /// report unknown so the wildcard covers `git`.
    #[test]
    fn test_command_heads_unknown_for_exec_prefixes() {
        for cmd in [
            "timeout 60 git log | tee out.txt",
            "env GIT_PAGER=cat git log | tee out.txt",
            "nice -n 5 git log > out.txt",
            "nohup git log > out.txt",
            "xargs git show < list",
        ] {
            assert!(
                command_heads(cmd).is_empty(),
                "`{cmd}` heads on a launcher — must report unknown"
            );
        }
        // `sudo` and `command` are stepped over, so the real tool is reached.
        assert_eq!(command_heads("sudo git log | tee out.txt"), ["git", "tee"]);
    }

    /// A head that is not a representable tool name makes the whole command
    /// unknown, rather than being silently dropped from an otherwise-populated
    /// set. `sudo -u bob git log` heads on `-u`: dropping it would mark only
    /// `tee` and leave `git` unmarked.
    #[test]
    fn test_command_heads_unknown_when_a_head_is_unrepresentable() {
        assert!(command_heads("sudo -u bob git log | tee out.txt").is_empty());
    }

    /// **Empty means "unknown", never "none".** A capture shape hides a whole
    /// command inside one whitespace token, so `tokens_head` would confidently
    /// return the wrong name (`out=$(git log)` tokenises to `out=$(git`, `log`).
    /// Reporting an empty set routes these to the wildcard marker instead.
    #[test]
    fn test_command_heads_refuses_to_guess_on_capture_shapes() {
        assert!(command_heads("out=$(git log -n 5)").is_empty());
        assert!(command_heads("echo `git log -n 5`").is_empty());
        assert!(command_heads("diff <(git log) <(git log -n 1)").is_empty());
        assert!(command_heads("tee >(gzip > out.gz)").is_empty());
    }

    /// `${VAR}` is parameter expansion, not capture: it must NOT arm rule S
    /// (asserted in `test_needs_exact_bytes_capture_and_process_substitution`).
    /// Head extraction is separately unknown for it, because `split_compound`
    /// bails on `${` for tokenisation safety — so the two answers are
    /// "not byte-exact" and "tools unknown", which together mean *no* marker.
    ///
    /// Pinned because the two predicates arrive at "unknown" by different
    /// routes, and a future change to either must not silently make this
    /// command start writing a wildcard marker.
    #[test]
    fn test_command_heads_unknown_for_parameter_expansion() {
        assert!(!command_needs_exact_bytes("git log -n ${N}"));
        assert!(command_heads("git log -n ${N}").is_empty());
    }

    /// **The invariant that keeps the narrowing lossless in the byte direction.**
    ///
    /// For every shape `command_needs_exact_bytes` accepts, the tool that
    /// actually produces the captured stdout must end up covered: either the
    /// head list names it, or the list is empty and the wildcard covers
    /// everything. A command that is byte-exact but whose producer is missing
    /// from a *non-empty* list would compress into a file — the exact loss this
    /// marker exists to prevent.
    #[test]
    fn test_byte_exact_producer_is_always_covered() {
        let cases = [
            ("git log -n 5 | tee out.txt", "git"),
            ("git log | sha256sum", "git"),
            ("git log -n 5 > out.txt", "git"),
            ("git log -n 5 >> out.txt", "git"),
            ("git log -n 5 2>f >&2", "git"),
            ("git log | gzip > out.gz", "git"),
            ("git log -n 5 > myfifo", "git"),
            ("out=$(git log -n 5)", "git"),
            ("echo `git log -n 5`", "git"),
            ("diff <(git log) <(git log -n 1)", "diff"),
            ("timeout 60 git log | tee out.txt", "git"),
            ("env GIT_PAGER=cat git log > out.txt", "git"),
            ("sudo -u bob git log | tee out.txt", "git"),
            ("cargo test && git log | tee out.txt", "git"),
        ];
        for (cmd, producer) in cases {
            assert!(
                command_needs_exact_bytes(cmd),
                "precondition: `{cmd}` must be byte-exact"
            );
            let heads = command_heads(cmd);
            assert!(
                heads.is_empty() || heads.iter().any(|h| h == producer),
                "`{cmd}`: producer `{producer}` is not covered by {heads:?} — \
                 a non-empty list that omits the producer leaves it unmarked"
            );
        }
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
    fn test_fix_c_trailing_newline_passes() {
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
    fn test_fix_c_interior_newline_bails() {
        assert!(
            command_needs_passthrough("git commit -m \"feat: subject\n\nBody paragraph.\""),
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
    fn test_fix_c_clean_command_passes() {
        assert!(
            !command_needs_passthrough("cargo test"),
            "clean command must not need passthrough"
        );
        assert!(
            !command_needs_passthrough("cargo test && cargo build"),
            "compound clean command must not need passthrough"
        );
    }

    // ========================================================================
    // AD-RW-2 reversal — narrow `<rewritable command> | cat` shape
    //
    // AD-RW-2 currently bails on EVERY pipeline via `has_pipe_operator`.  The
    // reversal allows the narrow shape `<source> | cat` (bare `cat`, no args,
    // single downstream stage) because `| cat` is a reader render whose sole
    // purpose is to defeat pagers — compressing what an agent is about to read
    // is skim's core value.
    //
    // RED tests (1–3): fail today (has_pipe_operator returns None); pass after
    //   the reversal.
    // CONTROL tests (4–9): pass today AND after the reversal — pin the safety
    //   gate so the reversal cannot silently escape its narrow shape.
    // ========================================================================

    /// Binary verdict: `skim rewrite 'git log -n 3 | cat'` → exit 1 (no rewrite).
    ///
    /// After the AD-RW-2 reversal the pure 2-stage pipeline `<source> | cat`
    /// must be rewritten: the source segment is compressed, `| cat` is
    /// preserved verbatim.
    ///
    /// RED: fails today because `has_pipe_operator` short-circuits to None for
    /// every pipeline; passes after the reversal.
    #[test]
    fn pipe_to_bare_cat_rewrites_the_source() {
        match split_compound("git log -n 3 | cat") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                let joined = result
                    .expect("git log -n 3 | cat must be rewritten after AD-RW-2 reversal")
                    .tokens
                    .join(" ");
                assert_eq!(
                    joined, "skim git log -n 3 | cat",
                    "source must be rewritten; downstream `| cat` preserved verbatim"
                );
            }
            other => panic!("Expected Compound for `git log -n 3 | cat`, got {other:?}"),
        }
    }

    /// Binary verdict: `skim rewrite 'cat README.md'` → exit 0,
    /// stdout = `SKIM_REWRITTEN_FROM=cat skim README.md --mode=pseudo`.
    ///
    /// `cat <file>` is rewritten standalone, so when it appears as the pipe
    /// source in `cat README.md | cat` the source segment must be rewritten
    /// exactly as the standalone form with ` | cat` appended.
    ///
    /// RED: fails today (has_pipe_operator bails); passes after the reversal.
    #[test]
    fn pipe_to_bare_cat_rewrites_file_read_source() {
        match split_compound("cat README.md | cat") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                let joined = result
                    .expect("cat README.md | cat must be rewritten after AD-RW-2 reversal")
                    .tokens
                    .join(" ");
                assert_eq!(
                    joined, "SKIM_REWRITTEN_FROM=cat skim README.md --mode=pseudo | cat",
                    "source rewritten as standalone form; | cat appended verbatim"
                );
            }
            other => panic!("Expected Compound for `cat README.md | cat`, got {other:?}"),
        }
    }

    /// Binary verdict: `skim rewrite 'grep -rn foo src'` → exit 0,
    /// stdout = `skim grep -rn foo src`.
    /// Binary verdict: `skim rewrite 'grep -rn foo src | cat'` → exit 1 (no rewrite).
    ///
    /// `grep -rn foo src` is rewritten standalone; when piped to bare `cat` the
    /// source must be rewritten as the standalone form with ` | cat` appended.
    ///
    /// Note: `grep foo file | head` is separately pinned as NOT rewritten by
    /// `test_pipe_catch_all_grep_not_rewritten` (non-cat downstream, outside
    /// the narrow shape).
    ///
    /// RED: fails today (has_pipe_operator bails); passes after the reversal.
    #[test]
    fn pipe_to_bare_cat_rewrites_grep_source() {
        match split_compound("grep -rn foo src | cat") {
            CompoundSplitResult::Compound(segments) => {
                let result = try_rewrite_compound(&segments);
                let joined = result
                    .expect("grep -rn foo src | cat must be rewritten after AD-RW-2 reversal")
                    .tokens
                    .join(" ");
                assert_eq!(
                    joined, "skim grep -rn foo src | cat",
                    "source rewritten as standalone form; | cat appended verbatim"
                );
            }
            other => {
                panic!("Expected Compound for `grep -rn foo src | cat`, got {other:?}")
            }
        }
    }

    /// `git log -n 3 | cat > /tmp/x.txt`: the stdout redirect on the `cat`
    /// stage routes the pipeline output to a file.  Rule R fires →
    /// `command_needs_exact_bytes` is true → the rewrite must be refused.
    ///
    /// Binary verdict: exit 1 (not rewritten — `rewrite_would_corrupt` bails
    /// on the stdout redirect before `try_rewrite_compound` is called).
    ///
    /// CONTROL: passes today (has_pipe_operator bails) and after the reversal
    /// (safety: the `cat` segment has a stripped redirect — not bare `cat` —
    /// so the narrow shape check must refuse it).
    #[test]
    fn pipe_to_cat_then_redirect_is_not_rewritten() {
        assert!(
            command_needs_exact_bytes("git log -n 3 | cat > /tmp/x.txt"),
            "safety gate precondition: Rule R must arm for the stdout redirect"
        );
        match split_compound("git log -n 3 | cat > /tmp/x.txt") {
            CompoundSplitResult::Compound(segments) => {
                assert!(
                    try_rewrite_compound(&segments).is_none(),
                    "git log -n 3 | cat > /tmp/x.txt must not be rewritten \
                     (cat segment carries a stripped stdout redirect)"
                );
            }
            other => panic!("Expected Compound, got {other:?}"),
        }
    }

    /// `git log -n 3 | cat | tee /tmp/x.txt`: `tee` is a byte-exact consumer
    /// (Rule T); the downstream is not bare `cat` (it is a 3-stage pipeline).
    ///
    /// Binary verdict: exit 1 (not rewritten).
    ///
    /// CONTROL: passes today (has_pipe_operator bails) and after the reversal
    /// (safety: 3 pipeline stages → not the narrow 2-stage `<source> | cat`
    /// shape; Rule T also arms `command_needs_exact_bytes`).
    #[test]
    fn pipe_to_cat_then_tee_is_not_rewritten() {
        assert!(
            command_needs_exact_bytes("git log -n 3 | cat | tee /tmp/x.txt"),
            "safety gate precondition: Rule T must arm for tee"
        );
        match split_compound("git log -n 3 | cat | tee /tmp/x.txt") {
            CompoundSplitResult::Compound(segments) => {
                assert!(
                    try_rewrite_compound(&segments).is_none(),
                    "git log -n 3 | cat | tee /tmp/x.txt must not be rewritten \
                     (3-stage pipeline, tee downstream)"
                );
            }
            other => panic!("Expected Compound, got {other:?}"),
        }
    }

    /// `git log -n 3 | cat -n`: downstream `cat` carries the `-n` flag
    /// (number lines).  The narrow shape requires bare `cat` with NO arguments.
    ///
    /// Binary verdict: exit 1 (not rewritten).
    ///
    /// CONTROL: passes today (has_pipe_operator bails) and after the reversal
    /// (safety: `cat` with any argument is not bare `cat` → shape rejected).
    #[test]
    fn pipe_to_cat_with_args_is_not_rewritten() {
        match split_compound("git log -n 3 | cat -n") {
            CompoundSplitResult::Compound(segments) => {
                assert!(
                    try_rewrite_compound(&segments).is_none(),
                    "git log -n 3 | cat -n must not be rewritten (`cat -n` has args)"
                );
            }
            other => panic!("Expected Compound for `git log -n 3 | cat -n`, got {other:?}"),
        }
    }

    /// Non-cat reader consumers (`head`, `wc`, `grep`) are outside the narrow
    /// `| cat` shape and must never trigger the reversal.
    ///
    /// Binary verdict: all three exit 1 (not rewritten).
    ///
    /// CONTROL: passes today (has_pipe_operator bails) and after the reversal
    /// (only bare `cat` with no args qualifies as the downstream stage).
    #[test]
    fn pipe_to_non_cat_reader_is_not_rewritten() {
        for cmd in [
            "git log -n 3 | head -5",
            "git log -n 3 | wc -l",
            "git log -n 3 | grep fix",
        ] {
            match split_compound(cmd) {
                CompoundSplitResult::Compound(segments) => {
                    assert!(
                        try_rewrite_compound(&segments).is_none(),
                        "`{cmd}` must not be rewritten (non-cat downstream)"
                    );
                }
                other => panic!("Expected Compound for `{cmd}`, got {other:?}"),
            }
        }
    }

    /// `echo $(git log -n 3 | cat)`: command substitution wraps the pipeline
    /// in `$(…)`.  Rule S fires in `command_needs_exact_bytes`; the outer
    /// command also bails via `rewrite_would_corrupt` (and `split_compound`
    /// returns Bail on `$(`).  The `| cat` inside the capture is never
    /// visible to the reversal.
    ///
    /// Binary verdict: exit 1 (not rewritten).
    ///
    /// CONTROL: passes today and after the reversal — the outer guards fire
    /// before `try_rewrite_compound` is ever reached.
    #[test]
    fn pipe_to_cat_inside_capture_is_not_rewritten() {
        assert!(
            rewrite_would_corrupt("echo $(git log -n 3 | cat)"),
            "Rule S: $( triggers the corruption guard"
        );
        assert!(
            command_needs_exact_bytes("echo $(git log -n 3 | cat)"),
            "Rule S: command substitution arms exact-bytes"
        );
        match split_compound("echo $(git log -n 3 | cat)") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for capture shape, got {other:?}"),
        }
    }

    /// `git log -n 3 && git status | cat`: 3-segment compound mixing `&&` and
    /// `|` operators.
    ///
    /// Binary verdict: exit 1 (not rewritten today).
    ///
    /// After the reversal this must STILL not be rewritten.  The narrow
    /// AD-RW-2 reversal applies only to pure 2-stage pipelines `<source> | cat`
    /// where the ENTIRE command is that shape (no interleaved `&&`/`||`/`;`).
    /// `&&` sequences ARE rewritten segment-by-segment today, but that
    /// existing path only fires on non-pipe compounds; introducing a mixed
    /// `&&` + `|` rewrite path would require new multi-operator logic that is
    /// out of scope for the narrow reversal.  Pinned as a safety invariant to
    /// prevent the reversal from silently growing beyond its approved shape.
    ///
    /// CONTROL: passes today (has_pipe_operator bails) and after the reversal
    /// (narrow shape check: more than 2 segments, and a non-Pipe operator
    /// precedes the `|`, so the shape is rejected).
    #[test]
    fn pipe_to_cat_after_and_sequence() {
        match split_compound("git log -n 3 && git status | cat") {
            CompoundSplitResult::Compound(segments) => {
                assert!(
                    try_rewrite_compound(&segments).is_none(),
                    "git log -n 3 && git status | cat must not be rewritten: \
                     the narrow AD-RW-2 reversal applies only to pure 2-stage \
                     `<source> | cat` pipelines; this 3-segment mixed-operator \
                     compound is outside the approved shape"
                );
            }
            other => {
                panic!("Expected Compound for `git log -n 3 && git status | cat`, got {other:?}")
            }
        }
    }
}
