//! Golden-set schema, loading, and integrity checks for the search scoreboard
//! (#203).
//!
//! One checked-in file per corpus, `crates/rskim-bench/scoreboard/golden/<corpus>.toml`,
//! named by `rskim_research::clone::extract_repo_name(url)`. Every table
//! denies unknown fields, so a typo (`limit` for `limits`) fails the load
//! instead of silently dropping a field.
//!
//! Golden integrity is a precondition, not a gate: the scoreboard maps any
//! [`IntegrityViolation`] (or a load error) to a harness error, exit 2.
//! Candidate `[[ident]]` entries come from `golden-gen`
//! ([`crate::scoreboard::golden_gen`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::scoreboard::MAX_PAGES;
use crate::scoreboard::oracle::{LangFilter, LexicalQuery, MatchMode, ground_truth};
use crate::scoreboard::types::{Arm, CheckId, EntryKind, VerifyMode};
use crate::scoreboard::universe::Universe;

// ============================================================================
// Schema
// ============================================================================

/// A definition site: repo-relative `path` and 1-based `line` (1 + the number
/// of `\n` bytes before the definition).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefSite {
    pub path: String,
    pub line: u32,
}

/// Where an `[[ident]]` entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Curated from the 2026-09-25 seed benchmark.
    Seed,
    /// Hand-picked for this corpus (not from the seed benchmark), with its
    /// definition line verified at the pinned commit.
    Curated,
    /// Produced by `golden-gen`, reviewed, then frozen.
    Generated,
}

/// `[[ident]]`: a definition-ranking query.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentEntry {
    pub id: String,
    pub query: String,
    pub def: DefSite,
    pub origin: Origin,
}

/// `[[concept]]`: a ranking query whose relevance is a regex declared up
/// front, never derived from skim's output.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConceptEntry {
    pub id: String,
    pub query: String,
    /// A file is relevant iff this regex matches its text.
    pub relevant: String,
}

impl ConceptEntry {
    /// Compile [`ConceptEntry::relevant`].
    ///
    /// # Errors
    ///
    /// Returns an error if the regex does not compile.
    pub fn relevance(&self) -> anyhow::Result<Regex> {
        Regex::new(&self.relevant)
            .with_context(|| format!("{}: relevant regex {:?}", self.id, self.relevant))
    }
}

/// The `mode` of a `[[lexical]]` entry (default `and`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LexicalMode {
    #[default]
    And,
    Phrase,
    Near,
    Pnear,
}

/// The `category` of a `[[lexical]]` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LexicalCategory {
    Substr,
    Short,
    Punct,
    Case,
    Lang,
    ZeroHit,
    Phrase,
    Near,
}

/// `[[lexical]]`: a recall/precision edge case.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LexicalEntry {
    pub id: String,
    pub query: String,
    #[serde(default)]
    pub mode: LexicalMode,
    pub category: LexicalCategory,
    /// Word-position span; required for `near` / `pnear`, forbidden otherwise.
    #[serde(default)]
    pub near: Option<u32>,
    /// Passed as `--lang` and applied by the oracle's own extension map.
    #[serde(default)]
    pub lang: Option<String>,
}

impl LexicalEntry {
    /// The declared oracle mode.
    ///
    /// # Errors
    ///
    /// Returns an error if `near` is missing for `near` / `pnear` or present
    /// for `and` / `phrase`.
    pub fn match_mode(&self) -> anyhow::Result<MatchMode> {
        match (self.mode, self.near) {
            (LexicalMode::And, None) => Ok(MatchMode::And),
            (LexicalMode::Phrase, None) => Ok(MatchMode::Phrase),
            (LexicalMode::Near, Some(span)) => Ok(MatchMode::Near { span }),
            (LexicalMode::Pnear, Some(span)) => Ok(MatchMode::PhraseNear { span }),
            (LexicalMode::Near | LexicalMode::Pnear, None) => {
                anyhow::bail!("mode {:?} requires `near`", self.mode)
            }
            (LexicalMode::And | LexicalMode::Phrase, Some(_)) => {
                anyhow::bail!("`near` is only valid with mode near | pnear")
            }
        }
    }

    /// The CLI flags this entry runs with (`--phrase`, `--near N`, `--lang X`).
    ///
    /// # Errors
    ///
    /// As [`LexicalEntry::match_mode`], plus an unknown `lang`.
    pub fn flags(&self) -> anyhow::Result<QueryFlags> {
        let (phrase, near) = match self.match_mode()? {
            MatchMode::And => (false, None),
            MatchMode::Phrase => (true, None),
            MatchMode::Near { span } => (false, Some(span)),
            MatchMode::PhraseNear { span } => (true, Some(span)),
        };
        let flags = QueryFlags {
            phrase,
            near,
            lang: self.lang.clone(),
            ..QueryFlags::default()
        };
        flags.validate()?;
        Ok(flags)
    }

    /// The oracle query for this entry's ground truth.
    ///
    /// # Errors
    ///
    /// As [`LexicalEntry::flags`], plus any [`LexicalQuery::new`] refusal.
    pub fn oracle_query(&self) -> anyhow::Result<LexicalQuery> {
        let lang = self.lang.as_deref().map(LangFilter::parse).transpose()?;
        LexicalQuery::new(&self.query, self.match_mode()?, lang)
    }
}

/// `[[pagination]]`: a sweep of `--offset 0, L, 2L, …` for each `L` in
/// `limits`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaginationEntry {
    pub id: String,
    pub query: String,
    #[serde(default)]
    pub flags: Vec<String>,
    pub limits: Vec<u32>,
}

/// `[[prefix]]`: `--limit N` must equal the first N rows of the full list.
/// `query` is omitted for a standalone `--ast` / `--hot` run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixEntry {
    pub id: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub flags: Vec<String>,
    pub limits: Vec<u32>,
}

/// One corpus's golden file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenFile {
    pub corpus: String,
    /// Must equal the corpus commit in `corpora.toml`.
    pub commit: String,
    #[serde(default, rename = "ident")]
    pub idents: Vec<IdentEntry>,
    #[serde(default, rename = "concept")]
    pub concepts: Vec<ConceptEntry>,
    #[serde(default, rename = "lexical")]
    pub lexicals: Vec<LexicalEntry>,
    #[serde(default, rename = "pagination")]
    pub paginations: Vec<PaginationEntry>,
    #[serde(default, rename = "prefix")]
    pub prefixes: Vec<PrefixEntry>,
}

impl GoldenFile {
    /// Every `(id, kind)`, in file order within each kind (ident, concept,
    /// lexical, pagination, prefix).
    pub fn ids(&self) -> impl Iterator<Item = (&str, EntryKind)> {
        let idents = self
            .idents
            .iter()
            .map(|e| (e.id.as_str(), EntryKind::Ident));
        let concepts = self
            .concepts
            .iter()
            .map(|e| (e.id.as_str(), EntryKind::Concept));
        let lexicals = self
            .lexicals
            .iter()
            .map(|e| (e.id.as_str(), EntryKind::Lexical));
        let paginations = self
            .paginations
            .iter()
            .map(|e| (e.id.as_str(), EntryKind::Pagination));
        let prefixes = self
            .prefixes
            .iter()
            .map(|e| (e.id.as_str(), EntryKind::Prefix));
        idents
            .chain(concepts)
            .chain(lexicals)
            .chain(paginations)
            .chain(prefixes)
    }

    /// The kind of the entry with `id`.
    pub fn entry_kind(&self, id: &str) -> Option<EntryKind> {
        self.ids().find(|(i, _)| *i == id).map(|(_, k)| k)
    }
}

/// A golden file plus the SHA-256 of its raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedGolden {
    pub file: GoldenFile,
    /// Lowercase hex SHA-256 of the file bytes as read (any byte edit,
    /// comments included, changes it).
    pub sha256: String,
}

/// Parse golden TOML.
///
/// # Errors
///
/// Returns an error for invalid TOML, unknown fields, or wrong types.
pub fn parse_golden(raw: &str) -> anyhow::Result<GoldenFile> {
    toml::from_str(raw).context("parsing golden TOML")
}

/// Read, hash and parse a golden file. Integrity is checked separately
/// ([`check_integrity`]) because it needs corpus context.
///
/// # Errors
///
/// Returns an error naming `path` if it cannot be read or parsed.
pub fn load_golden(path: &Path) -> anyhow::Result<LoadedGolden> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading golden file {}", path.display()))?;
    let file = parse_golden(&raw).with_context(|| format!("in {}", path.display()))?;
    Ok(LoadedGolden {
        file,
        sha256: hex_sha256(raw.as_bytes()),
    })
}

/// One hash over a whole golden set (report / baseline `golden_sha256`):
/// SHA-256 over `"<corpus>\0<file sha256>\n"` lines sorted by corpus, so it
/// does not depend on load order.
pub fn golden_set_sha256<'a>(files: impl IntoIterator<Item = &'a LoadedGolden>) -> String {
    let lines: BTreeSet<String> = files
        .into_iter()
        .map(|g| format!("{}\0{}\n", g.file.corpus, g.sha256))
        .collect();
    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update(line.as_bytes());
    }
    hex(&hasher.finalize())
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ============================================================================
// Query flags
// ============================================================================

/// A temporal sort flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalSort {
    Hot,
    Cold,
    Risky,
}

impl TemporalSort {
    fn flag(self) -> &'static str {
        match self {
            TemporalSort::Hot => "--hot",
            TemporalSort::Cold => "--cold",
            TemporalSort::Risky => "--risky",
        }
    }
}

/// The search flags a golden entry may pass to skim, parsed from its `flags`
/// array. The runner owns `--json`, `--limit`, `--offset` and `--root`;
/// those, unknown flags, repeated flags, and flags missing their value are
/// rejected. Values are always separate tokens (`["--near", "5"]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryFlags {
    /// `--phrase`
    pub phrase: bool,
    /// `--near N` (N ≥ 1)
    pub near: Option<u32>,
    /// `--lang X` (a value [`LangFilter::parse`] accepts)
    pub lang: Option<String>,
    /// `--hot` / `--cold` / `--risky` (at most one)
    pub temporal: Option<TemporalSort>,
    /// `--blast-radius FILE`
    pub blast_radius: Option<String>,
    /// `--ast PATTERN`
    pub ast: Option<String>,
}

impl QueryFlags {
    /// Parse a golden `flags` array.
    ///
    /// # Errors
    ///
    /// See the type docs.
    pub fn parse(flags: &[String]) -> anyhow::Result<Self> {
        let mut out = QueryFlags::default();
        let mut i = 0;
        // Each iteration consumes at least one token: bounded by flags.len().
        while let Some(flag) = flags.get(i) {
            let value = || -> anyhow::Result<String> {
                flags
                    .get(i + 1)
                    .filter(|v| !v.starts_with("--"))
                    .cloned()
                    .with_context(|| format!("{flag} needs a value"))
            };
            let consumed_value = match flag.as_str() {
                "--phrase" => {
                    anyhow::ensure!(!out.phrase, "--phrase given twice");
                    out.phrase = true;
                    false
                }
                "--near" => {
                    anyhow::ensure!(out.near.is_none(), "--near given twice");
                    let raw = value()?;
                    let span: u32 = raw
                        .parse()
                        .with_context(|| format!("--near value {raw:?} is not an integer"))?;
                    out.near = Some(span);
                    true
                }
                "--lang" => {
                    anyhow::ensure!(out.lang.is_none(), "--lang given twice");
                    out.lang = Some(value()?);
                    true
                }
                "--hot" | "--cold" | "--risky" => {
                    anyhow::ensure!(
                        out.temporal.is_none(),
                        "at most one of --hot / --cold / --risky"
                    );
                    out.temporal = Some(match flag.as_str() {
                        "--hot" => TemporalSort::Hot,
                        "--cold" => TemporalSort::Cold,
                        _ => TemporalSort::Risky,
                    });
                    false
                }
                "--blast-radius" => {
                    anyhow::ensure!(out.blast_radius.is_none(), "--blast-radius given twice");
                    out.blast_radius = Some(value()?);
                    true
                }
                "--ast" => {
                    anyhow::ensure!(out.ast.is_none(), "--ast given twice");
                    out.ast = Some(value()?);
                    true
                }
                "--json" | "--limit" | "--offset" | "--root" => {
                    anyhow::bail!("{flag} is set by the scoreboard runner, not the golden set")
                }
                other => anyhow::bail!(
                    "unsupported flag {other:?} (allowed: --phrase, --near N, --lang X, \
                     --hot, --cold, --risky, --blast-radius FILE, --ast PATTERN)"
                ),
            };
            i += if consumed_value { 2 } else { 1 };
        }
        out.validate()?;
        Ok(out)
    }

    /// Value-level checks shared by [`QueryFlags::parse`] and
    /// [`LexicalEntry::flags`].
    fn validate(&self) -> anyhow::Result<()> {
        if self.near == Some(0) {
            anyhow::bail!("--near must be > 0");
        }
        if let Some(lang) = &self.lang {
            LangFilter::parse(lang)?;
        }
        Ok(())
    }

    /// Canonical CLI tokens (every flag goes before `--`; the runner adds the
    /// query after it).
    pub fn to_args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if self.phrase {
            args.push("--phrase".into());
        }
        if let Some(n) = self.near {
            args.extend(["--near".into(), n.to_string()]);
        }
        if let Some(lang) = &self.lang {
            args.extend(["--lang".into(), lang.clone()]);
        }
        if let Some(pattern) = &self.ast {
            args.extend(["--ast".into(), pattern.clone()]);
        }
        if let Some(sort) = self.temporal {
            args.push(sort.flag().into());
        }
        if let Some(file) = &self.blast_radius {
            args.extend(["--blast-radius".into(), file.clone()]);
        }
        args
    }

    /// The JSON envelope skim produces for these flags, with or without a
    /// text query.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no query and no standalone arm, or when
    /// a standalone run combines `--blast-radius` with a temporal sort.
    pub fn arm(&self, has_query: bool) -> anyhow::Result<Arm> {
        if has_query {
            return Ok(Arm::Lexical);
        }
        match (&self.ast, self.temporal, &self.blast_radius) {
            (Some(_), _, _) => Ok(Arm::Ast),
            (None, Some(_), Some(_)) => {
                anyhow::bail!("a standalone run cannot combine --blast-radius with a temporal sort")
            }
            (None, None, Some(_)) => Ok(Arm::BlastRadius),
            (None, Some(TemporalSort::Risky), None) => Ok(Arm::Risky),
            (None, Some(TemporalSort::Hot | TemporalSort::Cold), None) => Ok(Arm::HotCold),
            (None, None, None) => {
                anyhow::bail!(
                    "no query and no standalone arm (--ast, --hot, --cold, --risky, --blast-radius)"
                )
            }
        }
    }

    /// The verify mode `--phrase` / `--near` select.
    pub fn match_mode(&self) -> MatchMode {
        match (self.phrase, self.near) {
            (false, None) => MatchMode::And,
            (true, None) => MatchMode::Phrase,
            (false, Some(span)) => MatchMode::Near { span },
            (true, Some(span)) => MatchMode::PhraseNear { span },
        }
    }

    /// The `verify_mode` skim should report.
    pub fn verify_mode(&self) -> VerifyMode {
        VerifyMode::from(self.match_mode())
    }

    /// Whether the full list is ordered by something other than the lexical
    /// score (a temporal sort or `--blast-radius`), which exempts it from
    /// `order.score_monotone`.
    pub fn has_rank_override(&self) -> bool {
        self.temporal.is_some() || self.blast_radius.is_some()
    }

    /// The oracle query for `query`'s full result set, or `None` when that
    /// set is not a lexical predicate (`--ast` intersects with structural
    /// matches; `--blast-radius` adds co-change peers).
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown `--lang` or a query
    /// [`LexicalQuery::new`] refuses.
    pub fn oracle_query(&self, query: &str) -> anyhow::Result<Option<LexicalQuery>> {
        if self.ast.is_some() || self.blast_radius.is_some() {
            return Ok(None);
        }
        let lang = self.lang.as_deref().map(LangFilter::parse).transpose()?;
        LexicalQuery::new(query, self.match_mode(), lang).map(Some)
    }
}

// ============================================================================
// Integrity
// ============================================================================

/// Upper bound on a pagination entry's full count:
/// `min(limits) × (MAX_PAGES − 1)`. `None` when `limits` is empty.
pub fn pagination_bound(limits: &[u32]) -> Option<u64> {
    limits
        .iter()
        .min()
        .map(|&min| u64::from(min) * u64::from(MAX_PAGES - 1))
}

/// Whether a full count fits [`pagination_bound`]. The runner re-checks this
/// against skim's actual full list before sweeping.
pub fn within_pagination_bound(count: u64, limits: &[u32]) -> bool {
    pagination_bound(limits).is_some_and(|bound| count <= bound)
}

/// A reference into the ledger (`known_failures.toml`): one `(check, id)`
/// pair it expects to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerRef<'a> {
    pub check: CheckId,
    pub id: &'a str,
}

/// What a golden file is checked against.
#[derive(Debug, Clone, Copy)]
pub struct IntegrityContext<'a> {
    /// Expected corpus name (`extract_repo_name(url)`).
    pub corpus: &'a str,
    /// The corpus commit in `corpora.toml`.
    pub commit: &'a str,
    /// The corpus's oracle universe, computed from the verified clone at the
    /// pinned commit. Enables the def-line, pagination-bound and zero-hit
    /// checks; `None` runs the schema-level checks only.
    pub universe: Option<&'a Universe>,
    /// Ledger entries for this corpus.
    pub ledger: &'a [LedgerRef<'a>],
}

/// One integrity violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityViolation {
    /// The offending entry, or `None` for a file-level problem.
    pub id: Option<String>,
    pub message: String,
}

impl std::fmt::Display for IntegrityViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.id {
            Some(id) => write!(f, "{id}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Collects violations; every check runs, none short-circuits.
struct Violations(Vec<IntegrityViolation>);

impl Violations {
    fn file(&mut self, message: String) {
        self.0.push(IntegrityViolation { id: None, message });
    }

    fn entry(&mut self, id: &str, message: impl Into<String>) {
        self.0.push(IntegrityViolation {
            id: Some(id.to_string()),
            message: message.into(),
        });
    }

    fn entry_err(&mut self, id: &str, result: anyhow::Result<()>) {
        if let Err(e) = result {
            self.entry(id, format!("{e:#}"));
        }
    }
}

/// Validate `golden` against `ctx`. Returns every violation (empty = valid).
///
/// Always: `corpus` and `commit` match; ids are unique across all kinds and
/// prefixed with `<corpus>-`; queries are non-blank; `def.line >= 1`;
/// concept regexes compile; lexical `mode` / `near` / `lang` / `category`
/// agree and the oracle accepts the query; pagination and prefix `limits`
/// are non-empty and positive and their `flags` parse; a prefix without a
/// query selects a standalone arm; every ledger entry names an existing id
/// whose kind its check applies to.
///
/// With a universe: every `def.path` is indexed and its `def.line` contains
/// the query; every lexically-computable pagination entry's ground truth fits
/// [`pagination_bound`]; every `zero-hit` entry's ground truth is empty.
pub fn check_integrity(golden: &GoldenFile, ctx: &IntegrityContext<'_>) -> Vec<IntegrityViolation> {
    let mut v = Violations(Vec::new());

    if golden.corpus != ctx.corpus {
        v.file(format!(
            "corpus {:?} does not match the expected corpus {:?}",
            golden.corpus, ctx.corpus
        ));
    }
    if golden.commit != ctx.commit {
        v.file(format!(
            "commit {} does not match corpora.toml commit {}",
            golden.commit, ctx.commit
        ));
    }

    check_ids(golden, ctx.corpus, &mut v);
    for e in &golden.idents {
        v.entry_err(&e.id, check_ident(e, ctx.universe));
    }
    for e in &golden.concepts {
        v.entry_err(&e.id, non_blank(&e.query).and(e.relevance().map(|_| ())));
    }
    for e in &golden.lexicals {
        v.entry_err(&e.id, check_lexical(e, ctx.universe));
    }
    for e in &golden.paginations {
        v.entry_err(&e.id, check_pagination(e, ctx.universe));
    }
    for e in &golden.prefixes {
        v.entry_err(&e.id, check_prefix(e));
    }
    check_ledger(golden, ctx.ledger, &mut v);

    v.0
}

fn check_ids(golden: &GoldenFile, corpus: &str, v: &mut Violations) {
    let prefix = format!("{corpus}-");
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (id, _) in golden.ids() {
        *seen.entry(id).or_insert(0) += 1;
        if !id.starts_with(&prefix) || id.len() == prefix.len() {
            v.entry(id, format!("id must start with {prefix:?}"));
        }
    }
    for (id, n) in seen {
        if n > 1 {
            v.entry(id, format!("id is used by {n} entries"));
        }
    }
}

fn non_blank(query: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!query.trim().is_empty(), "query is blank");
    Ok(())
}

fn check_ident(e: &IdentEntry, universe: Option<&Universe>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    anyhow::ensure!(e.def.line >= 1, "def.line must be >= 1");
    let Some(universe) = universe else {
        return Ok(());
    };
    let text = universe.text(&e.def.path).with_context(|| {
        format!(
            "def.path {:?} is not in the oracle's indexed universe at the pinned commit",
            e.def.path
        )
    })?;
    let idx = usize::try_from(e.def.line - 1)?;
    let line = text
        .split('\n')
        .nth(idx)
        .with_context(|| format!("{} has no line {}", e.def.path, e.def.line))?;
    anyhow::ensure!(
        line.contains(&e.query),
        "{}:{} does not contain {:?} (line is {:?})",
        e.def.path,
        e.def.line,
        e.query,
        line
    );
    Ok(())
}

fn check_lexical(e: &LexicalEntry, universe: Option<&Universe>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    e.flags()?;
    let query = e.oracle_query()?;
    if e.category == LexicalCategory::Lang {
        anyhow::ensure!(e.lang.is_some(), "category \"lang\" requires `lang`");
    }
    if let (LexicalCategory::ZeroHit, Some(universe)) = (e.category, universe) {
        let hits = ground_truth(universe.files(), &query);
        anyhow::ensure!(
            hits.is_empty(),
            "category \"zero-hit\" but the oracle finds {} file(s), e.g. {:?}",
            hits.len(),
            hits.first()
        );
    }
    Ok(())
}

fn check_limits(limits: &[u32]) -> anyhow::Result<()> {
    anyhow::ensure!(!limits.is_empty(), "limits is empty");
    anyhow::ensure!(limits.iter().all(|&l| l >= 1), "every limit must be >= 1");
    Ok(())
}

fn check_pagination(e: &PaginationEntry, universe: Option<&Universe>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    check_limits(&e.limits)?;
    let flags = QueryFlags::parse(&e.flags)?;
    let query = flags.oracle_query(&e.query)?;
    if let (Some(query), Some(universe)) = (query, universe) {
        let count = u64::try_from(ground_truth(universe.files(), &query).len())?;
        anyhow::ensure!(
            within_pagination_bound(count, &e.limits),
            "ground truth has {count} files, over min(limits) x (MAX_PAGES - 1) = {:?}",
            pagination_bound(&e.limits)
        );
    }
    Ok(())
}

fn check_prefix(e: &PrefixEntry) -> anyhow::Result<()> {
    check_limits(&e.limits)?;
    if let Some(query) = &e.query {
        non_blank(query)?;
    }
    let flags = QueryFlags::parse(&e.flags)?;
    flags.arm(e.query.is_some())?;
    if let Some(query) = &e.query {
        flags.oracle_query(query)?;
    }
    Ok(())
}

fn check_ledger(golden: &GoldenFile, ledger: &[LedgerRef<'_>], v: &mut Violations) {
    for entry in ledger {
        match golden.entry_kind(entry.id) {
            None => v.entry(
                entry.id,
                format!(
                    "ledger entry ({}, {}) names an id that is not in the golden set",
                    entry.check, entry.id
                ),
            ),
            Some(kind) if !entry.check.applies_to(kind) => v.entry(
                entry.id,
                format!(
                    "ledger entry ({}, {}) pairs a check that never runs on a {kind:?} entry",
                    entry.check, entry.id
                ),
            ),
            Some(_) => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use crate::scoreboard::test_support::FixtureRepo;
    use crate::scoreboard::universe::{GitIsolation, Universe};

    const SHA: &str = "b8a0a79463382347820f1c2572bde37b68e87c76";

    /// The schema example from the #203 design, comments included.
    const DESIGN_EXAMPLE: &str = r#"
corpus = "skim"
commit = "b8a0a79463382347820f1c2572bde37b68e87c76"   # must equal corpora.toml

[[ident]]                  # definition-ranking queries
id = "skim-L01"
query = "check_staleness"
def = { path = "crates/rskim/src/cmd/search/staleness.rs", line = 321 }
origin = "seed"            # seed | curated | generated

[[concept]]                # ranking queries; relevance declared up front, never from skim output
id = "skim-C01"
query = "build lock"
relevant = '(?i)build[_\s-]*lock'

[[lexical]]                # recall/precision edge cases
id = "skim-X07"
query = "-D warnings"
mode = "and"               # and | phrase | near | pnear
category = "punct"         # substr | short | punct | case | lang | zero-hit | phrase | near
# near = 5                 # required when mode = near | pnear
# lang = "toml"            # optional: passed as --lang and applied by the oracle's own ext map

[[pagination]]
id = "skim-G001"
query = "elision marker"
flags = []
limits = [3, 7, 20]        # full count must be <= min(limits) x (MAX_PAGES - 1), else exit 2

[[prefix]]                 # limit=N must equal the first N rows of the full list
id = "skim-F001"
query = "fn"               # omit for standalone --ast / --hot
flags = ["--hot"]
limits = [5, 20]
"#;

    fn golden(src: &str) -> GoldenFile {
        parse_golden(src).unwrap()
    }

    fn flags(v: &[&str]) -> anyhow::Result<QueryFlags> {
        QueryFlags::parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn ctx<'a>(ledger: &'a [LedgerRef<'a>]) -> IntegrityContext<'a> {
        IntegrityContext {
            corpus: "skim",
            commit: SHA,
            universe: None,
            ledger,
        }
    }

    fn violations_for(src: &str) -> Vec<IntegrityViolation> {
        check_integrity(&golden(src), &ctx(&[]))
    }

    /// Violations mentioning `id`.
    fn flagged(violations: &[IntegrityViolation], id: &str) -> bool {
        violations.iter().any(|v| v.id.as_deref() == Some(id))
    }

    fn header() -> String {
        format!("corpus = \"skim\"\ncommit = \"{SHA}\"\n")
    }

    // --- schema ---------------------------------------------------------------

    #[test]
    fn the_design_example_parses_and_passes_schema_integrity() {
        let g = golden(DESIGN_EXAMPLE);
        assert_eq!((g.corpus.as_str(), g.commit.as_str()), ("skim", SHA));
        assert_eq!(g.idents[0].def.line, 321);
        assert_eq!(g.idents[0].origin, Origin::Seed);
        assert_eq!(g.lexicals[0].mode, LexicalMode::And);
        assert_eq!(g.lexicals[0].category, LexicalCategory::Punct);
        assert_eq!(g.paginations[0].limits, vec![3, 7, 20]);
        assert_eq!(g.prefixes[0].query.as_deref(), Some("fn"));
        assert_eq!(
            check_integrity(&g, &ctx(&[])),
            Vec::<IntegrityViolation>::new()
        );
    }

    #[test]
    fn misspelled_or_unknown_fields_are_rejected() {
        for body in [
            "[[pagination]]\nid = \"skim-G1\"\nquery = \"q\"\nlimit = [3]\n",
            "[[lexical]]\nid = \"skim-X1\"\nquery = \"q\"\ncategory = \"punct\"\nmode = \"fuzzy\"\n",
            "[[lexical]]\nid = \"skim-X1\"\nquery = \"q\"\ncategory = \"typo\"\n",
            "[[ident]]\nid = \"skim-L1\"\nquery = \"q\"\ndef = { path = \"a.rs\", line = 1 }\norigin = \"llm\"\n",
            "[[widget]]\nid = \"skim-W1\"\n",
        ] {
            assert!(
                parse_golden(&format!("{}{body}", header())).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn ident_origins_are_seed_curated_or_generated() {
        for (raw, want) in [
            ("seed", Origin::Seed),
            ("curated", Origin::Curated),
            ("generated", Origin::Generated),
        ] {
            let g = golden(&format!(
                "{}[[ident]]\nid = \"skim-L1\"\nquery = \"q\"\n\
                 def = {{ path = \"a.rs\", line = 1 }}\norigin = \"{raw}\"\n",
                header()
            ));
            assert_eq!(g.idents[0].origin, want, "{raw}");
        }
    }

    #[test]
    fn lexical_mode_defaults_to_and_and_categories_are_kebab_case() {
        let g = golden(&format!(
            "{}[[lexical]]\nid = \"skim-X20\"\nquery = \"zzq\"\ncategory = \"zero-hit\"\n",
            header()
        ));
        assert_eq!(g.lexicals[0].mode, LexicalMode::And);
        assert_eq!(g.lexicals[0].category, LexicalCategory::ZeroHit);
    }

    #[test]
    fn entry_kinds_are_indexed_by_id() {
        let g = golden(DESIGN_EXAMPLE);
        assert_eq!(g.entry_kind("skim-L01"), Some(EntryKind::Ident));
        assert_eq!(g.entry_kind("skim-G001"), Some(EntryKind::Pagination));
        assert_eq!(g.entry_kind("skim-F001"), Some(EntryKind::Prefix));
        assert_eq!(g.entry_kind("nope"), None);
        assert_eq!(g.ids().count(), 5);
    }

    // --- flags -----------------------------------------------------------------

    #[test]
    fn seed_flag_sets_parse_and_round_trip() {
        for set in [
            vec![],
            vec!["--phrase"],
            vec!["--near", "5"],
            vec!["--phrase", "--near", "4"],
            vec!["--hot"],
            vec!["--ast", "match-with-arms", "--hot"],
            vec!["--ast", "god-function"],
            vec!["--lang", "toml"],
            vec!["--blast-radius", "src/lib.rs"],
        ] {
            let parsed = flags(&set).unwrap();
            let again = QueryFlags::parse(&parsed.to_args()).unwrap();
            assert_eq!(again, parsed, "{set:?}");
        }
    }

    #[test]
    fn runner_owned_unknown_repeated_or_valueless_flags_are_rejected() {
        for set in [
            vec!["--limit", "5"],
            vec!["--offset", "1"],
            vec!["--json"],
            vec!["--root", "/x"],
            vec!["--bogus"],
            vec!["--near"],
            vec!["--near", "0"],
            vec!["--near", "five"],
            vec!["--near=5"],
            vec!["--hot", "--cold"],
            vec!["--hot", "--hot"],
            vec!["--phrase", "--phrase"],
            vec!["--lang", "haskell"],
            vec!["--ast"],
            vec!["--ast", "--hot"],
            vec!["--blast-radius"],
            vec!["positional"],
        ] {
            assert!(flags(&set).is_err(), "{set:?}");
        }
    }

    #[test]
    fn the_arm_follows_the_query_and_the_standalone_flag() {
        assert_eq!(flags(&["--hot"]).unwrap().arm(true).unwrap(), Arm::Lexical);
        assert_eq!(flags(&[]).unwrap().arm(true).unwrap(), Arm::Lexical);
        assert_eq!(
            flags(&["--ast", "match-with-arms", "--hot"])
                .unwrap()
                .arm(false)
                .unwrap(),
            Arm::Ast
        );
        assert_eq!(
            flags(&["--cold"]).unwrap().arm(false).unwrap(),
            Arm::HotCold
        );
        assert_eq!(flags(&["--risky"]).unwrap().arm(false).unwrap(), Arm::Risky);
        assert_eq!(
            flags(&["--blast-radius", "a.rs"])
                .unwrap()
                .arm(false)
                .unwrap(),
            Arm::BlastRadius
        );
        assert!(flags(&[]).unwrap().arm(false).is_err());
        assert!(flags(&["--phrase"]).unwrap().arm(false).is_err());
        assert!(
            flags(&["--hot", "--blast-radius", "a.rs"])
                .unwrap()
                .arm(false)
                .is_err()
        );
    }

    #[test]
    fn flags_map_to_a_match_mode_and_an_oracle_query_when_lexical() {
        let f = flags(&["--phrase", "--near", "4", "--hot"]).unwrap();
        assert_eq!(f.match_mode(), MatchMode::PhraseNear { span: 4 });
        assert_eq!(f.verify_mode(), VerifyMode::PhraseNear);
        assert!(f.has_rank_override(), "a temporal sort re-orders the list");
        let q = f.oracle_query("auto refresh stale").unwrap().unwrap();
        assert_eq!(q.mode(), MatchMode::PhraseNear { span: 4 });

        let lang = flags(&["--lang", "toml"]).unwrap();
        let q = lang.oracle_query("rskim-core").unwrap().unwrap();
        assert!(q.matches("Cargo.toml", "rskim-core") && !q.matches("a.md", "rskim-core"));
        assert!(!lang.has_rank_override());

        // The full set of an AST or blast-radius query is not a lexical predicate.
        assert!(
            flags(&["--ast", "try-catch"])
                .unwrap()
                .oracle_query("x")
                .unwrap()
                .is_none()
        );
        assert!(
            flags(&["--blast-radius", "a.rs"])
                .unwrap()
                .oracle_query("x")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn lexical_entries_expand_to_cli_flags_and_an_oracle_query() {
        let g = golden(&format!(
            "{}[[lexical]]\nid = \"skim-P04\"\nquery = \"auto refresh stale\"\nmode = \"pnear\"\n\
             category = \"near\"\nnear = 4\nlang = \"rust\"\n",
            header()
        ));
        let e = &g.lexicals[0];
        assert_eq!(
            e.flags().unwrap().to_args(),
            vec!["--phrase", "--near", "4", "--lang", "rust"]
        );
        let q = e.oracle_query().unwrap();
        assert_eq!(q.mode(), MatchMode::PhraseNear { span: 4 });
        assert!(q.matches("src/a.rs", "auto x refresh y stale"));
        assert!(!q.matches("docs/a.md", "auto x refresh y stale"));
    }

    // --- integrity: schema-level ------------------------------------------------

    #[test]
    fn corpus_and_commit_must_match_corpora_toml() {
        let g = golden(DESIGN_EXAMPLE);
        let wrong_commit = IntegrityContext {
            commit: "0123456789abcdef0123456789abcdef01234567",
            ..ctx(&[])
        };
        assert!(!check_integrity(&g, &wrong_commit).is_empty());
        let wrong_corpus = IntegrityContext {
            corpus: "zod",
            ..ctx(&[])
        };
        assert!(!check_integrity(&g, &wrong_corpus).is_empty());
    }

    #[test]
    fn ids_must_be_unique_across_kinds_and_prefixed_with_the_corpus() {
        let v = violations_for(&format!(
            "{}[[concept]]\nid = \"skim-D1\"\nquery = \"a\"\nrelevant = \"a\"\n\
             [[lexical]]\nid = \"skim-D1\"\nquery = \"b\"\ncategory = \"substr\"\n\
             [[lexical]]\nid = \"zod-X1\"\nquery = \"c\"\ncategory = \"substr\"\n",
            header()
        ));
        assert!(flagged(&v, "skim-D1"), "{v:?}");
        assert!(flagged(&v, "zod-X1"), "{v:?}");
    }

    #[test]
    fn each_malformed_entry_is_reported_and_nothing_short_circuits() {
        let v = violations_for(&format!(
            "{}\
             [[ident]]\nid = \"skim-L1\"\nquery = \" \"\ndef = {{ path = \"a.rs\", line = 1 }}\norigin = \"seed\"\n\
             [[ident]]\nid = \"skim-L2\"\nquery = \"x\"\ndef = {{ path = \"a.rs\", line = 0 }}\norigin = \"seed\"\n\
             [[concept]]\nid = \"skim-C1\"\nquery = \"a\"\nrelevant = \"(unclosed\"\n\
             [[lexical]]\nid = \"skim-X1\"\nquery = \"a b\"\nmode = \"near\"\ncategory = \"near\"\n\
             [[lexical]]\nid = \"skim-X2\"\nquery = \"a b\"\nmode = \"and\"\ncategory = \"substr\"\nnear = 3\n\
             [[lexical]]\nid = \"skim-X3\"\nquery = \"a b\"\nmode = \"pnear\"\ncategory = \"near\"\nnear = 0\n\
             [[lexical]]\nid = \"skim-X4\"\nquery = \"a\"\ncategory = \"lang\"\nlang = \"haskell\"\n\
             [[lexical]]\nid = \"skim-X5\"\nquery = \"->\"\nmode = \"phrase\"\ncategory = \"phrase\"\n\
             [[lexical]]\nid = \"skim-X6\"\nquery = \"a\"\ncategory = \"lang\"\n\
             [[pagination]]\nid = \"skim-G1\"\nquery = \"a\"\nlimits = []\n\
             [[pagination]]\nid = \"skim-G2\"\nquery = \"a\"\nlimits = [0, 5]\n\
             [[pagination]]\nid = \"skim-G3\"\nquery = \"a\"\nflags = [\"--limit\", \"5\"]\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F1\"\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F2\"\nquery = \"fn\"\nflags = [\"--hot\"]\nlimits = []\n",
            header()
        ));
        for id in [
            "skim-L1", "skim-L2", "skim-C1", "skim-X1", "skim-X2", "skim-X3", "skim-X4", "skim-X5",
            "skim-X6", "skim-G1", "skim-G2", "skim-G3", "skim-F1", "skim-F2",
        ] {
            assert!(flagged(&v, id), "{id} not flagged: {v:?}");
        }
    }

    #[test]
    fn standalone_prefix_entries_need_no_query() {
        let v = violations_for(&format!(
            "{}[[prefix]]\nid = \"skim-F2\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5, 20]\n",
            header()
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    // --- integrity: ledger ------------------------------------------------------

    #[test]
    fn ledger_entries_must_name_an_existing_id_their_check_applies_to() {
        let g = golden(DESIGN_EXAMPLE);
        let ok = [LedgerRef {
            check: CheckId::PaginationHasMoreHonest,
            id: "skim-G001",
        }];
        assert!(check_integrity(&g, &ctx(&ok)).is_empty());

        let unknown_id = [LedgerRef {
            check: CheckId::PaginationComplete,
            id: "skim-G999",
        }];
        assert!(flagged(
            &check_integrity(&g, &ctx(&unknown_id)),
            "skim-G999"
        ));

        let wrong_kind = [LedgerRef {
            check: CheckId::PaginationComplete,
            id: "skim-L01",
        }];
        assert!(flagged(&check_integrity(&g, &ctx(&wrong_kind)), "skim-L01"));
    }

    // --- integrity against the corpus universe ------------------------------------

    fn corpus() -> (FixtureRepo, String, Universe) {
        let repo = FixtureRepo::new();
        repo.write(
            "src/staleness.rs",
            "// header\npub fn check_staleness() {}\nfn other() {}\n",
        );
        repo.write(
            "notes.txt",
            "check_staleness mentioned in an unindexed file\n",
        );
        for i in 0..5 {
            repo.write(&format!("src/m{i}.rs"), "// shared marker\n");
        }
        let sha = repo.commit_all("init");
        let universe = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();
        (repo, sha, universe)
    }

    fn check_against(universe: &Universe, sha: &str, body: &str) -> Vec<IntegrityViolation> {
        let src = format!("corpus = \"skim\"\ncommit = \"{sha}\"\n{body}");
        let ctx = IntegrityContext {
            corpus: "skim",
            commit: sha,
            universe: Some(universe),
            ledger: &[],
        };
        check_integrity(&golden(&src), &ctx)
    }

    fn ident(path: &str, line: u32) -> String {
        format!(
            "[[ident]]\nid = \"skim-L01\"\nquery = \"check_staleness\"\n\
             def = {{ path = \"{path}\", line = {line} }}\norigin = \"seed\"\n"
        )
    }

    #[test]
    fn a_def_line_that_contains_the_query_passes() {
        let (_repo, sha, u) = corpus();
        assert!(check_against(&u, &sha, &ident("src/staleness.rs", 2)).is_empty());
    }

    #[test]
    fn def_line_drift_is_a_violation() {
        let (_repo, sha, u) = corpus();
        for line in [1, 3, 99] {
            let v = check_against(&u, &sha, &ident("src/staleness.rs", line));
            assert!(flagged(&v, "skim-L01"), "line {line}: {v:?}");
        }
    }

    #[test]
    fn a_def_outside_the_indexed_universe_is_a_violation() {
        let (_repo, sha, u) = corpus();
        for path in ["notes.txt", "src/missing.rs"] {
            let v = check_against(&u, &sha, &ident(path, 1));
            assert!(flagged(&v, "skim-L01"), "{path}: {v:?}");
        }
    }

    #[test]
    fn pagination_bound_is_min_limit_times_max_pages_minus_one() {
        assert_eq!(
            pagination_bound(&[7, 3, 20]),
            Some(3 * u64::from(MAX_PAGES - 1))
        );
        assert_eq!(pagination_bound(&[]), None);
        assert!(within_pagination_bound(63, &[1, 5]));
        assert!(!within_pagination_bound(64, &[1, 5]));
        assert!(!within_pagination_bound(1, &[]));
    }

    #[test]
    fn pagination_ground_truth_must_fit_the_page_bound() {
        // 64 matching files: over the bound for limit 1 (63), within it for 2.
        let repo = FixtureRepo::new();
        for i in 0..64 {
            repo.write(&format!("src/m{i:02}.rs"), "// shared marker\n");
        }
        let sha = repo.commit_all("init");
        let u = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();
        let entry = |limits: &str| {
            format!(
                "[[pagination]]\nid = \"skim-G1\"\nquery = \"shared marker\"\n\
                 flags = [\"--hot\"]\nlimits = {limits}\n"
            )
        };

        assert!(flagged(
            &check_against(&u, &sha, &entry("[1, 7]")),
            "skim-G1"
        ));
        assert!(check_against(&u, &sha, &entry("[2, 7]")).is_empty());
    }

    #[test]
    fn a_zero_hit_entry_with_ground_truth_hits_is_a_violation() {
        let (_repo, sha, u) = corpus();
        let v = check_against(
            &u,
            &sha,
            "[[lexical]]\nid = \"skim-Z1\"\nquery = \"shared marker\"\ncategory = \"zero-hit\"\n\
             [[lexical]]\nid = \"skim-Z2\"\nquery = \"no_such_token_anywhere\"\ncategory = \"zero-hit\"\n",
        );
        assert!(flagged(&v, "skim-Z1"), "{v:?}");
        assert!(!flagged(&v, "skim-Z2"), "{v:?}");
    }

    // --- loading and hashing --------------------------------------------------------

    #[test]
    fn load_golden_hashes_the_raw_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skim.toml");
        std::fs::write(&path, DESIGN_EXAMPLE).unwrap();

        let loaded = load_golden(&path).unwrap();
        assert_eq!(loaded.file, golden(DESIGN_EXAMPLE));
        assert_eq!(loaded.sha256.len(), 64);

        std::fs::write(&path, format!("{DESIGN_EXAMPLE}# trailing comment\n")).unwrap();
        let edited = load_golden(&path).unwrap();
        assert_eq!(edited.file, loaded.file, "same entries");
        assert_ne!(
            edited.sha256, loaded.sha256,
            "any byte edit changes the hash"
        );
    }

    #[test]
    fn load_golden_reports_the_path_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        std::fs::write(&path, "corpus = ").unwrap();
        let err = load_golden(&path).unwrap_err();
        assert!(format!("{err:#}").contains("broken.toml"), "{err:#}");
    }

    #[test]
    fn golden_set_hash_is_order_independent_and_content_sensitive() {
        let a = LoadedGolden {
            file: golden(DESIGN_EXAMPLE),
            sha256: "a".repeat(64),
        };
        let mut b = a.clone();
        b.file.corpus = "zod".to_string();
        b.sha256 = "b".repeat(64);

        let ab = golden_set_sha256([&a, &b]);
        assert_eq!(ab, golden_set_sha256([&b, &a]));
        let mut b2 = b.clone();
        b2.sha256 = "c".repeat(64);
        assert_ne!(ab, golden_set_sha256([&a, &b2]));
        assert_eq!(ab.len(), 64);
    }
}
