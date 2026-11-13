//! Shared data types for the search scoreboard (#203): what one skim
//! invocation returns ([`ResultPage`]) and the HARD-check vocabulary
//! ([`CheckId`], [`CheckOutcome`]).
//!
//! skim's `--json` envelope (`render.rs::json_line` in `crates/rskim/src/cmd/
//! search/`) is parsed leniently through [`serde_json::Value`]: unknown keys
//! are ignored. What the scoreboard needs must be present and well-typed. An
//! `error` key, a missing key or a value of the wrong type is an error, which
//! the scoreboard reports as a harness error (exit 2), never as a regression.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ============================================================================
// Rows and pages
// ============================================================================

/// One line of a result's snippet: an occurrence the file lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetLine {
    /// 1-based line number in the file.
    pub line_number: u32,
    pub content: String,
    /// Always `true`: the envelope lists matching lines only.
    pub is_match: bool,
}

/// One result row: a matched file.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultRow {
    /// Repo-relative path.
    pub path: String,
    /// The file's score.
    pub score: f64,
    /// The line of the file's first listed occurrence; `None` when the file
    /// lists none.
    pub line: Option<u32>,
    /// The file's listed occurrences, as snippet lines.
    pub snippet: Vec<SnippetLine>,
}

/// What a matched line is. The names are the role keys of the search engine:
/// `def`, `call`, `ref`, `import`, `comment`, `string` and `text` are the
/// roles a frozen site can have; `partial` (the hit is inside a longer
/// identifier) and `unlabelled` (the engine did not parse the file) only
/// occur in the engine's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Def,
    Call,
    Ref,
    Import,
    Comment,
    String,
    Text,
    Partial,
    Unlabelled,
}

impl Role {
    /// The lowercase name used in JSON and in failure details.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Def => "def",
            Role::Call => "call",
            Role::Ref => "ref",
            Role::Import => "import",
            Role::Comment => "comment",
            Role::String => "string",
            Role::Text => "text",
            Role::Partial => "partial",
            Role::Unlabelled => "unlabelled",
        }
    }

    /// The role named `name`, or `None` for a name this scoreboard does not
    /// know.
    pub fn from_name(name: &str) -> Option<Role> {
        const ALL: [Role; 9] = [
            Role::Def,
            Role::Call,
            Role::Ref,
            Role::Import,
            Role::Comment,
            Role::String,
            Role::Text,
            Role::Partial,
            Role::Unlabelled,
        ];
        ALL.into_iter().find(|r| r.as_str() == name)
    }
}

/// One matching line of a result page: `path:line`, with its role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub path: String,
    /// 1-based line number.
    pub line: u32,
    pub role: Option<Role>,
}

/// One definition site of the queried name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionSite {
    pub path: String,
    /// 1-based line that holds the name.
    pub line: u32,
}

/// The definitions of the queried name that a page reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definitions {
    /// How many definitions the engine counted. `None` = the output states
    /// no count.
    pub count: Option<u64>,
    /// The count is a lower bound: the parse budget ran out, so some matched
    /// files have no labels.
    pub at_least: bool,
    /// The definition sites, in the engine's order: the first is the one the
    /// result anchors on.
    pub sites: Vec<DefinitionSite>,
}

/// The work counters of one call (`cost` in the engine's envelope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cost {
    pub files_read: u64,
    pub bytes_read: u64,
    pub files_parsed: u64,
    pub bytes_parsed: u64,
}

/// One page of results from one skim invocation.
///
/// A result's rank is its index in `rows` plus the invocation's `--offset`.
/// `rows` are the files that hold every word of the query. In the word view,
/// the engine lists the files that hold only some words after those, when
/// fewer than three files hold all of them: such a fallback file is not a
/// row, and none of its lines is an occurrence.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultPage {
    pub rows: Vec<ResultRow>,
    /// `has_more` (absent = `false`).
    pub has_more: bool,
    /// The matching lines of the rows' files, with their roles.
    pub occurrences: Vec<Occurrence>,
    /// The definitions of the queried name; `None` = the output has none.
    pub definitions: Option<Definitions>,
    /// Matched files that have no labels (`parse.unlabelled_files`).
    pub unlabelled_files: u64,
    /// Files the search read (`searched_files`); `None` = not stated.
    pub searched_files: Option<u64>,
    /// Files left out of the search, by reason (`left_out`); `None` = not
    /// stated.
    pub left_out: Option<BTreeMap<String, u64>>,
    /// Matching lines per file (`files[].line_count`) of the rows' files.
    pub line_counts: BTreeMap<String, u64>,
    /// The work counters of the call; `None` = not stated.
    pub cost: Option<Cost>,
    /// The process exit code. A page parsed from JSON has `0`; the runner
    /// overwrites it with the real status.
    pub exit_code: i32,
}

/// One `files[]` element, before the word filter.
struct FileEntry {
    row: ResultRow,
    occurrences: Vec<Occurrence>,
    line_count: u64,
    words_matched: u64,
    words_total: u64,
}

impl ResultPage {
    /// Parse skim's `--json` stdout.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout is not a JSON object, the object has an
    /// `error` key, a key the scoreboard reads is missing or has the wrong
    /// type, or an occurrence names a role this scoreboard does not know.
    pub fn parse(stdout: &[u8]) -> anyhow::Result<Self> {
        let value: Value =
            serde_json::from_slice(stdout).context("skim stdout is not valid JSON")?;
        Self::from_json(&value)
    }

    /// [`ResultPage::parse`] over an already-parsed value.
    ///
    /// # Errors
    ///
    /// As [`ResultPage::parse`].
    pub fn from_json(value: &Value) -> anyhow::Result<Self> {
        let obj = as_object(value, "skim output")?;
        if let Some(error) = obj.get("error") {
            anyhow::bail!("skim reported an error: {error}");
        }
        let files = parse_each(req_array(obj, "files")?, "files", parse_file)?;
        let has_more = opt_bool(obj, "has_more")?.unwrap_or(false);
        let definitions = parse_definitions(req(obj, "definitions", Some, "an object")?)?;
        let parse_obj = as_object(req(obj, "parse", Some, "an object")?, "`parse`")?;
        let unlabelled_files = req_u64(parse_obj, "unlabelled_files")?;
        let cost_obj = as_object(req(obj, "cost", Some, "an object")?, "`cost`")?;

        let mut page = ResultPage {
            has_more,
            definitions: Some(definitions),
            unlabelled_files,
            searched_files: Some(req_u64(obj, "searched_files")?),
            left_out: Some(parse_left_out(req(obj, "left_out", Some, "an object")?)?),
            cost: Some(Cost {
                files_read: req_u64(cost_obj, "files_read")?,
                bytes_read: req_u64(cost_obj, "bytes_read")?,
                files_parsed: req_u64(cost_obj, "files_parsed")?,
                bytes_parsed: req_u64(cost_obj, "bytes_parsed")?,
            }),
            ..ResultPage::default()
        };
        for file in files
            .into_iter()
            .filter(|f| f.words_matched == f.words_total)
        {
            page.line_counts
                .insert(file.row.path.clone(), file.line_count);
            page.occurrences.extend(file.occurrences);
            page.rows.push(file.row);
        }
        Ok(page)
    }

    /// Parse the stdout of a `-l` run: one path per line. The page has one
    /// row per path, with no score, line or snippet, and the process's
    /// `exit_code`. Empty output is a page of no rows.
    ///
    /// # Errors
    ///
    /// Stdout is not UTF-8, or holds an empty line.
    pub fn from_files_text(stdout: &[u8], exit_code: i32) -> anyhow::Result<Self> {
        let mut page = ResultPage {
            exit_code,
            ..ResultPage::default()
        };
        for path in text_lines(stdout)? {
            page.rows.push(bare_row(path));
        }
        Ok(page)
    }

    /// Parse the stdout of a `-c` run: one `path:count` per line. The page
    /// has one row per path and the count of each in `line_counts`.
    ///
    /// # Errors
    ///
    /// Stdout is not UTF-8, or a line is not `path:count`.
    pub fn from_counts_text(stdout: &[u8], exit_code: i32) -> anyhow::Result<Self> {
        let mut page = ResultPage {
            exit_code,
            ..ResultPage::default()
        };
        for line in text_lines(stdout)? {
            let (path, count) = line
                .rsplit_once(':')
                .with_context(|| format!("count line {line:?} is not path:count"))?;
            let count = count
                .parse::<u64>()
                .with_context(|| format!("count line {line:?} has no integer count"))?;
            anyhow::ensure!(!path.is_empty(), "count line {line:?} has no path");
            page.line_counts.insert(path.to_string(), count);
            page.rows.push(bare_row(path));
        }
        Ok(page)
    }
}

/// The lines of a text-mode stdout.
fn text_lines(stdout: &[u8]) -> anyhow::Result<Vec<&str>> {
    let text = std::str::from_utf8(stdout).context("skim stdout is not UTF-8")?;
    let lines: Vec<&str> = text.lines().collect();
    anyhow::ensure!(
        lines.iter().all(|l| !l.is_empty()),
        "skim stdout has an empty line"
    );
    Ok(lines)
}

/// A row for a file a text-mode list names: no score, line or snippet.
fn bare_row(path: &str) -> ResultRow {
    ResultRow {
        path: path.to_string(),
        score: 0.0,
        line: None,
        snippet: Vec::new(),
    }
}

fn parse_file(value: &Value) -> anyhow::Result<FileEntry> {
    let obj = as_object(value, "file")?;
    let path = req_str(obj, "path")?.to_string();
    let score =
        req(obj, "score", Value::as_f64, "a number").with_context(|| format!("file {path:?}"))?;
    let occurrences = parse_each(req_array(obj, "occurrences")?, "occurrences", |o| {
        parse_occurrence(&path, o)
    })
    .with_context(|| format!("file {path:?}"))?;
    let snippet = occurrences
        .iter()
        .map(|o| SnippetLine {
            line_number: o.line,
            content: o.text.clone(),
            is_match: true,
        })
        .collect();
    let line = occurrences.first().map(|o| o.line);
    Ok(FileEntry {
        row: ResultRow {
            path: path.clone(),
            score,
            line,
            snippet,
        },
        line_count: req_u64(obj, "line_count").with_context(|| format!("file {path:?}"))?,
        words_matched: req_u64(obj, "words_matched").with_context(|| format!("file {path:?}"))?,
        words_total: req_u64(obj, "words_total").with_context(|| format!("file {path:?}"))?,
        occurrences: occurrences
            .into_iter()
            .map(|o| Occurrence {
                path: path.clone(),
                line: o.line,
                role: Some(o.role),
            })
            .collect(),
    })
}

/// One `occurrences[]` element, before it is tied to a page.
struct ListedLine {
    line: u32,
    role: Role,
    text: String,
}

fn parse_occurrence(path: &str, value: &Value) -> anyhow::Result<ListedLine> {
    let obj = as_object(value, "occurrence")?;
    let name = req_str(obj, "role")?;
    let role = Role::from_name(name)
        .with_context(|| format!("{path}: unknown role {name:?} in an occurrence"))?;
    Ok(ListedLine {
        line: req(obj, "line", as_line, "a line number")?,
        role,
        text: opt_str(obj, "text")?.unwrap_or_default().to_string(),
    })
}

fn parse_definitions(value: &Value) -> anyhow::Result<Definitions> {
    let obj = as_object(value, "`definitions`")?;
    let sites = parse_each(req_array(obj, "sites")?, "definitions.sites", |site| {
        let site = as_object(site, "definition site")?;
        Ok(DefinitionSite {
            path: req_str(site, "path")?.to_string(),
            line: req(site, "line", as_line, "a line number")?,
        })
    })?;
    Ok(Definitions {
        count: Some(req_u64(obj, "count")?),
        at_least: opt_bool(obj, "at_least")?.unwrap_or(false),
        sites,
    })
}

fn parse_left_out(value: &Value) -> anyhow::Result<BTreeMap<String, u64>> {
    as_object(value, "`left_out`")?
        .iter()
        .map(|(reason, n)| {
            n.as_u64()
                .map(|n| (reason.clone(), n))
                .with_context(|| format!("left_out.{reason} is not an integer"))
        })
        .collect()
}

// ============================================================================
// JSON helpers
// ============================================================================

fn as_object<'a>(value: &'a Value, what: &str) -> anyhow::Result<&'a Map<String, Value>> {
    value
        .as_object()
        .with_context(|| format!("{what} is not a JSON object"))
}

/// An optional key: absent or `null` is `None`; a present value must be
/// what `get` extracts (`what` names that type in the error).
fn opt<'a, T>(
    obj: &'a Map<String, Value>,
    key: &str,
    get: impl Fn(&'a Value) -> Option<T>,
    what: &str,
) -> anyhow::Result<Option<T>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => get(v)
            .map(Some)
            .with_context(|| format!("`{key}` is not {what}")),
    }
}

/// A required key: it must be present, not `null`, and what `get` extracts.
fn req<'a, T>(
    obj: &'a Map<String, Value>,
    key: &str,
    get: impl Fn(&'a Value) -> Option<T>,
    what: &str,
) -> anyhow::Result<T> {
    opt(obj, key, get, what)?.with_context(|| format!("skim output has no `{key}`"))
}

fn req_str<'a>(obj: &'a Map<String, Value>, key: &str) -> anyhow::Result<&'a str> {
    req(obj, key, Value::as_str, "a string")
}

fn req_u64(obj: &Map<String, Value>, key: &str) -> anyhow::Result<u64> {
    req(obj, key, Value::as_u64, "an integer")
}

fn req_array<'a>(obj: &'a Map<String, Value>, key: &str) -> anyhow::Result<&'a [Value]> {
    req(obj, key, |v| v.as_array().map(Vec::as_slice), "an array")
}

fn opt_str<'a>(obj: &'a Map<String, Value>, key: &str) -> anyhow::Result<Option<&'a str>> {
    opt(obj, key, Value::as_str, "a string")
}

fn opt_bool(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<bool>> {
    opt(obj, key, Value::as_bool, "a boolean")
}

/// A JSON integer that fits `u32`.
fn as_line(value: &Value) -> Option<u32> {
    value.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// Parse every element of the JSON array `what`, naming a failing element
/// `what[i]`.
fn parse_each<T>(
    values: &[Value],
    what: &str,
    parse: impl Fn(&Value) -> anyhow::Result<T>,
) -> anyhow::Result<Vec<T>> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| parse(v).with_context(|| format!("{what}[{i}]")))
        .collect()
}

// ============================================================================
// HARD checks
// ============================================================================

/// The kind of golden entry a check runs against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    Ident,
    Concept,
    Lexical,
    Pagination,
    Prefix,
    /// `[[refs]]`: every site of one name, frozen with its role (#540).
    Refs,
    /// `[[clash]]`: a name with two or more definitions, frozen with its
    /// definitions and calls (#540).
    Clash,
    /// `[[stakes]]`: a plain-text query whose lines, files, counts or exit
    /// code are compared with the regex oracle (#540).
    Stakes,
}

/// A HARD check (tolerance 0; the ledger in `known_failures.toml` names
/// these by their dotted name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckId {
    /// Every ground-truth file is returned.
    LexicalRecall,
    /// Every returned file is in the ground truth.
    LexicalPrecision,
    /// No ground-truth file is missing. The envelope has no channel that
    /// discloses a missed file, so a miss is always silent.
    LexicalSilentFn,
    /// The union of all pages equals the full list.
    PaginationComplete,
    /// No file appears on two pages.
    PaginationDisjoint,
    /// The concatenated pages equal the full list, in order.
    PaginationOrdered,
    /// No `has_more: true` on an empty page, `false` on the last page, and
    /// the sweep ends within `MAX_PAGES`.
    PaginationHasMoreHonest,
    /// `--limit N` equals the first N rows of the full list.
    OrderPrefixConsistent,
    /// `score` is non-increasing down a full list.
    OrderScoreMonotone,
    /// No path appears twice within one list: the full list, a `--limit`
    /// list, or one page of a sweep.
    ResultsUniquePaths,
    /// `[[refs]]`: every frozen site is among the page's occurrences.
    RefsRecall,
    /// `[[refs]]`: every frozen call site is an occurrence with the role
    /// `call`.
    RefsCallsRecall,
    /// `[[clash]]`: every frozen definition is among the definition sites.
    ClashDefsComplete,
    /// `[[clash]]`: the stated definition count equals the frozen number of
    /// definitions (or is a valid lower bound).
    ClashDefCount,
    /// `[[clash]]`: every frozen call site of either definition is an
    /// occurrence with the role `call`.
    ClashCallsRecall,
    /// `[[lexical]]`: every file of the text universe (no language
    /// allow-list) that holds the query is returned.
    LexicalRecallTracked,
    /// `[[refs]]`: the searched files plus each left-out reason count equal
    /// the oracle text universe's counts.
    UniverseAccounted,
    /// `[[ident]]`: the first definition site is the frozen definition line.
    IdentDefFirst,
    /// `[[concept]]` with `first`: a frozen first file is among the first
    /// five files.
    ConceptFirstRightTop5,
    /// `[[stakes]]` with `expect = "lines"`: the matching lines equal the
    /// regex oracle's.
    StakesLines,
    /// `[[stakes]]` with `expect = "files"`: the matching files equal the
    /// regex oracle's.
    StakesFiles,
    /// `[[stakes]]` with `expect = "counts"`: the matching lines per file
    /// equal the regex oracle's.
    StakesCounts,
    /// `[[stakes]]` with `expect = "exit"`: the exit code equals the frozen
    /// one.
    StakesExit,
}

impl CheckId {
    /// Every HARD check, in report order.
    pub const ALL: &'static [CheckId] = &[
        CheckId::LexicalRecall,
        CheckId::LexicalPrecision,
        CheckId::LexicalSilentFn,
        CheckId::PaginationComplete,
        CheckId::PaginationDisjoint,
        CheckId::PaginationOrdered,
        CheckId::PaginationHasMoreHonest,
        CheckId::OrderPrefixConsistent,
        CheckId::OrderScoreMonotone,
        CheckId::ResultsUniquePaths,
        CheckId::RefsRecall,
        CheckId::RefsCallsRecall,
        CheckId::ClashDefsComplete,
        CheckId::ClashDefCount,
        CheckId::ClashCallsRecall,
        CheckId::LexicalRecallTracked,
        CheckId::UniverseAccounted,
        CheckId::IdentDefFirst,
        CheckId::ConceptFirstRightTop5,
        CheckId::StakesLines,
        CheckId::StakesFiles,
        CheckId::StakesCounts,
        CheckId::StakesExit,
    ];

    /// The dotted name used in reports and the ledger.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckId::LexicalRecall => "lexical.recall",
            CheckId::LexicalPrecision => "lexical.precision",
            CheckId::LexicalSilentFn => "lexical.silent_fn",
            CheckId::PaginationComplete => "pagination.complete",
            CheckId::PaginationDisjoint => "pagination.disjoint",
            CheckId::PaginationOrdered => "pagination.ordered",
            CheckId::PaginationHasMoreHonest => "pagination.has_more_honest",
            CheckId::OrderPrefixConsistent => "order.prefix_consistent",
            CheckId::OrderScoreMonotone => "order.score_monotone",
            CheckId::ResultsUniquePaths => "results.unique_paths",
            CheckId::RefsRecall => "refs.recall",
            CheckId::RefsCallsRecall => "refs.calls_recall",
            CheckId::ClashDefsComplete => "clash.defs_complete",
            CheckId::ClashDefCount => "clash.def_count",
            CheckId::ClashCallsRecall => "clash.calls_recall",
            CheckId::LexicalRecallTracked => "lexical.recall_tracked",
            CheckId::UniverseAccounted => "universe.accounted",
            CheckId::IdentDefFirst => "ident.def_first",
            CheckId::ConceptFirstRightTop5 => "concept.first_right_top5",
            CheckId::StakesLines => "stakes.lines",
            CheckId::StakesFiles => "stakes.files",
            CheckId::StakesCounts => "stakes.counts",
            CheckId::StakesExit => "stakes.exit",
        }
    }

    /// Whether this check can run against an entry of `kind`. Used to reject
    /// a ledger entry that pairs a check with an entry it can never apply to
    /// (it would XFAIL nothing).
    pub fn applies_to(self, kind: EntryKind) -> bool {
        match self {
            CheckId::PaginationComplete
            | CheckId::PaginationDisjoint
            | CheckId::PaginationOrdered
            | CheckId::PaginationHasMoreHonest => kind == EntryKind::Pagination,
            CheckId::OrderPrefixConsistent => kind == EntryKind::Prefix,
            // `[[refs]]`, `[[clash]]` and `[[stakes]]` have their own
            // oracles and checks, so no lexical or ordering check runs on
            // them.
            CheckId::LexicalRecall
            | CheckId::LexicalPrecision
            | CheckId::LexicalSilentFn
            | CheckId::OrderScoreMonotone => matches!(
                kind,
                EntryKind::Ident
                    | EntryKind::Concept
                    | EntryKind::Lexical
                    | EntryKind::Pagination
                    | EntryKind::Prefix
            ),
            // Every entry kind fetches a full list of file rows, one row per
            // path, so the check applies to the new kinds too.
            CheckId::ResultsUniquePaths => true,
            CheckId::RefsRecall | CheckId::RefsCallsRecall | CheckId::UniverseAccounted => {
                kind == EntryKind::Refs
            }
            CheckId::ClashDefsComplete | CheckId::ClashDefCount | CheckId::ClashCallsRecall => {
                kind == EntryKind::Clash
            }
            CheckId::LexicalRecallTracked => kind == EntryKind::Lexical,
            CheckId::IdentDefFirst => kind == EntryKind::Ident,
            CheckId::ConceptFirstRightTop5 => kind == EntryKind::Concept,
            CheckId::StakesLines
            | CheckId::StakesFiles
            | CheckId::StakesCounts
            | CheckId::StakesExit => kind == EntryKind::Stakes,
        }
    }
}

impl fmt::Display for CheckId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CheckId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        CheckId::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| anyhow::anyhow!("unknown HARD check {s:?}"))
    }
}

impl Serialize for CheckId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CheckId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The raw outcome of one HARD check on one entry, before the ledger is
/// applied (XFAIL / XPASS are the gate's business).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    Fail { detail: String },
}

impl CheckOutcome {
    /// A failure with a human-readable `detail`.
    pub fn fail(detail: impl Into<String>) -> Self {
        CheckOutcome::Fail {
            detail: detail.into(),
        }
    }

    /// Whether the check passed.
    pub fn is_pass(&self) -> bool {
        matches!(self, CheckOutcome::Pass)
    }

    /// The failure detail, if any.
    pub fn detail(&self) -> Option<&str> {
        match self {
            CheckOutcome::Pass => None,
            CheckOutcome::Fail { detail } => Some(detail),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use serde_json::json;

    fn occurrence(line: u32, role: &str, text: &str) -> Value {
        json!({
            "line": line, "column": 1, "role": role, "holder": null,
            "holder_range": null, "text": text, "text_truncated_bytes": 0,
        })
    }

    fn file(path: &str, matched: u64, total: u64, lines: &[(u32, &str)]) -> Value {
        json!({
            "path": path, "rank": 1, "score": 12.5, "prior": "source", "labelled": true,
            "words_matched": matched, "words_total": total,
            "line_count": lines.len(),
            "roles": {"def": 0, "call": 0, "ref": 0, "import": 0, "string": 0,
                      "comment": 0, "text": 0, "partial": 0, "unlabelled": 0},
            "occurrences": lines.iter().map(|(n, r)| occurrence(*n, r, "x")).collect::<Vec<_>>(),
            "occurrences_elided": 0,
        })
    }

    fn envelope(files: Vec<Value>) -> Value {
        json!({
            "query": "q", "match_mode": "literal", "case": "sensitive", "view": "labelled",
            "total_lines": 0, "total_files": files.len(), "has_more": false,
            "definitions": {"count": 1, "at_least": false, "sites": [
                {"path": "src/a.rs", "line": 7, "first_line": 7, "end_line": 9, "holder": null}
            ]},
            "files": files,
            "searched_files": 40,
            "left_out": {"binary": 2, "not_utf8": 0, "too_large": 1, "unreadable": 0,
                         "vanished": 0, "outside_root": 0, "symlink": 3},
            "parse": {"files": 2, "bytes": 90, "budget_hit": false, "unlabelled_files": 1},
            "cost": {"files_read": 40, "bytes_read": 1000, "files_parsed": 2, "bytes_parsed": 90},
            "notices": [], "duration_ms": 3,
        })
    }

    fn page(value: &Value) -> anyhow::Result<ResultPage> {
        ResultPage::parse(value.to_string().as_bytes())
    }

    // --- the envelope ---------------------------------------------------------

    #[test]
    fn a_page_reads_rows_occurrences_definitions_and_accounting_from_the_envelope() {
        let p = page(&envelope(vec![
            file("src/a.rs", 1, 1, &[(7, "def"), (20, "call")]),
            file("src/b.rs", 1, 1, &[(3, "ref")]),
        ]))
        .unwrap();

        let paths: Vec<&str> = p.rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "src/b.rs"]);
        assert_eq!(p.rows[0].score, 12.5);
        assert_eq!(p.rows[0].line, Some(7), "the line of the first occurrence");
        assert_eq!(
            p.rows[0].snippet,
            vec![
                SnippetLine {
                    line_number: 7,
                    content: "x".into(),
                    is_match: true
                },
                SnippetLine {
                    line_number: 20,
                    content: "x".into(),
                    is_match: true
                },
            ]
        );
        let found: Vec<(&str, u32, Option<Role>)> = p
            .occurrences
            .iter()
            .map(|o| (o.path.as_str(), o.line, o.role))
            .collect();
        assert_eq!(
            found,
            [
                ("src/a.rs", 7, Some(Role::Def)),
                ("src/a.rs", 20, Some(Role::Call)),
                ("src/b.rs", 3, Some(Role::Ref)),
            ]
        );
        assert_eq!(
            p.definitions,
            Some(Definitions {
                count: Some(1),
                at_least: false,
                sites: vec![DefinitionSite {
                    path: "src/a.rs".into(),
                    line: 7
                }],
            })
        );
        assert_eq!(p.searched_files, Some(40));
        let left_out = p.left_out.unwrap();
        assert_eq!(left_out.get("symlink"), Some(&3));
        assert_eq!(left_out.len(), 7);
        assert_eq!(p.unlabelled_files, 1);
        assert_eq!(
            p.cost,
            Some(Cost {
                files_read: 40,
                bytes_read: 1000,
                files_parsed: 2,
                bytes_parsed: 90
            })
        );
        assert_eq!(p.line_counts.get("src/a.rs"), Some(&2));
        assert!(!p.has_more);
        assert_eq!(p.exit_code, 0);
    }

    #[test]
    fn a_file_with_no_occurrence_has_no_line() {
        let p = page(&envelope(vec![file("a.rs", 1, 1, &[])])).unwrap();
        assert_eq!(p.rows[0].line, None);
        assert!(p.rows[0].snippet.is_empty());
    }

    #[test]
    fn has_more_is_read_when_present() {
        let mut e = envelope(vec![]);
        e["has_more"] = json!(true);
        assert!(page(&e).unwrap().has_more);
    }

    #[test]
    fn a_no_match_envelope_is_an_empty_page() {
        let p = page(&envelope(vec![])).unwrap();
        assert!(p.rows.is_empty() && p.occurrences.is_empty());
        assert_eq!(p.searched_files, Some(40));
    }

    #[test]
    fn word_view_fallback_files_are_not_rows_and_state_no_occurrence() {
        let p = page(&envelope(vec![
            file("both.rs", 2, 2, &[(4, "text")]),
            file("one_word.rs", 1, 2, &[(9, "text"), (10, "text")]),
        ]))
        .unwrap();
        let paths: Vec<&str> = p.rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["both.rs"]);
        assert_eq!(p.occurrences.len(), 1);
        assert_eq!(p.occurrences[0].path, "both.rs");
        assert!(!p.line_counts.contains_key("one_word.rs"));
    }

    #[test]
    fn the_row_reads_no_field_key() {
        let mut f = file("a.rs", 1, 1, &[(1, "def")]);
        f["field"] = json!("not_a_known_field");
        assert!(page(&envelope(vec![f])).is_ok());
    }

    // --- malformed output is an error (harness error, exit 2) ----------------

    #[test]
    fn an_error_envelope_is_an_error_naming_the_message() {
        let err = page(&json!({"error": "invalid regex: oops"})).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid regex: oops"),
            "{err:#}"
        );
    }

    #[test]
    fn a_key_the_scoreboard_reads_is_required() {
        for key in [
            "files",
            "definitions",
            "searched_files",
            "left_out",
            "parse",
            "cost",
        ] {
            let mut e = envelope(vec![]);
            e.as_object_mut().unwrap().remove(key);
            let err = page(&e).unwrap_err();
            assert!(format!("{err:#}").contains(key), "{key}: {err:#}");
        }
    }

    #[test]
    fn a_file_needs_its_path_score_counts_and_occurrences() {
        for key in [
            "path",
            "score",
            "words_matched",
            "words_total",
            "line_count",
            "occurrences",
        ] {
            let mut f = file("a.rs", 1, 1, &[(1, "def")]);
            f.as_object_mut().unwrap().remove(key);
            let err = page(&envelope(vec![f])).unwrap_err();
            assert!(format!("{err:#}").contains(key), "{key}: {err:#}");
        }
    }

    #[test]
    fn an_unknown_role_is_a_harness_error_naming_the_value() {
        let err = page(&envelope(vec![file("a.rs", 1, 1, &[(1, "bogus")])])).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("bogus") && text.contains("a.rs"), "{text}");
    }

    #[test]
    fn values_of_the_wrong_type_are_rejected() {
        let mut e = envelope(vec![]);
        e["searched_files"] = json!("forty");
        assert!(page(&e).is_err());
        let mut e = envelope(vec![]);
        e["left_out"]["binary"] = json!("two");
        assert!(page(&e).is_err());
        let mut e = envelope(vec![file("a.rs", 1, 1, &[(1, "def")])]);
        e["files"][0]["occurrences"][0]["line"] = json!(-1);
        assert!(page(&e).is_err());
        let mut e = envelope(vec![]);
        e["has_more"] = json!("yes");
        assert!(page(&e).is_err());
    }

    #[test]
    fn output_that_is_not_a_json_object_is_rejected() {
        assert!(ResultPage::parse(b"not json").is_err());
        assert!(ResultPage::parse(b"[]").is_err());
        assert!(ResultPage::parse(b"").is_err());
    }

    // --- checks ----------------------------------------------------------------

    #[test]
    fn check_ids_round_trip_through_their_dotted_names() {
        for check in CheckId::ALL {
            assert_eq!(check.as_str().parse::<CheckId>().unwrap(), *check);
            let json = serde_json::to_string(check).unwrap();
            assert_eq!(json, format!("\"{}\"", check.as_str()));
            assert_eq!(serde_json::from_str::<CheckId>(&json).unwrap(), *check);
        }
        assert_eq!(CheckId::ALL.len(), 23);
        assert!("lexical.recal".parse::<CheckId>().is_err());
    }

    #[test]
    fn the_structural_and_verify_mode_checks_are_gone() {
        for name in [
            "structural.recall",
            "structural.precision",
            "structural.coverage",
            "lexical.verify_mode",
        ] {
            assert!(name.parse::<CheckId>().is_err(), "{name}");
        }
    }

    #[test]
    fn check_applicability_follows_the_entry_kind() {
        assert!(CheckId::PaginationHasMoreHonest.applies_to(EntryKind::Pagination));
        assert!(!CheckId::PaginationHasMoreHonest.applies_to(EntryKind::Ident));
        assert!(CheckId::OrderPrefixConsistent.applies_to(EntryKind::Prefix));
        assert!(!CheckId::OrderPrefixConsistent.applies_to(EntryKind::Pagination));
        assert!(CheckId::OrderScoreMonotone.applies_to(EntryKind::Prefix));
        assert!(CheckId::LexicalRecall.applies_to(EntryKind::Concept));
        for kind in ALL_KINDS {
            assert!(CheckId::ResultsUniquePaths.applies_to(kind), "{kind:?}");
        }
    }

    const ALL_KINDS: [EntryKind; 8] = [
        EntryKind::Ident,
        EntryKind::Concept,
        EntryKind::Lexical,
        EntryKind::Pagination,
        EntryKind::Prefix,
        EntryKind::Refs,
        EntryKind::Clash,
        EntryKind::Stakes,
    ];

    #[test]
    fn lexical_and_ordering_checks_never_apply_to_refs_clash_or_stakes_entries() {
        // A ledger entry that pairs one of these checks with such an entry
        // would XFAIL nothing, so `applies_to` must refuse it.
        for check in [
            CheckId::LexicalRecall,
            CheckId::LexicalPrecision,
            CheckId::LexicalSilentFn,
            CheckId::OrderScoreMonotone,
            CheckId::OrderPrefixConsistent,
            CheckId::PaginationComplete,
        ] {
            for kind in [EntryKind::Refs, EntryKind::Clash, EntryKind::Stakes] {
                assert!(!check.applies_to(kind), "{check} on {kind:?}");
            }
        }
        for kind in [
            EntryKind::Ident,
            EntryKind::Concept,
            EntryKind::Lexical,
            EntryKind::Pagination,
            EntryKind::Prefix,
        ] {
            assert!(CheckId::LexicalRecall.applies_to(kind), "{kind:?}");
        }
    }

    #[test]
    fn the_new_checks_have_the_dotted_names_of_the_plan_and_run_on_their_own_kind_only() {
        let table: [(CheckId, &str, EntryKind); 13] = [
            (CheckId::RefsRecall, "refs.recall", EntryKind::Refs),
            (
                CheckId::RefsCallsRecall,
                "refs.calls_recall",
                EntryKind::Refs,
            ),
            (
                CheckId::ClashDefsComplete,
                "clash.defs_complete",
                EntryKind::Clash,
            ),
            (CheckId::ClashDefCount, "clash.def_count", EntryKind::Clash),
            (
                CheckId::ClashCallsRecall,
                "clash.calls_recall",
                EntryKind::Clash,
            ),
            (
                CheckId::LexicalRecallTracked,
                "lexical.recall_tracked",
                EntryKind::Lexical,
            ),
            (
                CheckId::UniverseAccounted,
                "universe.accounted",
                EntryKind::Refs,
            ),
            (CheckId::IdentDefFirst, "ident.def_first", EntryKind::Ident),
            (
                CheckId::ConceptFirstRightTop5,
                "concept.first_right_top5",
                EntryKind::Concept,
            ),
            (CheckId::StakesLines, "stakes.lines", EntryKind::Stakes),
            (CheckId::StakesFiles, "stakes.files", EntryKind::Stakes),
            (CheckId::StakesCounts, "stakes.counts", EntryKind::Stakes),
            (CheckId::StakesExit, "stakes.exit", EntryKind::Stakes),
        ];
        for (check, name, own) in table {
            assert_eq!(check.as_str(), name);
            assert_eq!(name.parse::<CheckId>().unwrap(), check);
            for kind in ALL_KINDS {
                assert_eq!(check.applies_to(kind), kind == own, "{check} on {kind:?}");
            }
        }
    }

    #[test]
    fn role_names_round_trip_and_an_unknown_name_is_none() {
        for name in [
            "def",
            "call",
            "ref",
            "import",
            "comment",
            "string",
            "text",
            "partial",
            "unlabelled",
        ] {
            assert_eq!(Role::from_name(name).unwrap().as_str(), name);
        }
        assert_eq!(Role::from_name("Call"), None);
        assert_eq!(Role::from_name(""), None);
    }

    #[test]
    fn check_outcome_reports_pass_and_fail() {
        assert!(CheckOutcome::Pass.is_pass());
        assert_eq!(CheckOutcome::Pass.detail(), None);
        let f = CheckOutcome::fail("missing a.rs");
        assert!(!f.is_pass());
        assert_eq!(f.detail(), Some("missing a.rs"));
    }
}
