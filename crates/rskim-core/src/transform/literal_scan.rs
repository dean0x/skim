//! Lexer-lite per-line literal scanner (#511).
//!
//! Truncation cuts source at line boundaries. Cutting *inside* a multi-line
//! string literal (or a Markdown fenced code block) produces output whose
//! remaining text is no longer what it looks like: the tail of a template
//! literal reads as code, an unterminated heredoc swallows the marker, a
//! half-emitted fence turns the rest of a document into a code block.
//!
//! This module answers one question, cheaply and without a parser:
//!
//! > *after* line `i`, is the file still inside a multi-line literal, and if
//! > so, on which line did that literal open?
//!
//! It is deliberately **not** a lexer. It builds no tokens, allocates nothing
//! per line, and never fails: the worst case on an unsupported or pathological
//! construct is a wrong-but-safe answer, never a panic.
//!
//! # Language table
//!
//! | Language        | Multi-line forms                       | Line-terminated | Comments     |
//! |-----------------|----------------------------------------|-----------------|--------------|
//! | TypeScript / JS | backtick template                      | `"` `'`         | `//` `/* */` |
//! | Python          | `"""` `'''`                            | `"` `'`         | `#`          |
//! | Rust            | `"`, raw strings with hash counting    | —               | `//` `/* */` |
//! | Go              | backtick raw string                    | `"`             | `//` `/* */` |
//! | Java            | `"""`                                  | `"`             | `//` `/* */` |
//! | C               | —                                      | `"` `'`         | `//` `/* */` |
//! | C++             | `R"delim( … )delim"`                   | `"`             | `//` `/* */` |
//! | C#              | `"""`, `@"…"`                          | `"`             | `//` `/* */` |
//! | Ruby            | `"` `'`, `%q( … )`, heredocs           | —               | `#`          |
//! | Kotlin / Swift  | `"""`                                  | `"`             | `//` `/* */` |
//! | Bash            | `"` `'`, heredocs                      | —               | `#`          |
//! | SQL             | `'` with `''` doubling                 | —               | `--` `/* */` |
//! | JSON            | — (RFC 8259 forbids raw newlines)      | —               | —            |
//! | YAML            | `"` `'` flow scalars                   | —               | `#`          |
//! | TOML            | `"""` `'''`                            | `"` `'`         | `#`          |
//! | Markdown        | fenced code blocks                     | —               | —            |
//!
//! # Deliberate simplifications
//!
//! Each trades a rare mis-read for a table that stays small and a scan that
//! stays linear. The visible ones are pinned by tests.
//!
//! - **Block comments are tracked but reported clean.** A block comment
//!   spanning lines stops the scanner opening a literal on a quote inside the
//!   comment, but its lines report `None`: a comment is not a literal, and an
//!   elision marker dropped inside one is still readable text.
//! - **Nested block comments** (Rust, Kotlin, Swift) are treated as
//!   non-nesting: the first closer wins. A nested comment therefore ends
//!   early, which can only *under*-report an open state.
//! - **YAML block scalars** (`|`, `>`) are not modelled. They close by dedent,
//!   so a column-0 elision marker is still valid YAML — there is nothing to
//!   protect. Modelling them would need indentation tracking for no gain.
//! - **Ruby `%q(…)` does not track nested brackets**, and only the
//!   `(` `[` `{` `<` delimiter pairs are recognised (not `%q|…|`).
//! - **Heredoc terminators are matched on the trimmed line** even for the
//!   non-indented form, and only the first heredoc on a line is tracked
//!   (`cat <<A <<B` follows `A` only). A bare `<<ID` must be `SCREAMING_CASE`
//!   so Ruby's `arr<<x` append operator is not misread as an opener.
//! - **Markdown info strings are not validated** (CommonMark forbids
//!   backticks in a backtick fence's info string); a closer must still match
//!   the opener's character and be at least as long.

use crate::Language;

/// Upper bound on `#` characters accepted in a Rust raw-string opener.
const MAX_RAW_HASHES: usize = 64;

/// Upper bound on a C++ raw-string delimiter (the standard caps it at 16).
const MAX_CPP_RAW_DELIM: usize = 16;

const C_LINE_COMMENT: &[&str] = &["//"];
const HASH_LINE_COMMENT: &[&str] = &["#"];
const SQL_LINE_COMMENT: &[&str] = &["--"];
const C_BLOCK_COMMENT: Option<(&str, &str)> = Some(("/*", "*/"));

// ============================================================================
// Public surface
// ============================================================================

/// Per-line literal state for a whole text, computed in one forward pass.
///
/// Index `i` is a 0-based line index with [`str::lines`] semantics: a trailing
/// newline does not create an extra line, so `""` has zero lines.
#[derive(Debug, Clone)]
pub(crate) struct LiteralScan {
    /// `open_after[i]` is `Some(open_idx)` iff line `i` ends inside a
    /// multi-line literal opened on line `open_idx` (`open_idx <= i`).
    open_after: Vec<Option<usize>>,
}

/// Scan `text` for per-line multi-line-literal state.
///
/// One forward pass over the bytes of `text`. Allocates exactly one
/// `Vec<Option<usize>>` of one entry per line, plus a small reusable scratch
/// buffer for dynamic terminators (heredoc words, C++ raw delimiters).
///
/// Never panics: every index is bounds-checked, every delimiter is ASCII (so
/// multi-byte UTF-8 can never match one), and the byte loop is bounded by the
/// line length.
pub(crate) fn scan(text: &str, language: Language) -> LiteralScan {
    let syntax = literal_syntax(language);
    let capacity = if text.is_empty() {
        0
    } else {
        text.split('\n').count()
    };
    let mut open_after = Vec::with_capacity(capacity);
    let mut state = State::Clean;
    let mut scratch = String::new();

    for (index, line) in text.lines().enumerate() {
        state = scan_line(line, index, state, &syntax, &mut scratch);
        open_after.push(state.open_line());
    }

    LiteralScan { open_after }
}

impl LiteralScan {
    /// Line on which the literal still open after line `i` was opened.
    ///
    /// `None` when the state is clean after line `i`, and for any `i` at or
    /// past [`LiteralScan::line_count`].
    pub(crate) fn open_after(&self, i: usize) -> Option<usize> {
        self.open_after.get(i).copied().flatten()
    }

    /// Index of the **first line after `i` whose `open_after` is `None`**.
    ///
    /// For a line inside a literal that is the line which closes it — a line
    /// carrying a closing delimiter ends clean, so the closer line itself is
    /// returned (never the line after it). `None` when no such line exists:
    /// the literal runs to the end of the text, or `i` is the last line or out
    /// of range.
    ///
    /// The definition is relative to `i` alone, so it is meaningful whether or
    /// not line `i` is inside a literal: for a clean line it returns the next
    /// clean line.
    pub(crate) fn close_line(&self, i: usize) -> Option<usize> {
        let start = i.checked_add(1)?;
        (start..self.line_count()).find(|line| self.open_after(*line).is_none())
    }

    /// Number of lines scanned ([`str::lines`] semantics).
    pub(crate) fn line_count(&self) -> usize {
        self.open_after.len()
    }
}

// ============================================================================
// Syntax table
// ============================================================================

/// Per-language literal syntax.
///
/// The delimiter and comment tables carry the general shapes; the booleans
/// switch on the handful of language-specific forms whose opener and closer
/// are not the same static string (Rust raw strings, C++ `R"d( … )d"`, C#
/// `@"…"`, Ruby `%q(…)`, heredocs, Markdown fences).
#[derive(Debug, Clone, Copy)]
struct LiteralSyntax {
    /// Tokens that comment out the rest of the line.
    line_comment: &'static [&'static str],
    /// `(open, close)` for block comments.
    block_comment: Option<(&'static str, &'static str)>,
    /// Symmetric delimiters whose literal may span lines. Longest match wins,
    /// so `"""` beats `"`.
    multi_line_delims: &'static [&'static str],
    /// Symmetric delimiters force-closed at end of line — a false-positive
    /// suppressor for apostrophes and stray quotes.
    line_terminated_delims: &'static [&'static str],
    /// Delimiters from either table inside which backslash is *not* an escape
    /// (Go backticks, Bash and TOML single quotes).
    raw_delims: &'static [&'static str],
    /// Backslash escapes the next byte inside a literal.
    escape_char: bool,
    /// A doubled delimiter (`''`) inside a literal is an escaped delimiter,
    /// not a close.
    doubled_delim_escape: bool,
    /// An opening delimiter must sit at a value position (start of line, or
    /// after whitespace or one of `:` `-` `,` `[` `{`). YAML only: plain
    /// scalars carry apostrophes (`don't`) that must not open a literal, and
    /// YAML has no string prefixes that the rule would break.
    quote_at_value_start: bool,
    /// Rust raw strings: `r"…"` / `r#"…"#` (hash-counted, no escapes).
    rust_raw: bool,
    /// C++ `R"delim( … )delim"` (delimiter-matched, no escapes).
    cpp_raw: bool,
    /// C# `@"…"` verbatim strings (no escapes, `""` doubling).
    csharp_verbatim: bool,
    /// Ruby `%q( … )` and friends.
    percent_literal: bool,
    /// `<<ID` / `<<-ID` / `<<~ID` heredocs.
    heredoc: bool,
    /// Markdown fenced code blocks.
    markdown_fence: bool,
}

impl LiteralSyntax {
    /// No literal forms at all — the row for formats that cannot carry a
    /// multi-line literal, and the base every other row is built from.
    const NONE: Self = Self {
        line_comment: &[],
        block_comment: None,
        multi_line_delims: &[],
        line_terminated_delims: &[],
        raw_delims: &[],
        escape_char: false,
        doubled_delim_escape: false,
        quote_at_value_start: false,
        rust_raw: false,
        cpp_raw: false,
        csharp_verbatim: false,
        percent_literal: false,
        heredoc: false,
        markdown_fence: false,
    };
}

/// Literal syntax for `language`.
///
/// Matched exhaustively on purpose: a new [`Language`] variant must get a row
/// (or an explicitly empty one) rather than silently inheriting a default.
fn literal_syntax(language: Language) -> LiteralSyntax {
    match language {
        Language::TypeScript | Language::JavaScript => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            multi_line_delims: &["`"],
            line_terminated_delims: &["\"", "'"],
            escape_char: true,
            ..LiteralSyntax::NONE
        },
        // String prefixes (r, b, f, rb, …) do not change delimiter matching:
        // the scan keys on the quote, which follows the prefix.
        Language::Python => LiteralSyntax {
            line_comment: HASH_LINE_COMMENT,
            multi_line_delims: &["\"\"\"", "'''"],
            line_terminated_delims: &["\"", "'"],
            escape_char: true,
            ..LiteralSyntax::NONE
        },
        // Char literals are omitted: Rust lifetimes (`'a`) would open one on
        // every generic parameter, and a char literal cannot span lines.
        Language::Rust => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            multi_line_delims: &["\""],
            escape_char: true,
            rust_raw: true,
            ..LiteralSyntax::NONE
        },
        Language::Go => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            multi_line_delims: &["`"],
            line_terminated_delims: &["\""],
            raw_delims: &["`"],
            escape_char: true,
            ..LiteralSyntax::NONE
        },
        Language::Java => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            multi_line_delims: &["\"\"\""],
            line_terminated_delims: &["\""],
            escape_char: true,
            ..LiteralSyntax::NONE
        },
        Language::C => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            line_terminated_delims: &["\"", "'"],
            escape_char: true,
            ..LiteralSyntax::NONE
        },
        Language::Cpp => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            line_terminated_delims: &["\""],
            escape_char: true,
            cpp_raw: true,
            ..LiteralSyntax::NONE
        },
        Language::CSharp => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            multi_line_delims: &["\"\"\""],
            line_terminated_delims: &["\""],
            escape_char: true,
            csharp_verbatim: true,
            ..LiteralSyntax::NONE
        },
        Language::Ruby => LiteralSyntax {
            line_comment: HASH_LINE_COMMENT,
            multi_line_delims: &["\"", "'"],
            escape_char: true,
            percent_literal: true,
            heredoc: true,
            ..LiteralSyntax::NONE
        },
        Language::Kotlin | Language::Swift => LiteralSyntax {
            line_comment: C_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            multi_line_delims: &["\"\"\""],
            line_terminated_delims: &["\""],
            escape_char: true,
            ..LiteralSyntax::NONE
        },
        Language::Bash => LiteralSyntax {
            line_comment: HASH_LINE_COMMENT,
            multi_line_delims: &["\"", "'"],
            raw_delims: &["'"],
            escape_char: true,
            heredoc: true,
            ..LiteralSyntax::NONE
        },
        // Standard SQL escapes a quote by doubling it; backslash is literal.
        Language::Sql => LiteralSyntax {
            line_comment: SQL_LINE_COMMENT,
            block_comment: C_BLOCK_COMMENT,
            multi_line_delims: &["'"],
            doubled_delim_escape: true,
            ..LiteralSyntax::NONE
        },
        Language::Yaml => LiteralSyntax {
            line_comment: HASH_LINE_COMMENT,
            multi_line_delims: &["\"", "'"],
            raw_delims: &["'"],
            escape_char: true,
            doubled_delim_escape: true,
            quote_at_value_start: true,
            ..LiteralSyntax::NONE
        },
        Language::Toml => LiteralSyntax {
            line_comment: HASH_LINE_COMMENT,
            multi_line_delims: &["\"\"\"", "'''"],
            line_terminated_delims: &["\"", "'"],
            raw_delims: &["'''", "'"],
            escape_char: true,
            ..LiteralSyntax::NONE
        },
        Language::Markdown => LiteralSyntax {
            markdown_fence: true,
            ..LiteralSyntax::NONE
        },
        // RFC 8259 forbids raw newlines inside strings, so a JSON string can
        // never span lines and the scan is a no-op.
        Language::Json => LiteralSyntax::NONE,
    }
}

// ============================================================================
// Scanner state
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not inside anything.
    Clean,
    /// Inside a block comment (reported as clean — see the module docs).
    BlockComment,
    /// Inside a literal closed by a static delimiter.
    Str {
        delim: &'static str,
        open: usize,
        /// `false` for line-terminated delimiters: force-closed at end of line
        /// unless the line ended on a backslash continuation.
        multi: bool,
        escapes: bool,
        doubled: bool,
    },
    /// Inside a Rust raw string closed by `"` followed by `hashes` hashes.
    RustRaw { hashes: usize, open: usize },
    /// Inside a C++ raw string; the `)delim"` terminator lives in the scratch.
    CppRaw { open: usize },
    /// Inside a heredoc body; the terminator word lives in the scratch.
    Heredoc { open: usize },
    /// Inside a Markdown fenced code block.
    Fence { ch: u8, run: usize, open: usize },
}

impl State {
    /// The line this state opened on, or `None` when it is not a literal.
    const fn open_line(self) -> Option<usize> {
        match self {
            Self::Clean | Self::BlockComment => None,
            Self::Str { open, .. }
            | Self::RustRaw { open, .. }
            | Self::CppRaw { open }
            | Self::Heredoc { open }
            | Self::Fence { open, .. } => Some(open),
        }
    }
}

// ============================================================================
// Line scanning
// ============================================================================

/// Advance the scanner across one line, returning the state at end of line.
fn scan_line(
    line: &str,
    index: usize,
    entry: State,
    syn: &LiteralSyntax,
    scratch: &mut String,
) -> State {
    if syn.markdown_fence {
        return scan_fence_line(line, index, entry);
    }

    // Heredoc bodies are opaque and close on a whole-line terminator match.
    if matches!(entry, State::Heredoc { .. }) {
        if line.trim() == scratch.as_str() {
            scratch.clear();
            return State::Clean;
        }
        return entry;
    }

    let bytes = line.as_bytes();
    let mut state = entry;
    let mut position = 0usize;
    let mut pending_heredoc = false;
    let mut pending_escape = false;
    // Bound: every branch advances `position` by at least one byte, so the
    // loop runs at most once per byte. The budget enforces that regardless of
    // how the branches below are edited.
    let mut budget = bytes.len();

    while position < bytes.len() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        let start = position;

        match state {
            State::Clean => {
                if starts_comment(bytes, position, syn) {
                    break; // the rest of the line is a comment
                }
                if let Some(open) = block_open(bytes, position, syn) {
                    state = State::BlockComment;
                    position = position.saturating_add(open.len());
                    continue;
                }
                let rust_raw = syn
                    .rust_raw
                    .then(|| rust_raw_open(bytes, position))
                    .flatten();
                if let Some((hashes, next)) = rust_raw {
                    state = State::RustRaw {
                        hashes,
                        open: index,
                    };
                    position = next;
                    continue;
                }
                let cpp_raw = syn
                    .cpp_raw
                    .then(|| cpp_raw_open(bytes, position, scratch))
                    .flatten();
                if let Some(next) = cpp_raw {
                    state = State::CppRaw { open: index };
                    position = next;
                    continue;
                }
                if syn.csharp_verbatim && at(bytes, position, b"@\"") {
                    state = State::Str {
                        delim: "\"",
                        open: index,
                        multi: true,
                        escapes: false,
                        doubled: true,
                    };
                    position = position.saturating_add(2);
                    continue;
                }
                let percent = syn
                    .percent_literal
                    .then(|| percent_open(bytes, position))
                    .flatten();
                if let Some((close, next)) = percent {
                    state = State::Str {
                        delim: close,
                        open: index,
                        multi: true,
                        escapes: syn.escape_char,
                        doubled: false,
                    };
                    position = next;
                    continue;
                }
                let heredoc = (syn.heredoc && !pending_heredoc)
                    .then(|| heredoc_open(bytes, position, scratch))
                    .flatten();
                if let Some(next) = heredoc {
                    pending_heredoc = true;
                    position = next;
                    continue;
                }
                if let Some(delim) = open_delim(bytes, position, syn.multi_line_delims, syn) {
                    state = open_string(syn, delim, true, index);
                    position = position.saturating_add(delim.len());
                    continue;
                }
                if let Some(delim) = open_delim(bytes, position, syn.line_terminated_delims, syn) {
                    state = open_string(syn, delim, false, index);
                    position = position.saturating_add(delim.len());
                    continue;
                }
                position = position.saturating_add(1);
            }
            State::BlockComment => {
                let close = syn.block_comment.map_or("", |(_, close)| close);
                if !close.is_empty() && at(bytes, position, close.as_bytes()) {
                    state = State::Clean;
                    position = position.saturating_add(close.len());
                } else {
                    position = position.saturating_add(1);
                }
            }
            State::Str {
                delim,
                escapes,
                doubled,
                ..
            } => {
                if escapes && bytes.get(position) == Some(&b'\\') {
                    let next = position.saturating_add(2);
                    if next <= bytes.len() {
                        position = next;
                    } else {
                        // Trailing backslash: the escape consumes the newline,
                        // so even a line-terminated literal survives onto the
                        // next line (C string continuation).
                        pending_escape = true;
                        position = bytes.len();
                    }
                    continue;
                }
                if at(bytes, position, delim.as_bytes()) {
                    let after = position.saturating_add(delim.len());
                    if doubled && at(bytes, after, delim.as_bytes()) {
                        position = after.saturating_add(delim.len());
                        continue;
                    }
                    state = State::Clean;
                    position = after;
                    continue;
                }
                position = position.saturating_add(1);
            }
            State::RustRaw { hashes, .. } => {
                let closes = bytes.get(position) == Some(&b'"')
                    && hash_run(bytes, position.saturating_add(1)) >= hashes;
                if closes {
                    state = State::Clean;
                    position = position.saturating_add(1).saturating_add(hashes);
                } else {
                    position = position.saturating_add(1);
                }
            }
            State::CppRaw { .. } => {
                if !scratch.is_empty() && at(bytes, position, scratch.as_bytes()) {
                    state = State::Clean;
                    position = position.saturating_add(scratch.len());
                    scratch.clear();
                } else {
                    position = position.saturating_add(1);
                }
            }
            // Neither state is entered mid-line: both are resolved above.
            State::Heredoc { .. } | State::Fence { .. } => position = bytes.len(),
        }

        if position <= start {
            position = start.saturating_add(1);
        }
    }

    let mut state = match state {
        State::Str { multi: false, .. } if !pending_escape => State::Clean,
        other => other,
    };
    if pending_heredoc && matches!(state, State::Clean) {
        state = State::Heredoc { open: index };
    }
    state
}

/// Markdown fence toggling. Inside a fence nothing else is interpreted.
fn scan_fence_line(line: &str, index: usize, entry: State) -> State {
    match entry {
        State::Fence { ch, run, open } => match fence_marker(line) {
            Some(marker) if marker.closes(ch, run) => State::Clean,
            _ => State::Fence { ch, run, open },
        },
        _ => match fence_marker(line) {
            Some(marker) => State::Fence {
                ch: marker.ch,
                run: marker.run,
                open: index,
            },
            None => State::Clean,
        },
    }
}

/// A backtick or tilde run at the start of a line (at most 3 leading spaces).
struct FenceMarker {
    ch: u8,
    run: usize,
    /// Nothing but whitespace follows the run — required of a closer.
    blank_tail: bool,
}

impl FenceMarker {
    /// A closer uses the opener's character, is at least as long, and carries
    /// no info string.
    const fn closes(&self, ch: u8, run: usize) -> bool {
        self.ch == ch && self.run >= run && self.blank_tail
    }
}

fn fence_marker(line: &str) -> Option<FenceMarker> {
    let bytes = line.as_bytes();
    let mut position = 0usize;
    while bytes.get(position) == Some(&b' ') {
        position = position.saturating_add(1);
    }
    if position > 3 {
        return None; // indented code block, not a fence
    }
    let ch = match bytes.get(position).copied() {
        Some(byte) if byte == b'`' || byte == b'~' => byte,
        _ => return None,
    };
    let start = position;
    while bytes.get(position) == Some(&ch) {
        position = position.saturating_add(1);
    }
    let run = position.saturating_sub(start);
    if run < 3 {
        return None;
    }
    let blank_tail = bytes
        .get(position..)
        .is_some_and(|tail| tail.iter().all(u8::is_ascii_whitespace));
    Some(FenceMarker {
        ch,
        run,
        blank_tail,
    })
}

// ============================================================================
// Opener recognition
// ============================================================================

fn open_string(syn: &LiteralSyntax, delim: &'static str, multi: bool, open: usize) -> State {
    State::Str {
        delim,
        open,
        multi,
        escapes: syn.escape_char && !syn.raw_delims.contains(&delim),
        doubled: syn.doubled_delim_escape,
    }
}

/// Whether a line comment starts here.
fn starts_comment(bytes: &[u8], position: usize, syn: &LiteralSyntax) -> bool {
    let Some(token) = longest_match(bytes, position, syn.line_comment) else {
        return false;
    };
    !needs_word_start(token) || is_word_start(bytes, position)
}

/// The block-comment opener starting here, if any.
fn block_open(bytes: &[u8], position: usize, syn: &LiteralSyntax) -> Option<&'static str> {
    let (open, _) = syn.block_comment?;
    at(bytes, position, open.as_bytes()).then_some(open)
}

/// Longest delimiter from `table` starting here, subject to the language's
/// value-start rule.
fn open_delim(
    bytes: &[u8],
    position: usize,
    table: &[&'static str],
    syn: &LiteralSyntax,
) -> Option<&'static str> {
    let delim = longest_match(bytes, position, table)?;
    if syn.quote_at_value_start && !is_value_start(bytes, position) {
        return None;
    }
    Some(delim)
}

/// Rust `r"…"` / `r#"…"#` / `br#"…"#` opener: hash count and the index just
/// past the opening quote.
fn rust_raw_open(bytes: &[u8], position: usize) -> Option<(usize, usize)> {
    if bytes.get(position) != Some(&b'r') {
        return None;
    }
    // `br"…"` is a byte raw string, so a `b` may precede the `r`; any other
    // identifier byte means this `r` is part of a name (`for`, `expr`).
    let mut back = position;
    if prev_byte(bytes, back) == Some(b'b') {
        back = back.saturating_sub(1);
    }
    if prev_byte(bytes, back).is_some_and(is_ident_byte) {
        return None;
    }
    let hashes = hash_run(bytes, position.saturating_add(1));
    if hashes > MAX_RAW_HASHES {
        return None;
    }
    let quote = position.saturating_add(1).saturating_add(hashes);
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    Some((hashes, quote.saturating_add(1)))
}

/// C++ `R"delim( … )delim"` opener. Writes the `)delim"` terminator into
/// `scratch` and returns the index just past the opening parenthesis.
fn cpp_raw_open(bytes: &[u8], position: usize, scratch: &mut String) -> Option<usize> {
    if !at(bytes, position, b"R\"") {
        return None;
    }
    // Encoding prefixes (`L`, `u`, `U`, `u8`) may precede the `R`.
    let named = prev_byte(bytes, position)
        .is_some_and(|byte| is_ident_byte(byte) && !matches!(byte, b'L' | b'u' | b'U' | b'8'));
    if named {
        return None;
    }
    let start = position.saturating_add(2);
    let mut cursor = start;
    while cursor < bytes.len() && cursor.saturating_sub(start) <= MAX_CPP_RAW_DELIM {
        let byte = *bytes.get(cursor)?;
        if byte == b'(' {
            scratch.clear();
            scratch.push(')');
            for delim_byte in bytes.get(start..cursor).unwrap_or_default() {
                scratch.push(char::from(*delim_byte));
            }
            scratch.push('"');
            return Some(cursor.saturating_add(1));
        }
        if !byte.is_ascii_graphic() || byte == b')' || byte == b'\\' {
            return None;
        }
        cursor = cursor.saturating_add(1);
    }
    None
}

/// Ruby `%q( … )` and friends: the closing delimiter and the index just past
/// the opener.
fn percent_open(bytes: &[u8], position: usize) -> Option<(&'static str, usize)> {
    if bytes.get(position) != Some(&b'%') {
        return None;
    }
    // `x % (y)` is modulo, not a literal.
    let modulo = prev_byte(bytes, position)
        .is_some_and(|byte| is_ident_byte(byte) || byte == b')' || byte == b']');
    if modulo {
        return None;
    }
    let mut cursor = position.saturating_add(1);
    let typed = matches!(
        bytes.get(cursor).copied(),
        Some(b'q' | b'Q' | b'w' | b'W' | b'i' | b'I' | b'r' | b's')
    );
    if typed {
        cursor = cursor.saturating_add(1);
    }
    let close = match bytes.get(cursor).copied() {
        Some(b'(') => ")",
        Some(b'[') => "]",
        Some(b'{') => "}",
        Some(b'<') => ">",
        _ => return None,
    };
    Some((close, cursor.saturating_add(1)))
}

/// `<<ID` / `<<-ID` / `<<~ID`, optionally quoted. Writes the terminator word
/// into `scratch` and returns the index just past the opener.
fn heredoc_open(bytes: &[u8], position: usize, scratch: &mut String) -> Option<usize> {
    if !at(bytes, position, b"<<") {
        return None;
    }
    let mut cursor = position.saturating_add(2);
    let indented = matches!(bytes.get(cursor).copied(), Some(b'-' | b'~'));
    if indented {
        cursor = cursor.saturating_add(1);
    }
    let quote = match bytes.get(cursor).copied() {
        Some(byte) if byte == b'\'' || byte == b'"' => {
            cursor = cursor.saturating_add(1);
            Some(byte)
        }
        _ => None,
    };
    let start = cursor;
    while bytes.get(cursor).copied().is_some_and(is_ident_byte) {
        cursor = cursor.saturating_add(1);
    }
    let word = bytes.get(start..cursor)?;
    let first = *word.first()?;
    if !first.is_ascii_alphabetic() && first != b'_' {
        return None;
    }
    // A bare `<<ID` must look like a heredoc tag, or Ruby's `arr<<x` append
    // operator opens a literal that never closes.
    let bare_lowercase = !indented && quote.is_none() && first.is_ascii_lowercase();
    if bare_lowercase {
        return None;
    }
    match quote {
        Some(byte) if bytes.get(cursor) == Some(&byte) => cursor = cursor.saturating_add(1),
        Some(_) => return None,
        None => {}
    }
    scratch.clear();
    for word_byte in word {
        scratch.push(char::from(*word_byte));
    }
    Some(cursor)
}

// ============================================================================
// Byte helpers (ASCII only — multi-byte UTF-8 never matches a delimiter)
// ============================================================================

fn at(bytes: &[u8], position: usize, needle: &[u8]) -> bool {
    !needle.is_empty()
        && bytes
            .get(position..)
            .is_some_and(|rest| rest.starts_with(needle))
}

fn longest_match(bytes: &[u8], position: usize, table: &[&'static str]) -> Option<&'static str> {
    let mut best: Option<&'static str> = None;
    for candidate in table.iter().copied() {
        let longer = best.is_none_or(|current| candidate.len() > current.len());
        if longer && at(bytes, position, candidate.as_bytes()) {
            best = Some(candidate);
        }
    }
    best
}

fn hash_run(bytes: &[u8], position: usize) -> usize {
    let mut cursor = position;
    while bytes.get(cursor) == Some(&b'#') {
        cursor = cursor.saturating_add(1);
    }
    cursor.saturating_sub(position)
}

fn prev_byte(bytes: &[u8], position: usize) -> Option<u8> {
    position
        .checked_sub(1)
        .and_then(|index| bytes.get(index))
        .copied()
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A `#` only starts a comment at the start of a word: YAML's `a: b#c` and
/// Bash's `${#arr}` are not comments. `//` and `--` carry no such rule.
fn needs_word_start(token: &str) -> bool {
    token == "#"
}

fn is_word_start(bytes: &[u8], position: usize) -> bool {
    prev_byte(bytes, position).is_none_or(|byte| byte.is_ascii_whitespace())
}

/// A YAML quoted scalar starts a value; the apostrophe in `don't` does not.
fn is_value_start(bytes: &[u8], position: usize) -> bool {
    let Some(byte) = prev_byte(bytes, position) else {
        return true;
    };
    byte.is_ascii_whitespace() || matches!(byte, b':' | b'-' | b',' | b'[' | b'{')
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ts_template_literal_spanning_six_lines_marks_every_line_until_the_closer() {
        let source = "const q = `\nzero\none\ntwo\nthree\n`;\nconst after = 1;\n";
        let scan = scan(source, Language::TypeScript);

        assert_eq!(scan.line_count(), 7);
        for line in 0..=4 {
            assert_eq!(scan.open_after(line), Some(0), "line {line} is inside");
        }
        assert_eq!(scan.open_after(5), None, "closer line ends clean");
        assert_eq!(scan.open_after(6), None);
    }

    #[test]
    fn python_triple_quoted_docstring_marks_its_interior() {
        let source = "def f():\n    \"\"\"\n    doc\n    \"\"\"\n    return 1\n";
        let scan = scan(source, Language::Python);

        assert_eq!(scan.open_after(0), None);
        assert_eq!(scan.open_after(1), Some(1));
        assert_eq!(scan.open_after(2), Some(1));
        assert_eq!(scan.open_after(3), None);
        assert_eq!(scan.open_after(4), None);
    }

    #[test]
    fn python_apostrophe_in_prose_does_not_open_a_literal() {
        let source = "# it's fine\nx = 'it'\ny = 2\n";
        let scan = scan(source, Language::Python);

        assert_eq!(scan.open_after(0), None);
        assert_eq!(scan.open_after(1), None);
        assert_eq!(scan.open_after(2), None);
    }

    #[test]
    fn rust_raw_string_spans_lines_and_an_inner_quote_does_not_close_it() {
        let source = "let s = r#\"\na \"quoted\" word\n\"#;\nlet t = 1;\n";
        let scan = scan(source, Language::Rust);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn rust_line_comment_containing_a_quote_does_not_open_a_literal() {
        let source = "// say \"hi\nlet x = 1;\n";
        let scan = scan(source, Language::Rust);

        assert_eq!(scan.open_after(0), None);
        assert_eq!(scan.open_after(1), None);
    }

    #[test]
    fn go_raw_backtick_string_spans_lines() {
        let source = "const s = `\nraw \\ text\n`\nvar x = 1\n";
        let scan = scan(source, Language::Go);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn ruby_squiggly_heredoc_closes_on_its_terminator_line() {
        let source = "sql = <<~SQL\n  SELECT 1\nSQL\nputs sql\n";
        let scan = scan(source, Language::Ruby);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn ruby_append_operator_is_not_read_as_a_heredoc() {
        let source = "list = []\nlist<<item\nputs list\n";
        let scan = scan(source, Language::Ruby);

        assert_eq!(scan.open_after(1), None);
        assert_eq!(scan.open_after(2), None);
    }

    #[test]
    fn bash_heredoc_body_stays_open_until_the_terminator() {
        let source = "cat <<EOF\nhello\nEOF\necho done\n";
        let scan = scan(source, Language::Bash);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn bash_quoted_heredoc_tag_does_not_open_a_string() {
        let source = "cat <<'EOF'\n$not expanded\nEOF\necho done\n";
        let scan = scan(source, Language::Bash);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn sql_doubled_quote_escape_does_not_close_a_multi_line_string() {
        let source = "SELECT 'it''s\nstill open' FROM t;\nSELECT 1;\n";
        let scan = scan(source, Language::Sql);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), None);
        assert_eq!(scan.open_after(2), None);
    }

    #[test]
    fn toml_triple_quoted_string_spans_lines() {
        let source = "key = \"\"\"\nvalue\n\"\"\"\nother = 1\n";
        let scan = scan(source, Language::Toml);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn yaml_flow_double_quoted_scalar_spans_lines() {
        let source = "key: \"first\n  second\"\nnext: 1\n";
        let scan = scan(source, Language::Yaml);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), None);
        assert_eq!(scan.open_after(2), None);
    }

    #[test]
    fn yaml_block_scalar_is_deliberately_not_modelled() {
        let source = "key: |\n  line one\n  line two\nnext: 1\n";
        let scan = scan(source, Language::Yaml);

        for line in 0..scan.line_count() {
            assert_eq!(scan.open_after(line), None, "line {line} must read clean");
        }
    }

    #[test]
    fn yaml_apostrophe_in_a_plain_scalar_does_not_open_a_literal() {
        let source = "note: don't panic\nnext: 1\n";
        let scan = scan(source, Language::Yaml);

        assert_eq!(scan.open_after(0), None);
        assert_eq!(scan.open_after(1), None);
    }

    #[test]
    fn json_never_reports_an_open_literal() {
        let source = "{\n  \"a\": \"unterminated\n}\n";
        let scan = scan(source, Language::Json);

        assert_eq!(scan.line_count(), 3);
        for line in 0..scan.line_count() {
            assert_eq!(scan.open_after(line), None);
        }
    }

    #[test]
    fn markdown_backtick_fence_opens_and_closes() {
        let source = "text\n```rust\nlet x = 1;\n```\nafter\n";
        let scan = scan(source, Language::Markdown);

        assert_eq!(scan.open_after(0), None);
        assert_eq!(scan.open_after(1), Some(1));
        assert_eq!(scan.open_after(2), Some(1));
        assert_eq!(scan.open_after(3), None);
        assert_eq!(scan.open_after(4), None);
    }

    #[test]
    fn markdown_tilde_fence_is_not_closed_by_a_backtick_line() {
        let source = "~~~\ninside\n```\nstill inside\n~~~\nafter\n";
        let scan = scan(source, Language::Markdown);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(2), Some(0));
        assert_eq!(scan.open_after(3), Some(0));
        assert_eq!(scan.open_after(4), None);
        assert_eq!(scan.open_after(5), None);
    }

    #[test]
    fn markdown_four_backtick_fence_is_not_closed_by_three_backticks() {
        let source = "````\n```\nstill inside\n````\nafter\n";
        let scan = scan(source, Language::Markdown);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), Some(0));
        assert_eq!(scan.open_after(3), None);
        assert_eq!(scan.open_after(4), None);
    }

    #[test]
    fn close_line_returns_the_first_clean_line_after_the_index() {
        let source = "const q = `\nzero\none\ntwo\nthree\n`;\nconst after = 1;\n";
        let scan = scan(source, Language::TypeScript);

        assert_eq!(scan.close_line(0), Some(5), "the closing-delimiter line");
        assert_eq!(scan.close_line(3), Some(5));
        assert_eq!(scan.close_line(5), Some(6), "a clean line yields the next");
        assert_eq!(scan.close_line(6), None, "no line follows the last");
    }

    #[test]
    fn close_line_is_none_when_the_literal_runs_to_the_end_of_the_text() {
        let source = "const q = `\nzero\none\n";
        let scan = scan(source, Language::TypeScript);

        assert_eq!(scan.close_line(0), None);
    }

    #[test]
    fn out_of_range_indexes_report_none() {
        let scan = scan("let x = 1;\n", Language::Rust);

        assert_eq!(scan.line_count(), 1);
        assert_eq!(scan.open_after(1), None);
        assert_eq!(scan.open_after(usize::MAX), None);
        assert_eq!(scan.close_line(usize::MAX), None);
    }

    #[test]
    fn empty_text_has_no_lines() {
        let scan = scan("", Language::Rust);

        assert_eq!(scan.line_count(), 0);
        assert_eq!(scan.open_after(0), None);
        assert_eq!(scan.close_line(0), None);
    }

    #[test]
    fn line_terminated_quote_is_force_closed_at_end_of_line() {
        let source = "const s = \"oops\nconst t = 1;\n";
        let scan = scan(source, Language::TypeScript);

        assert_eq!(scan.open_after(0), None);
        assert_eq!(scan.open_after(1), None);
    }

    #[test]
    fn c_trailing_backslash_continues_a_string_onto_the_next_line() {
        let source = "const char *s = \"abc\\\ndef\";\nint x = 0;\n";
        let scan = scan(source, Language::C);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), None);
        assert_eq!(scan.open_after(2), None);
    }

    #[test]
    fn multi_line_block_comment_hides_quotes_and_reports_clean() {
        let source = "/* a \" quote\n   still comment */\nint x = 0;\n";
        let scan = scan(source, Language::C);

        for line in 0..scan.line_count() {
            assert_eq!(scan.open_after(line), None, "line {line} is a comment");
        }
    }

    #[test]
    fn csharp_verbatim_string_spans_lines() {
        let source = "var p = @\"C:\\one\nC:\\two\";\nvar q = 1;\n";
        let scan = scan(source, Language::CSharp);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), None);
        assert_eq!(scan.open_after(2), None);
    }

    #[test]
    fn cpp_raw_string_closes_only_on_its_own_delimiter() {
        let source = "auto s = R\"tag(\nplain )\" inside\n)tag\";\nint x = 0;\n";
        let scan = scan(source, Language::Cpp);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn ruby_percent_literal_spans_lines() {
        let source = "words = %w(\n  one two\n)\nputs words\n";
        let scan = scan(source, Language::Ruby);

        assert_eq!(scan.open_after(0), Some(0));
        assert_eq!(scan.open_after(1), Some(0));
        assert_eq!(scan.open_after(2), None);
        assert_eq!(scan.open_after(3), None);
    }

    #[test]
    fn multi_byte_text_scans_without_panicking_and_stays_clean() {
        let source = "// ünïcödé — “quotes”\nlet x = \"日本語\";\nlet y = 1;\n";
        let scan = scan(source, Language::Rust);

        assert_eq!(scan.line_count(), 3);
        for line in 0..scan.line_count() {
            assert_eq!(scan.open_after(line), None);
        }
    }
}
