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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyMode {
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

/// One page of results from one skim invocation.
///
/// A result's rank is its index in `rows` plus the invocation's `--offset`.
/// skim's `total` is deliberately not kept: it is the page size, not a count.
#[derive(Debug, Clone, PartialEq)]
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
        let rows = results
            .iter()
            .enumerate()
            .map(|(i, row)| parse_row(arm, row).with_context(|| format!("results[{i}]")))
            .collect::<anyhow::Result<Vec<_>>>()?;

        let has_more = opt_bool(obj, "has_more")?.unwrap_or(false);
        let verify_mode = opt_str(obj, "verify_mode")?
            .map(VerifyMode::from_json_name)
            .unwrap_or(VerifyMode::Substring);
        let degraded = match obj.get("degraded") {
            None | Some(Value::Null) => Vec::new(),
            Some(v) => v
                .as_array()
                .context("`degraded` is not an array")?
                .iter()
                .enumerate()
                .map(|(i, d)| parse_degraded(d).with_context(|| format!("degraded[{i}]")))
                .collect::<anyhow::Result<Vec<_>>>()?,
        };

        Ok(ResultPage {
            rows,
            has_more,
            verify_mode,
            degraded,
        })
    }
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
}

impl StatsSnapshot {
    /// Parse skim's `--stats --json` stdout.
    ///
    /// # Errors
    ///
    /// Returns an error for non-JSON output, skim's `{"error": …}` envelope
    /// (no index), a missing `file_count`, or a non-integer count.
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

/// A present, non-null key must be a string.
fn opt_str<'a>(obj: &'a Map<String, Value>, key: &str) -> anyhow::Result<Option<&'a str>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .with_context(|| format!("`{key}` is not a string")),
    }
}

/// A present, non-null key must be a boolean.
fn opt_bool(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<bool>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .with_context(|| format!("`{key}` is not a boolean")),
    }
}

/// A present, non-null key must be an integer that fits `u32`.
fn opt_u32(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<u32>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .with_context(|| format!("`{key}` is not a line number")),
    }
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
            CheckId::LexicalRecall
            | CheckId::LexicalPrecision
            | CheckId::LexicalSilentFn
            | CheckId::LexicalVerifyMode
            | CheckId::OrderScoreMonotone => true,
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
            r#"{"results":[{"path":"a.rs","score":0.0,"line_number":null,"snippet":null}]}"#,
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
            (Arm::Lexical, r#"{"results":[{"score":1.0}]}"#),
            (
                Arm::Lexical,
                r#"{"results":[{"path":"a.rs","score":"high"}]}"#,
            ),
            (Arm::Lexical, r#"{"results":[{"path":"a.rs"}]}"#),
            (Arm::Lexical, r#"{"has_more":"yes","results":[]}"#),
            (Arm::Lexical, r#"{"verify_mode":3,"results":[]}"#),
            (
                Arm::Lexical,
                r#"{"results":[{"path":"a.rs","score":1.0,"line_number":-1}]}"#,
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
        assert_eq!(CheckId::ALL.len(), 10);
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
    }

    #[test]
    fn check_outcome_reports_pass_and_fail() {
        assert!(CheckOutcome::Pass.is_pass());
        let fail = CheckOutcome::fail("2 files never shown");
        assert!(!fail.is_pass());
        assert_eq!(fail.detail(), Some("2 files never shown"));
    }
}
