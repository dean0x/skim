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
use crate::scoreboard::oracle::{
    LangFilter, LexicalQuery, LineMode, LineQuery, PathFilter, ground_truth, is_case_insensitive,
    line_truth,
};
use crate::scoreboard::types::{CheckId, EntryKind, Role};
use crate::scoreboard::universe::TextUniverse;

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
}

/// `[[lexical]]`: a recall/precision edge case. Every whitespace-separated
/// token of `query` must occur as a case-sensitive substring.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LexicalEntry {
    pub id: String,
    pub query: String,
    pub category: LexicalCategory,
    /// A language name (see [`LangFilter::parse`]); passed to skim as the
    /// matching `-t` type and applied by the oracle's own copy of that type.
    #[serde(default)]
    pub lang: Option<String>,
}

impl LexicalEntry {
    /// The CLI flags this entry runs with (`-t TYPE`).
    ///
    /// # Errors
    ///
    /// An unknown `lang`.
    pub fn flags(&self) -> anyhow::Result<QueryFlags> {
        QueryFlags::new(self.lang.as_deref())
    }

    /// The oracle query for this entry's ground truth.
    ///
    /// # Errors
    ///
    /// As [`LexicalEntry::flags`], plus any [`LexicalQuery::new`] refusal.
    pub fn oracle_query(&self) -> anyhow::Result<LexicalQuery> {
        self.flags()?.oracle_query(&self.query)
    }
}

/// `[[pagination]]`: a sweep of `--offset 0, L, 2L, …` for each `L` in
/// `limits`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaginationEntry {
    pub id: String,
    pub query: String,
    pub limits: Vec<u32>,
}

/// `[[prefix]]`: `--limit N` must equal the first N rows of the full list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixEntry {
    pub id: String,
    pub query: String,
    pub limits: Vec<u32>,
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
    /// rg-style tokens from [`STAKES_FLAGS`], passed to skim as written.
    #[serde(default)]
    pub flags: Vec<String>,
    pub expect: StakesExpect,
    /// The expected exit code: `0` (a match), `1` (no match) or `2` (skim
    /// rejects the query or its flags). Required when `expect = "exit"`,
    /// forbidden otherwise.
    #[serde(default)]
    pub exit: Option<u8>,
}

/// What the regex oracle says the plain-text search does with a
/// [`StakesEntry`].
#[derive(Debug, Clone)]
pub enum StakesRun {
    /// A valid query: the lines it must report, over the files its `-g` and
    /// `-t` filters keep.
    Search {
        query: LineQuery,
        filter: PathFilter,
    },
    /// A query the search rejects, with the reason (exit 2).
    Usage(String),
}

/// The flags the runner sets itself: a stakes entry must not repeat them.
const RUNNER_FLAGS: &[&str] = &["--json", "--limit", "--offset", "--root", "-h"];

/// Flags that exclude `-l` and `-c`.
const LIST_EXCLUDES: &[&str] = &["-C", "--all", "--full"];

impl StakesEntry {
    /// The regex oracle's reading of this entry.
    ///
    /// The flags that change what matches are the match-mode and case
    /// switches. `-E` reads the query as a regex; `-F` and the default read
    /// it as literal text. `-i` ignores case; `-S` ignores case when the query
    /// has no uppercase character, and `-i` wins over `-S`. `-w` keeps a match
    /// only as a whole word. `-g` and `-t` keep the files the filters keep.
    /// The other flags only change how a result is shown, so the oracle
    /// ignores them.
    ///
    /// A flag pair the search rejects (`-F` with `-E`, `-l` with `-c`, `-l` or
    /// `-c` with `-C`, `--all` or `--full`), an empty literal and a regex that
    /// does not compile are [`StakesRun::Usage`].
    ///
    /// # Errors
    ///
    /// - a flag that is not in [`STAKES_FLAGS`], or one that lacks its value;
    /// - a flag the runner sets itself;
    /// - a query with ASCII whitespace and neither `-F` nor `-E`: the search
    ///   reads it as a list of words, which the line oracle cannot judge;
    /// - a `-g` glob or `-t` type the oracle does not cover.
    pub fn run(&self) -> anyhow::Result<StakesRun> {
        let names = stakes_flag_names(&self.flags)?;
        let has = |flag: &str| names.contains(&flag);
        for owned in RUNNER_FLAGS {
            anyhow::ensure!(
                !has(owned),
                "flag {owned:?} is set by the scoreboard runner; leave it out"
            );
        }
        let explicit = has("-F") || has("-E");
        anyhow::ensure!(
            explicit || !self.query.bytes().any(|b| b.is_ascii_whitespace()),
            "a query with whitespace is a list of words; use -F or -E so the line oracle can judge it"
        );
        let filter = self.path_filter()?;
        if has("-F") && has("-E") {
            return Ok(StakesRun::Usage("-F and -E cannot be combined".into()));
        }
        if has("-l") && has("-c") {
            return Ok(StakesRun::Usage("-l and -c cannot be combined".into()));
        }
        for list in ["-l", "-c"] {
            if has(list)
                && let Some(other) = LIST_EXCLUDES.iter().find(|o| has(o))
            {
                return Ok(StakesRun::Usage(format!(
                    "{list} cannot be combined with {other}"
                )));
            }
        }
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
        Ok(match LineQuery::new(&self.query, mode) {
            Ok(query) => StakesRun::Search { query, filter },
            Err(e) => StakesRun::Usage(format!("{e:#}")),
        })
    }

    /// The `-g` and `-t` filters of this entry.
    ///
    /// # Errors
    ///
    /// A flag lacks its value, a glob uses syntax the oracle does not cover,
    /// or a type is not one the oracle copies.
    pub fn path_filter(&self) -> anyhow::Result<PathFilter> {
        let mut filter = PathFilter::new();
        let mut tokens = self.flags.iter();
        while let Some(flag) = tokens.next() {
            let takes_value = STAKES_FLAGS
                .iter()
                .find(|(name, _)| name == flag)
                .map(|(_, takes_value)| *takes_value)
                .with_context(|| {
                    format!("flag {flag:?} is not in the closed set of stakes flags")
                })?;
            if !takes_value {
                continue;
            }
            let value = tokens
                .next()
                .with_context(|| format!("flag {flag:?} needs a value"))?;
            filter = match flag.as_str() {
                "-g" => filter.with_glob(value)?,
                "-t" => filter.with_type(value)?,
                _ => filter,
            };
        }
        Ok(filter)
    }

    /// Whether `flag` is among the entry's flags.
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
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
    #[serde(default, rename = "refs")]
    pub refs: Vec<RefsEntry>,
    #[serde(default, rename = "clash")]
    pub clashes: Vec<ClashEntry>,
    #[serde(default, rename = "stakes")]
    pub stakes: Vec<StakesEntry>,
}

impl GoldenFile {
    /// Every `(id, kind)`, in file order within each kind (ident, concept,
    /// lexical, pagination, prefix, refs, clash, stakes).
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
            .chain(refs)
            .chain(clashes)
            .chain(stakes)
    }

    /// The kind of the entry with `id`.
    pub fn entry_kind(&self, id: &str) -> Option<EntryKind> {
        self.ids().find(|(i, _)| *i == id).map(|(_, k)| k)
    }
}

/// A golden file plus its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedGolden {
    pub file: GoldenFile,
    /// Lowercase hex SHA-256 of the file bytes as read (any byte edit,
    /// comments included, changes it). This is the per-corpus
    /// `golden_sha256` the gate and `bless` compare.
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
    let sha256 = hex_sha256(raw.as_bytes());
    Ok(LoadedGolden { file, sha256 })
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

/// The search flags a lexical golden entry passes to skim: the file type its
/// `lang` names. The runner owns `--json`, `--limit`, `--offset` and
/// `--root`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryFlags {
    /// `-t TYPE`, from a golden `lang`.
    pub lang: Option<LangFilter>,
}

impl QueryFlags {
    /// Flags for the golden `lang` value, if any.
    ///
    /// # Errors
    ///
    /// A `lang` value [`LangFilter::parse`] does not accept.
    pub fn new(lang: Option<&str>) -> anyhow::Result<Self> {
        Ok(QueryFlags {
            lang: lang.map(LangFilter::parse).transpose()?,
        })
    }

    /// The CLI tokens (every flag goes before `--`; the runner adds the query
    /// after it). A golden `lang` becomes `-t` with the rg type token:
    /// javascript to `js`, typescript and tsx to `ts`, python to `py`,
    /// markdown to `md`, and `rust`, `json` and `toml` as they are.
    pub fn to_args(&self) -> Vec<String> {
        self.lang
            .iter()
            .flat_map(|lang| ["-t".to_string(), lang.type_token().to_string()])
            .collect()
    }

    /// The oracle query for `query`'s full result set.
    ///
    /// # Errors
    ///
    /// A query [`LexicalQuery::new`] refuses.
    pub fn oracle_query(&self, query: &str) -> anyhow::Result<LexicalQuery> {
        LexicalQuery::new(query, self.lang)
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

/// Whether a full count fits [`pagination_bound`]. Golden integrity checks it
/// against the oracle's ground truth only. The runner does not check skim's
/// own full list: a sweep that does not end within [`MAX_PAGES`] pages fails
/// the `pagination.has_more_honest` check.
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
    /// The corpus's oracle universe
    /// ([`crate::scoreboard::universe::text_universe`]): every tracked text
    /// file, with no language allow-list. Enables the whole-word line checks
    /// of `[[ident]]`, `[[refs]]` and `[[clash]]`, the def-path, pagination-bound
    /// and zero-hit checks, and the hit-count rule of `[[stakes]]`; `None`
    /// runs the schema-level checks only.
    pub universe: Option<&'a TextUniverse>,
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
/// concept regexes compile; a lexical `lang` is a type the oracle copies and
/// the oracle accepts the query; pagination and prefix `limits` are non-empty
/// and positive; a concept's `first` list is non-empty with distinct,
/// non-blank paths; a `[[refs]]` or `[[clash]]` query is a bare identifier; a
/// `[[refs]]` entry has distinct sites that include its `def` and at least
/// one call site; a `[[clash]]` has 2 or more distinct defs, one or more
/// distinct calls, every call `def` index in range and `split = "none"`; a
/// `[[stakes]]` entry uses only flags of
/// [`STAKES_FLAGS`] and none the runner sets itself, reads as a regex-oracle
/// query (or, for `exit = 2`, as a query the search rejects), pairs `-l` with
/// `expect = "files"` and `-c` with `expect = "counts"`, and has `exit`
/// exactly when `expect = "exit"`, and that `exit` is 0, 1 or 2; every ledger
/// entry names an existing id whose kind its check applies to.
///
/// With the universe: every `[[ident]]` `def.path` is in it and its `def.line`
/// holds the query as a whole word; every concept `first` path is in it; every
/// pagination entry's ground truth fits [`pagination_bound`] and holds at
/// least [`PAGINATION_MIN_FILES`] files; every `zero-hit` entry's ground truth
/// is empty; every `[[refs]]` site set equals the whole-word line set of its
/// query, and its `def` line is one of them; every `[[clash]]` def and call
/// line holds the query as a whole word; every `[[stakes]]` `exit = 1` has no
/// matching line, and every `exit = 0` and every `lines`, `files` or `counts`
/// entry has at least one (an empty oracle answer would pass any output).
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
    for e in &golden.refs {
        v.entry_err(&e.id, check_refs(e, ctx.universe));
    }
    for e in &golden.clashes {
        v.entry_err(&e.id, check_clash(e, ctx.universe));
    }
    for e in &golden.stakes {
        v.entry_err(&e.id, check_stakes(e, ctx.universe));
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

/// The line at `path:line` in the text universe must hold the query of `word`
/// as a whole word.
fn check_word_line(
    universe: &TextUniverse,
    word: &LineQuery,
    path: &str,
    line: u32,
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
        word.matches_line(held),
        "{path}:{line} does not contain {:?} as a whole word (line is {held:?})",
        word.raw()
    );
    Ok(())
}

/// A `[[refs]]` or `[[clash]]` query is a bare identifier: Unicode letters and
/// digits (`char::is_alphanumeric`) and underscore.
///
/// The oracle reads `-w` as `rg -w` does: a word character is a character of
/// the regex crate's Unicode `\w` (letters, marks, decimal digits, connector
/// punctuation and join controls). `git grep -w`, which froze the sites,
/// counts only ASCII letters, digits and underscore as word characters. The
/// oracle's word set contains git's, so a line that git froze and the oracle
/// rejects (a match next to a combining mark, for example) is an integrity
/// violation (exit 2), not a silent pass.
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

/// An `[[ident]]` definition must be in the universe and its line must hold
/// the query as a whole word.
fn check_ident(e: &IdentEntry, universe: Option<&TextUniverse>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    anyhow::ensure!(e.def.line >= 1, "def.line must be >= 1");
    let Some(universe) = universe else {
        return Ok(());
    };
    anyhow::ensure!(
        universe.contains(&e.def.path),
        "{}:{} is not in the oracle's universe at the pinned commit",
        e.def.path,
        e.def.line
    );
    check_word_line(
        universe,
        &LineQuery::whole_word(&e.query)?,
        &e.def.path,
        e.def.line,
    )
}

fn check_concept(e: &ConceptEntry, universe: Option<&TextUniverse>) -> anyhow::Result<()> {
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
                "`first` path {path:?} is not in the oracle's universe at the pinned commit"
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
    anyhow::ensure!(
        e.sites.iter().any(|s| s.role == SiteRole::Call),
        "entry has no call site (role = \"call\"); the call check would pass on nothing"
    );
    let Some(universe) = universe else {
        return Ok(());
    };
    let word = LineQuery::whole_word(&e.query)?;
    check_word_line(universe, &word, &e.def.path, e.def.line)?;
    let truth = line_truth(universe.files(), &word);
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
        !e.calls.is_empty(),
        "entry has no calls; the call check would pass on nothing"
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
    let word = LineQuery::whole_word(&e.query)?;
    for d in &e.defs {
        check_word_line(universe, &word, &d.path, d.line)?;
    }
    for c in &e.calls {
        check_word_line(universe, &word, &c.path, c.line)?;
    }
    Ok(())
}

/// A `[[stakes]]` entry has known flags, a query the regex oracle can read
/// (or, for `exit = 2`, one the search rejects), and `exit` exactly when
/// `expect = "exit"`. The exit codes the oracle can judge are 0 (a match), 1
/// (no match) and 2 (a rejected query); any other value is refused, with or
/// without a universe. `-l` lists files and `-c` counts lines, so each pairs
/// with its own expectation. Against the universe, `exit = 1` needs no
/// matching line; `exit = 0` and every `lines`, `files` or `counts` entry need
/// one, because an empty oracle answer passes any output. The lines are those
/// of the files the `-g` and `-t` filters keep.
fn check_stakes(e: &StakesEntry, universe: Option<&TextUniverse>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    let run = e.run()?;
    let expected_exit = match (e.expect, e.exit) {
        (StakesExpect::Exit, None) => anyhow::bail!("expect = \"exit\" requires `exit`"),
        (StakesExpect::Exit, Some(code @ (0..=2))) => Some(code),
        (StakesExpect::Exit, Some(other)) => anyhow::bail!(
            "exit = {other} cannot be judged by the regex oracle; use 0 (a match), \
             1 (no match) or 2 (a rejected query)"
        ),
        (_, Some(_)) => anyhow::bail!("`exit` is only valid with expect = \"exit\""),
        (_, None) => None,
    };
    anyhow::ensure!(
        !e.has_flag("-l") || matches!(e.expect, StakesExpect::Files | StakesExpect::Exit),
        "-l lists files: use expect = \"files\" or \"exit\""
    );
    anyhow::ensure!(
        !e.has_flag("-c") || matches!(e.expect, StakesExpect::Counts | StakesExpect::Exit),
        "-c counts lines: use expect = \"counts\" or \"exit\""
    );
    let (query, filter) = match (run, expected_exit) {
        (StakesRun::Usage(_), Some(2)) => return Ok(()),
        (StakesRun::Usage(reason), _) => {
            anyhow::bail!("the search rejects this query ({reason}); only an exit = 2 entry may")
        }
        (StakesRun::Search { .. }, Some(2)) => {
            anyhow::bail!("exit = 2 but the oracle reads a valid query")
        }
        (StakesRun::Search { query, filter }, _) => (query, filter),
    };
    let Some(universe) = universe else {
        return Ok(());
    };
    let hits = line_truth(
        universe.files().filter(|(path, _)| filter.keeps(path)),
        &query,
    );
    if expected_exit == Some(1) {
        anyhow::ensure!(
            hits.is_empty(),
            "exit = 1 (no match) but the oracle finds {} matching line(s), e.g. {}",
            hits.len(),
            show_sites(hits.iter())
        );
    } else {
        anyhow::ensure!(
            !hits.is_empty(),
            "the entry needs a match (exit = 0, or a lines, files or counts answer) \
             but the oracle finds no matching line, so its checks would pass on nothing"
        );
    }
    Ok(())
}

fn check_lexical(e: &LexicalEntry, universe: Option<&TextUniverse>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
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

/// The fewest files a multi-word pagination query must match. In the word view the
/// engine lists only the files that hold every word when at least three do;
/// below that it adds files that hold some words, which are not in the
/// ground truth.
pub const PAGINATION_MIN_FILES: u64 = 3;

fn check_pagination(e: &PaginationEntry, universe: Option<&TextUniverse>) -> anyhow::Result<()> {
    non_blank(&e.query)?;
    check_limits(&e.limits)?;
    let query = LexicalQuery::new(&e.query, None)?;
    if let Some(universe) = universe {
        let count = u64::try_from(ground_truth(universe.files(), &query).len())?;
        anyhow::ensure!(
            within_pagination_bound(count, &e.limits),
            "ground truth has {count} files, over min(limits) x (MAX_PAGES - 1) = {:?}",
            pagination_bound(&e.limits)
        );
        anyhow::ensure!(
            count >= PAGINATION_MIN_FILES || e.query.split_whitespace().nth(1).is_none(),
            "ground truth has {count} file(s); the word view adds files that hold only some \
             words below {PAGINATION_MIN_FILES} files, so a sweep would page over them"
        );
    }
    Ok(())
}

fn check_prefix(e: &PrefixEntry) -> anyhow::Result<()> {
    check_limits(&e.limits)?;
    non_blank(&e.query)?;
    LexicalQuery::new(&e.query, None)?;
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
#[path = "golden_tests.rs"]
mod tests;
