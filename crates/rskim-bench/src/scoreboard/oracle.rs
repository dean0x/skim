//! Ground-truth predicates and in-process baselines for the search
//! scoreboard (#203).
//!
//! Deliberately independent of skim's code: nothing here imports a skim crate
//! or a skim tokenizer. Where a rule must agree with skim (the word-character
//! class, the `-t` type table, the `-g` glob rules), this module carries its
//! own copy, so a change on skim's side shows up as a scoreboard diff instead
//! of silently flowing through.
//!
//! Everything here is pure: callers pass `(path, text)` pairs, typically from
//! [`crate::scoreboard::universe::TextUniverse::files`].

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::Path;

use regex::{Regex, RegexBuilder};

// ============================================================================
// Query model
// ============================================================================

/// A word-set ground-truth query: every whitespace-separated token occurs as a
/// case-sensitive substring somewhere in the file, optionally restricted to
/// one file type. Non-empty by construction: [`LexicalQuery::new`] refuses a
/// query that would match vacuously.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexicalQuery {
    raw: String,
    tokens: Vec<String>,
    lang: Option<LangFilter>,
}

impl LexicalQuery {
    /// Parse `query`, optionally restricted to `lang`.
    ///
    /// # Errors
    ///
    /// The query has no whitespace-separated token.
    pub fn new(query: &str, lang: Option<LangFilter>) -> anyhow::Result<Self> {
        let tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
        if tokens.is_empty() {
            anyhow::bail!("query {query:?} has no whitespace-separated token");
        }
        Ok(LexicalQuery {
            raw: query.to_string(),
            tokens,
            lang,
        })
    }

    /// The query text as given.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The file-type restriction, if any.
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
        self.tokens.iter().all(|t| text.contains(t.as_str()))
    }
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
// ============================================================================
// File filters (`-t TYPE` and `-g GLOB`): the oracle's own copy of the rg rules
// ============================================================================

/// One `rg` default file type: the names `-t` accepts, the `lang` values a
/// golden entry may use for it, the token the scoreboard passes as `-t`, and
/// the file-name globs that define it.
///
/// The globs are a copy of the `ignore` crate's default type table
/// (`ignore` 0.4.25, `src/default_types.rs`): `js` line 118, `json` line 119,
/// `markdown`/`md` line 173, `py`/`python` line 217, `rust` line 241, `toml`
/// line 299, `ts`/`typescript` line 300. This module does not import
/// `ignore`: the copy is the point, so a change in that table moves the
/// scoreboard instead of flowing through. Types match the file name only, and
/// case-sensitively.
#[derive(Debug, PartialEq, Eq)]
struct TypeSpec {
    /// The token passed as `-t`.
    token: &'static str,
    /// Every name `-t` accepts for the type.
    names: &'static [&'static str],
    /// The `lang` values a golden entry may use.
    langs: &'static [&'static str],
    /// File-name globs: `*SUFFIX` (the name ends with `SUFFIX`) or a whole
    /// file name.
    globs: &'static [&'static str],
}

const TYPES: &[TypeSpec] = &[
    TypeSpec {
        token: "ts",
        names: &["ts", "typescript"],
        langs: &["typescript", "tsx", "ts"],
        globs: &["*.ts", "*.tsx", "*.cts", "*.mts"],
    },
    TypeSpec {
        token: "js",
        names: &["js"],
        langs: &["javascript", "js"],
        globs: &["*.js", "*.jsx", "*.vue", "*.cjs", "*.mjs"],
    },
    TypeSpec {
        token: "py",
        names: &["py", "python"],
        langs: &["python", "py"],
        globs: &["*.py", "*.pyi"],
    },
    TypeSpec {
        token: "rust",
        names: &["rust"],
        langs: &["rust"],
        globs: &["*.rs"],
    },
    TypeSpec {
        token: "json",
        names: &["json"],
        langs: &["json"],
        globs: &["*.json", "composer.lock", "*.sarif"],
    },
    TypeSpec {
        token: "md",
        names: &["markdown", "md"],
        langs: &["markdown", "md"],
        globs: &[
            "*.markdown",
            "*.md",
            "*.mdown",
            "*.mdwn",
            "*.mkd",
            "*.mkdn",
            "*.mdx",
        ],
    },
    TypeSpec {
        token: "toml",
        names: &["toml"],
        langs: &["toml"],
        globs: &["*.toml", "Cargo.lock"],
    },
];

/// Whether the type glob `glob` fits the file name `name`.
fn type_glob_fits(glob: &str, name: &str) -> bool {
    match glob.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => name == glob,
    }
}

/// A file-type restriction: one `rg` default type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LangFilter {
    spec: &'static TypeSpec,
}

impl LangFilter {
    /// Resolve a golden `lang` value, case-insensitively.
    ///
    /// # Errors
    ///
    /// The value names no supported type.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let lower = value.to_ascii_lowercase();
        TYPES
            .iter()
            .find(|t| t.langs.contains(&lower.as_str()))
            .map(|spec| LangFilter { spec })
            .ok_or_else(|| anyhow::anyhow!("unsupported lang value {value:?}"))
    }

    /// Resolve a `-t` value exactly as `rg` reads it: a type name, case
    /// sensitive.
    ///
    /// # Errors
    ///
    /// The oracle has no copy of that type.
    pub fn from_type_name(name: &str) -> anyhow::Result<Self> {
        TYPES
            .iter()
            .find(|t| t.names.contains(&name))
            .map(|spec| LangFilter { spec })
            .ok_or_else(|| anyhow::anyhow!("the oracle has no copy of file type {name:?}"))
    }

    /// The token to pass as `-t`.
    pub fn type_token(&self) -> &'static str {
        self.spec.token
    }

    /// The type's file-name globs.
    pub fn globs(&self) -> &'static [&'static str] {
        self.spec.globs
    }

    /// Whether the file name of `path` (the part after the last `/`) is in
    /// the type.
    pub fn matches_path(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.spec.globs.iter().any(|g| type_glob_fits(g, name))
    }
}

/// One `-g` glob, compiled to a regex over a `/`-separated relative path.
#[derive(Debug, Clone)]
struct GlobRule {
    regex: Regex,
    /// A trailing `/`: the glob names folders only.
    dir_only: bool,
    /// A leading `!`: a match leaves the path out.
    negated: bool,
}

/// Compile a `-g` glob. The oracle covers the gitignore subset the golden
/// entries use: literals, `*`, `?`, `**`, a leading `!` and a trailing `/`.
/// A glob with a `/` (after dropping a trailing one) is anchored to the root;
/// any other glob fits at any depth.
///
/// # Errors
///
/// An empty glob, or one with a character class, an alternation or an escape.
fn compile_glob(glob: &str) -> anyhow::Result<GlobRule> {
    let (negated, body) = match glob.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, glob),
    };
    let (body, dir_only) = match body.strip_suffix('/') {
        Some(rest) => (rest, true),
        None => (body, false),
    };
    if body.is_empty() {
        anyhow::bail!("glob {glob:?} is empty");
    }
    if body.contains(['[', ']', '{', '}', '\\']) {
        anyhow::bail!("glob {glob:?}: the oracle covers `*`, `?`, `**`, `!` and a trailing `/`");
    }
    let anchored = body.contains('/');
    let chars: Vec<char> = body.strip_prefix('/').unwrap_or(body).chars().collect();
    let mut pattern = String::from("^");
    if !anchored {
        pattern.push_str("(?:.*/)?");
    }
    let mut i = 0;
    // Each step moves `i` forward.
    while let Some(&c) = chars.get(i) {
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            let at_start = i == 0 || chars.get(i - 1) == Some(&'/');
            if at_start && chars.get(i + 2) == Some(&'/') {
                pattern.push_str("(?:.*/)?");
                i += 3;
            } else {
                pattern.push_str(".*");
                i += 2;
            }
        } else if c == '*' {
            pattern.push_str("[^/]*");
            i += 1;
        } else if c == '?' {
            pattern.push_str("[^/]");
            i += 1;
        } else {
            pattern.push_str(&regex::escape(&c.to_string()));
            i += 1;
        }
    }
    pattern.push('$');
    let regex =
        Regex::new(&pattern).map_err(|e| anyhow::anyhow!("glob {glob:?} does not compile: {e}"))?;
    Ok(GlobRule {
        regex,
        dir_only,
        negated,
    })
}

/// The `-t` and `-g` filters of one query, with `rg`'s rules:
///
/// - A path passes when it is in at least one selected type (no type selected
///   means every path).
/// - Globs follow the `ignore` crate's override rules. The last glob that
///   fits a path decides: a `!` glob leaves the path out, any other glob
///   keeps it. A file no glob fits is left out when some glob keeps paths.
/// - A glob that leaves out a folder leaves out every file below it.
#[derive(Debug, Clone, Default)]
pub struct PathFilter {
    types: Vec<LangFilter>,
    rules: Vec<GlobRule>,
}

impl PathFilter {
    /// A filter that keeps every path.
    pub fn new() -> Self {
        PathFilter::default()
    }

    /// Select the type `name` (a `-t` value).
    ///
    /// # Errors
    ///
    /// The oracle has no copy of that type.
    pub fn with_type(mut self, name: &str) -> anyhow::Result<Self> {
        self.types.push(LangFilter::from_type_name(name)?);
        Ok(self)
    }

    /// Add the glob `glob` (a `-g` value).
    ///
    /// # Errors
    ///
    /// See `compile_glob`: an empty glob or a syntax the oracle does not cover.
    pub fn with_glob(mut self, glob: &str) -> anyhow::Result<Self> {
        self.rules.push(compile_glob(glob)?);
        Ok(self)
    }

    /// Whether the filter keeps every path.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.rules.is_empty()
    }

    /// Whether the glob rules leave `path` out. `is_dir` marks a folder.
    fn glob_leaves_out(&self, path: &str, is_dir: bool) -> bool {
        let fit = self
            .rules
            .iter()
            .rev()
            .find(|r| (!r.dir_only || is_dir) && r.regex.is_match(path));
        match fit {
            Some(rule) => rule.negated,
            None => !is_dir && self.rules.iter().any(|r| !r.negated),
        }
    }

    /// Whether the file at `path` (relative, `/`-separated) passes.
    pub fn keeps(&self, path: &str) -> bool {
        if !self.rules.is_empty() {
            for (idx, _) in path.match_indices('/') {
                if let Some(folder) = path.get(..idx)
                    && self.glob_leaves_out(folder, true)
                {
                    return false;
                }
            }
            if self.glob_leaves_out(path, false) {
                return false;
            }
        }
        self.types.is_empty() || self.types.iter().any(|t| t.matches_path(path))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;

    fn q(query: &str) -> LexicalQuery {
        LexicalQuery::new(query, None).unwrap()
    }

    fn hit(query: &str, text: &str) -> bool {
        q(query).matches("f.rs", text)
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // --- query construction -------------------------------------------------

    #[test]
    fn empty_or_blank_query_is_rejected() {
        assert!(LexicalQuery::new("", None).is_err());
        assert!(LexicalQuery::new(" \t ", None).is_err());
    }

    #[test]
    fn raw_query_round_trips() {
        assert_eq!(q("build lock").raw(), "build lock");
    }

    // --- and ------------------------------------------------------------------

    #[test]
    fn and_needs_every_whitespace_token_somewhere_in_the_file() {
        assert!(hit("foo bar", "bar on line one\nfoo on line two"));
        assert!(!hit("foo bar", "only foo here"));
    }

    #[test]
    fn and_is_a_case_sensitive_substring_match() {
        assert!(hit("staleness", "fn check_staleness() {}"));
        assert!(!hit("Staleness", "fn check_staleness() {}"));
        assert!(hit("STALENESS", "const STALENESS_LIMIT: u8 = 1;"));
    }

    #[test]
    fn and_treats_punctuation_as_part_of_the_token() {
        assert!(hit("-D warnings", "cargo clippy -- -D warnings"));
        assert!(!hit("-D warnings", "D warnings"));
        assert!(hit("Option<&str>", "fn f(x: Option<&str>) {}"));
        assert!(hit("::new(", "let v = Vec::new();"));
    }

    #[test]
    fn and_splits_the_query_on_any_whitespace() {
        assert!(hit("  foo\t\tbar ", "foo bar"));
    }

    // --- file types (-t) ------------------------------------------------------

    #[test]
    fn a_type_matches_the_file_name_globs_of_the_rg_table() {
        let ts = LangFilter::from_type_name("ts").unwrap();
        for p in ["a.ts", "src/b.tsx", "c.mts", "d/e.cts"] {
            assert!(ts.matches_path(p), "{p}");
        }
        assert!(!ts.matches_path("e.js"));
        let js = LangFilter::from_type_name("js").unwrap();
        for p in ["a.js", "b.jsx", "c.vue", "d.cjs", "e.mjs"] {
            assert!(js.matches_path(p), "{p}");
        }
        assert!(!js.matches_path("a.ts"));
        let py = LangFilter::from_type_name("py").unwrap();
        assert!(py.matches_path("a.py") && py.matches_path("a.pyi"));
        let md = LangFilter::from_type_name("md").unwrap();
        for p in ["README.md", "x.markdown", "x.mdx", "x.mkd"] {
            assert!(md.matches_path(p), "{p}");
        }
        assert!(
            LangFilter::from_type_name("rust")
                .unwrap()
                .matches_path("a/b.rs")
        );
    }

    #[test]
    fn the_file_name_globs_of_a_type_fit_whole_names() {
        let toml = LangFilter::from_type_name("toml").unwrap();
        assert!(toml.matches_path("Cargo.lock") && toml.matches_path("x/Cargo.lock"));
        assert!(!toml.matches_path("cargo.lock") && !toml.matches_path("Cargo.lock.bak"));
        let json = LangFilter::from_type_name("json").unwrap();
        assert!(json.matches_path("composer.lock") && json.matches_path("a.sarif"));
        assert!(!json.matches_path("mycomposer.lock"));
    }

    #[test]
    fn a_type_match_is_case_sensitive_and_reads_the_file_name_only() {
        let md = LangFilter::from_type_name("md").unwrap();
        assert!(!md.matches_path("README.MD"));
        assert!(!md.matches_path("docs.md/notes"));
    }

    #[test]
    fn a_type_has_every_rg_name() {
        assert_eq!(
            LangFilter::from_type_name("typescript").unwrap(),
            LangFilter::from_type_name("ts").unwrap()
        );
        assert_eq!(
            LangFilter::from_type_name("python").unwrap(),
            LangFilter::from_type_name("py").unwrap()
        );
        assert_eq!(
            LangFilter::from_type_name("markdown").unwrap(),
            LangFilter::from_type_name("md").unwrap()
        );
        assert!(LangFilter::from_type_name("TS").is_err());
        assert!(LangFilter::from_type_name("haskell").is_err());
    }

    #[test]
    fn a_golden_lang_value_maps_to_its_rg_type_token() {
        let token = |lang: &str| LangFilter::parse(lang).unwrap().type_token();
        assert_eq!(token("javascript"), "js");
        assert_eq!(token("typescript"), "ts");
        assert_eq!(token("tsx"), "ts");
        assert_eq!(token("python"), "py");
        assert_eq!(token("rust"), "rust");
        assert_eq!(token("json"), "json");
        assert_eq!(token("markdown"), "md");
        assert_eq!(token("toml"), "toml");
        assert_eq!(token("TOML"), "toml");
        assert!(LangFilter::parse("haskell").is_err());
        assert!(LangFilter::parse("").is_err());
    }

    // --- path filter (-t and -g) ----------------------------------------------

    fn globbed(globs: &[&str]) -> PathFilter {
        globs
            .iter()
            .try_fold(PathFilter::new(), |f, g| f.with_glob(g))
            .unwrap()
    }

    #[test]
    fn an_empty_filter_keeps_every_path() {
        let f = PathFilter::new();
        assert!(f.is_empty());
        assert!(f.keeps("a/b.rs") && f.keeps("Makefile"));
    }

    #[test]
    fn a_glob_without_a_slash_fits_at_any_depth() {
        let f = globbed(&["*.rs"]);
        assert!(f.keeps("a.rs") && f.keeps("src/deep/b.rs"));
        assert!(!f.keeps("a.py"));
    }

    #[test]
    fn a_glob_with_a_slash_is_anchored_to_the_root() {
        let f = globbed(&["src/*.rs"]);
        assert!(f.keeps("src/a.rs"));
        assert!(!f.keeps("src/deep/a.rs") && !f.keeps("other/src/a.rs"));
        let g = globbed(&["/main.rs"]);
        assert!(g.keeps("main.rs") && !g.keeps("src/main.rs"));
    }

    #[test]
    fn a_double_star_crosses_folders() {
        let f = globbed(&["src/**/*.rs"]);
        assert!(f.keeps("src/a.rs") && f.keeps("src/x/y/b.rs"));
        assert!(!f.keeps("lib/a.rs"));
        let g = globbed(&["**/test_*.py"]);
        assert!(g.keeps("test_a.py") && g.keeps("x/y/test_a.py"));
    }

    #[test]
    fn a_star_does_not_cross_a_slash_and_a_question_mark_is_one_character() {
        assert!(!globbed(&["a/*.rs"]).keeps("a/b/c.rs"));
        let f = globbed(&["?.rs"]);
        assert!(f.keeps("a.rs") && !f.keeps("ab.rs"));
    }

    #[test]
    fn a_negated_glob_leaves_out_what_it_fits() {
        let f = globbed(&["!*.md"]);
        assert!(f.keeps("a.rs") && f.keeps("src/b.py"));
        assert!(!f.keeps("README.md") && !f.keeps("docs/x.md"));
    }

    #[test]
    fn a_negated_glob_that_fits_a_folder_leaves_out_everything_below_it() {
        let f = globbed(&["!tests"]);
        assert!(!f.keeps("tests/a.rs") && !f.keeps("tests/deep/b.rs"));
        assert!(f.keeps("src/a.rs"));
        // The glob floats: a folder of that name at any depth is left out.
        assert!(!f.keeps("src/tests/a.rs"));
        assert!(!f.keeps("src/tests"));
    }

    #[test]
    fn a_glob_with_a_trailing_slash_names_folders_only() {
        let f = globbed(&["!build/"]);
        assert!(!f.keeps("build/a.rs") && !f.keeps("x/build/a.rs"));
        assert!(f.keeps("build"));
    }

    #[test]
    fn the_last_glob_that_fits_decides() {
        let f = globbed(&["!*.md", "README.md"]);
        assert!(f.keeps("README.md"));
        assert!(!f.keeps("CHANGELOG.md"));
        let g = globbed(&["README.md", "!*.md"]);
        assert!(!g.keeps("README.md"));
    }

    #[test]
    fn a_whitelist_glob_leaves_out_a_file_no_glob_fits() {
        let f = globbed(&["*.rs", "*.py"]);
        assert!(f.keeps("a.rs") && f.keeps("b.py"));
        assert!(!f.keeps("c.md") && !f.keeps("Makefile"));
    }

    #[test]
    fn a_glob_the_oracle_does_not_cover_is_refused() {
        for glob in ["", "!", "/", "*.[ch]", "*.{rs,py}", "a\\*"] {
            assert!(PathFilter::new().with_glob(glob).is_err(), "{glob:?}");
        }
    }

    #[test]
    fn a_type_and_a_glob_must_both_pass() {
        let f = PathFilter::new()
            .with_type("rust")
            .unwrap()
            .with_glob("!tests")
            .unwrap();
        assert!(f.keeps("src/a.rs"));
        assert!(!f.keeps("tests/a.rs") && !f.keeps("src/a.py"));
    }

    #[test]
    fn several_types_are_a_union() {
        let f = PathFilter::new()
            .with_type("rust")
            .unwrap()
            .with_type("py")
            .unwrap();
        assert!(f.keeps("a.rs") && f.keeps("b.py") && !f.keeps("c.md"));
        assert!(PathFilter::new().with_type("nope").is_err());
    }

    // --- ground truth -----------------------------------------------------------

    #[test]
    fn ground_truth_is_the_sorted_set_of_matching_files() {
        let files = [
            ("z.rs", "fn foo() {}"),
            ("b.rs", "fn bar() {}"),
            ("a.rs", "fn foo_bar() {}"),
        ];
        let gt = ground_truth(files, &q("foo"));
        assert_eq!(gt, strings(&["a.rs", "z.rs"]));
    }

    #[test]
    fn ground_truth_applies_the_type_filter() {
        let files = [
            ("Cargo.toml", "rskim-core = 1"),
            ("README.md", "rskim-core"),
        ];
        let query =
            LexicalQuery::new("rskim-core", Some(LangFilter::parse("toml").unwrap())).unwrap();
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
