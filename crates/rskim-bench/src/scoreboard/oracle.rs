//! Ground-truth predicates and in-process baselines for the search
//! scoreboard (#203).
//!
//! Ported from the 2026-09-25 seed benchmark
//! (`.devflow/docs/reviews/vision-gap-2026-09/bench/lib.py`: `gt_and_substring`,
//! `gt_phrase`, `gt_near`). Deliberately independent of skim's code: nothing
//! here imports `rskim_core::Language` or a skim tokenizer. Where a rule must
//! agree with skim (the word-byte class, the `--lang` names), this module
//! carries its own copy, so a change on skim's side shows up as a scoreboard
//! diff instead of silently flowing through.
//!
//! Everything here is pure: callers pass `(path, text)` pairs, typically from
//! [`crate::scoreboard::universe::Universe::files`].

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use regex::{Regex, RegexBuilder};

// ============================================================================
// Query model
// ============================================================================

/// The declared ground-truth mode of a query, mirroring the golden set's
/// `mode` field (`and | phrase | near | pnear`) and skim's `verify_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    /// Every whitespace-separated token occurs as a case-sensitive substring
    /// somewhere in the file (skim's default substring verify).
    And,
    /// The query's word tokens occur contiguously and in order in the file's
    /// word-token stream, across line boundaries (`--phrase`).
    Phrase,
    /// Some window of word-token positions with `last - first <= span` holds
    /// every query word, with multiplicity, in any order (`--near span`).
    Near { span: u32 },
    /// Some in-order chain of word-token positions with
    /// `last - first <= span` (`--phrase --near span`).
    PhraseNear { span: u32 },
}

/// A parsed predicate. Non-empty by construction: [`LexicalQuery::new`]
/// refuses queries that would match vacuously.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Predicate {
    And { tokens: Vec<String> },
    Phrase { words: Vec<String> },
    Near { words: Vec<String>, span: usize },
    PhraseNear { words: Vec<String>, span: usize },
}

/// A lexical ground-truth query: a predicate plus an optional `--lang`
/// restriction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalQuery {
    raw: String,
    mode: MatchMode,
    predicate: Predicate,
    lang: Option<LangFilter>,
}

impl LexicalQuery {
    /// Parse `query` under `mode`, optionally restricted to `lang`.
    ///
    /// # Errors
    ///
    /// - the query has no whitespace-separated token;
    /// - a positional mode (`phrase` / `near` / `pnear`) is given a query with
    ///   no word token (a phrase of zero words would match every file — the
    ///   seed's vacuous-truth case — so it is refused rather than ported);
    /// - a `near` / `pnear` span is 0 (skim rejects `--near 0` too).
    pub fn new(query: &str, mode: MatchMode, lang: Option<LangFilter>) -> anyhow::Result<Self> {
        let tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
        if tokens.is_empty() {
            anyhow::bail!("query {query:?} has no whitespace-separated token");
        }

        let predicate = match mode {
            MatchMode::And => Predicate::And { tokens },
            MatchMode::Phrase => Predicate::Phrase {
                words: positional_words(query, mode)?,
            },
            MatchMode::Near { span } => Predicate::Near {
                words: positional_words(query, mode)?,
                span: positive_span(span)?,
            },
            MatchMode::PhraseNear { span } => Predicate::PhraseNear {
                words: positional_words(query, mode)?,
                span: positive_span(span)?,
            },
        };

        Ok(LexicalQuery {
            raw: query.to_string(),
            mode,
            predicate,
            lang,
        })
    }

    /// The query text as given.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The declared mode.
    pub fn mode(&self) -> MatchMode {
        self.mode
    }

    /// The `--lang` restriction, if any.
    pub fn lang(&self) -> Option<&LangFilter> {
        self.lang.as_ref()
    }

    /// Whether the file at `path` with contents `text` is in the ground truth.
    pub fn matches(&self, path: &str, text: &str) -> bool {
        if let Some(lang) = &self.lang
            && !lang.matches_path(path)
        {
            return false;
        }
        match &self.predicate {
            Predicate::And { tokens } => tokens.iter().all(|t| text.contains(t.as_str())),
            Predicate::Phrase { words } => phrase_matches(words, &word_tokens(text)),
            Predicate::Near { words, span } => near_matches(words, &word_tokens(text), *span),
            Predicate::PhraseNear { words, span } => {
                phrase_near_matches(words, &word_tokens(text), *span)
            }
        }
    }
}

fn positional_words(query: &str, mode: MatchMode) -> anyhow::Result<Vec<String>> {
    let words: Vec<String> = word_tokens(query).into_iter().map(str::to_string).collect();
    if words.is_empty() {
        anyhow::bail!("{mode:?} query {query:?} has no [A-Za-z0-9_] word token");
    }
    Ok(words)
}

fn positive_span(span: u32) -> anyhow::Result<usize> {
    if span == 0 {
        anyhow::bail!("near span must be > 0");
    }
    usize::try_from(span).map_err(|_| anyhow::anyhow!("near span {span} does not fit usize"))
}

/// The ground-truth file set for `query` over `files`: every matching path,
/// sorted byte-wise and de-duplicated.
pub fn ground_truth<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
    query: &LexicalQuery,
) -> Vec<String> {
    let mut out: Vec<String> = files
        .into_iter()
        .filter(|(path, text)| query.matches(path, text))
        .map(|(path, _)| path.to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

// ============================================================================
// Whole-word lines (the `-w` boundary rule)
// ============================================================================

/// The word-character class of the `-w` boundary rule:
/// `[\p{Alphabetic}\p{Nd}_]`.
const WORD_CHAR_CLASS: &str = r"[\p{Alphabetic}\p{Nd}_]";

/// Whole-word matching of a literal over lines, the ground truth for a
/// `-w` search. A match is a whole word iff the character before it and the
/// character after it (if any) are not word characters
/// (`WORD_CHAR_CLASS`). The literal is compared case-sensitively and
/// byte for byte, never as a regex.
///
/// Limit: for a literal that starts or ends with a non-word character this
/// rule can differ from `rg -w`. Golden queries for the whole-word tables are
/// bare identifiers, and the golden integrity checks refuse any other query.
#[derive(Debug, Clone)]
pub struct WholeWord {
    class: Regex,
}

impl WholeWord {
    /// Build the matcher.
    ///
    /// # Errors
    ///
    /// Returns an error if the word-character class does not compile.
    pub fn new() -> anyhow::Result<Self> {
        Ok(WholeWord {
            class: Regex::new(WORD_CHAR_CLASS)?,
        })
    }

    fn is_word_char(&self, c: char) -> bool {
        let mut buf = [0u8; 4];
        self.class.is_match(c.encode_utf8(&mut buf))
    }

    /// Whether `line` holds `word` as a whole word. An empty `word` never
    /// matches. Overlapping occurrences are all tried.
    pub fn in_line(&self, line: &str, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.any_bounded(line, |from| {
            line[from..]
                .find(word)
                .map(|rel| (from + rel, from + rel + word.len()))
        })
    }

    /// Whether the match `start..end` of `line` has no word character right
    /// before it and none right after it.
    fn bounded(&self, line: &str, start: usize, end: usize) -> bool {
        let before_ok = line[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !self.is_word_char(c));
        let after_ok = line[end..]
            .chars()
            .next()
            .is_none_or(|c| !self.is_word_char(c));
        before_ok && after_ok
    }

    /// Whether some match of `line` is [`bounded`](Self::bounded). `find(from)`
    /// returns the leftmost match that starts at or after the byte `from`.
    /// After a rejected match the search resumes one character past its start,
    /// so overlapping matches are all tried. Every step moves `from` forward,
    /// so the loop ends within `line.len() + 1` steps.
    fn any_bounded(&self, line: &str, find: impl Fn(usize) -> Option<(usize, usize)>) -> bool {
        let mut from = 0;
        while let Some((start, end)) = find(from) {
            if self.bounded(line, start, end) {
                return true;
            }
            // Step one character on (not one byte, to stay on a boundary).
            let Some(next) = line[start..].chars().next() else {
                return false;
            };
            from = start + next.len_utf8();
        }
        false
    }

    /// Every `(path, 1-based line)` over `files` whose line holds `word` as
    /// a whole word. Lines are split on `\n` only (the line number is 1 + the
    /// count of `\n` bytes before the line).
    pub fn lines<'a>(
        &self,
        files: impl IntoIterator<Item = (&'a str, &'a str)>,
        word: &str,
    ) -> BTreeSet<(String, u32)> {
        let mut out = BTreeSet::new();
        for (path, text) in files {
            for (idx, line) in text.split('\n').enumerate() {
                let Ok(number) = u32::try_from(idx + 1) else {
                    break;
                };
                if self.in_line(line, word) {
                    out.insert((path.to_string(), number));
                }
            }
        }
        out
    }
}

// ============================================================================
// Line-level ground truth (literal and regex lines, the `rg` flag set)
// ============================================================================

/// How a [`LineQuery`] reads its pattern, with the two switches that change
/// what a hit is.
///
/// This is not a [`MatchMode`] variant on purpose. `MatchMode` names the
/// file-level modes of the golden `lexical` table, and `verify_mode` in the
/// search JSON has a value for each of them. A line-level query has no such
/// value, and a file-level mode has no lines to judge. Two types keep both
/// facts in the signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMode {
    /// The pattern is literal text (`-F`, and the default).
    Literal { case_insensitive: bool, word: bool },
    /// The pattern is a regex (`-E`).
    Regex { case_insensitive: bool, word: bool },
}

impl LineMode {
    fn word(self) -> bool {
        match self {
            LineMode::Literal { word, .. } | LineMode::Regex { word, .. } => word,
        }
    }
}

/// Whether a search ignores case. `-i` always does. `-S` (smart case) does
/// when no character of `pattern` is uppercase (`char::is_uppercase`). With
/// both flags, `-i` wins.
pub fn is_case_insensitive(pattern: &str, ignore_case: bool, smart_case: bool) -> bool {
    ignore_case || (smart_case && !pattern.chars().any(char::is_uppercase))
}

/// How a [`LineQuery`] finds a match.
#[derive(Debug, Clone)]
enum Finder {
    /// Case-sensitive literal text, found with `str::find`. The regex crate
    /// is not involved.
    Exact(String),
    /// Everything else, compiled by the regex crate.
    Pattern(Regex),
}

impl Finder {
    /// The leftmost match that starts at or after the byte `from` (a
    /// character boundary of `line`), as `(start, end)`. `from` may equal
    /// `line.len()`: an empty match can sit at the end of the line. A regex
    /// reads the whole `line` for context, so `^` stays at the line start.
    fn find_from(&self, line: &str, from: usize) -> Option<(usize, usize)> {
        match self {
            Finder::Exact(text) => line[from..]
                .find(text.as_str())
                .map(|rel| (from + rel, from + rel + text.len())),
            Finder::Pattern(re) => re.find_at(line, from).map(|m| (m.start(), m.end())),
        }
    }
}

/// A line-level ground-truth query: the lines that hold a match of a literal
/// or a regex, in the way `rg -n` reads files.
///
/// The rules (the oracle's specification of the plain-text search):
///
/// - A literal is matched as text. A regex is the regex crate's syntax.
/// - Case is exact unless the mode says otherwise. [`is_case_insensitive`]
///   turns `-i` and `-S` into that switch.
/// - The unit is a line. A line is a hit when it holds a match. An
///   empty-match regex matches every line.
/// - With `word`, a match counts only when the character before it and the
///   one after it (if any) are not in `[\p{Alphabetic}\p{Nd}_]`. This is a
///   post-filter: the leftmost match at each start is tried in turn, and a
///   match that fails the filter is dropped. Limit: for a pattern that starts
///   or ends with a non-word character, or that can match at more than one
///   length from one start, this can differ from `rg -w`.
///
/// Limit: the engine and this oracle both use the regex crate. The frozen hit
/// lists in this module's tests guard against a fault in that crate.
#[derive(Debug, Clone)]
pub struct LineQuery {
    raw: String,
    mode: LineMode,
    finder: Finder,
    /// Set exactly when the mode asks for whole words.
    boundary: Option<WholeWord>,
}

impl LineQuery {
    /// Compile `pattern` under `mode`.
    ///
    /// # Errors
    ///
    /// - a literal pattern is empty (a regex may be, and then matches every
    ///   line);
    /// - a regex does not compile.
    pub fn new(pattern: &str, mode: LineMode) -> anyhow::Result<Self> {
        let finder = match mode {
            LineMode::Literal { .. } if pattern.is_empty() => {
                anyhow::bail!("a literal pattern must not be empty")
            }
            LineMode::Literal {
                case_insensitive: false,
                ..
            } => Finder::Exact(pattern.to_string()),
            LineMode::Literal {
                case_insensitive: true,
                ..
            } => Finder::Pattern(compile_pattern(&regex::escape(pattern), true)?),
            LineMode::Regex {
                case_insensitive, ..
            } => Finder::Pattern(compile_pattern(pattern, case_insensitive)?),
        };
        let boundary = if mode.word() {
            Some(WholeWord::new()?)
        } else {
            None
        };
        Ok(LineQuery {
            raw: pattern.to_string(),
            mode,
            finder,
            boundary,
        })
    }

    /// The pattern as given.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The mode.
    pub fn mode(&self) -> LineMode {
        self.mode
    }

    /// Whether `line` (without its `\n`) is a hit.
    pub fn matches_line(&self, line: &str) -> bool {
        match &self.boundary {
            None => self.finder.find_from(line, 0).is_some(),
            Some(boundary) => boundary.any_bounded(line, |from| self.finder.find_from(line, from)),
        }
    }
}

fn compile_pattern(pattern: &str, case_insensitive: bool) -> anyhow::Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|e| anyhow::anyhow!("pattern {pattern:?} does not compile: {e}"))
}

/// The lines of `text` as `rg` reads them, with 1-based numbers. Lines split
/// on `\n` only, so a `\r` stays in the line. A final `\n` ends the last line
/// and does not start another. An empty text has no line.
fn numbered_lines(text: &str) -> impl Iterator<Item = (u32, &str)> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    (!text.is_empty())
        .then(|| body.split('\n'))
        .into_iter()
        .flatten()
        .enumerate()
        .map_while(|(idx, line)| Some((u32::try_from(idx + 1).ok()?, line)))
}

/// Every `(path, 1-based line)` over `files` that is a hit of `query`.
pub fn line_truth<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
    query: &LineQuery,
) -> BTreeSet<(String, u32)> {
    let mut out = BTreeSet::new();
    for (path, text) in files {
        for (number, line) in numbered_lines(text) {
            if query.matches_line(line) {
                out.insert((path.to_string(), number));
            }
        }
    }
    out
}

// ============================================================================
// Word tokens (the oracle's own copy of skim's word-byte rule)
// ============================================================================

/// The oracle's word-byte rule: ASCII `[A-Za-z0-9_]`.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Maximal runs of word bytes, in order — the seed's
/// `re.findall(r"[A-Za-z0-9_]+", s)`. Slices only at ASCII boundaries, which
/// are always valid `str` boundaries.
fn word_tokens(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match (is_word_byte(b), start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                out.push(&s[st..i]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

/// `phrase`: `words` occurs as a contiguous run of `tokens`.
fn phrase_matches(words: &[String], tokens: &[&str]) -> bool {
    tokens.len() >= words.len()
        && tokens
            .windows(words.len())
            .any(|w| w.iter().zip(words).all(|(t, q)| *t == q.as_str()))
}

/// Positions of each distinct query word in `tokens` (only words that occur
/// in the query are recorded). `None` if some query word never occurs.
fn query_word_positions<'q>(
    words: &'q [String],
    tokens: &[&str],
) -> Option<HashMap<&'q str, Vec<usize>>> {
    let mut pos: HashMap<&'q str, Vec<usize>> =
        words.iter().map(|w| (w.as_str(), Vec::new())).collect();
    for (i, tok) in tokens.iter().enumerate() {
        if let Some(v) = pos.get_mut(*tok) {
            v.push(i);
        }
    }
    pos.values().all(|v| !v.is_empty()).then_some(pos)
}

/// `near` (any order), ported from the seed's unordered branch: a sliding
/// window over the query-word occurrences whose multiset covers every query
/// word's required count and whose position span is `<= span`.
fn near_matches(words: &[String], tokens: &[&str], span: usize) -> bool {
    let mut need: HashMap<&str, usize> = HashMap::new();
    for w in words {
        *need.entry(w.as_str()).or_insert(0) += 1;
    }
    let rel: Vec<(usize, &str)> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| need.contains_key(**t))
        .map(|(i, t)| (i, *t))
        .collect();

    let mut count: HashMap<&str, usize> = HashMap::new();
    let mut satisfied = 0usize;
    let mut left = 0usize;
    for &(right_pos, right_word) in &rel {
        let c = count.entry(right_word).or_insert(0);
        *c += 1;
        if Some(&*c) == need.get(right_word) {
            satisfied += 1;
        }
        // `left` never passes the current element: shrinking stops once the
        // span is within bound, and a one-element window has span 0.
        while right_pos - rel[left].0 > span {
            let left_word = rel[left].1;
            let lc = count.entry(left_word).or_insert(0);
            if Some(&*lc) == need.get(left_word) {
                satisfied -= 1;
            }
            *lc = lc.saturating_sub(1);
            left += 1;
        }
        if satisfied == need.len() {
            return true;
        }
    }
    false
}

/// `pnear` (in order), ported from the seed's ordered branch: from every
/// occurrence of the first query word, chain the earliest later occurrence of
/// each following word; accept when that chain spans `<= span`. The greedy
/// earliest successor gives the smallest end position for a given start, so
/// trying every start is exhaustive.
fn phrase_near_matches(words: &[String], tokens: &[&str], span: usize) -> bool {
    let Some(pos) = query_word_positions(words, tokens) else {
        return false;
    };
    let Some((first, rest)) = words.split_first() else {
        return false;
    };
    let starts = pos.get(first.as_str()).map(Vec::as_slice).unwrap_or(&[]);
    starts.iter().any(|&start| {
        let mut cur = start;
        for w in rest {
            let occurrences = pos.get(w.as_str()).map(Vec::as_slice).unwrap_or(&[]);
            // Occurrences are ascending: the first one after `cur` is the
            // earliest successor.
            let next = occurrences.partition_point(|&p| p <= cur);
            match occurrences.get(next) {
                Some(&p) => cur = p,
                None => return false,
            }
        }
        cur - start <= span
    })
}

// ============================================================================
// --lang (the oracle's own language → extension map)
// ============================================================================

/// One language: the display name, its file extensions (the oracle's copy of
/// `rskim_core::Language::from_extension`, `crates/rskim-core/src/types.rs`)
/// and the extra `--lang` names the CLI accepts for it
/// (`parse_lang_value`, `crates/rskim/src/cmd/search/mod.rs:496-527`).
struct LangSpec {
    language: &'static str,
    extensions: &'static [&'static str],
    names: &'static [&'static str],
}

const LANGS: &[LangSpec] = &[
    LangSpec {
        language: "typescript",
        extensions: &["ts", "tsx", "mts", "cts"],
        names: &["typescript"],
    },
    LangSpec {
        language: "javascript",
        extensions: &["js", "jsx", "cjs", "mjs"],
        names: &["javascript"],
    },
    LangSpec {
        language: "python",
        extensions: &["py", "pyi"],
        names: &["python"],
    },
    LangSpec {
        language: "rust",
        extensions: &["rs"],
        names: &["rust"],
    },
    LangSpec {
        language: "go",
        extensions: &["go"],
        names: &[],
    },
    LangSpec {
        language: "java",
        extensions: &["java"],
        names: &[],
    },
    LangSpec {
        language: "markdown",
        extensions: &["md", "markdown"],
        names: &[],
    },
    LangSpec {
        language: "json",
        extensions: &["json"],
        names: &[],
    },
    LangSpec {
        language: "yaml",
        extensions: &["yaml", "yml"],
        names: &[],
    },
    LangSpec {
        language: "c",
        extensions: &["c", "h"],
        names: &[],
    },
    LangSpec {
        language: "cpp",
        extensions: &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
        names: &["c++"],
    },
    LangSpec {
        language: "toml",
        extensions: &["toml"],
        names: &[],
    },
    LangSpec {
        language: "csharp",
        extensions: &["cs"],
        names: &["csharp", "c#"],
    },
    LangSpec {
        language: "ruby",
        extensions: &["rb"],
        names: &["ruby"],
    },
    LangSpec {
        language: "sql",
        extensions: &["sql"],
        names: &[],
    },
    LangSpec {
        language: "kotlin",
        extensions: &["kt", "kts"],
        names: &["kotlin"],
    },
    LangSpec {
        language: "swift",
        extensions: &["swift"],
        names: &[],
    },
    LangSpec {
        language: "bash",
        extensions: &["sh", "bash"],
        names: &[],
    },
];

/// Whether `ext` (case-sensitive) belongs to some language in the table
/// above — the oracle universe's extension allow-list.
pub fn is_indexable_extension(ext: &str) -> bool {
    LANGS.iter().any(|l| l.extensions.contains(&ext))
}

/// A `--lang` restriction: the set of file extensions of one language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangFilter {
    language: &'static str,
    extensions: &'static [&'static str],
}

impl LangFilter {
    /// Resolve a `--lang` value the way the CLI does: case-insensitively, as
    /// a file extension first (selecting that extension's whole language),
    /// then as a language name.
    ///
    /// # Errors
    ///
    /// Returns an error for a value the CLI would reject.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let lower = value.to_ascii_lowercase();
        LANGS
            .iter()
            .find(|l| l.extensions.contains(&lower.as_str()))
            .or_else(|| LANGS.iter().find(|l| l.names.contains(&lower.as_str())))
            .map(|l| LangFilter {
                language: l.language,
                extensions: l.extensions,
            })
            .ok_or_else(|| anyhow::anyhow!("unknown --lang value {value:?}"))
    }

    /// Canonical language name (e.g. `"markdown"` for `--lang md`).
    pub fn language(&self) -> &'static str {
        self.language
    }

    /// Extensions this filter admits.
    pub fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    /// Whether `path`'s extension (case-sensitive, as `Path::extension`
    /// reports it) belongs to the language.
    pub fn matches_path(&self, path: &str) -> bool {
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| self.extensions.contains(&ext))
    }
}

// ============================================================================
// Baselines
// ============================================================================

/// The file order `rg --sort path` produces: ripgrep sorts each directory's
/// entries by file name and walks depth-first, which is a component-wise
/// comparison (`std::path::Path`'s `Ord`). It differs from a raw byte-wise
/// sort of the full path only when a sibling name extends a directory name
/// with a byte below `/` — `a/b.rs` sorts before `a.rs` here, after it
/// byte-wise.
pub fn rg_path_cmp(a: &str, b: &str) -> Ordering {
    Path::new(a).cmp(Path::new(b))
}

/// `alphabetical` baseline: the ground-truth set in `rg -l --sort path`
/// order.
pub fn baseline_alphabetical(ground_truth: &[String]) -> Vec<String> {
    let mut v = ground_truth.to_vec();
    v.sort_by(|a, b| rg_path_cmp(a, b));
    v
}

/// `occurrence-count` baseline: the ground-truth set sorted by the total
/// number of (non-overlapping) occurrences of every whitespace-separated
/// query token, descending; ties in [`rg_path_cmp`] order. A path whose text
/// `text_of` cannot supply counts 0.
pub fn baseline_occurrence_count<'a>(
    ground_truth: &[String],
    query: &str,
    text_of: impl Fn(&str) -> Option<&'a str>,
) -> Vec<String> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut scored: Vec<(usize, &String)> = ground_truth
        .iter()
        .map(|path| {
            let count = text_of(path)
                .map(|text| tokens.iter().map(|t| text.matches(t).count()).sum())
                .unwrap_or(0);
            (count, path)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| rg_path_cmp(a.1, b.1)));
    scored.into_iter().map(|(_, p)| p.clone()).collect()
}

/// One output line of the simulated `rg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgLine {
    /// Repo-relative path.
    pub path: String,
    /// 1-based line number.
    pub line: u32,
    /// Byte offset just past this line's `\n` in the simulated output — the
    /// number of output bytes up to and including this line.
    pub end: usize,
}

/// Simulated `rg -n --sort path` output (see [`simulate_rg_fixed`] and
/// [`simulate_rg_word`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RgSimulation {
    output: Vec<u8>,
    lines: Vec<RgLine>,
}

impl RgSimulation {
    /// The simulated stdout bytes.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Total simulated stdout bytes.
    pub fn total_bytes(&self) -> usize {
        self.output.len()
    }

    /// Every emitted line, in output order.
    pub fn lines(&self) -> &[RgLine] {
        &self.lines
    }

    /// Output bytes up to and including line `line` of `path`, or `None` if
    /// that line is not in the output (it does not contain the pattern).
    pub fn bytes_through(&self, path: &str, line: u32) -> Option<usize> {
        self.lines
            .iter()
            .find(|l| l.line == line && l.path == path)
            .map(|l| l.end)
    }
}

/// Simulate `rg -n -F --sort path <pattern>` (what an agent types) over
/// `files`, with no `rg` binary: one `path:line:content\n` record per line
/// containing `pattern` as a case-sensitive literal, files in
/// [`rg_path_cmp`] order.
///
/// Lines split on `\n` only, so a `\r` before it stays in the content, and a
/// final line without a trailing newline still gets one — both as `rg`
/// prints them. Not modeled: `rg`'s binary-file suppression and UTF-8 BOM
/// stripping (the oracle's universe is UTF-8 text; both are corner cases).
pub fn simulate_rg_fixed<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
    pattern: &str,
) -> RgSimulation {
    render_rg(files, |line| line.contains(pattern))
}

/// Simulate `rg -n -w --sort path <word>` over `files` for a bare
/// identifier: one `path:line:content\n` record per line that holds `word`
/// as a whole word ([`LineQuery`] with `word` set, case-sensitive), files in
/// [`rg_path_cmp`] order. What is not modeled is the same as for
/// [`simulate_rg_fixed`].
///
/// # Errors
///
/// Returns an error if `word` is empty.
pub fn simulate_rg_word<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
    word: &str,
) -> anyhow::Result<RgSimulation> {
    let query = LineQuery::new(
        word,
        LineMode::Literal {
            case_insensitive: false,
            word: true,
        },
    )?;
    Ok(render_rg(files, |line| query.matches_line(line)))
}

/// The `rg -n --sort path` output for the lines of `files` that `is_hit`
/// accepts.
fn render_rg<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
    is_hit: impl Fn(&str) -> bool,
) -> RgSimulation {
    let mut ordered: Vec<(&str, &str)> = files.into_iter().collect();
    ordered.sort_by(|a, b| rg_path_cmp(a.0, b.0));

    let mut sim = RgSimulation::default();
    for (path, text) in ordered {
        for (line_no, line) in numbered_lines(text) {
            if !is_hit(line) {
                continue;
            }
            sim.output.extend_from_slice(path.as_bytes());
            sim.output.push(b':');
            sim.output.extend_from_slice(line_no.to_string().as_bytes());
            sim.output.push(b':');
            sim.output.extend_from_slice(line.as_bytes());
            sim.output.push(b'\n');
            sim.lines.push(RgLine {
                path: path.to_string(),
                line: line_no,
                end: sim.output.len(),
            });
        }
    }
    sim
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;

    fn q(query: &str, mode: MatchMode) -> LexicalQuery {
        LexicalQuery::new(query, mode, None).unwrap()
    }

    fn hit(query: &str, mode: MatchMode, text: &str) -> bool {
        q(query, mode).matches("f.rs", text)
    }

    fn near(span: u32) -> MatchMode {
        MatchMode::Near { span }
    }

    fn pnear(span: u32) -> MatchMode {
        MatchMode::PhraseNear { span }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // --- query construction -------------------------------------------------

    #[test]
    fn empty_or_blank_query_is_rejected_in_every_mode() {
        for mode in [MatchMode::And, MatchMode::Phrase, near(3), pnear(3)] {
            assert!(LexicalQuery::new("", mode, None).is_err(), "{mode:?}");
            assert!(LexicalQuery::new(" \t ", mode, None).is_err(), "{mode:?}");
        }
    }

    #[test]
    fn positional_modes_reject_a_query_with_no_word_tokens() {
        // `->` has no [A-Za-z0-9_] bytes: a phrase over zero words would match
        // every file (the seed's vacuous-truth behavior), so refuse it.
        for mode in [MatchMode::Phrase, near(3), pnear(3)] {
            assert!(LexicalQuery::new("->", mode, None).is_err(), "{mode:?}");
        }
        assert!(LexicalQuery::new("->", MatchMode::And, None).is_ok());
    }

    #[test]
    fn near_span_zero_is_rejected() {
        assert!(LexicalQuery::new("a b", near(0), None).is_err());
        assert!(LexicalQuery::new("a b", pnear(0), None).is_err());
    }

    #[test]
    fn mode_and_raw_query_round_trip() {
        let query = q("build lock", pnear(4));
        assert_eq!(query.mode(), pnear(4));
        assert_eq!(query.raw(), "build lock");
    }

    // --- and ------------------------------------------------------------------

    #[test]
    fn and_needs_every_whitespace_token_somewhere_in_the_file() {
        assert!(hit(
            "foo bar",
            MatchMode::And,
            "bar on line one\nfoo on line two"
        ));
        assert!(!hit("foo bar", MatchMode::And, "only foo here"));
    }

    #[test]
    fn and_is_a_case_sensitive_substring_match() {
        assert!(hit("staleness", MatchMode::And, "fn check_staleness() {}"));
        assert!(!hit("Staleness", MatchMode::And, "fn check_staleness() {}"));
        assert!(hit(
            "STALENESS",
            MatchMode::And,
            "const STALENESS_LIMIT: u8 = 1;"
        ));
    }

    #[test]
    fn and_treats_punctuation_as_part_of_the_token() {
        assert!(hit(
            "-D warnings",
            MatchMode::And,
            "cargo clippy -- -D warnings"
        ));
        assert!(!hit("-D warnings", MatchMode::And, "D warnings"));
        assert!(hit(
            "Option<&str>",
            MatchMode::And,
            "fn f(x: Option<&str>) {}"
        ));
        assert!(hit("::new(", MatchMode::And, "let v = Vec::new();"));
    }

    #[test]
    fn and_splits_the_query_on_any_whitespace() {
        assert!(hit("  foo\t\tbar ", MatchMode::And, "foo bar"));
    }

    // --- phrase ---------------------------------------------------------------

    #[test]
    fn phrase_needs_contiguous_words_in_order() {
        assert!(hit(
            "build lock",
            MatchMode::Phrase,
            "take the build lock now"
        ));
        assert!(!hit("build lock", MatchMode::Phrase, "lock the build"));
        assert!(!hit("build lock", MatchMode::Phrase, "build the lock"));
    }

    #[test]
    fn phrase_ignores_non_word_bytes_between_words_and_spans_lines() {
        assert!(hit(
            "build lock",
            MatchMode::Phrase,
            "the build-lock helper"
        ));
        assert!(hit("build lock", MatchMode::Phrase, "// build\n// lock"));
        assert!(hit(
            "fn check_staleness",
            MatchMode::Phrase,
            "pub fn check_staleness(root: &Path)"
        ));
    }

    #[test]
    fn phrase_treats_underscore_as_a_word_byte() {
        assert!(!hit(
            "build lock",
            MatchMode::Phrase,
            "the build_lock helper"
        ));
    }

    #[test]
    fn phrase_compares_whole_tokens_not_substrings() {
        assert!(hit("Result", MatchMode::Phrase, "-> Result<(), E>"));
        assert!(!hit("Result", MatchMode::Phrase, "MyResult Results"));
    }

    #[test]
    fn phrase_is_case_sensitive() {
        assert!(!hit("Build Lock", MatchMode::Phrase, "build lock"));
    }

    #[test]
    fn phrase_ignores_punctuation_in_the_query() {
        assert!(hit(
            "check_staleness()",
            MatchMode::Phrase,
            "check_staleness"
        ));
    }

    #[test]
    fn non_ascii_bytes_separate_word_tokens() {
        // The word-byte class is ASCII-only, so "café" tokenizes to "caf".
        assert!(hit("caf", MatchMode::Phrase, "un café"));
        assert!(!hit("café", MatchMode::Phrase, "un cafe"));
    }

    // --- near (any order) -----------------------------------------------------

    #[test]
    fn near_span_is_last_minus_first_word_position_inclusive_bound() {
        // lock@0 the@1 lever@2 build@3 → span 3.
        let text = "lock the lever build";
        assert!(hit("build lock", near(3), text));
        assert!(!hit("build lock", near(2), text));
    }

    #[test]
    fn near_accepts_either_order() {
        assert!(hit("build lock", near(1), "lock build"));
        assert!(hit("build lock", near(1), "build lock"));
    }

    #[test]
    fn near_counts_word_positions_not_bytes() {
        assert!(hit("build lock", near(1), "build(,,, ; )lock"));
    }

    #[test]
    fn near_needs_query_words_with_multiplicity() {
        assert!(hit("x x", near(3), "one x two x three"));
        assert!(!hit("x x", near(3), "one x two three"));
    }

    #[test]
    fn near_finds_a_later_window_after_an_early_miss() {
        let text = "build a a a a a a lock and later lock build";
        assert!(hit("build lock", near(1), text));
    }

    #[test]
    fn near_requires_every_query_word_to_be_present() {
        assert!(!hit("build lock stale", near(10), "build lock"));
    }

    #[test]
    fn near_with_one_word_matches_any_file_containing_that_token() {
        assert!(hit("lock", near(5), "a lock"));
        assert!(!hit("lock", near(5), "a locker"));
    }

    // --- pnear (--phrase --near) ----------------------------------------------

    #[test]
    fn pnear_needs_the_query_order() {
        assert!(hit("build lock", pnear(2), "build a lock"));
        assert!(!hit("build lock", pnear(2), "lock a build"));
    }

    #[test]
    fn pnear_with_span_k_minus_1_equals_phrase() {
        for text in ["build lock", "build x lock", "lock build", "the build-lock"] {
            assert_eq!(
                hit("build lock", pnear(1), text),
                hit("build lock", MatchMode::Phrase, text),
                "{text:?}"
            );
        }
    }

    #[test]
    fn pnear_takes_the_earliest_successor() {
        assert!(hit("build lock", pnear(1), "build lock filler filler lock"));
    }

    #[test]
    fn pnear_retries_from_each_start_occurrence() {
        // build@0 chains to lock@5 (span 5); build@4 chains to lock@5 (span 1).
        assert!(hit("build lock", pnear(1), "build x x x build lock"));
    }

    #[test]
    fn pnear_repeated_query_words_need_distinct_positions() {
        assert!(hit("a a", pnear(1), "a a"));
        assert!(!hit("a a", pnear(5), "a b c"));
    }

    #[test]
    fn pnear_three_words_span_bound() {
        // auto@0 x@1 refresh@2 y@3 stale@4 → span 4.
        let text = "auto x refresh y stale";
        assert!(hit("auto refresh stale", pnear(4), text));
        assert!(!hit("auto refresh stale", pnear(3), text));
    }

    // --- lang -----------------------------------------------------------------

    #[test]
    fn lang_accepts_extensions_and_names_case_insensitively() {
        for value in ["toml", "TOML"] {
            let f = LangFilter::parse(value).unwrap();
            assert!(f.matches_path("Cargo.toml"));
            assert!(!f.matches_path("src/main.rs"));
        }
        assert!(
            LangFilter::parse("rust")
                .unwrap()
                .matches_path("src/main.rs")
        );
        assert!(LangFilter::parse("RS").unwrap().matches_path("src/main.rs"));
    }

    #[test]
    fn lang_selects_every_extension_of_the_language() {
        let md = LangFilter::parse("markdown").unwrap();
        assert!(md.matches_path("README.md") && md.matches_path("docs/x.markdown"));
        // An extension alias selects the whole language, as `--lang tsx` does.
        let ts = LangFilter::parse("tsx").unwrap();
        for p in ["a.ts", "b.tsx", "c.mts", "d.cts"] {
            assert!(ts.matches_path(p), "{p}");
        }
        assert!(!ts.matches_path("e.js"));
        assert!(LangFilter::parse("h").unwrap().matches_path("x.c"));
        assert!(LangFilter::parse("c++").unwrap().matches_path("x.hpp"));
        assert!(LangFilter::parse("c#").unwrap().matches_path("x.cs"));
        assert!(LangFilter::parse("yml").unwrap().matches_path("x.yaml"));
    }

    #[test]
    fn lang_path_extension_match_is_case_sensitive() {
        assert!(
            !LangFilter::parse("markdown")
                .unwrap()
                .matches_path("README.MD")
        );
    }

    #[test]
    fn lang_rejects_unknown_values() {
        assert!(LangFilter::parse("haskell").is_err());
        assert!(LangFilter::parse("").is_err());
    }

    /// `LANGS` and the structural oracle's extension table
    /// (`rskim_oracle::structural::extension_classes`) are two copies of
    /// skim's `Language::from_extension` table. They must list the same
    /// extensions under the same language names, with one documented
    /// exception: the structural oracle parses `.tsx` with the TSX grammar,
    /// so its language there is `tsx`, where skim's is `typescript`.
    #[test]
    fn langs_agree_with_the_structural_oracles_extension_table() {
        use std::collections::BTreeMap;

        use rskim_oracle::structural::{LangClass, extension_classes};

        let structural: Vec<(&str, &str)> = extension_classes()
            .map(|(ext, class)| {
                let language = match class {
                    LangClass::Oracle(lang) => lang.as_str(),
                    LangClass::Unscored { language } => language,
                    LangClass::NotIndexed { language, .. } => language.unwrap_or("-"),
                };
                (ext, language)
            })
            .collect();
        let lexical: Vec<(&str, &str)> = LANGS
            .iter()
            .flat_map(|l| l.extensions.iter().map(move |&ext| (ext, l.language)))
            .collect();

        let structural_map: BTreeMap<&str, &str> = structural.iter().copied().collect();
        let lexical_map: BTreeMap<&str, &str> = lexical.iter().copied().collect();
        assert_eq!(
            structural_map.len(),
            structural.len(),
            "an extension listed twice"
        );
        assert_eq!(
            lexical_map.len(),
            lexical.len(),
            "an extension listed twice"
        );

        assert_eq!(lexical_map.get("tsx"), Some(&"typescript"));
        assert_eq!(structural_map.get("tsx"), Some(&"tsx"));
        let mut expected = lexical_map;
        expected.insert("tsx", "tsx");
        assert_eq!(structural_map, expected);
    }

    // --- ground truth -----------------------------------------------------------

    #[test]
    fn ground_truth_is_the_sorted_set_of_matching_files() {
        let files = [
            ("z.rs", "fn foo() {}"),
            ("b.rs", "fn bar() {}"),
            ("a.rs", "fn foo_bar() {}"),
        ];
        let gt = ground_truth(files, &q("foo", MatchMode::And));
        assert_eq!(gt, strings(&["a.rs", "z.rs"]));
    }

    #[test]
    fn ground_truth_applies_the_lang_filter() {
        let files = [
            ("Cargo.toml", "rskim-core = 1"),
            ("README.md", "rskim-core"),
        ];
        let query = LexicalQuery::new(
            "rskim-core",
            MatchMode::And,
            Some(LangFilter::parse("toml").unwrap()),
        )
        .unwrap();
        assert_eq!(ground_truth(files, &query), strings(&["Cargo.toml"]));
    }

    // --- path order -------------------------------------------------------------

    #[test]
    fn rg_path_order_is_component_wise_not_byte_wise() {
        // Byte-wise, "a.rs" < "a/b.rs" ('.' < '/'); `rg --sort path` walks the
        // directory "a" before its sibling file "a.rs".
        let mut paths = strings(&["a.rs", "a/b.rs", "B.rs", "a-b.rs"]);
        paths.sort_by(|x, y| rg_path_cmp(x, y));
        assert_eq!(paths, strings(&["B.rs", "a/b.rs", "a-b.rs", "a.rs"]));
    }

    // --- baselines --------------------------------------------------------------

    #[test]
    fn baseline_alphabetical_uses_rg_path_order() {
        let gt = strings(&["b.rs", "a.rs", "a/x.rs"]);
        assert_eq!(
            baseline_alphabetical(&gt),
            strings(&["a/x.rs", "a.rs", "b.rs"])
        );
    }

    #[test]
    fn baseline_occurrence_count_sorts_by_total_token_occurrences_descending() {
        let texts = [
            ("a.rs", "build x build x lock"),
            ("b.rs", "build lock"),
            ("c.rs", "lock lock lock lock build"),
        ];
        let gt = strings(&["a.rs", "b.rs", "c.rs"]);
        let text_of = |p: &str| texts.iter().find(|(k, _)| *k == p).map(|(_, v)| *v);
        assert_eq!(
            baseline_occurrence_count(&gt, "build lock", text_of),
            strings(&["c.rs", "a.rs", "b.rs"])
        );
    }

    #[test]
    fn baseline_occurrence_count_counts_non_overlapping_and_breaks_ties_by_path() {
        let texts = [("b.rs", "aaaa"), ("a.rs", "aa aa")];
        let gt = strings(&["b.rs", "a.rs"]);
        let text_of = |p: &str| texts.iter().find(|(k, _)| *k == p).map(|(_, v)| *v);
        // "aaaa" holds 2 non-overlapping "aa", same as "aa aa" → tie → path order.
        assert_eq!(
            baseline_occurrence_count(&gt, "aa", text_of),
            strings(&["a.rs", "b.rs"])
        );
    }

    #[test]
    fn simulated_rg_emits_path_line_content_in_rg_path_order() {
        let files = [
            ("b.rs", "no match\nfoo here\n"),
            ("a.rs", "foo first\nfoo foo twice\n"),
            ("c.rs", "nothing\n"),
        ];
        let sim = simulate_rg_fixed(files, "foo");
        assert_eq!(
            String::from_utf8(sim.output().to_vec()).unwrap(),
            "a.rs:1:foo first\na.rs:2:foo foo twice\nb.rs:2:foo here\n"
        );
        assert_eq!(sim.total_bytes(), sim.output().len());
        assert_eq!(sim.lines().len(), 3);
    }

    #[test]
    fn simulated_rg_keeps_carriage_returns_and_terminates_the_last_line() {
        let sim = simulate_rg_fixed([("w.rs", "foo\r\nbar foo")], "foo");
        assert_eq!(sim.output(), b"w.rs:1:foo\r\nw.rs:2:bar foo\n");
    }

    #[test]
    fn simulated_rg_bytes_through_counts_up_to_and_including_the_line() {
        let files = [("a.rs", "foo\n"), ("b.rs", "x\nfoo def\n")];
        let sim = simulate_rg_fixed(files, "foo");
        let first = "a.rs:1:foo\n".len();
        assert_eq!(sim.bytes_through("a.rs", 1), Some(first));
        assert_eq!(
            sim.bytes_through("b.rs", 2),
            Some(first + "b.rs:2:foo def\n".len())
        );
        assert_eq!(sim.bytes_through("b.rs", 1), None, "non-matching line");
        assert_eq!(sim.bytes_through("zz.rs", 1), None, "unknown file");
    }

    #[test]
    fn simulated_rg_with_no_match_is_empty() {
        let sim = simulate_rg_fixed([("a.rs", "bar\n")], "foo");
        assert_eq!(sim.total_bytes(), 0);
        assert!(sim.lines().is_empty());
    }

    // --- whole-word lines -----------------------------------------------------

    fn ww() -> WholeWord {
        WholeWord::new().unwrap()
    }

    #[test]
    fn a_word_between_non_word_characters_is_a_whole_word() {
        let w = ww();
        for line in [
            "foo",
            "foo()",
            "a.foo(b)",
            "let x = foo;",
            "-foo-",
            "::foo::bar",
            "\tfoo\r",
        ] {
            assert!(w.in_line(line, "foo"), "{line:?}");
        }
    }

    #[test]
    fn a_word_next_to_a_letter_digit_or_underscore_is_not_a_whole_word() {
        let w = ww();
        for line in [
            "foobar",
            "barfoo",
            "foo_bar",
            "_foo",
            "foo2",
            "2foo",
            "\u{e9}foo",
            "foo\u{e9}",
            "\u{663}foo",
        ] {
            assert!(!w.in_line(line, "foo"), "{line:?}");
        }
    }

    #[test]
    fn a_later_whole_word_counts_when_an_earlier_occurrence_is_inside_a_longer_word() {
        let w = ww();
        assert!(w.in_line("foobar foo", "foo"));
        assert!(w.in_line("xfoo foo!", "foo"));
        assert!(!w.in_line("foobar xfoo", "foo"));
    }

    #[test]
    fn overlapping_occurrences_are_all_tried() {
        let w = ww();
        // "aa" starts at 0 (a letter follows) and at 1 (a letter precedes):
        // neither is a whole word.
        assert!(!w.in_line("aaa-", "aa"));
        // "a-a" starts at 1 (a letter precedes) and, overlapping it, at 3
        // ('-' precedes, the line ends): only the second is a whole word.
        assert!(w.in_line("xa-a-a", "a-a"));
    }

    #[test]
    fn matching_is_case_sensitive_and_literal() {
        let w = ww();
        assert!(!w.in_line("Foo", "foo"));
        assert!(!w.in_line("foo", "f.o"), "the word is not a regex");
        assert!(w.in_line("a.b", "a.b"));
    }

    #[test]
    fn an_empty_word_never_matches() {
        assert!(!ww().in_line("anything", ""));
        assert!(ww().lines([("a.rs", "x\n")], "").is_empty());
    }

    #[test]
    fn whole_word_lines_give_path_and_one_based_line_over_every_file() {
        let files = [
            ("a.rs", "fn foo() {}\nlet foobar = 1;\n// foo again\n"),
            ("b.txt", "nothing\nfoo\n"),
            ("c.rs", "foo_bar\n"),
        ];
        let got = ww().lines(files, "foo");
        let want: BTreeSet<(String, u32)> = [("a.rs", 1), ("a.rs", 3), ("b.txt", 2)]
            .into_iter()
            .map(|(p, l)| (p.to_string(), l))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn a_crlf_line_keeps_its_line_number() {
        let got = ww().lines([("w.rs", "x\r\nfoo\r\ny")], "foo");
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            [("w.rs".to_string(), 2)]
        );
    }

    // --- line-level ground truth (literal and regex) ---------------------------

    // One frozen fixture. Every table below was derived by hand from these
    // lines (`\r` and `é` are real bytes of the text), not by running the
    // oracle:
    //
    //   src/a.rs     1 `fn run() {}`
    //                2 `let running = run();`
    //                3 `// Run it`
    //                4 `RUN run_all`
    //                5 `` (the text ends in `\n\n`)
    //   docs/b.txt   1 `x_run y\r`
    //                2 `run-time`
    //                3 `érun`
    //                4 `érun run`
    //   blank.txt    1 `` (the text is `\n`)
    //   empty.txt    no line (the text is empty)
    const LINE_FIXTURE: [(&str, &str); 4] = [
        (
            "src/a.rs",
            "fn run() {}\nlet running = run();\n// Run it\nRUN run_all\n\n",
        ),
        (
            "docs/b.txt",
            "x_run y\r\nrun-time\n\u{e9}run\n\u{e9}run run\n",
        ),
        ("blank.txt", "\n"),
        ("empty.txt", ""),
    ];

    fn literal(case_insensitive: bool, word: bool) -> LineMode {
        LineMode::Literal {
            case_insensitive,
            word,
        }
    }

    fn regex_mode(case_insensitive: bool, word: bool) -> LineMode {
        LineMode::Regex {
            case_insensitive,
            word,
        }
    }

    fn lq(pattern: &str, mode: LineMode) -> LineQuery {
        LineQuery::new(pattern, mode).unwrap()
    }

    fn truth(pattern: &str, mode: LineMode) -> BTreeSet<(String, u32)> {
        line_truth(LINE_FIXTURE, &lq(pattern, mode))
    }

    fn hits(table: &[(&str, &[u32])]) -> BTreeSet<(String, u32)> {
        table
            .iter()
            .flat_map(|(path, lines)| lines.iter().map(move |n| (path.to_string(), *n)))
            .collect()
    }

    #[test]
    fn a_case_sensitive_literal_hits_every_line_that_holds_it_as_a_substring() {
        // `Run` (line 3) has a capital; `running`, `x_run`, `run_all` and
        // `érun` hold `run` inside a longer word and count here.
        assert_eq!(
            truth("run", literal(false, false)),
            hits(&[("src/a.rs", &[1, 2, 4]), ("docs/b.txt", &[1, 2, 3, 4])])
        );
    }

    #[test]
    fn a_whole_word_literal_drops_the_hits_inside_a_longer_word() {
        // Line 2 of a.rs hits through its second `run`; `RUN run_all` does not
        // (`run_` is followed by `_`); `x_run` and `érun` have a word
        // character before; the last line of b.txt hits through its second
        // `run`.
        assert_eq!(
            truth("run", literal(false, true)),
            hits(&[("src/a.rs", &[1, 2]), ("docs/b.txt", &[2, 4])])
        );
    }

    #[test]
    fn a_case_insensitive_literal_adds_the_capitalised_lines() {
        assert_eq!(
            truth("run", literal(true, false)),
            hits(&[("src/a.rs", &[1, 2, 3, 4]), ("docs/b.txt", &[1, 2, 3, 4])])
        );
    }

    #[test]
    fn a_case_insensitive_whole_word_literal_keeps_the_capitalised_words() {
        assert_eq!(
            truth("run", literal(true, true)),
            hits(&[("src/a.rs", &[1, 2, 3, 4]), ("docs/b.txt", &[2, 4])])
        );
    }

    #[test]
    fn case_folding_is_unicode() {
        assert_eq!(
            truth("\u{c9}RUN", literal(true, false)),
            hits(&[("docs/b.txt", &[3, 4])])
        );
        assert!(truth("\u{c9}RUN", literal(false, false)).is_empty());
    }

    #[test]
    fn an_empty_match_regex_hits_every_line_and_no_line_past_the_last() {
        // a.rs has 5 lines (the last one empty) and no 6th; blank.txt has its
        // one empty line; empty.txt has none.
        assert_eq!(
            truth("", regex_mode(false, false)),
            hits(&[
                ("src/a.rs", &[1, 2, 3, 4, 5]),
                ("docs/b.txt", &[1, 2, 3, 4]),
                ("blank.txt", &[1])
            ])
        );
        assert_eq!(
            truth("x*", regex_mode(false, false)),
            truth("", regex_mode(false, false)),
            "a pattern that can match nothing also matches every line"
        );
    }

    #[test]
    fn a_regex_with_alternation_and_anchors_hits_the_anchored_lines() {
        assert_eq!(
            truth("^(fn|let) |time$", regex_mode(false, false)),
            hits(&[("src/a.rs", &[1, 2]), ("docs/b.txt", &[2])])
        );
    }

    #[test]
    fn a_whole_word_regex_judges_the_match_not_the_bare_literal() {
        // Only `running` (line 2 of a.rs) matches `run[a-z]*ing`, and it is a
        // whole word there.
        assert_eq!(
            truth("run[a-z]*ing", regex_mode(false, true)),
            hits(&[("src/a.rs", &[2])])
        );
    }

    #[test]
    fn a_whole_word_literal_agrees_with_the_whole_word_oracle() {
        let ww = WholeWord::new().unwrap();
        for word in ["run", "running", "Run", "run_all"] {
            assert_eq!(
                truth(word, literal(false, true)),
                ww.lines(LINE_FIXTURE, word),
                "{word}"
            );
        }
    }

    #[test]
    fn smart_case_is_insensitive_only_for_a_pattern_without_an_uppercase_character() {
        for (pattern, ignore_case, smart_case, want) in [
            ("run", false, false, false),
            ("run", true, false, true),
            ("run", false, true, true),
            ("Run", false, true, false),
            ("run_9", false, true, true),
            ("r\u{e9}", false, true, true),
            ("r\u{c9}", false, true, false),
            ("", false, true, true),
            // `-i` wins over `-S`.
            ("Run", true, true, true),
        ] {
            assert_eq!(
                is_case_insensitive(pattern, ignore_case, smart_case),
                want,
                "{pattern:?} i={ignore_case} S={smart_case}"
            );
        }
    }

    #[test]
    fn an_empty_literal_and_a_broken_regex_are_refused() {
        assert!(LineQuery::new("", literal(false, false)).is_err());
        assert!(LineQuery::new("", literal(true, true)).is_err());
        let err = LineQuery::new("(", regex_mode(false, false)).unwrap_err();
        assert!(err.to_string().contains("\"(\""), "{err}");
    }

    #[test]
    fn a_whole_word_empty_regex_needs_a_non_word_character_on_both_sides() {
        let q = lq("", regex_mode(false, true));
        for line in ["", " ", "a  b", "-"] {
            assert!(q.matches_line(line), "{line:?}");
        }
        // Every gap of these touches a word character; the gap at the end of
        // the line is tried too.
        for line in ["abc", "a b", "a-b"] {
            assert!(!q.matches_line(line), "{line:?}");
        }
    }

    #[test]
    fn a_later_overlapping_match_can_be_the_whole_word() {
        // `a-a` at 1 has `x` before it; the overlapping `a-a` at 3 does not.
        for mode in [
            literal(false, true),
            literal(true, true),
            regex_mode(false, true),
        ] {
            assert!(lq("a-a", mode).matches_line("xa-a-a"), "{mode:?}");
            assert!(!lq("a-a", mode).matches_line("xa-a-ax"), "{mode:?}");
        }
    }

    #[test]
    fn a_regex_anchor_stays_at_the_line_start_after_a_rejected_match() {
        // `^.` matches `-` at 0, which has a word character after it. The
        // search then resumes at 1, where `^` cannot match. (Run on the slice
        // `a-` it would match `a`, a whole word between `-` and `-`.)
        assert!(!lq("^.", regex_mode(false, true)).matches_line("-a-"));
    }

    #[test]
    fn line_numbers_count_newlines_and_keep_carriage_returns() {
        // Line 1 of b.txt is `x_run y\r`: `\r` is not a word character, so
        // `y` is a whole word there.
        assert_eq!(
            truth("y", literal(false, true)),
            hits(&[("docs/b.txt", &[1])])
        );
    }

    // --- the rival simulation ----------------------------------------------------

    #[test]
    fn simulated_rg_word_gives_the_frozen_bytes_of_the_fixture() {
        // Files come in `rg --sort path` order (blank.txt, docs/, empty.txt,
        // src/). The four whole-word lines are 22, 23, 23 and 32 bytes
        // (`é` is 2 bytes).
        let sim = simulate_rg_word(LINE_FIXTURE, "run").unwrap();
        assert_eq!(
            String::from_utf8(sim.output().to_vec()).unwrap(),
            "docs/b.txt:2:run-time\n\
             docs/b.txt:4:\u{e9}run run\n\
             src/a.rs:1:fn run() {}\n\
             src/a.rs:2:let running = run();\n"
        );
        assert_eq!(sim.total_bytes(), 100);
        let ends: Vec<usize> = sim.lines().iter().map(|l| l.end).collect();
        assert_eq!(ends, [22, 45, 68, 100]);
        assert_eq!(sim.bytes_through("docs/b.txt", 4), Some(45));
        assert_eq!(sim.bytes_through("src/a.rs", 2), Some(100));
        assert_eq!(
            sim.bytes_through("src/a.rs", 4),
            None,
            "`run_all` is not the word `run`"
        );
    }

    #[test]
    fn simulated_rg_word_keeps_carriage_returns_and_terminates_the_last_line() {
        let sim = simulate_rg_word([("w.rs", "foo\r\nbar foo")], "foo").unwrap();
        assert_eq!(sim.output(), b"w.rs:1:foo\r\nw.rs:2:bar foo\n");
    }

    #[test]
    fn simulated_rg_word_is_never_larger_than_the_substring_simulation() {
        let word = simulate_rg_word(LINE_FIXTURE, "run").unwrap();
        let fixed = simulate_rg_fixed(LINE_FIXTURE, "run");
        assert!(word.total_bytes() < fixed.total_bytes());
        assert!(
            word.lines()
                .iter()
                .all(|l| fixed.bytes_through(&l.path, l.line).is_some())
        );
    }

    #[test]
    fn simulated_rg_word_refuses_an_empty_word() {
        assert!(simulate_rg_word(LINE_FIXTURE, "").is_err());
    }
}
