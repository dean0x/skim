//! The line matcher of `skim search`: one compiled pattern, the case rules and
//! the whole-word rule.
//!
//! The `regex` crate is the only engine. A literal pattern is escaped first and
//! goes through the same engine, so a literal and a regex differ only in the
//! escaping. Every compile runs under the size limits below: a pattern that
//! needs more is a compile error, never a long run or an allocation bound by
//! the caller.
//!
//! The unit of a match is a line. [`Matcher::matches`] finds every match on a
//! line; [`Matcher::is_match`] only asks whether there is one.
//!
//! # Whole words (`-w`)
//!
//! A match counts when the character before it and the character after it (if
//! any) are not word characters. The word characters are those of the `regex`
//! crate's Unicode `\w`: letters, marks, decimal digits, connector punctuation
//! (`_` too) and join controls.
//!
//! The rule is part of the compiled pattern, as `rg -w` builds it since
//! ripgrep 14: `\b{start-half}(?:PATTERN)\b{end-half}`. Each assertion reads
//! one character and nothing else. So the engine looks for a match that meets
//! the rule, and does not check a match that it found first:
//!
//! - a search of a line costs the same with `-w` as without it;
//! - an alternative can take its longer branch: `foo|foobar` matches `foobar`;
//! - a pattern that starts or ends with a non-word character is read at the
//!   character next to the match, as `rg -w` reads it.
//!
//! The lines that match are the lines `rg -nw` prints. `git grep -w` differs on
//! text that is not ASCII: it takes every byte from 0x80 up as a non-word
//! character, so it matches `cafe` in `cafeé`. This matcher follows `rg`.

use std::ops::Range;

use memchr::memmem;
use regex::{Regex, RegexBuilder};

/// Compiled size limit of a pattern (10 MiB).
pub(super) const REGEX_SIZE_LIMIT: usize = 10 * 1024 * 1024;

/// Size limit of the lazy DFA cache of a pattern (2 MiB).
pub(super) const REGEX_DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;

/// Nesting limit of a pattern's syntax tree.
pub(super) const REGEX_NEST_LIMIT: u32 = 250;

/// How the pattern text is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatternKind {
    /// The text is escaped, so it matches itself.
    Literal,
    /// The text is a `regex` crate pattern.
    Regex,
}

/// The case rule of a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaseMode {
    /// Case matters.
    Sensitive,
    /// Case does not matter.
    Insensitive,
    /// Case does not matter when no character of the pattern is uppercase.
    Smart,
}

impl CaseMode {
    /// Whether a search of `pattern` under this rule ignores case.
    pub(super) fn is_insensitive(self, pattern: &str) -> bool {
        match self {
            CaseMode::Sensitive => false,
            CaseMode::Insensitive => true,
            CaseMode::Smart => !pattern.chars().any(char::is_uppercase),
        }
    }
}

/// What a [`Matcher`] is built from.
#[derive(Debug, Clone, Copy)]
pub(super) struct MatcherSpec<'a> {
    pub(super) pattern: &'a str,
    pub(super) kind: PatternKind,
    pub(super) case: CaseMode,
    /// Apply the whole-word rule.
    pub(super) word: bool,
}

/// Why a pattern cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MatcherError {
    /// A literal pattern with no text.
    EmptyPattern,
    /// The pattern does not compile, or compiles past a size limit.
    Invalid { pattern: String, detail: String },
}

impl std::fmt::Display for MatcherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatcherError::EmptyPattern => f.write_str("the pattern is empty"),
            MatcherError::Invalid { pattern, detail } => {
                write!(f, "invalid pattern {pattern:?}: {detail}")
            }
        }
    }
}

impl std::error::Error for MatcherError {}

/// A compiled pattern with its case and whole-word rules.
#[derive(Debug, Clone)]
pub(super) struct Matcher {
    regex: Regex,
    /// A byte-exact finder for a case-sensitive literal. A text without the
    /// literal has no match, so the finder rejects whole files before any line
    /// is read.
    prefilter: Option<memmem::Finder<'static>>,
    insensitive: bool,
}

impl Matcher {
    /// Compile `spec`.
    ///
    /// # Errors
    ///
    /// [`MatcherError::EmptyPattern`] for an empty literal (an empty regex is
    /// allowed and matches every line); [`MatcherError::Invalid`] when the
    /// pattern does not compile or exceeds a size limit.
    pub(super) fn new(spec: MatcherSpec<'_>) -> Result<Self, MatcherError> {
        if spec.kind == PatternKind::Literal && spec.pattern.is_empty() {
            return Err(MatcherError::EmptyPattern);
        }
        let insensitive = spec.case.is_insensitive(spec.pattern);
        let source = match spec.kind {
            PatternKind::Literal => regex::escape(spec.pattern),
            PatternKind::Regex => spec.pattern.to_owned(),
        };
        let invalid = |e: regex::Error| MatcherError::Invalid {
            pattern: spec.pattern.to_owned(),
            detail: last_line(&e.to_string()),
        };
        // The pattern alone is compiled first, also under `-w`. A pattern with
        // an unbalanced group, such as `a)|(b`, would compile inside the word
        // wrapper as a different pattern.
        let plain = compile(&source, insensitive).map_err(invalid)?;
        let regex = if spec.word {
            compile_whole_word(&source, insensitive).map_err(invalid)?
        } else {
            plain
        };
        let prefilter = (spec.kind == PatternKind::Literal && !insensitive)
            .then(|| memmem::Finder::new(spec.pattern.as_bytes()).into_owned());
        Ok(Matcher {
            regex,
            prefilter,
            insensitive,
        })
    }

    /// Whether the compiled pattern ignores case (smart case resolved).
    pub(super) fn is_case_insensitive(&self) -> bool {
        self.insensitive
    }

    /// `false` only when no line of `text` can match. `true` means "read the
    /// lines": it does not say a line matches.
    pub(super) fn text_may_match(&self, text: &str) -> bool {
        self.prefilter
            .as_ref()
            .is_none_or(|finder| finder.find(text.as_bytes()).is_some())
    }

    /// Whether `line` (without its `\n`) holds a match.
    pub(super) fn is_match(&self, line: &str) -> bool {
        self.regex.is_match(line)
    }

    /// Every match on `line`, as byte ranges in order. After a match the search
    /// goes on at its end, and after an empty match one character on. Each
    /// step moves forward, so the iterator ends within `line.len() + 1`
    /// steps. Each step is one search of the rest of the line.
    pub(super) fn matches<'a>(&'a self, line: &'a str) -> Matches<'a> {
        Matches {
            matcher: self,
            line,
            from: 0,
        }
    }
}

/// The matches of one line; see [`Matcher::matches`].
pub(super) struct Matches<'a> {
    matcher: &'a Matcher,
    line: &'a str,
    from: usize,
}

impl Iterator for Matches<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let end_of_line = self.line.len() + 1;
        if self.from >= end_of_line {
            return None;
        }
        let Some(found) = self.matcher.regex.find_at(self.line, self.from) else {
            self.from = end_of_line;
            return None;
        };
        let range = found.range();
        self.from = if range.is_empty() {
            self.line[range.end..]
                .chars()
                .next()
                .map_or(end_of_line, |c| range.end + c.len_utf8())
        } else {
            range.end
        };
        Some(range)
    }
}

/// Compile `source` under the size limits.
fn compile(source: &str, insensitive: bool) -> Result<Regex, regex::Error> {
    RegexBuilder::new(source)
        .case_insensitive(insensitive)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .nest_limit(REGEX_NEST_LIMIT)
        .build()
}

/// Compile `source` with the whole-word rule: the pattern between a start-half
/// and an end-half word boundary, in a group of its own so that a top-level
/// alternation stays inside it. `source` has compiled alone before, so its
/// groups are balanced.
///
/// A pattern in `(?x)` mode that ends with a comment would take the closing
/// parenthesis into the comment. The second form ends the comment with a line
/// break, which `(?x)` mode ignores. It is tried only when the first form does
/// not compile. Both forms add one level of nesting to `source`.
fn compile_whole_word(source: &str, insensitive: bool) -> Result<Regex, regex::Error> {
    let wrapped = |line_break: &str| {
        let pattern = format!(r"\b{{start-half}}(?:{source}{line_break})\b{{end-half}}");
        compile(&pattern, insensitive)
    };
    wrapped("").or_else(|first| wrapped("\n").map_err(|_| first))
}

/// The last non-empty line of a regex error message, without its `error: `
/// prefix. The crate's syntax errors span several lines and end with the cause.
fn last_line(message: &str) -> String {
    let line = message
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(message)
        .trim();
    line.strip_prefix("error: ").unwrap_or(line).to_owned()
}

#[cfg(test)]
#[path = "matcher_tests.rs"]
mod tests;
