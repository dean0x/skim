//! The scan engine of `skim search`: read the universe, find every matching
//! line, label the lines, and keep what the output needs.
//!
//! # Two phases
//!
//! Both run on one rayon pool of at most [`MAX_SCAN_THREADS`] threads, one phase
//! after the other.
//!
//! **Phase A** reads every admitted file once. It counts the matching lines
//! exactly (per word for a word query), notes whether a matching line holds a
//! declaration keyword before the match, takes the file prior and a digest of
//! the content. No line text is kept, so memory is O(files).
//!
//! **Phase B** visits the matched files in parse order. It re-reads each file
//! and compares the digest (a file that changed is counted and not listed), then
//! labels the file with one bounded parse, decides the role of every matching
//! line, and stores line records up to a fixed cap. Role counts are exact for
//! every matching line, stored or not. With `-C`, it then keeps the context
//! lines of the stored records.
//!
//! # Bounds
//!
//! Every bound is a named constant of [`Limits`]: stored lines, context lines,
//! definition lines (per file and over all files), the parse budget (files and
//! bytes), the per-file parse cap, the line width, the pool size. A file the
//! budget leaves unparsed keeps its lines under `unlabelled`; nothing is
//! dropped.
//!
//! The record quota of each file is assigned from the exact line counts of
//! phase A, in parse order, before phase B starts. The context budget is spent
//! in parse order too, on the lines that each file needs once it has its
//! records. The result does not depend on thread scheduling.
//!
//! Each cap that cuts lines is recorded where it cuts, in [`CapsHit`]. The
//! output names the caps from that record and does not infer them from sums.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher as _;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rskim_core::Language;

use super::matcher::Matcher;
use super::rank::{FilePrior, WORD_VIEW_BEST_LINES, WordInput, file_prior, rank_words};
use super::roles::{
    Holder, Labelling, Labels, Role, RoleCounts, Unlabelled, declaration_keywords,
    has_declaration_keyword,
};
use super::universe::{Candidate, Containment, LeftOut, LeftOutCounts, Listing, Read};

/// Most threads of the scan pool.
pub(super) const MAX_SCAN_THREADS: usize = 8;

/// Most line records kept over the whole result.
pub(super) const MAX_STORED_LINES: usize = 10_000;

/// Most context lines kept over the whole result.
pub(super) const MAX_CONTEXT_LINES: usize = 100_000;

/// Bytes of a stored line; a longer line is clipped at a character boundary.
pub(super) const LINE_WIDTH_CAP: usize = 300;

/// Most files parsed for labels.
pub(super) const PARSE_BUDGET_FILES: usize = 500;

/// Most bytes parsed for labels.
pub(super) const PARSE_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// A file above this size is not parsed.
pub(super) const PER_FILE_PARSE_CAP: usize = 1024 * 1024;

/// Most definition lines kept per file. The definitions past it are counted in
/// the role counts and not listed.
pub(super) const MAX_DEF_SITES_PER_FILE: usize = 100;

/// Most definition lines kept over all files, in parse order. The definitions
/// past it are counted in the role counts and not listed.
pub(super) const MAX_DEF_SITES: usize = 10_000;

/// The bounds of one scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Limits {
    pub(super) max_stored_lines: usize,
    pub(super) max_def_sites: usize,
    pub(super) max_def_sites_per_file: usize,
    pub(super) max_context_lines: usize,
    pub(super) parse_files: usize,
    pub(super) parse_bytes: usize,
    pub(super) per_file_parse_cap: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_stored_lines: MAX_STORED_LINES,
            max_def_sites: MAX_DEF_SITES,
            max_def_sites_per_file: MAX_DEF_SITES_PER_FILE,
            max_context_lines: MAX_CONTEXT_LINES,
            parse_files: PARSE_BUDGET_FILES,
            parse_bytes: PARSE_BUDGET_BYTES,
            per_file_parse_cap: PER_FILE_PARSE_CAP,
        }
    }
}

/// The number of threads of the scan pool for a machine with `available`
/// hardware threads: at least one, at most [`MAX_SCAN_THREADS`].
pub(super) fn pool_threads(available: usize) -> usize {
    available.clamp(1, MAX_SCAN_THREADS)
}

// ============================================================================
// The query and the result
// ============================================================================

/// What the scan looks for.
pub(super) enum QueryPlan {
    /// One pattern: the labelled view. The matcher is boxed, because it is
    /// much larger than the word list.
    Pattern(Box<Matcher>),
    /// Several plain words: the word view. A line matches when it holds any
    /// word.
    Words(Vec<Matcher>),
}

/// The query and how to read hits.
pub(super) struct ScanQuery {
    pub(super) plan: QueryPlan,
    /// The pattern is a bare identifier: a hit inside a longer identifier is
    /// `partial`.
    pub(super) identifier: bool,
}

/// What a scan needs besides the universe.
pub(super) struct ScanConfig<'a> {
    pub(super) root: &'a Path,
    pub(super) query: &'a ScanQuery,
    pub(super) limits: Limits,
    /// Context lines kept above and below each stored record (`-C`).
    pub(super) context: usize,
}

/// Why a scan cannot run.
#[derive(Debug)]
pub(super) enum ScanError {
    /// The root cannot be canonicalized.
    Root(std::io::Error),
    /// The thread pool cannot be built.
    Pool(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Root(e) => write!(f, "cannot resolve the search root: {e}"),
            ScanError::Pool(e) => write!(f, "cannot start the scan threads: {e}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// One stored line of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    /// 1-based line number.
    pub(super) line: u32,
    /// 1-based byte column of the match that gave the role.
    pub(super) column: u32,
    pub(super) role: Role,
    /// The enclosing declaration, `None` at the top level.
    pub(super) holder: Option<Holder>,
    /// For a `def`: the 1-based first and last line of the declaration.
    pub(super) def_lines: Option<(u32, u32)>,
    /// The line, clipped to [`LINE_WIDTH_CAP`] bytes, without a trailing `\r`.
    pub(super) text: String,
    /// Bytes clipped from the end of the line.
    pub(super) truncated_bytes: u32,
    /// How many of the query words the line holds (`1` for a pattern).
    pub(super) distinct_words: u32,
}

/// A line next to a stored record, kept for `-C`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContextLine {
    pub(super) line: u32,
    pub(super) text: String,
    pub(super) truncated_bytes: u32,
}

/// One matched file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileScan {
    pub(super) path: PathBuf,
    pub(super) prior: FilePrior,
    /// Matching lines in the file, exact.
    pub(super) lines: u32,
    /// Matching lines of each word (word view only; empty otherwise).
    pub(super) word_lines: Vec<u32>,
    /// `None` when the file has labels; the reason when it has none.
    pub(super) unlabelled: Option<Unlabelled>,
    /// Exact role counts over all matching lines. Empty after a counts-only
    /// scan.
    pub(super) roles: RoleCounts,
    /// Definition lines, up to [`Limits::max_def_sites_per_file`].
    pub(super) defs: Vec<Record>,
    /// Other stored lines, in line order.
    pub(super) records: Vec<Record>,
    /// Context lines of the stored records, in line order, no line a record.
    pub(super) context: Vec<ContextLine>,
}

impl FileScan {
    /// The text of `line` if the file stored it as a record, a definition or a
    /// context line.
    #[cfg(test)]
    pub(super) fn text_of(&self, line: u32) -> Option<(&str, u32)> {
        let record = self
            .defs
            .iter()
            .chain(&self.records)
            .find(|r| r.line == line)
            .map(|r| (r.text.as_str(), r.truncated_bytes));
        record.or_else(|| {
            self.context
                .iter()
                .find(|c| c.line == line)
                .map(|c| (c.text.as_str(), c.truncated_bytes))
        })
    }
}

/// What the scan read and parsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Cost {
    /// Files read in phase A: the searched files.
    pub(super) files_read: u64,
    pub(super) bytes_read: u64,
    /// Files a parse was started on in phase B.
    pub(super) files_parsed: u64,
    pub(super) bytes_parsed: u64,
}

/// What the parse budget did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ParseStats {
    /// A matched file was left unparsed because the files or bytes budget was
    /// spent.
    pub(super) budget_hit: bool,
    /// Matched files with no labels, for any reason.
    pub(super) unlabelled_files: u64,
    pub(super) by_reason: BTreeMap<Unlabelled, u64>,
}

/// The caps that cut lines from a result. The scan sets each flag where the cut
/// happens, so the output does not need to guess the cause from the totals.
/// Context lines have no flag: [`ScanOutput::context_hidden`] counts the lines
/// the context cap cut.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CapsHit {
    /// The stored-line quota left a matching line without a record.
    pub(super) stored_lines: bool,
    /// The per-file cap dropped a definition line.
    pub(super) file_defs: bool,
    /// The cap over all files dropped a definition line.
    pub(super) all_defs: bool,
}

impl CapsHit {
    fn merge(&mut self, other: CapsHit) {
        self.stored_lines |= other.stored_lines;
        self.file_defs |= other.file_defs;
        self.all_defs |= other.all_defs;
    }
}

/// The result of a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScanOutput {
    /// The matched files, in path order.
    pub(super) files: Vec<FileScan>,
    /// Files read and admitted.
    pub(super) searched_files: u64,
    pub(super) left_out: LeftOutCounts,
    pub(super) cost: Cost,
    pub(super) parse: ParseStats,
    /// The caps that cut lines. None after a counts-only scan.
    pub(super) caps: CapsHit,
    /// Context lines that were wanted and not kept because of the cap.
    pub(super) context_hidden: u64,
}

impl ScanOutput {
    /// Matching lines over all files.
    pub(super) fn total_lines(&self) -> u64 {
        self.files.iter().map(|f| u64::from(f.lines)).sum()
    }

    /// Lines kept as records or definitions.
    #[cfg(test)]
    pub(super) fn listed_lines(&self) -> u64 {
        self.files
            .iter()
            .map(|f| (f.defs.len() + f.records.len()) as u64)
            .sum()
    }
}

// ============================================================================
// Lines
// ============================================================================

/// The lines of `text`: `(1-based number, byte offset, line)`. Lines split on
/// `\n` only, a `\r` stays in the line, a final `\n` ends the last line and
/// does not start another, and an empty text has no line.
pub(super) fn numbered_lines(text: &str) -> impl Iterator<Item = (u32, usize, &str)> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let mut number = 0u32;
    std::iter::from_fn(move || {
        if pos >= bytes.len() {
            return None;
        }
        let end = memchr::memchr(b'\n', &bytes[pos..]).map_or(bytes.len(), |i| pos + i);
        let start = pos;
        pos = end + 1;
        number = number.saturating_add(1);
        Some((number, start, &text[start..end]))
    })
}

/// Clip `line` for storage: drop one trailing `\r`, then cut at `width` bytes
/// on a character boundary. Returns the text and the bytes cut.
pub(super) fn clip_line(line: &str, width: usize) -> (String, u32) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() <= width {
        return (line.to_owned(), 0);
    }
    let mut end = width;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let hidden = u32::try_from(line.len() - end).unwrap_or(u32::MAX);
    (line[..end].to_owned(), hidden)
}

fn u32_or_max(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

// ============================================================================
// Phase A
// ============================================================================

/// What phase A keeps of one matched file.
#[derive(Debug, Clone)]
struct Matched {
    /// Index into the candidate list.
    candidate: usize,
    len: usize,
    digest: u64,
    lines: u32,
    word_lines: Vec<u32>,
    keyword_line: bool,
    prior: FilePrior,
}

enum Outcome {
    Searched {
        bytes: usize,
        matched: Option<Matched>,
    },
    LeftOut(LeftOut),
    NotAFile,
}

/// The result of phase A: the counts, and what phase B needs to label.
pub(super) struct Counted<'a> {
    config: &'a ScanConfig<'a>,
    candidates: Vec<Candidate>,
    containment: Containment,
    pool: rayon::ThreadPool,
    matched: Vec<Matched>,
    searched_files: u64,
    bytes_read: u64,
    left_out: LeftOutCounts,
}

/// Run phase A over `listing`.
///
/// # Errors
///
/// [`ScanError`] when the root cannot be resolved or the pool cannot start.
pub(super) fn count<'a>(
    config: &'a ScanConfig<'a>,
    listing: Listing,
) -> Result<Counted<'a>, ScanError> {
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(pool_threads(available))
        .build()
        .map_err(|e| ScanError::Pool(e.to_string()))?;
    let Listing {
        candidates,
        left_out: listed_out,
    } = listing;
    let containment = Containment::new(config.root, &candidates, &pool).map_err(ScanError::Root)?;

    let outcomes: Vec<Outcome> = pool.install(|| {
        candidates
            .par_iter()
            .enumerate()
            .map(|(index, candidate)| phase_a_file(index, candidate, &containment, config.query))
            .collect()
    });

    let mut left_out = listed_out;
    let mut matched = Vec::new();
    let (mut searched_files, mut bytes_read) = (0u64, 0u64);
    for outcome in outcomes {
        match outcome {
            Outcome::Searched { bytes, matched: m } => {
                searched_files += 1;
                bytes_read += bytes as u64;
                matched.extend(m);
            }
            Outcome::LeftOut(reason) => left_out.add(reason),
            Outcome::NotAFile => {}
        }
    }
    Ok(Counted {
        config,
        candidates,
        containment,
        pool,
        matched,
        searched_files,
        bytes_read,
        left_out,
    })
}

fn phase_a_file(
    index: usize,
    candidate: &Candidate,
    containment: &Containment,
    query: &ScanQuery,
) -> Outcome {
    let text = match containment.read(candidate) {
        Read::Text(text) => text,
        Read::LeftOut(reason) => return Outcome::LeftOut(reason),
        Read::NotAFile => return Outcome::NotAFile,
    };
    let lang = Language::from_path(&candidate.rel);
    let found = match_file(&text, query, lang);
    let matched = (found.lines > 0).then(|| Matched {
        candidate: index,
        len: text.len(),
        digest: digest(&text),
        lines: found.lines,
        word_lines: found.word_lines,
        keyword_line: found.keyword_line,
        prior: file_prior(&candidate.rel, &text),
    });
    Outcome::Searched {
        bytes: text.len(),
        matched,
    }
}

fn digest(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(text.as_bytes());
    hasher.finish()
}

struct FileMatch {
    lines: u32,
    word_lines: Vec<u32>,
    keyword_line: bool,
}

/// Count the matching lines of `text`.
fn match_file(text: &str, query: &ScanQuery, lang: Option<Language>) -> FileMatch {
    let keywords = lang.map_or(&[][..], declaration_keywords);
    let mut found = FileMatch {
        lines: 0,
        word_lines: Vec::new(),
        keyword_line: false,
    };
    let note_keyword = |matcher: &Matcher, line: &str, found: &mut FileMatch| {
        if !found.keyword_line
            && let Some(first) = matcher.matches(line).next()
        {
            found.keyword_line = has_declaration_keyword(&line[..first.start], keywords);
        }
    };
    match &query.plan {
        QueryPlan::Pattern(matcher) => {
            if !matcher.text_may_match(text) {
                return found;
            }
            for (_, _, line) in numbered_lines(text) {
                if matcher.is_match(line) {
                    found.lines = found.lines.saturating_add(1);
                    note_keyword(matcher, line, &mut found);
                }
            }
        }
        QueryPlan::Words(words) => {
            let active: Vec<(usize, &Matcher)> = words
                .iter()
                .enumerate()
                .filter(|(_, m)| m.text_may_match(text))
                .collect();
            found.word_lines = vec![0; words.len()];
            if active.is_empty() {
                return found;
            }
            for (_, _, line) in numbered_lines(text) {
                let mut hit: Option<&Matcher> = None;
                for &(w, matcher) in &active {
                    if matcher.is_match(line) {
                        found.word_lines[w] = found.word_lines[w].saturating_add(1);
                        hit.get_or_insert(matcher);
                    }
                }
                if let Some(matcher) = hit {
                    found.lines = found.lines.saturating_add(1);
                    note_keyword(matcher, line, &mut found);
                }
            }
        }
    }
    found
}

// ============================================================================
// Between the phases: parse order, budget, quotas
// ============================================================================

/// What phase B does with one matched file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Parse it for labels.
    Parse,
    /// No grammar: every hit is `text`, no parse.
    Text,
    /// No labels, for this reason.
    Skip(Unlabelled),
}

/// How phase B picks the line records of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Select {
    /// The first lines of the file.
    First,
    /// The lines with the most distinct words, earliest first.
    BestWords,
}

struct Job {
    /// Index into `Counted::matched`.
    matched: usize,
    plan: Plan,
    record_quota: usize,
}

/// The matched files in parse order, with the plan and the record quota of
/// each. The quota counts every matching line of a file, definitions included:
/// phase A cannot tell them apart, and the definitions have caps of their own.
fn plan_jobs(counted: &Counted<'_>) -> (Vec<Job>, bool) {
    let limits = &counted.config.limits;
    let matched = &counted.matched;
    let path = |m: &Matched| counted.candidates[m.candidate].rel.as_path();

    let mut order: Vec<usize> = (0..matched.len()).collect();
    let select = match &counted.config.query.plan {
        QueryPlan::Pattern(_) => {
            order.sort_by(|&a, &b| {
                let (a, b) = (&matched[a], &matched[b]);
                (
                    a.prior,
                    !a.keyword_line,
                    std::cmp::Reverse(a.lines),
                    path(a),
                )
                    .cmp(&(
                        b.prior,
                        !b.keyword_line,
                        std::cmp::Reverse(b.lines),
                        path(b),
                    ))
            });
            Select::First
        }
        QueryPlan::Words(_) => {
            let inputs: Vec<WordInput<'_>> = matched
                .iter()
                .map(|m| WordInput {
                    path: path(m),
                    prior: m.prior,
                    counts: &m.word_lines,
                })
                .collect();
            order = rank_words(&inputs).into_iter().map(|r| r.index).collect();
            Select::BestWords
        }
    };

    let mut jobs = Vec::with_capacity(order.len());
    let (mut parsed_files, mut parsed_bytes) = (0usize, 0usize);
    let mut stored_left = limits.max_stored_lines;
    let mut budget_hit = false;
    for index in order {
        let m = &matched[index];
        let rel = path(m);
        let plan = match Language::from_path(rel) {
            None => Plan::Text,
            Some(_) if m.len > limits.per_file_parse_cap => Plan::Skip(Unlabelled::ParseCap),
            Some(_)
                if parsed_files >= limits.parse_files
                    || parsed_bytes.saturating_add(m.len) > limits.parse_bytes =>
            {
                budget_hit = true;
                Plan::Skip(Unlabelled::Budget)
            }
            Some(_) => {
                parsed_files += 1;
                parsed_bytes = parsed_bytes.saturating_add(m.len);
                Plan::Parse
            }
        };
        let wanted = match select {
            Select::First => m.lines as usize,
            Select::BestWords => (m.lines as usize).min(WORD_VIEW_BEST_LINES),
        };
        let record_quota = wanted.min(stored_left);
        stored_left -= record_quota;
        jobs.push(Job {
            matched: index,
            plan,
            record_quota,
        });
    }
    (jobs, budget_hit)
}

// ============================================================================
// Phase B
// ============================================================================

/// Counts phase A made, as a scan result with no labels and no records.
impl Counted<'_> {
    /// Finish a counts-only scan: no phase B, no roles.
    pub(super) fn into_counts(self) -> ScanOutput {
        let mut files: Vec<FileScan> = self
            .matched
            .iter()
            .map(|m| {
                file_scan(
                    &self.candidates[m.candidate],
                    m,
                    None,
                    RoleCounts::default(),
                )
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        ScanOutput {
            files,
            searched_files: self.searched_files,
            left_out: self.left_out,
            cost: Cost {
                files_read: self.searched_files,
                bytes_read: self.bytes_read,
                ..Cost::default()
            },
            parse: ParseStats::default(),
            caps: CapsHit::default(),
            context_hidden: 0,
        }
    }

    /// Run phase B over `jobs`: the results are in parse order.
    ///
    /// Without `-C` every file runs at once. With `-C` the files run in waves
    /// of at most [`MAX_SCAN_THREADS`], and after each wave the context budget
    /// is spent on the files of the wave in parse order. The budget goes to the
    /// lines each file needs, which are known only once it has its records, so
    /// a file that needs few lines leaves the rest to the files after it. A
    /// wave holds the text of its files, so that memory is at most
    /// [`MAX_SCAN_THREADS`] files.
    fn run_jobs(&self, jobs: &[Job]) -> Vec<Labelled> {
        let context = self.config.context;
        let wave = if context == 0 {
            jobs.len().max(1)
        } else {
            MAX_SCAN_THREADS
        };
        let mut context_left = self.config.limits.max_context_lines;
        let mut results = Vec::with_capacity(jobs.len());
        for jobs in jobs.chunks(wave) {
            let mut labelled: Vec<Labelled> = self
                .pool
                .install(|| jobs.par_iter().map(|job| phase_b_file(job, self)).collect());
            for result in &mut labelled {
                result.keep_context(context, &mut context_left);
            }
            results.append(&mut labelled);
        }
        results
    }

    /// Run phase B: label the matched files and keep the line records.
    pub(super) fn label(self) -> ScanOutput {
        let (jobs, budget_hit) = plan_jobs(&self);
        let results = self.run_jobs(&jobs);

        let mut parse = ParseStats {
            budget_hit,
            ..ParseStats::default()
        };
        let mut cost = Cost {
            files_read: self.searched_files,
            bytes_read: self.bytes_read,
            ..Cost::default()
        };
        let mut context_hidden = 0u64;
        let mut caps = CapsHit::default();
        let mut defs_kept = 0usize;
        let mut files = Vec::with_capacity(results.len());
        for mut result in results {
            // The results are in parse order, so the cap is the same on every run.
            let room = self.config.limits.max_def_sites.saturating_sub(defs_kept);
            if result.file.defs.len() > room {
                caps.all_defs = true;
                result.file.defs.truncate(room);
            }
            caps.merge(result.caps);
            defs_kept += result.file.defs.len();
            if let Some(bytes) = result.parsed_bytes {
                cost.files_parsed += 1;
                cost.bytes_parsed += bytes as u64;
            }
            if let Some(reason) = result.file.unlabelled {
                parse.unlabelled_files += 1;
                *parse.by_reason.entry(reason).or_insert(0) += 1;
            }
            context_hidden += result.context_hidden;
            files.push(result.file);
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        ScanOutput {
            files,
            searched_files: self.searched_files,
            left_out: self.left_out,
            cost,
            parse,
            caps,
            context_hidden,
        }
    }
}

fn file_scan(
    candidate: &Candidate,
    m: &Matched,
    unlabelled: Option<Unlabelled>,
    roles: RoleCounts,
) -> FileScan {
    FileScan {
        path: candidate.rel.clone(),
        prior: m.prior,
        lines: m.lines,
        word_lines: m.word_lines.clone(),
        unlabelled,
        roles,
        defs: Vec::new(),
        records: Vec::new(),
        context: Vec::new(),
    }
}

struct Labelled {
    file: FileScan,
    parsed_bytes: Option<usize>,
    /// The caps that cut lines of this file.
    caps: CapsHit,
    /// The text of the file, held until its context lines are chosen. `None`
    /// without `-C`, with no stored record, or for a file that changed.
    text: Option<String>,
    context_hidden: u64,
}

impl Labelled {
    /// Keep the context lines of the stored records, up to `left` lines, and
    /// take what was kept off `left`. The text is released.
    fn keep_context(&mut self, context: usize, left: &mut usize) {
        let Some(text) = self.text.take() else {
            return;
        };
        self.context_hidden = collect_context(&text, context, *left, &mut self.file);
        *left = left.saturating_sub(self.file.context.len());
    }
}

fn phase_b_file(job: &Job, counted: &Counted<'_>) -> Labelled {
    let m = &counted.matched[job.matched];
    let candidate = &counted.candidates[m.candidate];
    let config = counted.config;

    // The file must still be what phase A matched.
    let text = match counted.containment.read(candidate) {
        Read::Text(text) if text.len() == m.len && digest(&text) == m.digest => text,
        _ => return changed(candidate, m),
    };

    let lang = Language::from_path(&candidate.rel);
    let ext = candidate
        .rel
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (labelling, parsed_bytes) = match (job.plan, lang) {
        (Plan::Text, _) | (Plan::Parse, None) => (Labelling::Labelled(Labels::text()), None),
        (Plan::Skip(reason), _) => (Labelling::Unlabelled(reason), None),
        (Plan::Parse, Some(lang)) => {
            let labelling = match Labels::new(&text, lang, &ext) {
                Ok(labels) => Labelling::Labelled(labels),
                Err(reason) => Labelling::Unlabelled(reason),
            };
            (labelling, Some(text.len()))
        }
    };
    let unlabelled = match &labelling {
        Labelling::Unlabelled(reason) => Some(*reason),
        Labelling::Labelled(_) => None,
    };

    let mut file = file_scan(candidate, m, unlabelled, RoleCounts::default());
    let select = match config.query.plan {
        QueryPlan::Pattern(_) => Select::First,
        QueryPlan::Words(_) => Select::BestWords,
    };
    let quota = LineQuota {
        records: job.record_quota,
        defs: config.limits.max_def_sites_per_file,
    };
    let caps = collect_lines(&text, &labelling, config.query, select, quota, &mut file);
    let text = (config.context > 0 && !file.records.is_empty()).then_some(text);
    Labelled {
        file,
        parsed_bytes,
        caps,
        text,
        context_hidden: 0,
    }
}

/// A matched file that is gone or different at the second read: its lines are
/// counted from phase A and none is listed.
fn changed(candidate: &Candidate, m: &Matched) -> Labelled {
    let mut roles = RoleCounts::default();
    for _ in 0..m.lines {
        roles.add(Role::Unlabelled);
    }
    Labelled {
        file: file_scan(candidate, m, Some(Unlabelled::Changed), roles),
        parsed_bytes: None,
        caps: CapsHit::default(),
        text: None,
        context_hidden: 0,
    }
}

/// How many lines of one file phase B keeps.
#[derive(Debug, Clone, Copy)]
struct LineQuota {
    /// Line records of the roles other than `def`.
    records: usize,
    /// Definition lines.
    defs: usize,
}

/// Decide the role of every matching line of `text`, count the roles, and keep
/// the line records up to `quota` (definitions are kept apart). Returns the
/// caps that cut a line of this file.
fn collect_lines(
    text: &str,
    labelling: &Labelling,
    query: &ScanQuery,
    select: Select,
    quota: LineQuota,
    file: &mut FileScan,
) -> CapsHit {
    let mut caps = CapsHit::default();
    let mut other_lines = 0usize;
    for (number, start, line) in numbered_lines(text) {
        let (hit, distinct) = match &query.plan {
            QueryPlan::Pattern(matcher) => (
                labelling.classify_line(line, start, matcher.matches(line), query.identifier),
                1,
            ),
            QueryPlan::Words(words) => {
                let distinct = u32_or_max(words.iter().filter(|m| m.is_match(line)).count());
                (
                    labelling.classify_line(
                        line,
                        start,
                        words.iter().flat_map(|m| m.matches(line)),
                        query.identifier,
                    ),
                    distinct,
                )
            }
        };
        let Some(hit) = hit else {
            continue;
        };
        file.roles.add(hit.role);
        let is_def = hit.role == Role::Def;
        other_lines += usize::from(!is_def);
        let room = if is_def {
            let room = file.defs.len() < quota.defs;
            caps.file_defs |= !room;
            room
        } else {
            match select {
                Select::First => file.records.len() < quota.records,
                Select::BestWords => {
                    quota.records > 0
                        && (file.records.len() < quota.records
                            || file.records.iter().any(|r| r.distinct_words < distinct))
                }
            }
        };
        if !room {
            continue;
        }
        let (text, truncated_bytes) = clip_line(line, LINE_WIDTH_CAP);
        let record = Record {
            line: number,
            column: u32_or_max(hit.column.saturating_add(1)),
            role: hit.role,
            holder: hit.holder,
            def_lines: hit.def_lines,
            text,
            truncated_bytes,
            distinct_words: distinct,
        };
        if is_def {
            file.defs.push(record);
        } else if select == Select::BestWords {
            keep_best_words(&mut file.records, record, quota.records);
        } else {
            file.records.push(record);
        }
    }
    if select == Select::BestWords {
        file.records.sort_by_key(|r| r.line);
    }
    // The file wanted a record for each line of a role other than `def`, or
    // for the best few of them in the word view. It has fewer only when the
    // quota was smaller than that.
    let wanted = match select {
        Select::First => other_lines,
        Select::BestWords => other_lines.min(WORD_VIEW_BEST_LINES),
    };
    caps.stored_lines = file.records.len() < wanted;
    caps
}

/// Keep `record` among the best `quota` records of the word view: most distinct
/// words first, earliest line on a tie. A full list drops its worst record: the
/// fewest words, then the latest line.
fn keep_best_words(records: &mut Vec<Record>, record: Record, quota: usize) {
    if records.len() >= quota {
        let worst = records
            .iter()
            .enumerate()
            .min_by_key(|(_, r)| (r.distinct_words, std::cmp::Reverse(r.line)))
            .map(|(i, _)| i);
        if let Some(i) = worst {
            records.remove(i);
        }
    }
    records.push(record);
}

/// Keep the context lines of the stored records, up to `quota`. Returns the
/// number of wanted context lines that were not kept.
fn collect_context(text: &str, context: usize, quota: usize, file: &mut FileScan) -> u64 {
    if context == 0 || file.records.is_empty() {
        return 0;
    }
    let total_lines = numbered_lines(text).count();
    let last = u32_or_max(total_lines);
    let stored: std::collections::BTreeSet<u32> = file.records.iter().map(|r| r.line).collect();
    let ctx = u32_or_max(context);
    let mut wanted: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for &line in &stored {
        let from = line.saturating_sub(ctx).max(1);
        let to = line.saturating_add(ctx).min(last);
        wanted.extend((from..=to).filter(|n| !stored.contains(n)));
    }
    let needed = wanted.len();
    let keep: std::collections::BTreeSet<u32> = wanted.into_iter().take(quota).collect();
    file.context = numbered_lines(text)
        .filter(|(number, _, _)| keep.contains(number))
        .map(|(number, _, line)| {
            let (text, truncated_bytes) = clip_line(line, LINE_WIDTH_CAP);
            ContextLine {
                line: number,
                text,
                truncated_bytes,
            }
        })
        .collect();
    (needed - keep.len()) as u64
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
