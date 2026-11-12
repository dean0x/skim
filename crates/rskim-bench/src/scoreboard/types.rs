//! Shared data types for the search scoreboard (#203): what one skim
//! invocation returns ([`ResultPage`], [`StatsSnapshot`]) and the HARD-check
//! vocabulary ([`CheckId`], [`CheckOutcome`]).
//!
//! skim's JSON is parsed leniently through [`serde_json::Value`] (the design's
//! "JSON the runner reads" table): unknown keys are ignored, and the
//! additive `skip_serializing_if` fields default when absent — `has_more` to
//! `false`, `verify_mode` to substring, `degraded` to empty. What the
//! scoreboard needs (`results`, each row's `path` and score) must be present
//! and well-typed; anything else is an error, which the scoreboard reports as
//! a harness error (exit 2), never as a regression.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::scoreboard::oracle::MatchMode;

// ============================================================================
// Arms and rows
// ============================================================================

/// Which JSON envelope a `skim search --json` invocation produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arm {
    /// `QueryOutput` (`crates/rskim/src/cmd/search/types.rs:378-430`): every
    /// invocation with a text query, including `--phrase`, `--near`, `--lang`,
    /// text + temporal flags, and text + `--ast`. Rows use `score` and
    /// `line_number`; `snippet.lines[]` holds the context window.
    Lexical,
    /// Standalone `--ast` (`crates/rskim-search/src/compound/output.rs:204-222`).
    /// Rows use `score` and `line`; `snippet` is one string.
    Ast,
    /// Standalone `--hot` / `--cold` (`HotColdJson`); score key `hotspot_score`.
    HotCold,
    /// Standalone `--risky` (`RiskyJson`); score key `risk_score`.
    Risky,
    /// Standalone `--blast-radius` (`BlastRadiusJson`); score key `jaccard`.
    BlastRadius,
}

impl Arm {
    /// The key holding each row's score.
    fn score_key(self) -> &'static str {
        match self {
            Arm::Lexical | Arm::Ast => "score",
            Arm::HotCold => "hotspot_score",
            Arm::Risky => "risk_score",
            Arm::BlastRadius => "jaccard",
        }
    }
}

/// skim's `verify_mode` (absent = substring), or the mode a golden entry
/// declares.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VerifyMode {
    #[default]
    Substring,
    Phrase,
    Near,
    PhraseNear,
    /// A value this scoreboard does not know; kept so `lexical.verify_mode`
    /// can fail with the value in its detail instead of aborting the run.
    Unknown(String),
}

impl VerifyMode {
    fn from_json_name(name: &str) -> Self {
        match name {
            "phrase" => VerifyMode::Phrase,
            "near" => VerifyMode::Near,
            "phrase_near" => VerifyMode::PhraseNear,
            other => VerifyMode::Unknown(other.to_string()),
        }
    }
}

impl From<MatchMode> for VerifyMode {
    fn from(mode: MatchMode) -> Self {
        match mode {
            MatchMode::And => VerifyMode::Substring,
            MatchMode::Phrase => VerifyMode::Phrase,
            MatchMode::Near { .. } => VerifyMode::Near,
            MatchMode::PhraseNear { .. } => VerifyMode::PhraseNear,
        }
    }
}

/// One line of a result's snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnippetLine {
    /// 1-based line number in the file.
    pub line_number: u32,
    pub content: String,
    /// The anchor line (always `true` for the single AST snippet line).
    pub is_match: bool,
}

/// One result row, normalized across arms.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultRow {
    /// Repo-relative path.
    pub path: String,
    /// The arm's score (see [`Arm`] for the key).
    pub score: f64,
    /// Anchor line: `line_number` (lexical; `None` when `null`, e.g. queries
    /// under 3 bytes) or `line` (AST; `None` when absent). Always `None` for
    /// the temporal arms.
    pub line: Option<u32>,
    /// Lexical `snippet.lines[]`, or the AST `snippet` string as a single
    /// matching line. Empty when absent.
    pub snippet: Vec<SnippetLine>,
}

/// One `degraded[]` element (AD-414-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Degraded {
    pub subsystem: String,
    pub reason: String,
    pub requested: Option<String>,
    pub applied: Option<String>,
}

/// What a matched line is. The names follow the role rules of the search
/// engine: `def`, `call`, `ref`, `import`, `comment`, `string` and `text`
/// are the roles a frozen site can have; `partial` (the hit is inside a longer
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

/// One matching line of a result page: `path:line`, with its role when the
/// engine labels it (`None` = the engine gives no role).
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
/// skim's `total` is deliberately not kept: it is the page size, not a count.
///
/// [`ResultPage::parse`] fills the first four fields, and for the lexical
/// envelope `occurrences` and `definitions` too (the adapter for the current
/// output). The other fields are the accounting answers the #540 checks
/// read: the current output states none of them, so they stay at their
/// default (nothing observed) until [`ResultPage::with_accounting`] or the
/// runner fills them, and every check that reads one fails closed on an
/// absent value.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultPage {
    pub rows: Vec<ResultRow>,
    /// `has_more` (absent = `false`).
    pub has_more: bool,
    /// `verify_mode` (absent = [`VerifyMode::Substring`]). Only the lexical
    /// envelope carries it.
    pub verify_mode: VerifyMode,
    /// `degraded[]` (absent = empty). Standalone `--ast` never carries it
    /// (#483).
    pub degraded: Vec<Degraded>,
    /// The matching lines, one per `path:line`, with their roles.
    pub occurrences: Vec<Occurrence>,
    /// The definitions of the queried name; `None` = the output has none.
    pub definitions: Option<Definitions>,
    /// Matched files that have no labels (`parse.unlabelled_files`); `0` when
    /// absent.
    pub unlabelled_files: u64,
    /// Files the search read (`searched_files`); `None` = not stated.
    pub searched_files: Option<u64>,
    /// Files left out of the search, by reason (`left_out`); `None` = not
    /// stated.
    pub left_out: Option<BTreeMap<String, u64>>,
    /// Matching lines per file (`files[].line_count`); empty when the output
    /// has no counts.
    pub line_counts: BTreeMap<String, u64>,
    /// The work counters of the call; `None` = not stated.
    pub cost: Option<Cost>,
    /// The process exit code. A page parsed from JSON has `0`; the runner
    /// overwrites it with the real status.
    pub exit_code: i32,
}

impl ResultPage {
    /// Parse skim's stdout for `arm`.
    ///
    /// # Errors
    ///
    /// Returns an error if stdout is not a JSON object, `results` is missing
    /// or not an array, a row lacks a string `path` or a numeric score, or a
    /// present envelope/row field has the wrong type.
    pub fn parse(arm: Arm, stdout: &[u8]) -> anyhow::Result<Self> {
        let value: Value =
            serde_json::from_slice(stdout).context("skim stdout is not valid JSON")?;
        Self::from_json(arm, &value)
    }

    /// [`ResultPage::parse`] over an already-parsed value.
    ///
    /// # Errors
    ///
    /// As [`ResultPage::parse`].
    pub fn from_json(arm: Arm, value: &Value) -> anyhow::Result<Self> {
        let obj = as_object(value, "skim output")?;
        let results = obj
            .get("results")
            .context("skim output has no `results`")?
            .as_array()
            .context("`results` is not an array")?;
        let rows = parse_each(results, "results", |row| parse_row(arm, row))?;

        let has_more = opt_bool(obj, "has_more")?.unwrap_or(false);
        let verify_mode = opt_str(obj, "verify_mode")?
            .map(VerifyMode::from_json_name)
            .unwrap_or(VerifyMode::Substring);
        let degraded = match opt(obj, "degraded", Value::as_array, "an array")? {
            None => Vec::new(),
            Some(entries) => parse_each(entries, "degraded", parse_degraded)?,
        };

        let (occurrences, definitions) = match arm {
            Arm::Lexical => {
                let roles = parse_each(results, "results", field_role)?;
                let (occurrences, definitions) = line_answers(&rows, &roles);
                (occurrences, Some(definitions))
            }
            Arm::Ast | Arm::HotCold | Arm::Risky | Arm::BlastRadius => (Vec::new(), None),
        };

        Ok(ResultPage {
            rows,
            has_more,
            verify_mode,
            degraded,
            occurrences,
            definitions,
            ..ResultPage::default()
        })
    }

    /// This page with the corpus accounting of `stats`: `searched_files` is
    /// the index's file count and `left_out` its persisted skips by reason.
    /// The current output states neither, so the run takes them from
    /// `skim search --stats --json`.
    #[must_use]
    pub fn with_accounting(self, stats: &StatsSnapshot) -> Self {
        ResultPage {
            searched_files: Some(stats.file_count),
            left_out: Some(stats.skipped_by_reason.clone()),
            ..self
        }
    }
}

/// The `field` values that name a definition (`SearchField::TypeDefinition`
/// and `FunctionSignature`).
const DEFINITION_FIELDS: [&str; 2] = ["type_definition", "function_signature"];

/// The `field` values that name no role this scoreboard uses: the other six
/// `SearchField` names, and the co-change-only result of `--blast-radius`.
const OTHER_FIELDS: [&str; 7] = [
    "symbol_name",
    "import_export",
    "function_body",
    "comment",
    "string_literal",
    "other",
    "co_change_partner",
];

/// The role a lexical result's `field` gives its matching line: `def` for
/// a definition field, `None` for any other known field. The binary prints
/// the name `SearchField::name` returns
/// (`crates/rskim-search/src/types.rs:154-165`) in
/// `ResolvedResult::field` (`crates/rskim/src/cmd/search/types.rs:338`), and
/// `"co_change_partner"` for a co-change-only result
/// (`crates/rskim/src/cmd/search/query.rs:1234`).
///
/// # Errors
///
/// The row has no string `field`, or its value is none of those names: a
/// value this scoreboard does not know is a harness error, never a missing
/// role.
fn field_role(row: &Value) -> anyhow::Result<Option<Role>> {
    let obj = as_object(row, "row")?;
    let field = opt_str(obj, "field")?.context("row has no `field`")?;
    if DEFINITION_FIELDS.contains(&field) {
        Ok(Some(Role::Def))
    } else if OTHER_FIELDS.contains(&field) {
        Ok(None)
    } else {
        anyhow::bail!(
            "unknown `field` value {field:?} (known: {})",
            DEFINITION_FIELDS
                .iter()
                .chain(OTHER_FIELDS.iter())
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// The line-level answers of a lexical page: one occurrence for each
/// `is_match` snippet line of a result, with the result's role, and the
/// definitions, which are the results whose role is `def` and that state a
/// line. The output states no definition count.
fn line_answers(rows: &[ResultRow], roles: &[Option<Role>]) -> (Vec<Occurrence>, Definitions) {
    let mut occurrences = Vec::new();
    let mut sites = Vec::new();
    for (row, &role) in rows.iter().zip(roles) {
        occurrences.extend(
            row.snippet
                .iter()
                .filter(|l| l.is_match)
                .map(|l| Occurrence {
                    path: row.path.clone(),
                    line: l.line_number,
                    role,
                }),
        );
        if let (Some(Role::Def), Some(line)) = (role, row.line) {
            sites.push(DefinitionSite {
                path: row.path.clone(),
                line,
            });
        }
    }
    let definitions = Definitions {
        count: None,
        at_least: false,
        sites,
    };
    (occurrences, definitions)
}

fn parse_row(arm: Arm, value: &Value) -> anyhow::Result<ResultRow> {
    let obj = as_object(value, "row")?;
    let path = opt_str(obj, "path")?
        .context("row has no `path`")?
        .to_string();
    let score_key = arm.score_key();
    let score = obj
        .get(score_key)
        .and_then(Value::as_f64)
        .with_context(|| format!("row {path:?} has no numeric `{score_key}`"))?;

    let (line, snippet) = match arm {
        Arm::Lexical => (opt_u32(obj, "line_number")?, parse_snippet_lines(obj)?),
        Arm::Ast => {
            let line = opt_u32(obj, "line")?;
            let snippet = match (line, opt_str(obj, "snippet")?) {
                (Some(line_number), Some(content)) => vec![SnippetLine {
                    line_number,
                    content: content.to_string(),
                    is_match: true,
                }],
                _ => Vec::new(),
            };
            (line, snippet)
        }
        Arm::HotCold | Arm::Risky | Arm::BlastRadius => (None, Vec::new()),
    };

    Ok(ResultRow {
        path,
        score,
        line,
        snippet,
    })
}

fn parse_snippet_lines(row: &Map<String, Value>) -> anyhow::Result<Vec<SnippetLine>> {
    let Some(snippet) = row.get("snippet").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let lines = as_object(snippet, "snippet")?
        .get("lines")
        .context("snippet has no `lines`")?
        .as_array()
        .context("snippet `lines` is not an array")?;
    lines
        .iter()
        .map(|l| {
            let obj = as_object(l, "snippet line")?;
            Ok(SnippetLine {
                line_number: opt_u32(obj, "line_number")?
                    .context("snippet line has no `line_number`")?,
                content: opt_str(obj, "content")?.unwrap_or_default().to_string(),
                is_match: opt_bool(obj, "is_match")?.unwrap_or(false),
            })
        })
        .collect()
}

fn parse_degraded(value: &Value) -> anyhow::Result<Degraded> {
    let obj = as_object(value, "degraded entry")?;
    Ok(Degraded {
        subsystem: opt_str(obj, "subsystem")?
            .context("degraded entry has no `subsystem`")?
            .to_string(),
        reason: opt_str(obj, "reason")?
            .context("degraded entry has no `reason`")?
            .to_string(),
        requested: opt_str(obj, "requested")?.map(str::to_string),
        applied: opt_str(obj, "applied")?.map(str::to_string),
    })
}

// ============================================================================
// Standalone --ast: rows plus ast_coverage
// ============================================================================

/// The size-cap accounting a standalone `--ast` envelope carries in
/// `ast_coverage` (`AstJsonEnvelope`,
/// `crates/rskim-search/src/compound/output.rs:204-225`; the struct is
/// `rskim_search::AstCoverage`, `crates/rskim-search/src/ast_index/coverage.rs:81-97`).
///
/// skim omits the key when the coverage is clean — no size-excluded and no
/// undetermined file (`skip_serializing_if`, AD-405-3 / AC-405-9,
/// `crates/rskim/src/cmd/search/ast.rs:658-670`) — so an absent or `null`
/// key reads as all-zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AstCoverage {
    /// Indexed files in an AST-participating language over the AST size cap
    /// (the authoritative total).
    pub size_excluded_files: u64,
    /// Indexed files whose size or language skim could not determine.
    pub undetermined_files: u64,
    /// `size_excluded_files` by skim language name.
    pub excluded_by_lang: BTreeMap<String, u64>,
}

impl AstCoverage {
    /// Read `ast_coverage` from a standalone `--ast` envelope.
    ///
    /// # Errors
    ///
    /// `ast_coverage` is present but not an object, lacks an integer
    /// `size_excluded_files` / `undetermined_files`, or has a non-integer
    /// `excluded_by_lang` count.
    pub fn from_envelope(envelope: &Value) -> anyhow::Result<Self> {
        let obj = as_object(envelope, "skim output")?;
        let Some(coverage) = obj.get("ast_coverage").filter(|v| !v.is_null()) else {
            return Ok(AstCoverage::default());
        };
        let coverage = as_object(coverage, "ast_coverage")?;
        let count = |key: &str| {
            coverage
                .get(key)
                .and_then(Value::as_u64)
                .with_context(|| format!("ast_coverage has no integer `{key}`"))
        };
        let excluded_by_lang = match coverage.get("excluded_by_lang") {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(v) => as_object(v, "ast_coverage.excluded_by_lang")?
                .iter()
                .map(|(lang, n)| {
                    n.as_u64().map(|n| (lang.clone(), n)).with_context(|| {
                        format!("ast_coverage.excluded_by_lang.{lang} is not an integer")
                    })
                })
                .collect::<anyhow::Result<BTreeMap<_, _>>>()?,
        };
        Ok(AstCoverage {
            size_excluded_files: count("size_excluded_files")?,
            undetermined_files: count("undetermined_files")?,
            excluded_by_lang,
        })
    }
}

/// One standalone `--ast <pattern>` answer: its rows and its coverage.
#[derive(Debug, Clone, PartialEq)]
pub struct AstPage {
    pub page: ResultPage,
    pub coverage: AstCoverage,
}

impl AstPage {
    /// Parse a standalone `--ast` envelope.
    ///
    /// # Errors
    ///
    /// As [`ResultPage::from_json`] for [`Arm::Ast`], and
    /// [`AstCoverage::from_envelope`].
    pub fn from_json(value: &Value) -> anyhow::Result<Self> {
        Ok(AstPage {
            page: ResultPage::from_json(Arm::Ast, value)?,
            coverage: AstCoverage::from_envelope(value)?,
        })
    }
}

// ============================================================================
// --stats --json
// ============================================================================

/// The parts of `skim search --stats --json` the scoreboard compares
/// (`build_stats_json`, `crates/rskim/src/cmd/search/mod.rs:2199-2244`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// Files in the index.
    pub file_count: u64,
    /// Persisted (producer-phase) skips by reason, zero counts absent —
    /// compare with [`crate::scoreboard::universe::Universe::persisted_skipped_by_reason`].
    pub skipped_by_reason: BTreeMap<String, u64>,
    /// `temporal_state`: the health of skim's temporal data (`"ready"`, or
    /// `"empty"` / `"corrupt"` / `"newer-schema"` / `"missing"`). `None` when
    /// the key is absent.
    pub temporal_state: Option<String>,
}

impl StatsSnapshot {
    /// Parse skim's `--stats --json` stdout.
    ///
    /// # Errors
    ///
    /// Returns an error for non-JSON output, skim's `{"error": …}` envelope
    /// (no index), a missing `file_count`, a non-integer count, or a
    /// `temporal_state` that is not a string.
    pub fn parse(stdout: &[u8]) -> anyhow::Result<Self> {
        let value: Value =
            serde_json::from_slice(stdout).context("skim --stats stdout is not valid JSON")?;
        let obj = as_object(&value, "skim --stats output")?;
        if let Some(err) = obj.get("error") {
            anyhow::bail!("skim --stats reported an error: {err}");
        }
        let file_count = obj
            .get("file_count")
            .and_then(Value::as_u64)
            .context("skim --stats output has no integer `file_count`")?;
        let skipped_by_reason = match obj.get("skipped_by_reason") {
            None | Some(Value::Null) => BTreeMap::new(),
            Some(v) => as_object(v, "skipped_by_reason")?
                .iter()
                .map(|(k, n)| {
                    n.as_u64()
                        .map(|n| (k.clone(), n))
                        .with_context(|| format!("skipped_by_reason.{k} is not an integer"))
                })
                .collect::<anyhow::Result<BTreeMap<_, _>>>()?,
        };
        Ok(StatsSnapshot {
            file_count,
            skipped_by_reason,
            temporal_state: opt_str(obj, "temporal_state")?.map(str::to_string),
        })
    }
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

fn opt_str<'a>(obj: &'a Map<String, Value>, key: &str) -> anyhow::Result<Option<&'a str>> {
    opt(obj, key, Value::as_str, "a string")
}

fn opt_bool(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<bool>> {
    opt(obj, key, Value::as_bool, "a boolean")
}

/// An integer that fits `u32`.
fn opt_u32(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<u32>> {
    opt(
        obj,
        key,
        |v| v.as_u64().and_then(|n| u32::try_from(n).ok()),
        "a line number",
    )
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
    /// `[[ast]]`: one catalog pattern in one oracle language, scored against
    /// the structural oracle (#541).
    Ast,
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
    /// Every ground-truth file on the indexed universe is returned.
    LexicalRecall,
    /// Every returned file is in the ground truth.
    LexicalPrecision,
    /// No ground-truth file is missing while `degraded[]` is empty.
    LexicalSilentFn,
    /// JSON `verify_mode` equals the declared mode.
    LexicalVerifyMode,
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
    /// `score` is non-increasing down a full list without a temporal sort or
    /// `--blast-radius`.
    OrderScoreMonotone,
    /// No path appears twice within one list: the full list, a `--limit`
    /// list, or one page of a sweep.
    ResultsUniquePaths,
    /// `[[ast]]`: every file the structural oracle matches in the entry's
    /// language is returned (file-level recall 1.0).
    StructuralRecall,
    /// `[[ast]]` with `precision = "hard"`: every returned file of the
    /// entry's language is an oracle match (file-level precision 1.0).
    StructuralPrecision,
    /// `[[ast]]`: the call's `ast_coverage.size_excluded_files` equals the
    /// oracle's own over-cap count, and nothing is undetermined.
    StructuralCoverage,
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
        CheckId::LexicalVerifyMode,
        CheckId::PaginationComplete,
        CheckId::PaginationDisjoint,
        CheckId::PaginationOrdered,
        CheckId::PaginationHasMoreHonest,
        CheckId::OrderPrefixConsistent,
        CheckId::OrderScoreMonotone,
        CheckId::ResultsUniquePaths,
        CheckId::StructuralRecall,
        CheckId::StructuralPrecision,
        CheckId::StructuralCoverage,
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
            CheckId::LexicalVerifyMode => "lexical.verify_mode",
            CheckId::PaginationComplete => "pagination.complete",
            CheckId::PaginationDisjoint => "pagination.disjoint",
            CheckId::PaginationOrdered => "pagination.ordered",
            CheckId::PaginationHasMoreHonest => "pagination.has_more_honest",
            CheckId::OrderPrefixConsistent => "order.prefix_consistent",
            CheckId::OrderScoreMonotone => "order.score_monotone",
            CheckId::ResultsUniquePaths => "results.unique_paths",
            CheckId::StructuralRecall => "structural.recall",
            CheckId::StructuralPrecision => "structural.precision",
            CheckId::StructuralCoverage => "structural.coverage",
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
            // An `[[ast]]` entry is judged by the structural oracle; the
            // standalone `--ast` arm's ordering is `[[prefix]]` territory.
            // `[[refs]]`, `[[clash]]` and `[[stakes]]` have their own
            // oracles and checks, so no lexical or ordering check runs on
            // them.
            CheckId::LexicalRecall
            | CheckId::LexicalPrecision
            | CheckId::LexicalSilentFn
            | CheckId::LexicalVerifyMode
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
            CheckId::StructuralRecall
            | CheckId::StructuralPrecision
            | CheckId::StructuralCoverage => kind == EntryKind::Ast,
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

    fn page(arm: Arm, json: &str) -> anyhow::Result<ResultPage> {
        ResultPage::parse(arm, json.as_bytes())
    }

    // --- lexical envelope (QueryOutput) --------------------------------------

    #[test]
    fn lexical_page_defaults_every_omitted_envelope_field() {
        // has_more / verify_mode / degraded are all skip_serializing_if in skim.
        let p = page(
            Arm::Lexical,
            r#"{"query":"check_staleness","total":1,"duration_ms":3,
                "results":[{"path":"src/a.rs","score":2.5,"field":"function_signature",
                            "line_number":12,"line_range":{"start":12,"end":13},
                            "snippet":{"lines":[{"line_number":12,"content":"fn check_staleness()","is_match":true}]},
                            "stale":false}]}"#,
        )
        .unwrap();
        assert!(!p.has_more);
        assert_eq!(p.verify_mode, VerifyMode::Substring);
        assert!(p.degraded.is_empty());
        assert_eq!(
            p.rows,
            vec![ResultRow {
                path: "src/a.rs".to_string(),
                score: 2.5,
                line: Some(12),
                snippet: vec![SnippetLine {
                    line_number: 12,
                    content: "fn check_staleness()".to_string(),
                    is_match: true,
                }],
            }]
        );
    }

    #[test]
    fn lexical_page_reads_has_more_verify_mode_and_degraded() {
        let p = page(
            Arm::Lexical,
            r#"{"total":0,"has_more":true,"verify_mode":"phrase_near","results":[],
                "degraded":[{"subsystem":"temporal","reason":"missing","requested":"hot",
                             "applied":"lexical","message":"m","remediation":"r"}]}"#,
        )
        .unwrap();
        assert!(p.has_more);
        assert_eq!(p.verify_mode, VerifyMode::PhraseNear);
        assert_eq!(
            p.degraded,
            vec![Degraded {
                subsystem: "temporal".to_string(),
                reason: "missing".to_string(),
                requested: Some("hot".to_string()),
                applied: Some("lexical".to_string()),
            }]
        );
    }

    #[test]
    fn short_query_rows_carry_a_null_line() {
        let p = page(
            Arm::Lexical,
            r#"{"results":[{"path":"a.rs","score":0.0,"field":"other","line_number":null,"snippet":null}]}"#,
        )
        .unwrap();
        assert_eq!(p.rows[0].line, None);
        assert!(p.rows[0].snippet.is_empty());
    }

    #[test]
    fn verify_mode_names_map_and_unknown_names_are_kept() {
        for (name, mode) in [
            ("phrase", VerifyMode::Phrase),
            ("near", VerifyMode::Near),
            ("phrase_near", VerifyMode::PhraseNear),
            ("fuzzy", VerifyMode::Unknown("fuzzy".to_string())),
        ] {
            let json = format!(r#"{{"verify_mode":"{name}","results":[]}}"#);
            assert_eq!(
                page(Arm::Lexical, &json).unwrap().verify_mode,
                mode,
                "{name}"
            );
        }
    }

    #[test]
    fn verify_mode_follows_the_declared_match_mode() {
        use crate::scoreboard::oracle::MatchMode;
        assert_eq!(VerifyMode::from(MatchMode::And), VerifyMode::Substring);
        assert_eq!(VerifyMode::from(MatchMode::Phrase), VerifyMode::Phrase);
        assert_eq!(
            VerifyMode::from(MatchMode::Near { span: 5 }),
            VerifyMode::Near
        );
        assert_eq!(
            VerifyMode::from(MatchMode::PhraseNear { span: 5 }),
            VerifyMode::PhraseNear
        );
    }

    // --- other arms ---------------------------------------------------------

    #[test]
    fn ast_rows_use_line_and_a_string_snippet() {
        let p = page(
            Arm::Ast,
            r#"{"mode":"ast","pattern":"god-function","description":"d","total":2,
                "results":[{"path":"a.rs","score":1.5,"line":7,"snippet":"fn big() {","layers_matched":["ast"]},
                           {"path":"b.rs","score":1.0,"layers_matched":["ast"]}]}"#,
        )
        .unwrap();
        assert!(!p.has_more);
        assert_eq!(p.rows[0].line, Some(7));
        assert_eq!(
            p.rows[0].snippet,
            vec![SnippetLine {
                line_number: 7,
                content: "fn big() {".to_string(),
                is_match: true,
            }]
        );
        assert_eq!((p.rows[1].line, p.rows[1].snippet.len()), (None, 0));
    }

    #[test]
    fn temporal_arms_read_their_own_score_key() {
        let hot = page(
            Arm::HotCold,
            r#"{"mode":"hot","total":1,"has_more":true,"results":[{"path":"a.rs","hotspot_score":0.9,"changes_30d":1,"changes_90d":2}]}"#,
        )
        .unwrap();
        assert_eq!((hot.rows[0].score, hot.has_more), (0.9, true));

        let risky = page(
            Arm::Risky,
            r#"{"mode":"risky","total":1,"results":[{"path":"a.rs","risk_score":0.4,"fix_density":0.1,"fix_commits":1,"total_commits":9}]}"#,
        )
        .unwrap();
        assert_eq!(risky.rows[0].score, 0.4);

        let blast = page(
            Arm::BlastRadius,
            r#"{"mode":"blast-radius","target":"a.rs","total":1,"results":[{"path":"b.rs","jaccard":0.25,"count":3}]}"#,
        )
        .unwrap();
        assert_eq!((blast.rows[0].score, blast.rows[0].line), (0.25, None));
    }

    // --- malformed output is an error (harness error, exit 2) ----------------

    #[test]
    fn malformed_output_is_rejected() {
        for (arm, json) in [
            (Arm::Lexical, "skim search: no index found"),
            (Arm::Lexical, r#"{"total":0}"#),
            (Arm::Lexical, r#"{"results":{}}"#),
            (
                Arm::Lexical,
                r#"{"results":[{"score":1.0,"field":"other"}]}"#,
            ),
            (
                Arm::Lexical,
                r#"{"results":[{"path":"a.rs","score":"high","field":"other"}]}"#,
            ),
            (
                Arm::Lexical,
                r#"{"results":[{"path":"a.rs","field":"other"}]}"#,
            ),
            (Arm::Lexical, r#"{"has_more":"yes","results":[]}"#),
            (Arm::Lexical, r#"{"verify_mode":3,"results":[]}"#),
            (
                Arm::Lexical,
                r#"{"results":[{"path":"a.rs","score":1.0,"field":"other","line_number":-1}]}"#,
            ),
            (Arm::HotCold, r#"{"results":[{"path":"a.rs","score":1.0}]}"#),
        ] {
            assert!(page(arm, json).is_err(), "{arm:?}: {json}");
        }
    }

    // --- stats ----------------------------------------------------------------

    #[test]
    fn stats_snapshot_reads_file_count_and_persisted_skips() {
        let s = StatsSnapshot::parse(
            br#"{"file_count":828,"total_ngrams":1,"skipped":[{"path":"a.js","reason":"minified"}],
                "skipped_by_reason":{"minified":1},"git_head_state":"resolved"}"#,
        )
        .unwrap();
        assert_eq!(s.file_count, 828);
        assert_eq!(
            s.skipped_by_reason,
            BTreeMap::from([("minified".to_string(), 1)])
        );
    }

    #[test]
    fn stats_snapshot_without_skips_has_an_empty_breakdown() {
        let s = StatsSnapshot::parse(br#"{"file_count":3}"#).unwrap();
        assert!(s.skipped_by_reason.is_empty());
    }

    #[test]
    fn stats_snapshot_reads_the_temporal_state_when_present() {
        let ready = StatsSnapshot::parse(br#"{"file_count":3,"temporal_state":"ready"}"#).unwrap();
        assert_eq!(ready.temporal_state.as_deref(), Some("ready"));
        let refused =
            StatsSnapshot::parse(br#"{"file_count":3,"temporal_state":"newer-schema"}"#).unwrap();
        assert_eq!(refused.temporal_state.as_deref(), Some("newer-schema"));
        let absent = StatsSnapshot::parse(br#"{"file_count":3}"#).unwrap();
        assert_eq!(absent.temporal_state, None);
        assert!(StatsSnapshot::parse(br#"{"file_count":3,"temporal_state":2}"#).is_err());
    }

    #[test]
    fn stats_error_envelope_is_surfaced() {
        let err = StatsSnapshot::parse(br#"{"error":"no index found","cache_dir":"/x"}"#)
            .expect_err("an error envelope is not a snapshot");
        assert!(err.to_string().contains("no index found"), "{err}");
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
        assert_eq!(CheckId::ALL.len(), 27);
        for (check, name) in [
            (CheckId::StructuralRecall, "structural.recall"),
            (CheckId::StructuralPrecision, "structural.precision"),
            (CheckId::StructuralCoverage, "structural.coverage"),
        ] {
            assert_eq!(name.parse::<CheckId>().unwrap(), check);
        }
        assert!("lexical.recal".parse::<CheckId>().is_err());
    }

    #[test]
    fn check_applicability_follows_the_entry_kind() {
        assert!(CheckId::PaginationHasMoreHonest.applies_to(EntryKind::Pagination));
        assert!(!CheckId::PaginationHasMoreHonest.applies_to(EntryKind::Ident));
        assert!(CheckId::OrderPrefixConsistent.applies_to(EntryKind::Prefix));
        assert!(!CheckId::OrderPrefixConsistent.applies_to(EntryKind::Pagination));
        assert!(CheckId::OrderScoreMonotone.applies_to(EntryKind::Prefix));
        assert!(CheckId::LexicalRecall.applies_to(EntryKind::Concept));
        for kind in [
            EntryKind::Ident,
            EntryKind::Concept,
            EntryKind::Lexical,
            EntryKind::Pagination,
            EntryKind::Prefix,
            EntryKind::Ast,
            EntryKind::Refs,
            EntryKind::Clash,
            EntryKind::Stakes,
        ] {
            assert!(CheckId::ResultsUniquePaths.applies_to(kind), "{kind:?}");
        }
    }

    #[test]
    fn lexical_and_ordering_checks_never_apply_to_refs_clash_or_stakes_entries() {
        // A ledger entry that pairs one of these checks with such an entry
        // would XFAIL nothing, so `applies_to` must refuse it.
        for check in [
            CheckId::LexicalRecall,
            CheckId::LexicalPrecision,
            CheckId::LexicalSilentFn,
            CheckId::LexicalVerifyMode,
            CheckId::OrderScoreMonotone,
            CheckId::OrderPrefixConsistent,
            CheckId::PaginationComplete,
            CheckId::StructuralRecall,
        ] {
            for kind in [EntryKind::Refs, EntryKind::Clash, EntryKind::Stakes] {
                assert!(!check.applies_to(kind), "{check} on {kind:?}");
            }
        }
        // The five older kinds keep their lexical checks.
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
    fn structural_checks_apply_to_ast_entries_only() {
        let structural = [
            CheckId::StructuralRecall,
            CheckId::StructuralPrecision,
            CheckId::StructuralCoverage,
        ];
        for check in structural {
            assert!(check.applies_to(EntryKind::Ast), "{check}");
            for kind in [
                EntryKind::Ident,
                EntryKind::Concept,
                EntryKind::Lexical,
                EntryKind::Pagination,
                EntryKind::Prefix,
                EntryKind::Refs,
                EntryKind::Clash,
                EntryKind::Stakes,
            ] {
                assert!(!check.applies_to(kind), "{check} on {kind:?}");
            }
        }
        // An [[ast]] entry has no lexical oracle, and its standalone list is
        // path-ordered (#547): no lexical or ordering check can apply.
        for check in [
            CheckId::LexicalRecall,
            CheckId::LexicalPrecision,
            CheckId::LexicalSilentFn,
            CheckId::LexicalVerifyMode,
            CheckId::OrderScoreMonotone,
            CheckId::OrderPrefixConsistent,
            CheckId::PaginationComplete,
        ] {
            assert!(!check.applies_to(EntryKind::Ast), "{check}");
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
        let kinds = [
            EntryKind::Ident,
            EntryKind::Concept,
            EntryKind::Lexical,
            EntryKind::Pagination,
            EntryKind::Prefix,
            EntryKind::Ast,
            EntryKind::Refs,
            EntryKind::Clash,
            EntryKind::Stakes,
        ];
        for (check, name, own) in table {
            assert_eq!(check.as_str(), name);
            assert_eq!(name.parse::<CheckId>().unwrap(), check);
            for kind in kinds {
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

    // --- the adapter for the current search output (#540) ---------------------

    /// A `skim search --json` envelope as the current binary prints it (skim
    /// 2.11.0, `--limit 10 -- check_staleness` over a three-file repository),
    /// less the `index_stats` object. The first file defines the name, the
    /// second calls it, and the match line is the `is_match` snippet line.
    const CURRENT_ENVELOPE: &str = r#"{"query":"check_staleness","total":2,"results":[
        {"path":"staleness.rs","score":3.636363557547579,"field":"function_signature",
         "line_number":2,"line_range":{"start":2,"end":3},
         "snippet":{"lines":[
            {"line_number":1,"content":"// staleness checks","is_match":false},
            {"line_number":2,"content":"pub fn check_staleness() -> bool {","is_match":true},
            {"line_number":3,"content":"    true","is_match":false},
            {"line_number":4,"content":"}","is_match":false}]},
         "stale":false},
        {"path":"other.rs","score":0.6818181670401711,"field":"function_body",
         "line_number":2,"line_range":{"start":2,"end":3},
         "snippet":{"lines":[
            {"line_number":1,"content":"fn refresh() {","is_match":false},
            {"line_number":2,"content":"    if check_staleness() {}","is_match":true},
            {"line_number":3,"content":"}","is_match":false},
            {"line_number":4,"content":"// check_staleness is here","is_match":false}]},
         "stale":false}],
        "duration_ms":2}"#;

    fn occurrence(path: &str, line: u32, role: Option<Role>) -> Occurrence {
        Occurrence {
            path: path.to_string(),
            line,
            role,
        }
    }

    #[test]
    fn the_adapter_reads_occurrences_and_definition_sites_from_a_recorded_current_envelope() {
        let page = page(Arm::Lexical, CURRENT_ENVELOPE).unwrap();
        // One occurrence per result: its `is_match` line, never a context line.
        assert_eq!(
            page.occurrences,
            vec![
                occurrence("staleness.rs", 2, Some(Role::Def)),
                occurrence("other.rs", 2, None),
            ]
        );
        // The definition sites are the results whose field names a definition.
        assert_eq!(
            page.definitions,
            Some(Definitions {
                count: None,
                at_least: false,
                sites: vec![DefinitionSite {
                    path: "staleness.rs".to_string(),
                    line: 2,
                }],
            })
        );
    }

    #[test]
    fn the_current_output_states_nothing_beyond_occurrences_and_definition_sites() {
        let page = page(Arm::Lexical, CURRENT_ENVELOPE).unwrap();
        assert_eq!(page.unlabelled_files, 0);
        assert_eq!(page.searched_files, None);
        assert_eq!(page.left_out, None);
        assert!(page.line_counts.is_empty());
        assert_eq!(page.cost, None);
        assert_eq!(page.exit_code, 0);
    }

    #[test]
    fn every_field_value_the_current_binary_prints_maps_to_a_role() {
        // `SearchField::name` (eight values) and the co-change stub.
        for (field, role) in [
            ("type_definition", Some(Role::Def)),
            ("function_signature", Some(Role::Def)),
            ("symbol_name", None),
            ("import_export", None),
            ("function_body", None),
            ("comment", None),
            ("string_literal", None),
            ("other", None),
            ("co_change_partner", None),
        ] {
            let json = format!(
                r#"{{"results":[{{"path":"a.rs","score":1.0,"field":"{field}","line_number":3,
                    "snippet":{{"lines":[{{"line_number":3,"content":"x","is_match":true}}]}}}}]}}"#
            );
            let p = page(Arm::Lexical, &json).unwrap();
            assert_eq!(p.occurrences, vec![occurrence("a.rs", 3, role)], "{field}");
        }
    }

    #[test]
    fn an_unknown_field_value_is_a_harness_error_naming_the_value() {
        for field in ["text", "Function_Signature", "", "functionSignature"] {
            let json = format!(
                r#"{{"results":[{{"path":"a.rs","score":1.0,"field":"{field}","line_number":3}}]}}"#
            );
            let err = page(Arm::Lexical, &json).expect_err("an unknown field value");
            let msg = format!("{err:#}");
            assert!(msg.contains("field"), "{field:?}: {msg}");
            assert!(msg.contains(&format!("{field:?}")), "{field:?}: {msg}");
        }
    }

    #[test]
    fn a_lexical_row_without_a_string_field_is_a_harness_error() {
        for row in [
            r#"{"path":"a.rs","score":1.0,"line_number":3}"#,
            r#"{"path":"a.rs","score":1.0,"field":null,"line_number":3}"#,
            r#"{"path":"a.rs","score":1.0,"field":7,"line_number":3}"#,
        ] {
            let json = format!(r#"{{"results":[{row}]}}"#);
            assert!(page(Arm::Lexical, &json).is_err(), "{row}");
        }
    }

    #[test]
    fn a_result_with_no_snippet_or_line_gives_no_occurrence_and_no_definition_site() {
        // A query under three bytes: `line_number` and `snippet` are null.
        let p = page(
            Arm::Lexical,
            r#"{"results":[{"path":"a.rs","score":0.0,"field":"function_signature",
                            "line_number":null,"snippet":null}]}"#,
        )
        .unwrap();
        assert!(p.occurrences.is_empty());
        assert_eq!(p.definitions.unwrap().sites, vec![]);
    }

    #[test]
    fn an_empty_lexical_page_reports_no_definition_sites() {
        let p = page(Arm::Lexical, r#"{"results": []}"#).unwrap();
        assert!(p.occurrences.is_empty());
        let definitions = p.definitions.expect("a lexical page states definitions");
        assert_eq!(definitions.count, None);
        assert!(definitions.sites.is_empty());
    }

    #[test]
    fn definition_sites_keep_the_rank_order_of_the_results() {
        let row = |path: &str, field: &str, line: u32| {
            format!(
                r#"{{"path":"{path}","score":1.0,"field":"{field}","line_number":{line},
                    "snippet":{{"lines":[{{"line_number":{line},"content":"x","is_match":true}}]}}}}"#
            )
        };
        let json = format!(
            r#"{{"results":[{},{},{}]}}"#,
            row("b.rs", "type_definition", 9),
            row("c.rs", "function_body", 4),
            row("a.rs", "function_signature", 1)
        );
        let sites = page(Arm::Lexical, &json)
            .unwrap()
            .definitions
            .unwrap()
            .sites;
        let at: Vec<(&str, u32)> = sites.iter().map(|s| (s.path.as_str(), s.line)).collect();
        assert_eq!(at, [("b.rs", 9), ("a.rs", 1)]);
    }

    #[test]
    fn the_other_arms_state_no_line_level_answers() {
        // Their rows carry no `field`, and they name no definition.
        let p = page(
            Arm::HotCold,
            r#"{"results":[{"path":"a.rs","hotspot_score":0.9}]}"#,
        )
        .unwrap();
        assert!(p.occurrences.is_empty());
        assert_eq!(p.definitions, None);
        let p = page(
            Arm::Ast,
            r#"{"results":[{"path":"a.rs","score":1.5,"line":7,"snippet":"fn big() {"}]}"#,
        )
        .unwrap();
        assert!(p.occurrences.is_empty());
        assert_eq!(p.definitions, None);
    }

    #[test]
    fn the_corpus_accounting_comes_from_the_stats_snapshot() {
        let stats = StatsSnapshot::parse(
            br#"{"file_count":828,"skipped_by_reason":{"minified":1,"binary":2}}"#,
        )
        .unwrap();
        let p = page(Arm::Lexical, CURRENT_ENVELOPE)
            .unwrap()
            .with_accounting(&stats);
        assert_eq!(p.searched_files, Some(828));
        assert_eq!(
            p.left_out,
            Some(BTreeMap::from([
                ("binary".to_string(), 2),
                ("minified".to_string(), 1)
            ]))
        );
        // Nothing else changes.
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.occurrences.len(), 2);
    }

    // --- standalone --ast coverage ------------------------------------------------

    #[test]
    fn a_clean_ast_envelope_omits_ast_coverage_and_reads_as_zero() {
        let page = AstPage::from_json(
            &serde_json::from_str(
                r#"{"mode":"ast","pattern":"try-catch","description":"d","total":1,
                    "results":[{"path":"a.ts","score":1.0,"line":3,"snippet":"} catch (e) {"}]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(page.coverage, AstCoverage::default());
        assert_eq!(page.page.rows[0].line, Some(3));
        let null = serde_json::json!({"results": [], "ast_coverage": null});
        assert_eq!(
            AstCoverage::from_envelope(&null).unwrap(),
            AstCoverage::default()
        );
    }

    #[test]
    fn ast_coverage_reads_the_excluded_and_undetermined_counts() {
        let envelope = serde_json::json!({
            "results": [],
            "ast_coverage": {
                "size_eligible_files": 810,
                "size_excluded_files": 2,
                "undetermined_files": 0,
                "excluded_by_lang": {"rust": 1, "javascript": 1},
                "excluded": [{"path": "a.rs", "lang": "rust", "size_bytes": 2000000,
                              "limit_bytes": 1048576, "reason": "ast_size_cap"}]
            }
        });
        assert_eq!(
            AstCoverage::from_envelope(&envelope).unwrap(),
            AstCoverage {
                size_excluded_files: 2,
                undetermined_files: 0,
                excluded_by_lang: BTreeMap::from([
                    ("javascript".to_string(), 1),
                    ("rust".to_string(), 1)
                ]),
            }
        );
    }

    #[test]
    fn malformed_ast_coverage_is_rejected() {
        for coverage in [
            serde_json::json!([]),
            serde_json::json!({"undetermined_files": 0}),
            serde_json::json!({"size_excluded_files": "2", "undetermined_files": 0}),
            serde_json::json!({"size_excluded_files": 2}),
            serde_json::json!({"size_excluded_files": 2, "undetermined_files": 0,
                               "excluded_by_lang": {"rust": -1}}),
        ] {
            let envelope = serde_json::json!({"results": [], "ast_coverage": coverage});
            assert!(AstCoverage::from_envelope(&envelope).is_err(), "{coverage}");
        }
    }

    #[test]
    fn check_outcome_reports_pass_and_fail() {
        assert!(CheckOutcome::Pass.is_pass());
        let fail = CheckOutcome::fail("2 files never shown");
        assert!(!fail.is_pass());
        assert_eq!(fail.detail(), Some("2 files never shown"));
    }
}
