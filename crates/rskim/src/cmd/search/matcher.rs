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
//! any) are not in `[\p{Alphabetic}\p{Nd}_]`. This is a post-filter on the
//! matches of the engine, not the boundary rule of `rg -w`: for a pattern that
//! starts or ends with a non-word character, or that can match at more than one
//! length from one start, the two can differ. The scoreboard oracle applies
//! the same rule, so the two sides agree.

use std::ops::Range;
use std::sync::OnceLock;

use memchr::memmem;
use regex::{Regex, RegexBuilder};

/// Compiled size limit of a pattern (10 MiB).
pub(super) const REGEX_SIZE_LIMIT: usize = 10 * 1024 * 1024;

/// Size limit of the lazy DFA cache of a pattern (2 MiB).
pub(super) const REGEX_DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;

/// Nesting limit of a pattern's syntax tree.
pub(super) const REGEX_NEST_LIMIT: u32 = 250;

/// The class of word characters of the `-w` rule.
const WORD_CHAR_CLASS: &str = r"[\p{Alphabetic}\p{Nd}_]";

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
    word: bool,
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
        let regex = RegexBuilder::new(&source)
            .case_insensitive(insensitive)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
            .nest_limit(REGEX_NEST_LIMIT)
            .build()
            .map_err(|e| MatcherError::Invalid {
                pattern: spec.pattern.to_owned(),
                detail: last_line(&e.to_string()),
            })?;
        let prefilter = (spec.kind == PatternKind::Literal && !insensitive)
            .then(|| memmem::Finder::new(spec.pattern.as_bytes()).into_owned());
        Ok(Matcher {
            regex,
            prefilter,
            word: spec.word,
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
        if self.word {
            self.matches(line).next().is_some()
        } else {
            self.regex.is_match(line)
        }
    }

    /// Every match on `line`, as byte ranges in order. After a match the search
    /// goes on at its end; after a match that fails the whole-word rule it
    /// goes on one character past its start. The loop moves forward at every
    /// step, so it ends within `line.len() + 1` steps.
    pub(super) fn matches<'a>(&'a self, line: &'a str) -> Matches<'a> {
        Matches {
            matcher: self,
            line,
            from: 0,
        }
    }

    fn bounded(&self, line: &str, range: &Range<usize>) -> bool {
        let before_ok = line[..range.start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = line[range.end..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        before_ok && after_ok
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
        while self.from <= self.line.len() {
            let found = self.matcher.regex.find_at(self.line, self.from)?;
            let range = found.range();
            let step = |at: usize| {
                self.line[at..]
                    .chars()
                    .next()
                    .map_or(self.line.len() + 1, |c| at + c.len_utf8())
            };
            if !self.matcher.word || self.matcher.bounded(self.line, &range) {
                self.from = if range.is_empty() {
                    step(range.end)
                } else {
                    range.end
                };
                return Some(range);
            }
            self.from = step(range.start);
        }
        None
    }
}

/// Whether `c` is in `[\p{Alphabetic}\p{Nd}_]`.
fn is_word_char(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    static CLASS: OnceLock<Option<Regex>> = OnceLock::new();
    let class = CLASS.get_or_init(|| Regex::new(WORD_CHAR_CLASS).ok());
    let mut buf = [0u8; 4];
    class
        .as_ref()
        .is_some_and(|class| class.is_match(c.encode_utf8(&mut buf)))
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
