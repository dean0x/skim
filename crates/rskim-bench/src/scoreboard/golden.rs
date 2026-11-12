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
use rskim_oracle::structural::{self, OracleLang, PatternCoverage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::scoreboard::MAX_PAGES;
use crate::scoreboard::catalog::{CatalogPattern, catalog_coverage};
use crate::scoreboard::oracle::{
    LangFilter, LexicalQuery, LineMode, LineQuery, MatchMode, WholeWord, ground_truth,
    is_case_insensitive, line_truth,
};
use crate::scoreboard::types::{Arm, CheckId, EntryKind, Role, VerifyMode};
use crate::scoreboard::universe::{TextUniverse, Universe};

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
    /// Repo-relative paths, any one of which is an acceptable first result
    /// (`concept.first_right_top5`). Absent: the entry has no first-file
    /// check. Present but empty is a golden error, because no result could
    /// ever satisfy it.
    #[serde(default)]
    pub first: Option<Vec<String>>,
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

/// The precision class an `[[ast]]` entry declares. It is frozen in the
/// golden file (`golden-gen` proposes `hard` for a catalog pattern whose
/// `exact` flag is set, `ratchet` otherwise); the gate never re-reads the
/// catalog, so reclassifying is a reviewed golden edit plus a bless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrecisionClass {
    /// `structural.precision` is a HARD check: every returned file matches.
    Hard,
    /// Precision is the RATCHET value `structural.precision.<id>`.
    Ratchet,
}

impl PrecisionClass {
    /// The golden / report name.
    pub fn as_str(self) -> &'static str {
        match self {
            PrecisionClass::Hard => "hard",
            PrecisionClass::Ratchet => "ratchet",
        }
    }
}

/// `[[ast]]`: skim's standalone `--ast <pattern>` answer in one oracle
/// language, scored against the structural oracle (#541). skim is called
/// once per pattern and its rows are split by file extension, so each
/// `(pattern, lang)` pair has at most one entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AstEntry {
    pub id: String,
    /// A catalog pattern name (`rskim_search::all_patterns`) the oracle
    /// has a query for in `lang`.
    pub pattern: String,
    /// The oracle language whose files this entry scores.
    pub lang: OracleLang,
    pub precision: PrecisionClass,
    /// A false-positive guard: the oracle is expected to match no file in
    /// `lang`, so the entry guards precision only (it exists because skim
    /// returned a file the oracle rejects). Such an entry is scored even when
    /// skim returns no row either — the state a fix of that false positive
    /// leaves, where recall and precision both read 1 (an empty denominator)
    /// and a returning false positive fails `structural.precision` again — so
    /// it is exempt from the vacuity guard. An oracle match on a flagged entry
    /// is a golden error (the flag is stale and would hide recall). Absent =
    /// `false`; `golden-gen --ast` sets it on an entry whose oracle is empty.
    #[serde(default)]
    pub expect_oracle_empty: bool,
}

/// What a frozen site is. The names follow the role rules of the search
/// engine (`def`, `call`, `ref`, `import`, `comment`, `string`, `text`); the
/// hand classification of the golden entries uses the same names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteRole {
    Def,
    Call,
    Ref,
    Import,
    Comment,
    String,
    Text,
}

impl From<SiteRole> for Role {
    fn from(role: SiteRole) -> Self {
        match role {
            SiteRole::Def => Role::Def,
            SiteRole::Call => Role::Call,
            SiteRole::Ref => Role::Ref,
            SiteRole::Import => Role::Import,
            SiteRole::Comment => Role::Comment,
            SiteRole::String => Role::String,
            SiteRole::Text => Role::Text,
        }
    }
}

/// One frozen site of a `[[refs]]` entry: a line that holds the query as a
/// whole word, with its role.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefSite {
    pub path: String,
    pub line: u32,
    pub role: SiteRole,
}

/// `[[refs]]`: every site of one name. The `sites` set equals the whole-word
/// line set of the query over the oracle universe at the pin (golden
/// integrity), so a missing or an extra site exits 2 instead of moving the
/// target. Frozen from `git grep -nw` and a read of each line, never from
/// skim's output.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefsEntry {
    pub id: String,
    /// A bare identifier.
    pub query: String,
    /// The definition line; it is also one of `sites`.
    pub def: DefSite,
    pub sites: Vec<RefSite>,
    pub origin: Origin,
}

/// The definition a `[[clash]]` call belongs to: the index into
/// [`ClashEntry::defs`], or `"unassigned"` when the call is not split by
/// definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum CallDef {
    Index(usize),
    Unassigned(Unassigned),
}

/// The one string a [`CallDef`] accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unassigned {
    Unassigned,
}

/// One frozen call site of a `[[clash]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClashCall {
    pub path: String,
    pub line: u32,
    pub def: CallDef,
}

/// How a `[[clash]]` entry assigns calls to definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClashSplit {
    /// Calls are not split by definition (the only value v1 accepts).
    None,
    /// Every call carries the index of its definition. Reserved for a later
    /// version; golden integrity refuses it today.
    Exact,
}

/// `[[clash]]`: a name with two or more definitions. The `defs` are the
/// definition sites and the `calls` are the call sites of the name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClashEntry {
    pub id: String,
    /// A bare identifier.
    pub query: String,
    pub defs: Vec<DefSite>,
    pub calls: Vec<ClashCall>,
    pub split: ClashSplit,
    pub origin: Origin,
}

/// What a `[[stakes]]` entry compares against the regex oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StakesExpect {
    Lines,
    Files,
    Counts,
    Exit,
}

/// `[[stakes]]`: a plain-text query judged against the regex oracle, either
/// for its matching lines, files or counts, or for its exit code.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StakesEntry {
    pub id: String,
    pub query: String,
    /// rg-style tokens from [`STAKES_FLAGS`].
    #[serde(default)]
    pub flags: Vec<String>,
    pub expect: StakesExpect,
    /// The expected exit code: `0` (a match) or `1` (no match), the only
    /// codes the regex oracle can judge. Required when `expect = "exit"`,
    /// forbidden otherwise.
    #[serde(default)]
    pub exit: Option<u8>,
}

impl StakesEntry {
    /// The regex oracle's query for this entry: the lines the plain-text
    /// search must report.
    ///
    /// The flags that change what matches are the match-mode and case
    /// switches. `-E` reads the query as a regex; `-F` and the default read
    /// it as literal text. `-i` ignores case; `-S` ignores case when the query
    /// has no uppercase character, and `-i` wins over `-S`. `-w` keeps a match
    /// only as a whole word. The other flags only change how a result is
    /// shown, so the oracle ignores them.
    ///
    /// # Errors
    ///
    /// - a flag that is not in [`STAKES_FLAGS`], or one that lacks its value;
    /// - `-g` or `-t`: they filter files, and the oracle has no file filter
    ///   in this version;
    /// - `-F` and `-E` together;
    /// - the query is an empty literal, or a regex that does not compile.
    pub fn line_query(&self) -> anyhow::Result<LineQuery> {
        let names = stakes_flag_names(&self.flags)?;
        let has = |flag: &str| names.contains(&flag);
        for filter in ["-g", "-t"] {
            anyhow::ensure!(
                !has(filter),
                "flag {filter:?} filters files, and the regex oracle has no file filter"
            );
        }
        anyhow::ensure!(
            !(has("-F") && has("-E")),
            "flags \"-F\" and \"-E\" contradict each other"
        );
        let case_insensitive = is_case_insensitive(&self.query, has("-i"), has("-S"));
        let word = has("-w");
        let mode = if has("-E") {
            LineMode::Regex {
                case_insensitive,
                word,
            }
        } else {
            LineMode::Literal {
                case_insensitive,
                word,
            }
        };
        LineQuery::new(&self.query, mode)
    }
}

/// The flag names of a stakes entry, in order, with the value tokens that
/// follow `-g`, `-t`, `-C`, `--limit`, `--offset` and `--root` removed.
fn stakes_flag_names(flags: &[String]) -> anyhow::Result<Vec<&str>> {
    let mut names = Vec::new();
    let mut tokens = flags.iter();
    while let Some(flag) = tokens.next() {
        let takes_value = STAKES_FLAGS
            .iter()
            .find(|(name, _)| *name == flag)
            .map(|(_, takes_value)| *takes_value)
            .with_context(|| format!("flag {flag:?} is not in the closed set of stakes flags"))?;
        if takes_value {
            tokens
                .next()
                .with_context(|| format!("flag {flag:?} needs a value"))?;
        }
        names.push(flag.as_str());
    }
    Ok(names)
}

/// The closed set of flags a `[[stakes]]` entry may use: the flag set of the
/// plain-text search. `-n` is accepted and does nothing. The second field is
/// whether the flag takes a value (the token that follows it).
pub const STAKES_FLAGS: &[(&str, bool)] = &[
    ("-i", false),
    ("-S", false),
    ("-w", false),
    ("-F", false),
    ("-E", false),
    ("-g", true),
    ("-t", true),
    ("-l", false),
    ("-c", false),
    ("-C", true),
    ("-n", false),
    ("--all", false),
    ("--full", false),
    ("--json", false),
    ("--limit", true),
    ("--offset", true),
    ("--root", true),
    ("-h", false),
];

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
    #[serde(default, rename = "ast")]
    pub asts: Vec<AstEntry>,
    #[serde(default, rename = "refs")]
    pub refs: Vec<RefsEntry>,
    #[serde(default, rename = "clash")]
    pub clashes: Vec<ClashEntry>,
    #[serde(default, rename = "stakes")]
    pub stakes: Vec<StakesEntry>,
}

impl GoldenFile {
    /// Every `(id, kind)`, in file order within each kind (ident, concept,
    /// lexical, pagination, prefix, ast, refs, clash, stakes).
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
        let asts = self.asts.iter().map(|e| (e.id.as_str(), EntryKind::Ast));
        let refs = self.refs.iter().map(|e| (e.id.as_str(), EntryKind::Refs));
        let clashes = self
            .clashes
            .iter()
            .map(|e| (e.id.as_str(), EntryKind::Clash));
        let stakes = self
            .stakes
            .iter()
            .map(|e| (e.id.as_str(), EntryKind::Stakes));
        idents
            .chain(concepts)
            .chain(lexicals)
            .chain(paginations)
            .chain(prefixes)
            .chain(asts)
            .chain(refs)
            .chain(clashes)
            .chain(stakes)
    }

    /// The kind of the entry with `id`.
    pub fn entry_kind(&self, id: &str) -> Option<EntryKind> {
        self.ids().find(|(i, _)| *i == id).map(|(_, k)| k)
    }
}

/// A golden file plus its digest ([`golden_digest`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedGolden {
    pub file: GoldenFile,
    /// Lowercase hex SHA-256 of the file bytes as read (any byte edit,
    /// comments included, changes it); a file with `[[ast]]` entries also
    /// folds in [`structural_oracle_sha256`], so editing any oracle query
    /// changes it too. This is the per-corpus `golden_sha256` the gate and
    /// `bless` compare.
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
    // The oracle fingerprint is rendered and hashed only for a file that
    // folds it in.
    let oracle = (!file.asts.is_empty()).then(structural_oracle_sha256);
    let sha256 = golden_digest(raw.as_bytes(), oracle.as_deref());
    Ok(LoadedGolden { file, sha256 })
}

/// Lowercase hex SHA-256 of [`structural::fingerprint`]: every table the
/// oracle's answers depend on — the size cap, the extension table, the
/// body-element attribute kinds, every registered query (file name,
/// post-filter, text) and every intent spec.
pub fn structural_oracle_sha256() -> String {
    hex_sha256(structural::fingerprint().as_bytes())
}

/// A golden file's digest: the SHA-256 of its `raw` bytes, or — for a file
/// with `[[ast]]` entries, which passes `oracle_sha256`
/// ([`structural_oracle_sha256`]) — the SHA-256 of the bytes, a separator
/// and that digest. The oracle queries are compiled into the scoreboard, so
/// this is how an edit to one reaches the per-corpus `golden_sha256`: the
/// gate then asks for a bless, and `bless` refuses a report made with other
/// queries.
pub fn golden_digest(raw: &[u8], oracle_sha256: Option<&str>) -> String {
    let Some(oracle_sha256) = oracle_sha256 else {
        return hex_sha256(raw);
    };
    let mut hasher = Sha256::new();
    hasher.update(raw);
    hasher.update(b"\0structural-oracle\0");
    hasher.update(oracle_sha256.as_bytes());
    hex(&hasher.finalize())
}

/// One hash over a whole golden set (report / baseline `golden_sha256`):
/// SHA-256 over `"<corpus>\0<file sha256>\n"` lines sorted by corpus, so it
/// does not depend on load order. Each file's digest already carries the
/// structural oracle digest when it has `[[ast]]` entries
/// ([`golden_digest`]), so a query edit changes this hash too.
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

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
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

    /// Whether skim serves these flags from its temporal data (`temporal.db`):
    /// a temporal sort, or `--blast-radius`'s co-change peers. Without that
    /// data skim falls back to another order.
    pub fn uses_temporal_data(&self) -> bool {
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
    /// The corpus's text universe ([`crate::scoreboard::universe::text_universe`]):
    /// every tracked text file, with no language allow-list. Enables the
    /// whole-word line checks of `[[ident]]`, `[[refs]]` and `[[clash]]`
    /// (`git grep -nw` finds hits in files the allow-listed index never
    /// reads) and the hit-count rule of `[[stakes]]`; `None` skips them.
    pub text: Option<&'a TextUniverse>,
    /// Ledger entries for this corpus.
    pub ledger: &'a [LedgerRef<'a>],
    /// skim's pattern catalog
    /// ([`crate::scoreboard::catalog::skim_catalog`]): every `[[ast]]`
    /// pattern must be one of its names.
    pub catalog: &'a [CatalogPattern],
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
/// query selects a standalone arm; every `[[ast]]` pattern is in skim's
/// catalog and has an oracle query in the entry's language, and no
/// `(pattern, lang)` pair has two entries; a concept's `first` list is
/// non-empty with distinct, non-blank paths; a `[[refs]]` or `[[clash]]` query
/// is a bare identifier; a `[[refs]]` entry has distinct sites that include its
/// `def`; a `[[clash]]` has 2 or more distinct defs, distinct calls, every call
/// `def` index in range and `split = "none"`; a `[[stakes]]` entry uses only
/// flags of [`STAKES_FLAGS`], none of which filters files (`-g` and `-t`: the
/// regex oracle has no file filter), compiles into a regex-oracle query, and
/// has `exit` exactly when `expect = "exit"`, and that `exit` is 0 or 1;
/// every ledger entry names an existing id whose kind its check applies to.
///
/// With the indexed universe: every `[[ident]]` `def.path` is indexed, every
/// concept `first` path is indexed, every lexically-computable pagination
/// entry's ground truth fits [`pagination_bound`], and every `zero-hit`
/// entry's ground truth is empty.
///
/// With the text universe: every `[[ident]]` `def.line` holds the query as a
/// whole word; every `[[refs]]` site set equals the whole-word line set of its
/// query, and its `def` line is one of them; every `[[clash]]` def and call
/// line holds the query as a whole word; every `[[stakes]]` `exit = 1` has no
/// matching line and every `exit = 0` has one.
///
/// Two `[[ast]]` golden errors need the structural oracle's answers, so the
/// pipeline checks them: an entry flagged `expect_oracle_empty` whose oracle
/// matches a file (`pipeline::require_expected_empty_oracles`, before skim
/// runs), and an unflagged entry where the oracle and skim both find nothing
/// (`pipeline::require_non_vacuous_structural`, after the run).
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
        v.entry_err(&e.id, check_ident(e, ctx.universe, ctx.text));
    }
    for e in &golden.concepts {
        v.entry_err(&e.id, check_concept(e, ctx.universe));
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
    if !golden.asts.is_empty() {
        let catalog = catalog_coverage(ctx.catalog);
        for e in &golden.asts {
            v.entry_err(&e.id, check_ast(e, &catalog));
        }
        check_ast_pairs(golden, &mut v);
    }
    for e in &golden.refs {
        v.entry_err(&e.id, check_refs(e, ctx.text));
    }
    for e in &golden.clashes {
        v.entry_err(&e.id, check_clash(e, ctx.text));
    }
    for e in &golden.stakes {
        v.entry_err(&e.id, check_stakes(e, ctx.text));
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

/// How many offending sites one violation message names.
const SITES_SHOWN: usize = 5;

/// The line at `path:line` in the text universe must hold `query` as a
/// whole word.
fn check_word_line(
    universe: &TextUniverse,
    word: &WholeWord,
    path: &str,
    line: u32,
    query: &str,
) -> anyhow::Result<()> {
    let text = universe.text(path).with_context(|| {
        format!("{path}:{line} is not in the oracle's text universe at the pinned commit")
    })?;
    let idx = usize::try_from(line.checked_sub(1).context("line must be >= 1")?)?;
    let held = text
        .split('\n')
        .nth(idx)
        .with_context(|| format!("{path} has no line {line}"))?;
    anyhow::ensure!(
        word.in_line(held, query),
        "{path}:{line} does not contain {query:?} as a whole word (line is {held:?})"
    );
    Ok(())
}

/// A `[[refs]]` or `[[clash]]` query is a bare identifier: Unicode letters and
/// digits (`char::is_alphanumeric`) and underscore.
///
/// The oracle's word class is Unicode (`[\p{Alphabetic}\p{Nd}_]`). `git grep
/// -w`, which froze the sites, counts only ASCII letters, digits and
/// underscore as word characters. The oracle's word set contains git's, so a
/// line that git froze and the oracle rejects is an integrity violation
/// (exit 2), not a silent pass.
fn check_identifier(query: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !query.is_empty() && query.chars().all(|c| c == '_' || c.is_alphanumeric()),
        "query {query:?} is not a bare identifier (letters, digits and underscore only)"
    );
    Ok(())
}

/// `path:line` of up to [`SITES_SHOWN`] sites, for a violation message.
fn show_sites<'a>(sites: impl Iterator<Item = &'a (String, u32)>) -> String {
    sites
        .take(SITES_SHOWN)
        .map(|(path, line)| format!("{path}:{line}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// An `[[ident]]` definition must be in the indexed universe (its rank is
/// judged against the index) and its line, read from the text universe, must
/// hold the query as a whole word.
fn check_ident(
    e: &IdentEntry,
    universe: Option<&Universe>,
    text: Option<&TextUniverse>,
) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    anyhow::ensure!(e.def.line >= 1, "def.line must be >= 1");
    if let Some(universe) = universe {
        anyhow::ensure!(
            universe.contains(&e.def.path),
            "{}:{} is not in the oracle's indexed universe at the pinned commit",
            e.def.path,
            e.def.line
        );
    }
    let Some(text) = text else {
        return Ok(());
    };
    check_word_line(text, &WholeWord::new()?, &e.def.path, e.def.line, &e.query)
}

fn check_concept(e: &ConceptEntry, universe: Option<&Universe>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    e.relevance()?;
    let Some(first) = &e.first else {
        return Ok(());
    };
    anyhow::ensure!(
        !first.is_empty(),
        "`first` is empty; omit it or list the acceptable first files"
    );
    let mut seen = BTreeSet::new();
    for path in first {
        anyhow::ensure!(!path.trim().is_empty(), "`first` has a blank path");
        anyhow::ensure!(seen.insert(path.as_str()), "`first` lists {path:?} twice");
        if let Some(universe) = universe {
            anyhow::ensure!(
                universe.contains(path),
                "`first` path {path:?} is not in the oracle's indexed universe at the pinned commit"
            );
        }
    }
    Ok(())
}

fn check_refs(e: &RefsEntry, universe: Option<&TextUniverse>) -> anyhow::Result<()> {
    check_identifier(&e.query)?;
    anyhow::ensure!(e.def.line >= 1, "def.line must be >= 1");
    anyhow::ensure!(!e.sites.is_empty(), "entry has no sites");
    anyhow::ensure!(
        e.sites.iter().all(|s| s.line >= 1),
        "every site line must be >= 1"
    );
    let mut frozen: BTreeSet<(String, u32)> = BTreeSet::new();
    for s in &e.sites {
        anyhow::ensure!(
            frozen.insert((s.path.clone(), s.line)),
            "site {}:{} is listed twice",
            s.path,
            s.line
        );
    }
    anyhow::ensure!(
        frozen.contains(&(e.def.path.clone(), e.def.line)),
        "def is not one of the sites ({}:{})",
        e.def.path,
        e.def.line
    );
    let Some(universe) = universe else {
        return Ok(());
    };
    let word = WholeWord::new()?;
    check_word_line(universe, &word, &e.def.path, e.def.line, &e.query)?;
    let truth = word.lines(universe.files(), &e.query);
    let missing: Vec<&(String, u32)> = truth.difference(&frozen).collect();
    let extra: Vec<&(String, u32)> = frozen.difference(&truth).collect();
    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!(
            "{} whole-word line(s) are missing from sites, e.g. {}",
            missing.len(),
            show_sites(missing.iter().copied())
        ));
    }
    if !extra.is_empty() {
        problems.push(format!(
            "{} site(s) are not a whole-word line at the pin, e.g. {}",
            extra.len(),
            show_sites(extra.iter().copied())
        ));
    }
    anyhow::ensure!(problems.is_empty(), "{}", problems.join("; "));
    Ok(())
}

fn check_clash(e: &ClashEntry, universe: Option<&TextUniverse>) -> anyhow::Result<()> {
    check_identifier(&e.query)?;
    anyhow::ensure!(
        e.defs.len() >= 2,
        "a clash needs 2 or more defs, found {}",
        e.defs.len()
    );
    anyhow::ensure!(
        e.split == ClashSplit::None,
        "split = \"exact\" is not accepted in v1; use split = \"none\""
    );
    anyhow::ensure!(
        e.defs.iter().all(|d| d.line >= 1) && e.calls.iter().all(|c| c.line >= 1),
        "every def and call line must be >= 1"
    );
    let mut seen = BTreeSet::new();
    for d in &e.defs {
        anyhow::ensure!(
            seen.insert((d.path.as_str(), d.line)),
            "def {}:{} is listed twice",
            d.path,
            d.line
        );
    }
    seen.clear();
    for c in &e.calls {
        anyhow::ensure!(
            seen.insert((c.path.as_str(), c.line)),
            "call {}:{} is listed twice",
            c.path,
            c.line
        );
        if let CallDef::Index(i) = c.def {
            anyhow::ensure!(
                i < e.defs.len(),
                "call {}:{} has def index {i}, out of range for {} defs",
                c.path,
                c.line,
                e.defs.len()
            );
        }
    }
    let Some(universe) = universe else {
        return Ok(());
    };
    let word = WholeWord::new()?;
    for d in &e.defs {
        check_word_line(universe, &word, &d.path, d.line, &e.query)?;
    }
    for c in &e.calls {
        check_word_line(universe, &word, &c.path, c.line, &e.query)?;
    }
    Ok(())
}

/// A `[[stakes]]` entry has known flags, a query the regex oracle can read,
/// and `exit` exactly when `expect = "exit"`. The only exit codes the regex
/// oracle can judge are 0 (a match) and 1 (no match), the exit codes of `rg`;
/// any other value is refused, with or without a text universe. Against the
/// text universe, `exit = 1` needs no matching line and `exit = 0` needs one.
fn check_stakes(e: &StakesEntry, text: Option<&TextUniverse>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    let query = e.line_query()?;
    let expects_match = match (e.expect, e.exit) {
        (StakesExpect::Exit, None) => anyhow::bail!("expect = \"exit\" requires `exit`"),
        (StakesExpect::Exit, Some(0)) => Some(true),
        (StakesExpect::Exit, Some(1)) => Some(false),
        (StakesExpect::Exit, Some(other)) => anyhow::bail!(
            "exit = {other} cannot be judged by the regex oracle; use 0 (a match) or 1 (no match)"
        ),
        (_, Some(_)) => anyhow::bail!("`exit` is only valid with expect = \"exit\""),
        (_, None) => None,
    };
    let (Some(text), Some(expects_match)) = (text, expects_match) else {
        return Ok(());
    };
    let hits = line_truth(text.files(), &query);
    if expects_match {
        anyhow::ensure!(
            !hits.is_empty(),
            "exit = 0 (a match) but the oracle finds no matching line"
        );
    } else {
        anyhow::ensure!(
            hits.is_empty(),
            "exit = 1 (no match) but the oracle finds {} matching line(s), e.g. {}",
            hits.len(),
            show_sites(hits.iter())
        );
    }
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

/// An `[[ast]]` pattern must be a catalog pattern the oracle has a query for
/// in the entry's language.
fn check_ast(e: &AstEntry, catalog: &BTreeMap<&str, PatternCoverage>) -> anyhow::Result<()> {
    match catalog.get(e.pattern.as_str()) {
        None => anyhow::bail!(
            "pattern {:?} is not in skim's pattern catalog (rskim_search::all_patterns)",
            e.pattern
        ),
        Some(PatternCoverage::Uncovered { reason }) => {
            anyhow::bail!(
                "pattern {:?} has no structural oracle query: {reason}",
                e.pattern
            )
        }
        Some(PatternCoverage::Covered { langs }) if !langs.contains(&e.lang) => {
            let covered: Vec<&str> = langs.iter().map(|l| l.as_str()).collect();
            anyhow::bail!(
                "the structural oracle has no {} query for pattern {:?} (it has: {})",
                e.lang,
                e.pattern,
                covered.join(", ")
            )
        }
        Some(PatternCoverage::Covered { .. }) => Ok(()),
    }
}

/// skim is called once per pattern and its rows are split by language, so
/// two entries for one `(pattern, lang)` would score the same rows twice.
fn check_ast_pairs(golden: &GoldenFile, v: &mut Violations) {
    let mut first: BTreeMap<(&str, OracleLang), &str> = BTreeMap::new();
    for e in &golden.asts {
        match first.get(&(e.pattern.as_str(), e.lang)) {
            Some(earlier) => v.entry(
                &e.id,
                format!(
                    "({}, {}) is already scored by {earlier}; one [[ast]] entry per pattern and language",
                    e.pattern, e.lang
                ),
            ),
            None => {
                first.insert((e.pattern.as_str(), e.lang), e.id.as_str());
            }
        }
    }
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
    use crate::scoreboard::test_support::{FixtureRepo, catalog};
    use crate::scoreboard::universe::{GitIsolation, Universe, text_universe};

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
            text: None,
            ledger,
            catalog: catalog(),
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

    // --- [[ast]] ---------------------------------------------------------------

    fn ast(id: &str, pattern: &str, lang: &str, precision: &str) -> String {
        format!(
            "[[ast]]\nid = \"{id}\"\npattern = \"{pattern}\"\nlang = \"{lang}\"\nprecision = \"{precision}\"\n"
        )
    }

    #[test]
    fn ast_entries_parse_with_their_language_and_precision_class() {
        let g = golden(&format!(
            "{}{}{}",
            header(),
            ast(
                "skim-ast-try-catch-finally-javascript",
                "try-catch-finally",
                "javascript",
                "hard"
            ),
            ast(
                "skim-ast-rust-nested-loop-rust",
                "rust-nested-loop",
                "rust",
                "ratchet"
            ),
        ));
        assert_eq!(
            g.asts[0],
            AstEntry {
                id: "skim-ast-try-catch-finally-javascript".to_string(),
                pattern: "try-catch-finally".to_string(),
                lang: OracleLang::JavaScript,
                precision: PrecisionClass::Hard,
                expect_oracle_empty: false,
            }
        );
        assert_eq!(g.asts[1].precision, PrecisionClass::Ratchet);
        assert_eq!(
            g.entry_kind("skim-ast-rust-nested-loop-rust"),
            Some(EntryKind::Ast)
        );
        assert_eq!(g.ids().count(), 2);
        assert!(check_integrity(&g, &ctx(&[])).is_empty());
    }

    #[test]
    fn expect_oracle_empty_is_an_optional_bool_that_defaults_to_false() {
        let entry = ast("skim-A1", "try-catch-finally", "javascript", "hard");
        let flagged = golden(&format!("{}{entry}expect_oracle_empty = true\n", header()));
        assert!(flagged.asts[0].expect_oracle_empty);
        let explicit = golden(&format!("{}{entry}expect_oracle_empty = false\n", header()));
        assert!(!explicit.asts[0].expect_oracle_empty);
        assert!(!golden(&format!("{}{entry}", header())).asts[0].expect_oracle_empty);
        // The flag needs the oracle's answers, so schema integrity accepts it.
        assert!(check_integrity(&flagged, &ctx(&[])).is_empty());
        assert!(
            parse_golden(&format!(
                "{}{entry}expect_oracle_empty = \"yes\"\n",
                header()
            ))
            .is_err()
        );
    }

    #[test]
    fn ast_entries_with_a_bad_class_language_or_field_do_not_load() {
        for body in [
            ast("skim-A1", "try-catch", "typescript", "strict"),
            ast("skim-A1", "try-catch", "haskell", "hard"),
            ast("skim-A1", "try-catch", "TypeScript", "hard"),
            "[[ast]]\nid = \"skim-A1\"\npattern = \"try-catch\"\nlang = \"typescript\"\n"
                .to_string(),
            format!(
                "{}flags = []\n",
                ast("skim-A1", "try-catch", "typescript", "hard")
            ),
        ] {
            assert!(
                parse_golden(&format!("{}{body}", header())).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn ast_patterns_must_be_catalog_patterns_the_oracle_covers_in_that_language() {
        let v = violations_for(&format!(
            "{}{}{}{}{}",
            header(),
            ast("skim-A1", "no-such-pattern", "rust", "hard"),
            ast("skim-A2", "deep-nesting", "rust", "ratchet"),
            ast("skim-A3", "god-function", "typescript", "ratchet"),
            ast("skim-A4", "god-function", "rust", "ratchet"),
        ));
        let message = |id: &str| {
            v.iter()
                .find(|x| x.id.as_deref() == Some(id))
                .map(|x| x.message.clone())
                .unwrap_or_default()
        };
        assert!(
            message("skim-A1").contains("not in skim's pattern catalog"),
            "{v:?}"
        );
        assert!(
            message("skim-A2").contains("no structural oracle query"),
            "{v:?}"
        );
        assert!(
            message("skim-A3").contains("no typescript query")
                && message("skim-A3").contains("rust"),
            "{v:?}"
        );
        assert!(!flagged(&v, "skim-A4"), "{v:?}");
    }

    #[test]
    fn one_ast_entry_per_pattern_and_language_with_ids_unique_across_kinds() {
        let v = violations_for(&format!(
            "{}{}{}{}[[concept]]\nid = \"skim-A9\"\nquery = \"a\"\nrelevant = \"a\"\n{}",
            header(),
            ast("skim-A1", "try-catch", "typescript", "hard"),
            ast("skim-A2", "try-catch", "typescript", "ratchet"),
            ast("skim-A3", "try-catch", "tsx", "hard"),
            ast("skim-A9", "try-catch", "javascript", "hard"),
        ));
        assert!(!flagged(&v, "skim-A1"), "the first entry stands: {v:?}");
        assert!(flagged(&v, "skim-A2"), "{v:?}");
        assert!(!flagged(&v, "skim-A3"), ".tsx is its own language: {v:?}");
        assert!(flagged(&v, "skim-A9"), "id shared with a concept: {v:?}");
    }

    #[test]
    fn ledger_entries_pair_structural_checks_with_ast_entries_only() {
        let g = golden(&format!(
            "{}{}[[lexical]]\nid = \"skim-X01\"\nquery = \"q\"\ncategory = \"substr\"\n",
            header(),
            ast(
                "skim-ast-try-catch-finally-javascript",
                "try-catch-finally",
                "javascript",
                "hard"
            ),
        ));
        let ok = [LedgerRef {
            check: CheckId::StructuralPrecision,
            id: "skim-ast-try-catch-finally-javascript",
        }];
        assert!(check_integrity(&g, &ctx(&ok)).is_empty());
        for (check, id) in [
            (CheckId::StructuralRecall, "skim-X01"),
            (
                CheckId::LexicalRecall,
                "skim-ast-try-catch-finally-javascript",
            ),
            (
                CheckId::OrderScoreMonotone,
                "skim-ast-try-catch-finally-javascript",
            ),
        ] {
            let refs = [LedgerRef { check, id }];
            assert!(
                flagged(&check_integrity(&g, &ctx(&refs)), id),
                "{check} on {id}"
            );
        }
    }

    // --- [[refs]] / [[clash]] / [[stakes]] / concept `first` ---------------------

    /// One entry of each new table, in the shape the design shows.
    const NEW_TABLES: &str = r#"
[[refs]]
id = "skim-R01"
query = "run"
def = { path = "src/a.rs", line = 1 }
sites = [
  { path = "src/a.rs", line = 1, role = "def" },
  { path = "src/a.rs", line = 2, role = "call" },
]
origin = "curated"

[[clash]]
id = "skim-D01"
query = "run"
defs = [ { path = "src/a.rs", line = 1 }, { path = "src/b.rs", line = 1 } ]
calls = [
  { path = "src/a.rs", line = 2, def = 0 },
  { path = "src/c.rs", line = 3, def = "unassigned" },
]
split = "none"
origin = "curated"

[[stakes]]
id = "skim-S01"
query = "qzvx_absent_7713"
flags = []
expect = "exit"
exit = 1
"#;

    fn refs(id: &str, query: &str, def: (&str, u32), sites: &[(&str, u32, &str)]) -> String {
        let sites: Vec<String> = sites
            .iter()
            .map(|(p, l, r)| format!("{{ path = \"{p}\", line = {l}, role = \"{r}\" }}"))
            .collect();
        format!(
            "[[refs]]\nid = \"{id}\"\nquery = \"{query}\"\n\
             def = {{ path = \"{}\", line = {} }}\nsites = [{}]\norigin = \"curated\"\n",
            def.0,
            def.1,
            sites.join(", ")
        )
    }

    /// `calls` are `(path, line, def)` with `def` already in TOML form
    /// (`0` or `"unassigned"`).
    fn clash(
        id: &str,
        query: &str,
        defs: &[(&str, u32)],
        calls: &[(&str, u32, &str)],
        split: &str,
    ) -> String {
        let defs: Vec<String> = defs
            .iter()
            .map(|(p, l)| format!("{{ path = \"{p}\", line = {l} }}"))
            .collect();
        let calls: Vec<String> = calls
            .iter()
            .map(|(p, l, d)| format!("{{ path = \"{p}\", line = {l}, def = {d} }}"))
            .collect();
        format!(
            "[[clash]]\nid = \"{id}\"\nquery = \"{query}\"\ndefs = [{}]\ncalls = [{}]\n\
             split = \"{split}\"\norigin = \"curated\"\n",
            defs.join(", "),
            calls.join(", ")
        )
    }

    fn stakes(id: &str, query: &str, flags: &str, expect: &str, exit: Option<u8>) -> String {
        let exit = exit.map(|c| format!("exit = {c}\n")).unwrap_or_default();
        format!(
            "[[stakes]]\nid = \"{id}\"\nquery = \"{query}\"\nflags = {flags}\nexpect = \"{expect}\"\n{exit}"
        )
    }

    fn message_of(violations: &[IntegrityViolation], id: &str) -> String {
        violations
            .iter()
            .filter(|v| v.id.as_deref() == Some(id))
            .map(|v| v.message.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn the_new_tables_parse_with_every_field_and_pass_schema_integrity() {
        let g = golden(&format!("{}{NEW_TABLES}", header()));
        assert_eq!(g.refs[0].query, "run");
        assert_eq!(g.refs[0].def.line, 1);
        assert_eq!(g.refs[0].sites[1].role, SiteRole::Call);
        assert_eq!(g.refs[0].origin, Origin::Curated);
        assert_eq!(g.clashes[0].defs.len(), 2);
        assert_eq!(g.clashes[0].split, ClashSplit::None);
        assert_eq!(g.stakes[0].expect, StakesExpect::Exit);
        assert_eq!(g.stakes[0].exit, Some(1));
        assert!(g.stakes[0].flags.is_empty());
        assert_eq!(check_integrity(&g, &ctx(&[])), Vec::new());
    }

    #[test]
    fn every_site_role_name_loads() {
        for (raw, want) in [
            ("def", SiteRole::Def),
            ("call", SiteRole::Call),
            ("ref", SiteRole::Ref),
            ("import", SiteRole::Import),
            ("comment", SiteRole::Comment),
            ("string", SiteRole::String),
            ("text", SiteRole::Text),
        ] {
            let g = golden(&format!(
                "{}{}",
                header(),
                refs("skim-R1", "run", ("a.rs", 1), &[("a.rs", 1, raw)])
            ));
            assert_eq!(g.refs[0].sites[0].role, want, "{raw}");
        }
    }

    #[test]
    fn a_call_def_is_an_index_or_the_word_unassigned() {
        let g = golden(&format!("{}{NEW_TABLES}", header()));
        assert_eq!(g.clashes[0].calls[0].def, CallDef::Index(0));
        assert_eq!(
            g.clashes[0].calls[1].def,
            CallDef::Unassigned(Unassigned::Unassigned)
        );
    }

    #[test]
    fn typos_and_unknown_values_in_the_new_tables_do_not_load() {
        let bad = [
            // `site` for `sites`.
            "[[refs]]\nid = \"skim-R1\"\nquery = \"q\"\ndef = { path = \"a.rs\", line = 1 }\n\
             site = []\norigin = \"curated\"\n"
                .to_string(),
            // unknown role.
            refs("skim-R1", "q", ("a.rs", 1), &[("a.rs", 1, "callee")]),
            // a role on the clash call.
            "[[clash]]\nid = \"skim-D1\"\nquery = \"q\"\ndefs = []\n\
             calls = [{ path = \"a.rs\", line = 1, def = 0, role = \"call\" }]\n\
             split = \"none\"\norigin = \"curated\"\n"
                .to_string(),
            // a call def that is neither an index nor "unassigned".
            clash(
                "skim-D1",
                "q",
                &[("a.rs", 1), ("b.rs", 1)],
                &[("a.rs", 2, "\"first\"")],
                "none",
            ),
            clash("skim-D1", "q", &[("a.rs", 1)], &[], "approx"),
            // an `origin` on stakes (the table has none).
            format!(
                "{}origin = \"curated\"\n",
                stakes("skim-S1", "q", "[]", "lines", None)
            ),
            stakes("skim-S1", "q", "[]", "bytes", None),
            // `exit` out of range for an exit code.
            stakes("skim-S1", "q", "[]", "exit", Some(1)).replace("exit = 1", "exit = 300"),
        ];
        for body in bad {
            assert!(
                parse_golden(&format!("{}{body}", header())).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn the_new_entries_are_indexed_by_id_and_share_the_id_rules() {
        let g = golden(&format!("{}{NEW_TABLES}", header()));
        assert_eq!(g.entry_kind("skim-R01"), Some(EntryKind::Refs));
        assert_eq!(g.entry_kind("skim-D01"), Some(EntryKind::Clash));
        assert_eq!(g.entry_kind("skim-S01"), Some(EntryKind::Stakes));
        assert_eq!(g.ids().count(), 3);

        let v = violations_for(&format!(
            "{}{}{}",
            header(),
            refs("skim-R1", "run", ("a.rs", 1), &[("a.rs", 1, "def")]),
            stakes("skim-R1", "q", "[]", "lines", None),
        ));
        assert!(
            message_of(&v, "skim-R1").contains("used by 2 entries"),
            "{v:?}"
        );
        let v = violations_for(&format!(
            "{}{}",
            header(),
            stakes("zod-S1", "q", "[]", "lines", None)
        ));
        assert!(flagged(&v, "zod-S1"), "wrong corpus prefix: {v:?}");
    }

    // --- concept `first` ---------------------------------------------------------

    #[test]
    fn a_concept_without_first_still_loads_so_old_golden_files_stay_valid() {
        let g = golden(DESIGN_EXAMPLE);
        assert_eq!(g.concepts[0].first, None);
        assert!(check_integrity(&g, &ctx(&[])).is_empty());
    }

    #[test]
    fn a_concept_first_lists_the_acceptable_first_files() {
        let g = golden(&format!(
            "{}[[concept]]\nid = \"skim-C1\"\nquery = \"build lock\"\nrelevant = \"lock\"\n\
             first = [\"src/a.rs\", \"src/b.rs\"]\n",
            header()
        ));
        assert_eq!(
            g.concepts[0].first,
            Some(vec!["src/a.rs".to_string(), "src/b.rs".to_string()])
        );
        assert!(check_integrity(&g, &ctx(&[])).is_empty());
    }

    #[test]
    fn an_empty_or_blank_concept_first_is_a_violation() {
        for first in ["[]", "[\"\"]", "[\"a.rs\", \" \"]", "[\"a.rs\", \"a.rs\"]"] {
            let v = violations_for(&format!(
                "{}[[concept]]\nid = \"skim-C1\"\nquery = \"q\"\nrelevant = \"q\"\nfirst = {first}\n",
                header()
            ));
            assert!(flagged(&v, "skim-C1"), "first = {first}: {v:?}");
        }
    }

    // --- [[clash]] schema rules --------------------------------------------------

    fn clash_violations(body: &str) -> Vec<IntegrityViolation> {
        violations_for(&format!("{}{body}", header()))
    }

    #[test]
    fn a_clash_needs_two_defs_a_call_def_in_range_and_no_split() {
        let one_def = clash_violations(&clash(
            "skim-D1",
            "run",
            &[("a.rs", 1)],
            &[("a.rs", 2, "\"unassigned\"")],
            "none",
        ));
        assert!(
            message_of(&one_def, "skim-D1").contains("2 or more"),
            "{one_def:?}"
        );

        let out_of_range = clash_violations(&clash(
            "skim-D2",
            "run",
            &[("a.rs", 1), ("b.rs", 1)],
            &[("a.rs", 2, "2")],
            "none",
        ));
        assert!(
            message_of(&out_of_range, "skim-D2").contains("out of range"),
            "{out_of_range:?}"
        );

        let exact = clash_violations(&clash(
            "skim-D3",
            "run",
            &[("a.rs", 1), ("b.rs", 1)],
            &[("a.rs", 2, "0")],
            "exact",
        ));
        assert!(message_of(&exact, "skim-D3").contains("split"), "{exact:?}");
    }

    #[test]
    fn a_valid_clash_has_no_violation_and_an_index_may_name_the_last_def() {
        let v = clash_violations(&clash(
            "skim-D1",
            "run",
            &[("a.rs", 1), ("b.rs", 1)],
            &[("a.rs", 2, "1"), ("a.rs", 3, "\"unassigned\"")],
            "none",
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn a_clash_with_a_repeated_def_or_call_site_is_a_violation() {
        let repeated_def = clash_violations(&clash(
            "skim-D1",
            "run",
            &[("a.rs", 1), ("a.rs", 1)],
            &[],
            "none",
        ));
        assert!(
            message_of(&repeated_def, "skim-D1").contains("def a.rs:1 is listed twice"),
            "{repeated_def:?}"
        );
        let repeated_call = clash_violations(&clash(
            "skim-D2",
            "run",
            &[("a.rs", 1), ("b.rs", 1)],
            &[("a.rs", 2, "0"), ("a.rs", 2, "1")],
            "none",
        ));
        assert!(
            message_of(&repeated_call, "skim-D2").contains("call a.rs:2 is listed twice"),
            "{repeated_call:?}"
        );
    }

    // --- [[refs]] schema rules ---------------------------------------------------

    #[test]
    fn a_refs_entry_needs_sites_that_are_distinct_and_include_its_def() {
        let no_sites = clash_violations(&refs("skim-R1", "run", ("a.rs", 1), &[]));
        assert!(
            message_of(&no_sites, "skim-R1").contains("no sites"),
            "{no_sites:?}"
        );

        let duplicate = clash_violations(&refs(
            "skim-R2",
            "run",
            ("a.rs", 1),
            &[("a.rs", 1, "def"), ("a.rs", 1, "call")],
        ));
        assert!(
            message_of(&duplicate, "skim-R2").contains("is listed twice"),
            "{duplicate:?}"
        );

        let def_missing =
            clash_violations(&refs("skim-R3", "run", ("a.rs", 1), &[("a.rs", 2, "call")]));
        assert!(
            message_of(&def_missing, "skim-R3").contains("def is not one of the sites"),
            "{def_missing:?}"
        );

        let ok = clash_violations(&refs(
            "skim-R4",
            "run",
            ("a.rs", 1),
            &[("a.rs", 1, "def"), ("a.rs", 2, "call")],
        ));
        assert!(ok.is_empty(), "{ok:?}");
    }

    #[test]
    fn the_refs_and_clash_queries_must_be_bare_identifiers() {
        for query in ["", "two words", "a-b", "a.b", "run()", "$run"] {
            let r = clash_violations(&refs("skim-R1", query, ("a.rs", 1), &[("a.rs", 1, "def")]));
            assert!(flagged(&r, "skim-R1"), "refs {query:?}: {r:?}");
            let c = clash_violations(&clash(
                "skim-D1",
                query,
                &[("a.rs", 1), ("b.rs", 1)],
                &[],
                "none",
            ));
            assert!(flagged(&c, "skim-D1"), "clash {query:?}: {c:?}");
        }
        for query in ["run", "_run", "run2", "Run_Fast"] {
            let r = clash_violations(&refs("skim-R1", query, ("a.rs", 1), &[("a.rs", 1, "def")]));
            assert!(r.is_empty(), "refs {query:?}: {r:?}");
        }
    }

    #[test]
    fn a_refs_def_on_line_zero_is_a_violation() {
        let v = clash_violations(&refs("skim-R1", "run", ("a.rs", 0), &[("a.rs", 1, "def")]));
        assert!(
            message_of(&v, "skim-R1").contains("def.line must be >= 1"),
            "{v:?}"
        );
    }

    #[test]
    fn a_refs_site_on_line_zero_is_a_violation() {
        let v = clash_violations(&refs(
            "skim-R1",
            "run",
            ("a.rs", 1),
            &[("a.rs", 1, "def"), ("a.rs", 0, "call")],
        ));
        assert!(
            message_of(&v, "skim-R1").contains("every site line must be >= 1"),
            "{v:?}"
        );
    }

    #[test]
    fn a_clash_def_on_line_zero_is_a_violation() {
        let v = clash_violations(&clash(
            "skim-D1",
            "run",
            &[("a.rs", 0), ("b.rs", 1)],
            &[("a.rs", 2, "1")],
            "none",
        ));
        assert!(
            message_of(&v, "skim-D1").contains("every def and call line must be >= 1"),
            "{v:?}"
        );
    }

    #[test]
    fn a_clash_call_on_line_zero_is_a_violation() {
        let v = clash_violations(&clash(
            "skim-D1",
            "run",
            &[("a.rs", 1), ("b.rs", 1)],
            &[("a.rs", 0, "1")],
            "none",
        ));
        assert!(
            message_of(&v, "skim-D1").contains("every def and call line must be >= 1"),
            "{v:?}"
        );
    }

    // --- [[stakes]] schema rules -------------------------------------------------

    #[test]
    fn a_stakes_exit_is_required_for_expect_exit_and_forbidden_otherwise() {
        let v = clash_violations(&stakes("skim-S1", "q", "[]", "exit", None));
        assert!(
            message_of(&v, "skim-S1").contains("requires `exit`"),
            "{v:?}"
        );
        for expect in ["lines", "files", "counts"] {
            let v = clash_violations(&stakes("skim-S2", "q", "[]", expect, Some(1)));
            assert!(flagged(&v, "skim-S2"), "{expect}: {v:?}");
            let ok = clash_violations(&stakes("skim-S3", "q", "[]", expect, None));
            assert!(ok.is_empty(), "{expect}: {ok:?}");
        }
    }

    #[test]
    fn a_stakes_exit_other_than_0_or_1_is_refused_without_a_text_universe() {
        for exit in [2, 3, 101, 255] {
            let v = clash_violations(&stakes("skim-S1", "q", "[]", "exit", Some(exit)));
            let message = message_of(&v, "skim-S1");
            assert!(
                message.contains(&format!(
                    "exit = {exit} cannot be judged by the regex oracle"
                )),
                "exit {exit}: {v:?}"
            );
        }
        for exit in [0, 1] {
            let ok = clash_violations(&stakes("skim-S2", "q", "[]", "exit", Some(exit)));
            assert!(ok.is_empty(), "exit {exit}: {ok:?}");
        }
    }

    #[test]
    fn a_stakes_query_must_not_be_blank() {
        let v = clash_violations(&stakes("skim-S1", "  ", "[]", "lines", None));
        assert!(flagged(&v, "skim-S1"), "{v:?}");
    }

    #[test]
    fn stakes_flags_must_come_from_the_closed_set_with_their_values() {
        for flags in [
            "[]",
            "[\"-i\"]",
            "[\"-w\", \"-F\"]",
            "[\"-C\", \"2\", \"--limit\", \"5\"]",
            "[\"--limit\", \"-1\"]",
        ] {
            let v = clash_violations(&stakes("skim-S1", "q", flags, "lines", None));
            assert!(v.is_empty(), "{flags}: {v:?}");
        }
        for flags in [
            "[\"-I\"]",
            "[\"--hidden\"]",
            "[\"pattern\"]",
            "[\"-g\"]",
            "[\"--limit\"]",
        ] {
            let v = clash_violations(&stakes("skim-S1", "q", flags, "lines", None));
            assert!(flagged(&v, "skim-S1"), "{flags}: {v:?}");
        }
    }

    #[test]
    fn a_stakes_file_filter_is_refused_because_the_oracle_cannot_judge_it() {
        for flags in ["[\"-g\", \"*.rs\"]", "[\"-t\", \"rust\"]"] {
            let v = clash_violations(&stakes("skim-S1", "q", flags, "lines", None));
            assert!(flagged(&v, "skim-S1"), "{flags}: {v:?}");
        }
    }

    // --- ledger pairing for the new kinds ----------------------------------------

    #[test]
    fn a_lexical_check_in_the_ledger_cannot_name_a_refs_clash_or_stakes_entry() {
        let g = golden(&format!("{}{NEW_TABLES}", header()));
        for id in ["skim-R01", "skim-D01", "skim-S01"] {
            let refs = [LedgerRef {
                check: CheckId::LexicalRecall,
                id,
            }];
            assert!(flagged(&check_integrity(&g, &ctx(&refs)), id), "{id}");
        }
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
        assert!(f.uses_temporal_data(), "--hot is served from temporal.db");
        assert!(
            flags(&["--blast-radius", "src/a.rs"])
                .unwrap()
                .uses_temporal_data(),
            "co-change peers are served from temporal.db"
        );
        assert!(!flags(&["--ast", "try-catch"]).unwrap().uses_temporal_data());
        let q = f.oracle_query("auto refresh stale").unwrap().unwrap();
        assert_eq!(q.mode(), MatchMode::PhraseNear { span: 4 });

        let lang = flags(&["--lang", "toml"]).unwrap();
        let q = lang.oracle_query("rskim-core").unwrap().unwrap();
        assert!(q.matches("Cargo.toml", "rskim-core") && !q.matches("a.md", "rskim-core"));
        assert!(!lang.has_rank_override());
        assert!(!lang.uses_temporal_data());

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

    /// Both universes of one fixture repository.
    struct Universes {
        universe: Universe,
        text: TextUniverse,
    }

    impl Universes {
        fn of(repo: &FixtureRepo) -> Self {
            let git = GitIsolation::new(repo.home());
            Universes {
                universe: Universe::compute(repo.root(), &git).unwrap(),
                text: text_universe(repo.root(), &git).unwrap(),
            }
        }
    }

    fn corpus() -> (FixtureRepo, String, Universes) {
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
        let universe = Universes::of(&repo);
        (repo, sha, universe)
    }

    fn check_against(universes: &Universes, sha: &str, body: &str) -> Vec<IntegrityViolation> {
        let src = format!("corpus = \"skim\"\ncommit = \"{sha}\"\n{body}");
        let ctx = IntegrityContext {
            corpus: "skim",
            commit: sha,
            universe: Some(&universes.universe),
            text: Some(&universes.text),
            ledger: &[],
            catalog: catalog(),
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
        let u = Universes::of(&repo);
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

    // --- whole-word integrity against the corpus universe ------------------------

    /// `run` is a whole word on a.rs lines 1, 2 and 3 and b.rs line 1; lines
    /// that hold it only inside a longer word are a.rs:4 and b.rs:2.
    fn word_corpus() -> (FixtureRepo, String, Universes) {
        let repo = FixtureRepo::new();
        repo.write(
            "src/a.rs",
            "pub fn run() {}\nfn helper() { run(); }\n// rerun is not run\nlet runner = 1;\n",
        );
        repo.write("src/b.rs", "pub fn run(x: u32) {}\nfn rerun_it() {}\n");
        let sha = repo.commit_all("init");
        let universe = Universes::of(&repo);
        (repo, sha, universe)
    }

    fn word_violations(body: &str) -> Vec<IntegrityViolation> {
        let (_repo, sha, u) = word_corpus();
        check_against(&u, &sha, body)
    }

    #[test]
    fn a_def_line_that_holds_the_query_only_inside_a_longer_word_is_a_violation() {
        let (_repo, sha, u) = word_corpus();
        let ident = |path: &str, line: u32| {
            format!(
                "[[ident]]\nid = \"skim-L01\"\nquery = \"run\"\n\
                 def = {{ path = \"{path}\", line = {line} }}\norigin = \"seed\"\n"
            )
        };
        assert!(check_against(&u, &sha, &ident("src/a.rs", 1)).is_empty());
        for (path, line) in [("src/a.rs", 4), ("src/b.rs", 2)] {
            let v = check_against(&u, &sha, &ident(path, line));
            assert!(
                message_of(&v, "skim-L01").contains("does not contain"),
                "{path}:{line}: {v:?}"
            );
        }
    }

    #[test]
    fn a_refs_site_set_equal_to_the_whole_word_lines_passes() {
        let v = word_violations(&refs(
            "skim-R1",
            "run",
            ("src/a.rs", 1),
            &[
                ("src/a.rs", 1, "def"),
                ("src/a.rs", 2, "call"),
                ("src/a.rs", 3, "comment"),
                ("src/b.rs", 1, "def"),
            ],
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn a_refs_site_set_that_misses_a_whole_word_line_is_a_violation() {
        let v = word_violations(&refs(
            "skim-R1",
            "run",
            ("src/a.rs", 1),
            &[
                ("src/a.rs", 1, "def"),
                ("src/a.rs", 2, "call"),
                ("src/b.rs", 1, "def"),
            ],
        ));
        let message = message_of(&v, "skim-R1");
        assert!(
            message.contains("missing") && message.contains("src/a.rs:3"),
            "{v:?}"
        );
    }

    #[test]
    fn a_refs_site_on_a_line_without_the_whole_word_is_a_violation() {
        let v = word_violations(&refs(
            "skim-R1",
            "run",
            ("src/a.rs", 1),
            &[
                ("src/a.rs", 1, "def"),
                ("src/a.rs", 2, "call"),
                ("src/a.rs", 3, "comment"),
                ("src/a.rs", 4, "ref"),
                ("src/b.rs", 1, "def"),
            ],
        ));
        let message = message_of(&v, "skim-R1");
        assert!(
            message.contains("not a whole-word line") && message.contains("src/a.rs:4"),
            "{v:?}"
        );
    }

    #[test]
    fn a_refs_site_outside_the_universe_is_a_violation() {
        let v = word_violations(&refs(
            "skim-R1",
            "run",
            ("src/a.rs", 1),
            &[
                ("src/a.rs", 1, "def"),
                ("src/a.rs", 2, "call"),
                ("src/a.rs", 3, "comment"),
                ("src/b.rs", 1, "def"),
                ("src/gone.rs", 7, "call"),
            ],
        ));
        assert!(message_of(&v, "skim-R1").contains("src/gone.rs:7"), "{v:?}");
    }

    #[test]
    fn a_refs_def_that_is_not_a_whole_word_line_is_a_violation() {
        let v = word_violations(&refs(
            "skim-R1",
            "run",
            ("src/a.rs", 4),
            &[
                ("src/a.rs", 1, "def"),
                ("src/a.rs", 2, "call"),
                ("src/a.rs", 3, "comment"),
                ("src/a.rs", 4, "def"),
                ("src/b.rs", 1, "def"),
            ],
        ));
        let message = message_of(&v, "skim-R1");
        assert!(
            message.contains("src/a.rs:4 does not contain \"run\" as a whole word"),
            "{v:?}"
        );
    }

    #[test]
    fn clash_defs_and_calls_must_be_whole_word_lines() {
        let ok = word_violations(&clash(
            "skim-D1",
            "run",
            &[("src/a.rs", 1), ("src/b.rs", 1)],
            &[("src/a.rs", 2, "0"), ("src/a.rs", 3, "\"unassigned\"")],
            "none",
        ));
        assert!(ok.is_empty(), "{ok:?}");

        let bad_call = word_violations(&clash(
            "skim-D2",
            "run",
            &[("src/a.rs", 1), ("src/b.rs", 1)],
            &[("src/a.rs", 4, "\"unassigned\"")],
            "none",
        ));
        assert!(
            message_of(&bad_call, "skim-D2").contains("src/a.rs:4"),
            "{bad_call:?}"
        );

        let bad_def = word_violations(&clash(
            "skim-D3",
            "run",
            &[("src/a.rs", 1), ("src/b.rs", 2)],
            &[],
            "none",
        ));
        assert!(
            message_of(&bad_def, "skim-D3").contains("src/b.rs:2"),
            "{bad_def:?}"
        );

        let gone = word_violations(&clash(
            "skim-D4",
            "run",
            &[("src/a.rs", 1), ("src/gone.rs", 1)],
            &[],
            "none",
        ));
        assert!(flagged(&gone, "skim-D4"), "{gone:?}");
    }

    #[test]
    fn a_concept_first_path_must_be_in_the_universe() {
        let concept = |first: &str| {
            format!(
                "[[concept]]\nid = \"skim-C1\"\nquery = \"run\"\nrelevant = \"run\"\nfirst = {first}\n"
            )
        };
        assert!(word_violations(&concept("[\"src/a.rs\", \"src/b.rs\"]")).is_empty());
        let v = word_violations(&concept("[\"src/a.rs\", \"src/gone.rs\"]"));
        assert!(message_of(&v, "skim-C1").contains("src/gone.rs"), "{v:?}");
    }

    // --- the text universe and the stakes exit rule -------------------------------

    /// `run` is a whole word in `src/a.rs:1` (indexed) and in `LICENSE:1`,
    /// a file the lexical index skips but the text universe reads.
    fn licensed_corpus() -> (FixtureRepo, String, Universes) {
        let repo = FixtureRepo::new();
        repo.write("src/a.rs", "pub fn run() {}\n");
        repo.write("LICENSE", "you may run it\n");
        let sha = repo.commit_all("init");
        let universe = Universes::of(&repo);
        (repo, sha, universe)
    }

    #[test]
    fn refs_sites_are_judged_against_the_text_universe_not_the_indexed_one() {
        let (_repo, sha, u) = licensed_corpus();
        assert_eq!(
            (u.universe.len(), u.text.len()),
            (1, 2),
            "LICENSE is in the text universe only"
        );
        let with_license = refs(
            "skim-R1",
            "run",
            ("src/a.rs", 1),
            &[("src/a.rs", 1, "def"), ("LICENSE", 1, "text")],
        );
        assert!(check_against(&u, &sha, &with_license).is_empty());

        let without = refs("skim-R2", "run", ("src/a.rs", 1), &[("src/a.rs", 1, "def")]);
        let v = check_against(&u, &sha, &without);
        assert!(
            message_of(&v, "skim-R2").contains("LICENSE:1"),
            "the file the index skips still owes a site: {v:?}"
        );
    }

    #[test]
    fn an_ident_def_must_stay_in_the_indexed_universe_even_with_a_text_universe() {
        let (_repo, sha, u) = licensed_corpus();
        let ident = |path: &str| {
            format!(
                "[[ident]]\nid = \"skim-L01\"\nquery = \"run\"\n\
                 def = {{ path = \"{path}\", line = 1 }}\norigin = \"seed\"\n"
            )
        };
        assert!(check_against(&u, &sha, &ident("src/a.rs")).is_empty());
        let v = check_against(&u, &sha, &ident("LICENSE"));
        assert!(
            message_of(&v, "skim-L01").contains("indexed universe"),
            "{v:?}"
        );
    }

    #[test]
    fn a_stakes_exit_must_agree_with_the_oracle_hit_count() {
        let (_repo, sha, u) = word_corpus();
        let check = |query: &str, flags: &str, exit: u8| {
            check_against(
                &u,
                &sha,
                &stakes("skim-S1", query, flags, "exit", Some(exit)),
            )
        };
        // No match: exit 1 holds, exit 0 does not.
        assert!(check("qzvx_absent", "[]", 1).is_empty());
        let v = check("qzvx_absent", "[]", 0);
        assert!(
            message_of(&v, "skim-S1").contains("finds no matching line"),
            "{v:?}"
        );
        // A match: exit 0 holds, exit 1 does not, and the message names a site.
        assert!(check("run", "[]", 0).is_empty());
        let v = check("run", "[]", 1);
        assert!(message_of(&v, "skim-S1").contains("src/a.rs:1"), "{v:?}");
        // Any other exit code is one the regex oracle cannot judge: refused.
        let v = check("run", "[]", 2);
        assert!(
            message_of(&v, "skim-S1").contains("cannot be judged by the regex oracle"),
            "{v:?}"
        );
    }

    #[test]
    fn a_stakes_exit_is_not_judged_without_a_text_universe() {
        let ctx = IntegrityContext {
            corpus: "skim",
            commit: "b8a0a79463382347820f1c2572bde37b68e87c76",
            universe: None,
            text: None,
            ledger: &[],
            catalog: catalog(),
        };
        let src = format!(
            "corpus = \"skim\"\ncommit = \"{}\"\n{}",
            ctx.commit,
            stakes("skim-S1", "run", "[]", "exit", Some(1))
        );
        assert!(check_integrity(&golden(&src), &ctx).is_empty());
    }

    #[test]
    fn stakes_flags_change_what_the_oracle_counts_as_a_match() {
        let (_repo, sha, u) = word_corpus();
        let exit_of = |query: &str, flags: &str| -> u8 {
            // The exit that holds is the one the check lets through.
            let holds = |exit: u8| {
                check_against(
                    &u,
                    &sha,
                    &stakes("skim-S1", query, flags, "exit", Some(exit)),
                )
                .is_empty()
            };
            match (holds(0), holds(1)) {
                (true, false) => 0,
                (false, true) => 1,
                other => unreachable!("exactly one exit must hold: {other:?}"),
            }
        };
        // Case: the default is exact, `-i` ignores case, `-S` ignores it only
        // for an all-lowercase query, and `-i` wins over `-S`.
        assert_eq!(exit_of("RUN", "[]"), 1);
        assert_eq!(exit_of("RUN", "[\"-i\"]"), 0);
        assert_eq!(exit_of("run", "[\"-S\"]"), 0);
        assert_eq!(exit_of("RUN", "[\"-S\"]"), 1);
        assert_eq!(exit_of("RUN", "[\"-i\", \"-S\"]"), 0);
        // Whole word: `ru` is inside `run` but is no word of its own.
        assert_eq!(exit_of("ru", "[]"), 0);
        assert_eq!(exit_of("ru", "[\"-w\"]"), 1);
        // Regex: `-E` reads the query as a pattern, `-F` and the default as text.
        assert_eq!(exit_of("ru[n]", "[]"), 1);
        assert_eq!(exit_of("ru[n]", "[\"-F\"]"), 1);
        assert_eq!(exit_of("ru[n]", "[\"-E\"]"), 0);
        // Display flags change nothing.
        assert_eq!(
            exit_of("run", "[\"-n\", \"-C\", \"2\", \"--limit\", \"3\"]"),
            0
        );
    }

    #[test]
    fn contradicting_or_unreadable_stakes_queries_are_violations() {
        let (_repo, sha, u) = word_corpus();
        for (query, flags) in [("run", "[\"-F\", \"-E\"]"), ("ru(n", "[\"-E\"]")] {
            let v = check_against(&u, &sha, &stakes("skim-S1", query, flags, "lines", None));
            assert!(flagged(&v, "skim-S1"), "{query} {flags}: {v:?}");
        }
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
    fn a_golden_file_with_ast_entries_folds_the_oracle_digest_into_its_own() {
        let raw = b"corpus = \"skim\"\n";
        // No [[ast]] entry, so no oracle digest: the raw bytes' SHA-256.
        assert_eq!(golden_digest(raw, None), hex_sha256(raw));
        // With [[ast]] entries: any change to the oracle changes the digest.
        let with = golden_digest(raw, Some("q1"));
        assert_ne!(with, hex_sha256(raw));
        assert_ne!(with, golden_digest(raw, Some("q2")));
        assert_eq!(with, golden_digest(raw, Some("q1")));
        assert_eq!(with.len(), 64);
    }

    #[test]
    fn the_oracle_digest_covers_every_query_text_filter_and_intent() {
        let fingerprint = structural::fingerprint();
        for q in structural::query_sources() {
            assert!(
                fingerprint.contains(&q.file_name()) && fingerprint.contains(q.source),
                "{} is not in the golden digest",
                q.file_name()
            );
        }
        assert!(
            fingerprint.contains("at-least 20 @body"),
            "god-function threshold"
        );
        assert!(
            fingerprint.contains("intent rust-nested-loop"),
            "intent specs"
        );
        assert_eq!(structural_oracle_sha256(), structural_oracle_sha256());
        assert_eq!(structural_oracle_sha256().len(), 64);
    }

    #[test]
    fn load_golden_folds_the_oracle_digest_only_into_files_with_ast_entries() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.toml");
        std::fs::write(&plain, DESIGN_EXAMPLE).unwrap();
        assert_eq!(
            load_golden(&plain).unwrap().sha256,
            hex_sha256(DESIGN_EXAMPLE.as_bytes())
        );

        let with_ast = format!(
            "{DESIGN_EXAMPLE}{}",
            ast("skim-A1", "try-catch", "typescript", "hard")
        );
        let structural_file = dir.path().join("ast.toml");
        std::fs::write(&structural_file, &with_ast).unwrap();
        assert_eq!(
            load_golden(&structural_file).unwrap().sha256,
            golden_digest(with_ast.as_bytes(), Some(&structural_oracle_sha256()))
        );
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
