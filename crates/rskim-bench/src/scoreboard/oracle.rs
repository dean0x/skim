//! Ground-truth predicates and in-process baselines for the search
//! scoreboard (#203).
//!
//! Ported from the 2026-09-25 seed benchmark
//! (`.devflow/docs/reviews/vision-gap-2026-09/bench/lib.py`: `gt_and_substring`,
//! `gt_phrase`, `gt_near`). Deliberately independent of skim's code: nothing
//! here imports `rskim_search::query_substring_present`,
//! `rskim_core::Language`, or an `rskim-search` tokenizer. Where a rule must
//! agree with skim (the word-byte class, the `--lang` names), this module
//! carries its own copy, so a change on skim's side shows up as a scoreboard
//! diff instead of silently flowing through.
//!
//! Everything here is pure: callers pass `(path, text)` pairs, typically from
//! [`crate::scoreboard::universe::Universe::files`].

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

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
// Word tokens (the oracle's own copy of skim's word-byte rule)
// ============================================================================

/// The oracle's copy of `rskim_search::lexical::tokenize::is_word_byte`
/// (`crates/rskim-search/src/lexical/tokenize.rs:22-24`): ASCII `[A-Za-z0-9_]`.
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
/// `rskim_core::Language::from_extension`, `crates/rskim-core/src/types.rs:55-80`)
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

/// Simulated `rg -n -F --sort path <pattern>` output (see
/// [`simulate_rg_fixed`]).
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
    let mut ordered: Vec<(&str, &str)> = files.into_iter().collect();
    ordered.sort_by(|a, b| rg_path_cmp(a.0, b.0));

    let mut sim = RgSimulation::default();
    for (path, text) in ordered {
        if text.is_empty() {
            continue;
        }
        let body = text.strip_suffix('\n').unwrap_or(text);
        for (idx, line) in body.split('\n').enumerate() {
            if !line.contains(pattern) {
                continue;
            }
            let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
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
}
