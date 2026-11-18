//! The output of `skim search`: the text views, the `-l` and `-c` forms, and
//! the JSON envelope.
//!
//! Every function here turns a finished [`ScanOutput`] into strings. Nothing
//! reads a file or writes to a stream, so the caller owns the exit code, the
//! broken-pipe rule and the buffering.
//!
//! # Views
//!
//! - The **labelled view** (one pattern): the `def` lines, a clash note, then
//!   one section per role in tier order, an `other:` line for the roles the
//!   default view only counts, notes, and the `total:` line.
//! - The **word view** (several plain words): a `files:` line, then per file
//!   one line with the word counts and the best lines.
//!
//! # Escapes
//!
//! Text output never prints a control byte or a bidirectional format control
//! (U+202A to U+202E, U+2066 to U+2069) from a file name or a line. A path with
//! one of them is C-quoted (`"a\nb"`; a C1 control or a format control prints as
//! the octal escapes of its UTF-8 bytes, `"\302\233"`); so is a path that
//! starts with a quote, because it would otherwise look like the quoted form of
//! another path; a path that is not UTF-8 is quoted with octal escapes for
//! every byte above `0x7F`, as git does with `core.quotePath`. In line text,
//! C0 controls except TAB and DEL print as `\xHH`, and C1 controls and format
//! controls print as `\u{HH}`, so a hit cannot move the cursor, rewrite the
//! screen or reorder the text around it. Letters of a right-to-left script
//! print as they are. The JSON output keeps the serde escapes and marks a path
//! that is not UTF-8 with `"path_lossy": true`.
//!
//! # Bounds
//!
//! A bound that hides lines prints a note with exact counts: the stored-line
//! cap, the definition caps, the context cap, the files the parse budget left
//! unlabelled, and the files a `--limit` page leaves out. The scan records
//! which cap cut lines ([`CapsHit`]); the notes name the caps from that record
//! and never infer a cause from the totals.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{Map, Value, json};

use super::rank::{
    FALLBACK_MIN_FILES, FilePrior, LabelledInput, Ranked, WORD_VIEW_BEST_LINES,
    WORD_VIEW_DEFAULT_LIMIT, WordInput, files_with_all_words, rank_labelled, rank_words,
};
use super::roles::{Role, Unlabelled, WordDeclaration};
use super::scan::{CapsHit, FileScan, Limits, Record, ScanOutput};
use super::universe::LeftOut;
use crate::output::elision_marker_with_remedy;

/// A section above this many lines prints line numbers only, unless `--full`
/// or `-C` is given.
pub(super) const COMPACT_GROUP_LINES: usize = 30;

/// The remedy clause of the stderr marker for lines the stored-line cap or the
/// definition cap over all files left out: `-c` and `-l` still reach every file.
const FILES_REMEDY: &str = "use -c or -l for every file";

/// The remedy clause of the stderr marker for definitions the per-file cap left
/// out: `-c` and `-l` list no line, and a narrower pattern keeps the same
/// definitions of the same file, so the way to see them is to read the file.
const FILE_DEFS_REMEDY: &str = "read the file with skim FILE --mode=signatures";

/// The last remedy clause of every cap marker.
const NARROW_REMEDY: &str = "or narrow the pattern";

/// The remedy of the stderr marker for context lines the context cap left out.
const CONTEXT_REMEDY: &str = "use a smaller -C, or narrow the pattern";

/// Sections of the default labelled view, in output order.
const DEFAULT_SECTIONS: [Role; 3] = [Role::Call, Role::Ref, Role::Unlabelled];

/// Sections of the `--all` labelled view, in output order.
const ALL_SECTIONS: [Role; 8] = [
    Role::Call,
    Role::Ref,
    Role::Unlabelled,
    Role::Import,
    Role::StringLit,
    Role::Comment,
    Role::Text,
    Role::Partial,
];

/// Characters of a query that make it a regex when `-E` is given.
const REGEX_META: &str = ".*+?[](){}|^$\\";

// ============================================================================
// Inputs
// ============================================================================

/// How the query text is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MatchMode {
    /// The pattern is a literal string.
    Literal,
    /// A literal that must not sit inside a longer identifier (`-w`).
    Word,
    /// A regular expression (`-E`).
    Regex,
    /// Several plain words: the word view.
    Words,
}

impl MatchMode {
    /// The name in the JSON `match_mode` field.
    pub(super) fn key(self) -> &'static str {
        match self {
            MatchMode::Literal => "literal",
            MatchMode::Word => "word",
            MatchMode::Regex => "regex",
            MatchMode::Words => "words",
        }
    }
}

/// The query as the user gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueryInfo {
    /// The pattern, or the whole phrase of a word query.
    pub(super) text: String,
    /// The words of a word query, in order; empty for a pattern.
    pub(super) words: Vec<String>,
    pub(super) mode: MatchMode,
    pub(super) ignore_case: bool,
}

/// Which of the two views a query gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Labelled,
    Words,
}

impl View {
    fn key(self) -> &'static str {
        match self {
            View::Labelled => "labelled",
            View::Words => "words",
        }
    }
}

impl QueryInfo {
    fn view(&self) -> View {
        if self.mode == MatchMode::Words {
            View::Words
        } else {
            View::Labelled
        }
    }
}

/// What the flags ask of the layout.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Layout {
    /// `--all`: list every section, with no `other:` line.
    pub(super) all: bool,
    /// `--full`: print the text of every line, whatever the group size.
    pub(super) full: bool,
    /// `-C N`: context lines around each listed line. It implies `--full`.
    pub(super) context: usize,
    /// `--limit N`.
    pub(super) limit: Option<usize>,
    /// `--offset N`.
    pub(super) offset: usize,
    /// The bounds the scan ran with, for the notes that name them.
    pub(super) limits: Limits,
}

/// Everything the renderer needs.
pub(super) struct Report<'a> {
    pub(super) query: &'a QueryInfo,
    pub(super) scan: &'a ScanOutput,
    pub(super) layout: Layout,
    pub(super) duration_ms: u64,
}

/// The output form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    /// The labelled view or the word view.
    Text,
    /// One JSON object on one line.
    Json,
    /// `-l`: the paths, one per line.
    FilesWithMatches,
    /// `-c`: `path:count`, one per line.
    Counts,
}

/// The rendered streams. Each stderr entry is one whole line, without the
/// newline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Rendered {
    pub(super) stdout: String,
    pub(super) stderr: Vec<String>,
}

/// Render `report` in `format`. A query with no match prints nothing on stdout
/// in the text forms, a JSON envelope with `total_lines: 0` in JSON, and one
/// line on stderr in all forms.
pub(super) fn render(report: &Report<'_>, format: Format) -> Rendered {
    let no_match = report.scan.total_lines() == 0;
    let mut rendered = match (format, no_match) {
        (Format::Json, _) => Rendered {
            stdout: json_line(report),
            stderr: Vec::new(),
        },
        (_, true) => Rendered::default(),
        (Format::Text, false) => match report.query.view() {
            View::Labelled => labelled_text(report),
            View::Words => words_text(report),
        },
        (Format::FilesWithMatches, false) => files_with_matches(report),
        (Format::Counts, false) => counts(report),
    };
    if no_match {
        rendered
            .stderr
            .push(no_match_line(report.query, report.scan));
    }
    rendered
}

/// The JSON object of an error, on one line.
pub(super) fn json_error(message: &str) -> String {
    json!({ "error": message }).to_string()
}

/// The stderr line of a query with no match.
pub(super) fn no_match_line(query: &QueryInfo, scan: &ScanOutput) -> String {
    let mut line = format!(
        "skim search: no match for {:?} ({} files searched, {} left out)",
        query.text,
        scan.searched_files,
        scan.left_out.total()
    );
    if query.mode != MatchMode::Regex && query.text.chars().any(|c| REGEX_META.contains(c)) {
        line.push_str("; the pattern is a literal, use -E for a regex");
    }
    line
}

// ============================================================================
// Escapes
// ============================================================================

/// The bytes of a path.
fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Cow::Borrowed(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        Cow::Owned(path.to_string_lossy().into_owned().into_bytes())
    }
}

/// A bidirectional format control: an embedding, an override or an isolate
/// (U+202A to U+202E, U+2066 to U+2069). It is not a control character, so
/// `char::is_control` misses it, yet it reorders the text around it on a
/// terminal and makes a name or a line read as another one. Letters of a
/// right-to-left script are not format controls and print as they are.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A character that text output never prints raw: a control character or a
/// bidirectional format control.
fn is_hidden(c: char) -> bool {
    c.is_control() || is_bidi_control(c)
}

/// The path as text output prints it: as is, or C-quoted when it holds a
/// control character or a bidirectional format control, starts with a quote,
/// or is not UTF-8.
///
/// A reader must not take one name for the quoted form of another. The quoted
/// form of every path starts with `"`, so a name that prints as is can equal a
/// quoted form only when it starts with `"` too, and such a name is quoted.
/// The quoting also has an inverse: it escapes the backslash and the quote, so
/// each other backslash in the output starts an escape (`\n`, `\t`, `\r` or
/// `\ooo`) that stands for one byte, and reading the output back gives the
/// original bytes. Two different paths therefore never print as one name.
pub(super) fn quote_path(path: &Path) -> String {
    let bytes = path_bytes(path);
    match std::str::from_utf8(&bytes) {
        Ok(text) if !text.starts_with('"') && !text.chars().any(is_hidden) => text.to_owned(),
        Ok(text) => {
            let mut out = String::with_capacity(text.len() + 2);
            out.push('"');
            for c in text.chars() {
                match c {
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    // A C0 control, a C1 control or a bidirectional format
                    // control: the octal escape of each UTF-8 byte.
                    c if is_hidden(c) => {
                        let mut buf = [0u8; 4];
                        for b in c.encode_utf8(&mut buf).bytes() {
                            out.push_str(&format!("\\{b:03o}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Err(_) => {
            let mut out = String::with_capacity(bytes.len() + 2);
            out.push('"');
            for &b in bytes.iter() {
                match b {
                    b'\n' => out.push_str("\\n"),
                    b'\t' => out.push_str("\\t"),
                    b'\r' => out.push_str("\\r"),
                    b'"' => out.push_str("\\\""),
                    b'\\' => out.push_str("\\\\"),
                    0x20..=0x7E => out.push(char::from(b)),
                    _ => out.push_str(&format!("\\{b:03o}")),
                }
            }
            out.push('"');
            out
        }
    }
}

/// `text` with every control character and bidirectional format control made
/// visible: C0 controls except TAB, and DEL, as `\xHH`; C1 controls and
/// bidirectional format controls as `\u{HH}`.
pub(super) fn escape_text(text: &str) -> Cow<'_, str> {
    let hidden = |c: char| {
        (c.is_ascii_control() && c != '\t')
            || ('\u{80}'..='\u{9f}').contains(&c)
            || is_bidi_control(c)
    };
    if !text.chars().any(hidden) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_ascii_control() && c != '\t' {
            out.push_str(&format!("\\x{:02X}", c as u32));
        } else if ('\u{80}'..='\u{9f}').contains(&c) || is_bidi_control(c) {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// The text of a stored line as output prints it. A match line drops its
/// leading blanks; a context line keeps them. A clipped line ends with the
/// number of bytes cut.
fn shown_text(text: &str, truncated_bytes: u32, trim: bool) -> String {
    let body = if trim { text.trim_start() } else { text };
    let mut shown = escape_text(body).into_owned();
    if truncated_bytes > 0 {
        shown.push_str(&format!("…[+{truncated_bytes} bytes]"));
    }
    shown
}

// ============================================================================
// Wording
// ============================================================================

/// `1 file`, `2 files`.
fn plural(n: u64, one: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// `2 lines in 2 files`.
fn lines_in_files(lines: u64, files: u64) -> String {
    format!("{} in {}", plural(lines, "line"), plural(files, "file"))
}

/// `364 files searched, 0 left out (3 binary, 1 not_utf8)`.
fn searched_clause(scan: &ScanOutput) -> String {
    let left_out = scan.left_out.total();
    let mut clause = format!(
        "{} files searched, {left_out} left out",
        scan.searched_files
    );
    if left_out > 0 {
        let parts: Vec<String> = scan
            .left_out
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(reason, n)| format!("{n} {}", reason.key()))
            .collect();
        clause.push_str(&format!(" ({})", parts.join(", ")));
    }
    clause
}

/// `32 MiB` for a whole number of MiB, bytes otherwise.
fn bytes_label(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// Why matched files have no labels, for the note.
fn unlabelled_reason(reason: Unlabelled, limits: &Limits) -> String {
    match reason {
        Unlabelled::Budget => format!(
            "parse budget: {} or {}",
            plural(limits.parse_files as u64, "file"),
            bytes_label(limits.parse_bytes)
        ),
        Unlabelled::ParseCap => format!(
            "above the per-file parse cap of {}",
            bytes_label(limits.per_file_parse_cap)
        ),
        Unlabelled::ParseFailed => "the parser failed".to_owned(),
        Unlabelled::Changed => "changed during the search".to_owned(),
        Unlabelled::WalkCap => "declaration walk cap".to_owned(),
    }
}

// ============================================================================
// Paging and notes
// ============================================================================

/// `--limit` and `--offset` over a list of files in rank order.
#[derive(Debug, Clone, Copy)]
struct Page {
    offset: usize,
    limit: Option<usize>,
}

impl Page {
    fn of(layout: &Layout, view: View) -> Self {
        Page {
            offset: layout.offset,
            limit: match (layout.limit, view) {
                (Some(limit), _) => Some(limit),
                (None, View::Words) => Some(WORD_VIEW_DEFAULT_LIMIT),
                (None, View::Labelled) => None,
            },
        }
    }

    /// The items of the page, and how many items follow it.
    fn take<'a, T>(&self, items: &'a [T]) -> (&'a [T], usize) {
        let start = self.offset.min(items.len());
        let end = self.limit.map_or(items.len(), |limit| {
            start.saturating_add(limit).min(items.len())
        });
        (&items[start..end], items.len() - end)
    }
}

/// Lines a view wanted to list and could not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Shortfall {
    /// Lines the view lists when nothing is cut.
    wanted: u64,
    /// Lines it wanted and has no text for.
    lines: u64,
    /// Files that lack at least one such line.
    files: u64,
}

impl Shortfall {
    fn add(&mut self, wanted: u64, shown: u64) {
        self.wanted += wanted;
        if wanted > shown {
            self.lines += wanted - shown;
            self.files += 1;
        }
    }
}

/// The matched files that hold the line records of `roles`, summed.
fn labelled_shortfall(files: &[FileScan], roles: &[Role]) -> Shortfall {
    let mut shortfall = Shortfall::default();
    for file in files {
        let wanted: u64 = roles.iter().map(|r| u64::from(file.roles.get(*r))).sum();
        let shown = file
            .defs
            .iter()
            .chain(&file.records)
            .filter(|r| roles.contains(&r.role))
            .count();
        shortfall.add(wanted, shown as u64);
    }
    shortfall
}

/// The caps that cut stored lines, from the record the scan made where each
/// cut happened: each as the name and the value the notes show.
fn caps_reached(caps: &CapsHit, limits: &Limits) -> Vec<(&'static str, usize)> {
    let mut reached = Vec::new();
    if caps.stored_lines {
        reached.push(("stored-line cap", limits.max_stored_lines));
    }
    if caps.file_defs {
        reached.push(("per-file definition cap", limits.max_def_sites_per_file));
    }
    if caps.all_defs {
        reached.push(("definition cap", limits.max_def_sites));
    }
    reached
}

/// The remedy of the stderr marker for the stored lines the caps left out: one
/// clause for each way out that the reached caps leave open.
fn cap_remedy(caps: &CapsHit) -> String {
    let mut clauses = Vec::new();
    if caps.stored_lines || caps.all_defs {
        clauses.push(FILES_REMEDY);
    }
    if caps.file_defs {
        clauses.push(FILE_DEFS_REMEDY);
    }
    clauses.push(NARROW_REMEDY);
    clauses.join(", ")
}

/// The notes of a result, without the `note: ` prefix: the files the parse
/// budget left unlabelled, the lines the stored-line cap and the definition
/// caps left out, and the context lines the context cap left out.
fn notices(
    report: &Report<'_>,
    shortfall: Shortfall,
    called: Option<&CalledDefinitions<'_>>,
) -> Vec<String> {
    let scan = report.scan;
    let limits = &report.layout.limits;
    let mut notes = Vec::new();
    if let Some(called) = called.filter(|c| c.hidden > 0) {
        notes.push(format!(
            "not listed: {} more called definitions (display cap {}; narrow the pattern to see others)",
            called.hidden, limits.max_called_shown
        ));
    }

    let unlabelled = scan.parse.unlabelled_files;
    if unlabelled > 0 {
        let reasons: Vec<(Unlabelled, u64)> =
            scan.parse.by_reason.iter().map(|(r, n)| (*r, *n)).collect();
        let detail = match reasons.as_slice() {
            [(reason, _)] => unlabelled_reason(*reason, limits),
            many => many
                .iter()
                .map(|(r, n)| format!("{n} {}", unlabelled_reason(*r, limits)))
                .collect::<Vec<_>>()
                .join(", "),
        };
        let verb = if unlabelled == 1 {
            "file has"
        } else {
            "files have"
        };
        notes.push(format!(
            "{unlabelled} matched {verb} no labels ({detail}); \
             their lines are under \"unlabelled\""
        ));
    }

    if shortfall.lines > 0 {
        let verb = if shortfall.lines == 1 { "is" } else { "are" };
        let caps = caps_reached(&scan.caps, limits);
        let cause = if caps.is_empty() {
            String::new()
        } else {
            let named: Vec<String> = caps
                .iter()
                .map(|(name, value)| format!("{name} {value}"))
                .collect();
            format!(" ({})", named.join(", "))
        };
        notes.push(format!(
            "{} {verb} counted, not listed{cause}",
            lines_in_files(shortfall.lines, shortfall.files)
        ));
    }

    if scan.context_hidden > 0 {
        notes.push(format!(
            "{} are not shown (context cap {})",
            plural(scan.context_hidden, "context line"),
            limits.max_context_lines
        ));
    }
    if let Some(called) = called {
        name_link_notices(report, called, &mut notes);
    }
    notes
}

/// Name-link cuts use only the scan's counters and cap flags. They do not
/// describe lost matching lines. Both output forms use these same notes.
fn name_link_notices(report: &Report<'_>, called: &CalledDefinitions<'_>, notes: &mut Vec<String>) {
    let scan = report.scan;
    let limits = &report.layout.limits;
    if scan.caps.called_names {
        let dropped: u64 = scan.files.iter().map(|f| f.links.called_dropped).sum();
        notes.push(format!(
            "{dropped} called names are not stored (per-file called-name cap {}); \
             narrow the pattern or read the source files with skim FILE --mode=full",
            limits.max_called_names_per_file
        ));
    }
    if scan.caps.file_word_defs || scan.caps.all_word_defs {
        let dropped: u64 = scan.files.iter().map(|f| f.links.defs_dropped).sum();
        let mut causes = Vec::new();
        if scan.caps.file_word_defs {
            causes.push(format!(
                "per-file word-definition cap {}",
                limits.max_word_defs_per_file
            ));
        }
        if scan.caps.all_word_defs {
            causes.push(format!("word-definition cap {}", limits.max_word_defs));
        }
        notes.push(format!(
            "{dropped} callable definitions are not stored ({}); \
             narrow the pattern or read the files with skim FILE --mode=signatures",
            causes.join(", ")
        ));
    }
    if scan.caps.name_bytes {
        let dropped: u64 = scan.files.iter().map(|f| f.links.long_names).sum();
        notes.push(format!(
            "{dropped} names are not stored (name-byte cap {}); \
             read the files with skim FILE --mode=full",
            limits.max_name_bytes
        ));
    }
    if scan.parse.unlabelled_files > 0 {
        notes.push(format!(
            "{} called names have no definition in the parsed files; \
             {} matched files have no labels; narrow the pattern to label more files",
            called.unmatched, scan.parse.unlabelled_files
        ));
    }
}

/// The stderr marker for the lines the stored-line cap and the definition caps
/// left out, if they left any out.
fn cap_marker(report: &Report<'_>, shortfall: Shortfall) -> Option<String> {
    let caps = &report.scan.caps;
    if shortfall.lines == 0 || caps_reached(caps, &report.layout.limits).is_empty() {
        return None;
    }
    let wanted = usize::try_from(shortfall.wanted).unwrap_or(usize::MAX);
    let left = usize::try_from(shortfall.lines).unwrap_or(usize::MAX);
    elision_marker_with_remedy(
        wanted.saturating_sub(left),
        wanted,
        "lines",
        &cap_remedy(caps),
    )
}

/// The stderr marker for the context lines the context cap left out, if it
/// left any out. Its counts are exact: the lines shown and the lines wanted.
fn context_marker(scan: &ScanOutput) -> Option<String> {
    let shown: u64 = scan.files.iter().map(|f| f.context.len() as u64).sum();
    let wanted = shown.saturating_add(scan.context_hidden);
    elision_marker_with_remedy(
        usize::try_from(shown).unwrap_or(usize::MAX),
        usize::try_from(wanted).unwrap_or(usize::MAX),
        "context lines",
        CONTEXT_REMEDY,
    )
}

/// The stderr markers of a text result, one line each.
fn markers(report: &Report<'_>, shortfall: Shortfall) -> Vec<String> {
    cap_marker(report, shortfall)
        .into_iter()
        .chain(context_marker(report.scan))
        .collect()
}

/// The listing so far, one line at a time.
#[derive(Default)]
struct Text(String);

impl Text {
    fn line(&mut self, line: &str) {
        self.0.push_str(line);
        self.0.push('\n');
    }
}

// ============================================================================
// Ranking
// ============================================================================

fn word_inputs(files: &[FileScan]) -> Vec<WordInput<'_>> {
    files
        .iter()
        .map(|f| WordInput {
            path: &f.path,
            prior: f.prior,
            counts: &f.word_lines,
            name_hit: f.name_hit,
        })
        .collect()
}

/// Every matched file in rank order, for the view of the query.
fn ranked_files(report: &Report<'_>) -> Vec<Ranked> {
    let files = &report.scan.files;
    match report.query.view() {
        View::Labelled => {
            let inputs: Vec<LabelledInput<'_>> = files
                .iter()
                .map(|f| LabelledInput {
                    path: &f.path,
                    prior: f.prior,
                    lines: f.lines,
                    best: f.roles.best().unwrap_or(Role::Unlabelled),
                })
                .collect();
            rank_labelled(&inputs)
        }
        View::Words => rank_words(&word_inputs(files)),
    }
}

/// How many files hold all the words of a word query.
fn all_words_count(report: &Report<'_>) -> usize {
    files_with_all_words(&word_inputs(&report.scan.files), report.query.words.len())
}

/// The files a view pages through: for a word query that is the files holding
/// all the words, or every matched file when fewer than
/// [`FALLBACK_MIN_FILES`] hold them all.
fn candidates<'r>(report: &Report<'_>, ranked: &'r [Ranked]) -> &'r [Ranked] {
    match report.query.view() {
        View::Labelled => ranked,
        View::Words => {
            let all = all_words_count(report);
            if all >= FALLBACK_MIN_FILES {
                &ranked[..all.min(ranked.len())]
            } else {
                ranked
            }
        }
    }
}

/// The lines the word view lists for a file: the lines with the most distinct
/// words, the earliest first, at most [`WORD_VIEW_BEST_LINES`], in line order.
fn best_word_lines(file: &FileScan) -> Vec<&Record> {
    let mut lines: Vec<&Record> = file.defs.iter().chain(&file.records).collect();
    lines.sort_by_key(|r| (std::cmp::Reverse(r.distinct_words), r.line));
    lines.truncate(WORD_VIEW_BEST_LINES);
    lines.sort_by_key(|r| r.line);
    lines
}

/// A name-only pointer from the first listed caller to an already parsed site.
struct CalledDefinition<'a> {
    site: &'a WordDeclaration,
    path: &'a Path,
    caller: &'a Path,
    caller_line: u32,
    caller_order: usize,
}

#[derive(Default)]
struct CalledDefinitions<'a> {
    sites: Vec<CalledDefinition<'a>>,
    hidden: usize,
    unmatched: usize,
}

/// Join exact names from stored evidence only. Caller priority is page rank,
/// then first call line. The map is never an output iterator, so its seed
/// cannot change order. Each declaration site is visited once. The scan bounds
/// the input; the display cap cuts only the sorted pointers, not file rows.
/// A definition in the page still satisfies a name but needs no extra pointer.
fn called_definitions<'a>(report: &Report<'a>, page: &[Ranked]) -> CalledDefinitions<'a> {
    if report.query.view() != View::Words {
        return CalledDefinitions::default();
    }
    let mut names = HashMap::new();
    let mut listed: Vec<usize> = page.iter().map(|r| r.index).collect();
    listed.sort_unstable();
    for r in page {
        let file = &report.scan.files[r.index];
        if file.prior != FilePrior::Source {
            continue;
        }
        let mut called: Vec<_> = file.links.called.iter().collect();
        called.sort_by_key(|c| c.line);
        for call in called {
            let order = names.len();
            names.entry(call.name.as_str()).or_insert((
                order,
                file.path.as_path(),
                call.line,
                false,
            ));
        }
    }
    let mut sites = Vec::new();
    for (index, file) in report.scan.files.iter().enumerate() {
        for site in &file.links.defs {
            if let Some((order, caller, line, found)) = names.get_mut(site.name.as_str()) {
                *found = true;
                if listed.binary_search(&index).is_ok() {
                    continue;
                }
                sites.push(CalledDefinition {
                    site,
                    path: &file.path,
                    caller,
                    caller_line: *line,
                    caller_order: *order,
                });
            }
        }
    }
    sites.sort_by(|a, b| {
        (a.caller_order, a.path, a.site.line).cmp(&(b.caller_order, b.path, b.site.line))
    });
    let hidden = sites
        .len()
        .saturating_sub(report.layout.limits.max_called_shown);
    sites.truncate(report.layout.limits.max_called_shown);
    CalledDefinitions {
        sites,
        hidden,
        unmatched: names.values().filter(|(_, _, _, found)| !found).count(),
    }
}

// ============================================================================
// Text lines
// ============================================================================

/// `[holder] ` for a line with a holder, and `[top] ` for a code line without
/// one. Roles that never sit in a declaration get no prefix.
fn holder_prefix(record: &Record) -> String {
    match &record.holder {
        Some(holder) => format!("[{}] ", escape_text(&holder.name)),
        None if matches!(
            record.role,
            Role::Def | Role::Call | Role::Ref | Role::Comment | Role::StringLit
        ) =>
        {
            "[top] ".to_owned()
        }
        None => String::new(),
    }
}

/// The exact definition count covers all files, not just the displayed page.
fn definition_count(scan: &ScanOutput) -> u64 {
    scan.files
        .iter()
        .map(|file| u64::from(file.roles.get(Role::Def)))
        .sum()
}

/// Only call records expose an assignment, under the shared output gate.
/// The scan keeps the evidence; a view with fewer than two definitions needs
/// no split, and the word view never shows one.
fn call_definition(record: &Record, assignments: bool) -> Option<u32> {
    if assignments && record.role == Role::Call {
        record.call_def
    } else {
        None
    }
}

/// `path:line: [holder] (def N) text`; omit `(def N)` without an assignment.
fn match_line(quoted_path: &str, record: &Record, assignments: bool) -> String {
    let definition = call_definition(record, assignments)
        .map(|line| format!("(def {line}) "))
        .unwrap_or_default();
    format!(
        "{quoted_path}:{}: {}{definition}{}",
        record.line,
        holder_prefix(record),
        shown_text(&record.text, record.truncated_bytes, true)
    )
}

/// `def path:first-last: text`, with `(name at LINE)` when the name is not on
/// the first line of the declaration.
fn def_line(file: &FileScan, record: &Record) -> String {
    let (first, last) = record.def_lines.unwrap_or((record.line, record.line));
    let at = if first == record.line {
        String::new()
    } else {
        format!(" (name at {})", record.line)
    };
    format!(
        "def {}:{first}-{last}{at}: {}",
        quote_path(&file.path),
        shown_text(&record.text, record.truncated_bytes, true)
    )
}

/// The stored lines of a file by number: definitions, records and context.
fn line_index(file: &FileScan) -> BTreeMap<u32, (&str, u32)> {
    let mut index = BTreeMap::new();
    for record in file.defs.iter().chain(&file.records) {
        index.insert(record.line, (record.text.as_str(), record.truncated_bytes));
    }
    for line in &file.context {
        index.insert(line.line, (line.text.as_str(), line.truncated_bytes));
    }
    index
}

// ============================================================================
// Labelled view
// ============================================================================

fn section_name(role: Role) -> &'static str {
    match role {
        Role::Def => "def",
        Role::Call => "calls",
        Role::Ref => "refs",
        Role::Unlabelled => "unlabelled",
        Role::Import => "imports",
        Role::StringLit => "strings",
        Role::Comment => "comments",
        Role::Text => "text",
        Role::Partial => "partial",
    }
}

/// Split assigned and other call lines without a repeated line number.
/// One file has at most one assignment target. Wholly unsplit groups keep the
/// existing `path:10,22,31` form; split groups use `path (def N): 10,22` and
/// `path: 31`, with either empty group omitted.
fn compact_line(file: &FileScan, role: Role, assignments: bool) -> Option<String> {
    let mut assigned = Vec::new();
    let mut other = Vec::new();
    let mut target = None;
    for record in file.records.iter().filter(|r| r.role == role) {
        if let Some(line) = call_definition(record, assignments) {
            target = Some(line);
            assigned.push(record.line.to_string());
        } else {
            other.push(record.line.to_string());
        }
    }
    let path = quote_path(&file.path);
    let mut lines = Vec::new();
    if let Some(line) = target {
        lines.push(format!("{path} (def {line}): {}", assigned.join(",")));
    }
    if !other.is_empty() {
        let space = if target.is_some() { " " } else { "" };
        lines.push(format!("{path}:{space}{}", other.join(",")));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The lines of one role in one file with `context` lines around each, as
/// blocks. A block that touches the one before it joins it; any other block
/// follows a `--` line.
fn context_blocks(
    out: &mut Text,
    file: &FileScan,
    role: Role,
    context: usize,
    first: &mut bool,
    assignments: bool,
) {
    let matches: BTreeMap<u32, &Record> = file
        .records
        .iter()
        .filter(|r| r.role == role)
        .map(|r| (r.line, r))
        .collect();
    if matches.is_empty() {
        return;
    }
    let lookup = line_index(file);
    let last_known = lookup.keys().next_back().copied().unwrap_or(0);
    let reach = u32::try_from(context).unwrap_or(u32::MAX);

    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &line in matches.keys() {
        let from = line.saturating_sub(reach).max(1);
        let to = line.saturating_add(reach).min(last_known);
        match ranges.last_mut() {
            Some(range) if from <= range.1.saturating_add(1) => range.1 = range.1.max(to),
            _ => ranges.push((from, to)),
        }
    }

    let path = quote_path(&file.path);
    for (from, to) in ranges {
        if !*first {
            out.line("--");
        }
        *first = false;
        for number in from..=to {
            if let Some(record) = matches.get(&number) {
                out.line(&match_line(&path, record, assignments));
            } else if let Some((text, truncated)) = lookup.get(&number) {
                out.line(&format!(
                    "{path}-{number}-{}",
                    shown_text(text, *truncated, false)
                ));
            }
        }
    }
}

/// One section of the labelled view.
fn section(out: &mut Text, report: &Report<'_>, ranked: &[Ranked], role: Role, assignments: bool) {
    let files = &report.scan.files;
    let in_section: Vec<&Ranked> = ranked
        .iter()
        .filter(|r| files[r.index].roles.get(role) > 0)
        .collect();
    let lines: u64 = in_section
        .iter()
        .map(|r| u64::from(files[r.index].roles.get(role)))
        .sum();
    if lines == 0 {
        return;
    }

    let layout = &report.layout;
    let full = layout.full || layout.context > 0;
    let compact = !full && lines > COMPACT_GROUP_LINES as u64;
    let mut header = format!(
        "{}: {}",
        section_name(role),
        lines_in_files(lines, in_section.len() as u64)
    );
    if compact {
        header.push_str(" (line numbers only; --full adds the text)");
    }
    out.line(&header);

    let (page, hidden) = Page::of(layout, View::Labelled).take(&in_section);
    let mut first_block = true;
    for ranked_file in page {
        let file = &files[ranked_file.index];
        if compact {
            if let Some(line) = compact_line(file, role, assignments) {
                out.line(&line);
            }
        } else if layout.context > 0 {
            context_blocks(
                out,
                file,
                role,
                layout.context,
                &mut first_block,
                assignments,
            );
        } else {
            let path = quote_path(&file.path);
            for record in file.records.iter().filter(|r| r.role == role) {
                out.line(&match_line(&path, record, assignments));
            }
        }
    }
    if hidden > 0 {
        out.line(&not_listed(hidden, section_name(role)));
    }
}

fn not_listed(hidden: usize, what: &str) -> String {
    format!(
        "not listed: {} in {what} (--limit N lists more)",
        plural(hidden as u64, "more file")
    )
}

/// The `other:` line: the roles the default view counts and does not list.
fn other_line(scan: &ScanOutput) -> Option<String> {
    let parts: Vec<String> = Role::ALL
        .into_iter()
        .filter(|r| *r != Role::Def && !DEFAULT_SECTIONS.contains(r))
        .filter_map(|role| {
            let count: u64 = scan
                .files
                .iter()
                .map(|f| u64::from(f.roles.get(role)))
                .sum();
            (count > 0).then(|| format!("{count} {}", role.key()))
        })
        .collect();
    (!parts.is_empty()).then(|| format!("other: {} lines (--all lists them)", parts.join(", ")))
}

/// The note about a name with several definitions. Counts measure call lines,
/// not calls or stored records, so a stored-line cap cannot change the split.
fn clash_note(
    query: &QueryInfo,
    count: u64,
    unlabelled_files: u64,
    assigned: u64,
    calls: u64,
) -> String {
    let name = escape_text(&query.text);
    let mut note = if unlabelled_files > 0 {
        format!(
            "note: at least {count} definitions of {name} ({} no labels)",
            if unlabelled_files == 1 {
                "1 matched file has".to_owned()
            } else {
                format!("{unlabelled_files} matched files have")
            }
        )
    } else {
        format!("note: {count} definitions of {name}")
    };
    if assigned == 0 {
        note.push_str("; calls are not split by definition");
    } else {
        note.push_str(&format!(
            "; call lines assigned to the one definition in their file: {assigned}; \
             call lines not split by definition: {}",
            calls.saturating_sub(assigned)
        ));
    }
    note
}

fn labelled_text(report: &Report<'_>) -> Rendered {
    let scan = report.scan;
    let layout = &report.layout;
    let ranked = ranked_files(report);
    let mut out = Text::default();

    // The definitions.
    let def_files: Vec<&Ranked> = ranked
        .iter()
        .filter(|r| scan.files[r.index].roles.get(Role::Def) > 0)
        .collect();
    let (page, hidden) = Page::of(layout, View::Labelled).take(&def_files);
    for ranked_file in page {
        let file = &scan.files[ranked_file.index];
        for record in &file.defs {
            out.line(&def_line(file, record));
        }
    }
    if hidden > 0 {
        out.line(&not_listed(hidden, "def"));
    }

    // The clash note.
    let def_count = definition_count(scan);
    if def_count == 0 {
        out.line("def: none in labelled files");
    } else if def_count >= 2 {
        let (assigned, calls) = scan.files.iter().fold((0, 0), |(assigned, calls), file| {
            (
                assigned + u64::from(file.assigned_calls),
                calls + u64::from(file.roles.get(Role::Call)),
            )
        });
        out.line(&clash_note(
            report.query,
            def_count,
            scan.parse.unlabelled_files,
            assigned,
            calls,
        ));
    }

    // The sections.
    let sections: &[Role] = if layout.all {
        &ALL_SECTIONS
    } else {
        &DEFAULT_SECTIONS
    };
    for role in sections {
        section(&mut out, report, &ranked, *role, def_count >= 2);
    }
    if !layout.all
        && let Some(line) = other_line(scan)
    {
        out.line(&line);
    }

    // The notes and the total.
    let mut shown_roles = vec![Role::Def];
    shown_roles.extend_from_slice(sections);
    let shortfall = labelled_shortfall(&scan.files, &shown_roles);
    for note in notices(report, shortfall, None) {
        out.line(&format!("note: {note}"));
    }
    out.line(&format!(
        "total: {}, all listed or counted; {}",
        lines_in_files(scan.total_lines(), scan.files.len() as u64),
        searched_clause(scan)
    ));

    Rendered {
        stdout: out.0,
        stderr: markers(report, shortfall),
    }
}

// ============================================================================
// Word view
// ============================================================================

/// `temporal=12 staleness=30`: the words the file holds, with their line counts.
fn word_counts(words: &[String], file: &FileScan) -> String {
    words
        .iter()
        .zip(&file.word_lines)
        .filter(|(_, count)| **count > 0)
        .map(|(word, count)| format!("{}={count}", escape_text(word)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn words_text(report: &Report<'_>) -> Rendered {
    let scan = report.scan;
    let words_total = report.query.words.len();
    let ranked = ranked_files(report);
    let all = all_words_count(report);
    let fallback = all < FALLBACK_MIN_FILES;
    let list = candidates(report, &ranked);
    let (page, hidden) = Page::of(&report.layout, View::Words).take(list);
    let mut out = Text::default();

    if fallback {
        let holds = match all {
            0 => "no file holds".to_owned(),
            1 => "only 1 file holds".to_owned(),
            n => format!("only {n} files hold"),
        };
        out.line(&format!(
            "note: {holds} all {words_total} words; files with fewer words follow \
             (most words first)"
        ));
        let some = scan.files.len() - all;
        out.line(&format!(
            "files: {} all {words_total} words, {some} hold some; {} listed",
            match all {
                1 => "1 holds".to_owned(),
                n => format!("{n} hold"),
            },
            page.len()
        ));
    } else {
        out.line(&format!(
            "files: {all} hold all {words_total} words; {} listed",
            page.len()
        ));
    }

    let mut shortfall = Shortfall::default();
    for ranked_file in page {
        let file = &scan.files[ranked_file.index];
        let path = quote_path(&file.path);
        out.line(&format!(
            "{path}  {}",
            word_counts(&report.query.words, file)
        ));
        let best = best_word_lines(file);
        shortfall.add(
            u64::from(file.lines).min(WORD_VIEW_BEST_LINES as u64),
            best.len() as u64,
        );
        for record in best {
            out.line(&match_line(&path, record, false));
        }
    }
    if hidden > 0 {
        let verb = if hidden == 1 { "holds" } else { "hold" };
        out.line(&format!(
            "not listed: {} {verb} the words (--limit N lists more)",
            plural(hidden as u64, "more file")
        ));
    }

    let called = called_definitions(report, page);
    if !called.sites.is_empty() {
        out.line(&format!(
            "called: {} definitions that the listed files call hold a query word (by name)",
            called.sites.len()
        ));
        for pointer in &called.sites {
            let site = pointer.site;
            out.line(&format!(
                "def {}:{}-{}: {}, called from {}:{}",
                quote_path(pointer.path),
                site.first_line,
                site.end_line,
                escape_text(&site.name),
                quote_path(pointer.caller),
                pointer.caller_line
            ));
        }
    }
    for note in notices(report, shortfall, Some(&called)) {
        // Display cuts keep the `not listed:` grammar in text and in JSON notes.
        if note.starts_with("not listed:") {
            out.line(&note);
        } else {
            out.line(&format!("note: {note}"));
        }
    }
    let holds = if scan.files.len() == 1 {
        "holds"
    } else {
        "hold"
    };
    out.line(&format!(
        "total: {} {holds} a word; {}",
        lines_in_files(scan.total_lines(), scan.files.len() as u64),
        searched_clause(scan)
    ));

    Rendered {
        stdout: out.0,
        stderr: markers(report, shortfall),
    }
}

// ============================================================================
// -l and -c
// ============================================================================

/// The summary on stderr of `-l` and `-c`.
fn summary_stderr(scan: &ScanOutput) -> String {
    format!(
        "skim search: total: {}; {}",
        lines_in_files(scan.total_lines(), scan.files.len() as u64),
        searched_clause(scan)
    )
}

fn files_with_matches(report: &Report<'_>) -> Rendered {
    let mut out = Text::default();
    for file in &report.scan.files {
        out.line(&quote_path(&file.path));
    }
    Rendered {
        stdout: out.0,
        stderr: vec![summary_stderr(report.scan)],
    }
}

fn counts(report: &Report<'_>) -> Rendered {
    let mut out = Text::default();
    for file in &report.scan.files {
        out.line(&format!("{}:{}", quote_path(&file.path), file.lines));
    }
    Rendered {
        stdout: out.0,
        stderr: vec![summary_stderr(report.scan)],
    }
}

// ============================================================================
// JSON
// ============================================================================

/// `path`, and `path_lossy` when the path is not UTF-8.
fn insert_path(map: &mut Map<String, Value>, path: &Path) {
    map.insert("path".to_owned(), json!(path.to_string_lossy()));
    if path.to_str().is_none() {
        map.insert("path_lossy".to_owned(), json!(true));
    }
}

fn occurrence_json(record: &Record, assignments: bool) -> Value {
    let mut occurrence = json!({
        "line": record.line,
        "column": record.column,
        "role": record.role.key(),
        "holder": record.holder.as_ref().map(|h| h.name.as_str()),
        "holder_range": record.holder.as_ref().map(|h| [h.first_line, h.last_line]),
        "text": record.text,
        "text_truncated_bytes": record.truncated_bytes,
    });
    if let Some(line) = call_definition(record, assignments) {
        occurrence["def_line"] = json!(line);
    }
    occurrence
}

fn roles_json(file: &FileScan) -> Value {
    let mut roles = Map::new();
    for role in Role::ALL {
        roles.insert(role.key().to_owned(), json!(file.roles.get(role)));
    }
    Value::Object(roles)
}

/// The score as the output prints it: one decimal. Rounding keeps the order.
fn rounded(score: f64) -> f64 {
    (score * 10.0).round() / 10.0
}

fn file_json(
    report: &Report<'_>,
    file: &FileScan,
    rank: usize,
    score: f64,
    assignments: bool,
) -> Value {
    let view = report.query.view();
    let mut map = Map::new();
    insert_path(&mut map, &file.path);
    map.insert("rank".to_owned(), json!(rank));
    map.insert("score".to_owned(), json!(rounded(score)));
    map.insert("prior".to_owned(), json!(file.prior.key()));
    map.insert("labelled".to_owned(), json!(file.unlabelled.is_none()));
    let (matched, total) = match view {
        View::Labelled => (1, 1),
        View::Words => (
            file.word_lines.iter().filter(|c| **c > 0).count(),
            report.query.words.len(),
        ),
    };
    map.insert("words_matched".to_owned(), json!(matched));
    map.insert("words_total".to_owned(), json!(total));
    if view == View::Words {
        map.insert("word_counts".to_owned(), json!(file.word_lines));
    }
    map.insert("line_count".to_owned(), json!(file.lines));
    map.insert("roles".to_owned(), roles_json(file));
    if let Some(reason) = file.unlabelled {
        map.insert("unlabelled_reason".to_owned(), json!(reason.key()));
    }

    let listed: Vec<&Record> = match view {
        View::Labelled => {
            let mut all: Vec<&Record> = file.defs.iter().chain(&file.records).collect();
            all.sort_by_key(|r| r.line);
            all
        }
        View::Words => best_word_lines(file),
    };
    let elided = u64::from(file.lines).saturating_sub(listed.len() as u64);
    map.insert(
        "occurrences".to_owned(),
        Value::Array(
            listed
                .iter()
                .map(|r| occurrence_json(r, assignments))
                .collect(),
        ),
    );
    map.insert("occurrences_elided".to_owned(), json!(elided));
    Value::Object(map)
}

fn definitions_json(report: &Report<'_>, ranked: &[Ranked]) -> Value {
    let scan = report.scan;
    let count = definition_count(scan);
    let mut sites = Vec::new();
    for ranked_file in ranked {
        let file = &scan.files[ranked_file.index];
        for record in &file.defs {
            let (first, last) = record.def_lines.unwrap_or((record.line, record.line));
            let mut site = Map::new();
            insert_path(&mut site, &file.path);
            site.insert("line".to_owned(), json!(record.line));
            site.insert("first_line".to_owned(), json!(first));
            site.insert("end_line".to_owned(), json!(last));
            site.insert(
                "holder".to_owned(),
                json!(record.holder.as_ref().map(|h| h.name.as_str())),
            );
            sites.push(Value::Object(site));
        }
    }
    json!({
        "count": count,
        "at_least": scan.parse.unlabelled_files > 0,
        "sites": sites,
    })
}

/// Paths use the same lossy-path marker on both ends of a pointer. Names
/// retain serde's escapes, like occurrence text; text-view escapes are separate.
fn called_definition_json(pointer: &CalledDefinition<'_>) -> Value {
    let mut caller = Map::new();
    insert_path(&mut caller, pointer.caller);
    caller.insert("line".to_owned(), json!(pointer.caller_line));
    let site = pointer.site;
    let mut map = Map::new();
    map.insert("name".to_owned(), json!(site.name));
    insert_path(&mut map, pointer.path);
    map.insert("line".to_owned(), json!(site.line));
    map.insert("first_line".to_owned(), json!(site.first_line));
    map.insert("end_line".to_owned(), json!(site.end_line));
    map.insert("caller".to_owned(), Value::Object(caller));
    Value::Object(map)
}

fn json_line(report: &Report<'_>) -> String {
    let scan = report.scan;
    let query = report.query;
    let view = query.view();
    let ranked = ranked_files(report);
    let list = candidates(report, &ranked);
    let (page, _) = Page::of(&report.layout, view).take(list);
    let offset = report.layout.offset.min(list.len());
    let assignments = view == View::Labelled && definition_count(scan) >= 2;

    let files: Vec<Value> = page
        .iter()
        .enumerate()
        .map(|(position, r)| {
            file_json(
                report,
                &scan.files[r.index],
                offset + position + 1,
                r.score,
                assignments,
            )
        })
        .collect();
    let has_more = offset + page.len() < list.len();

    let shortfall = match view {
        View::Labelled => labelled_shortfall(&scan.files, &Role::ALL),
        View::Words => {
            let mut shortfall = Shortfall::default();
            for r in page {
                let file = &scan.files[r.index];
                shortfall.add(
                    u64::from(file.lines).min(WORD_VIEW_BEST_LINES as u64),
                    best_word_lines(file).len() as u64,
                );
            }
            shortfall
        }
    };

    let mut left_out = Map::new();
    for reason in LeftOut::ALL {
        left_out.insert(reason.key().to_owned(), json!(scan.left_out.get(reason)));
    }

    let mut envelope = Map::new();
    envelope.insert("query".to_owned(), json!(query.text));
    envelope.insert("match_mode".to_owned(), json!(query.mode.key()));
    envelope.insert(
        "case".to_owned(),
        json!(if query.ignore_case {
            "insensitive"
        } else {
            "sensitive"
        }),
    );
    envelope.insert("view".to_owned(), json!(view.key()));
    if view == View::Words {
        envelope.insert("words".to_owned(), json!(query.words));
        envelope.insert(
            "files_with_all_words".to_owned(),
            json!(all_words_count(report)),
        );
    }
    envelope.insert("total_lines".to_owned(), json!(scan.total_lines()));
    envelope.insert("total_files".to_owned(), json!(list.len()));
    envelope.insert("has_more".to_owned(), json!(has_more));
    envelope.insert("definitions".to_owned(), definitions_json(report, &ranked));
    let called = called_definitions(report, page);
    envelope.insert(
        "called_definitions".to_owned(),
        Value::Array(called.sites.iter().map(called_definition_json).collect()),
    );
    envelope.insert("files".to_owned(), Value::Array(files));
    envelope.insert("searched_files".to_owned(), json!(scan.searched_files));
    envelope.insert("left_out".to_owned(), Value::Object(left_out));
    envelope.insert(
        "parse".to_owned(),
        json!({
            "files": scan.cost.files_parsed,
            "bytes": scan.cost.bytes_parsed,
            "budget_hit": scan.parse.budget_hit,
            "unlabelled_files": scan.parse.unlabelled_files,
        }),
    );
    envelope.insert(
        "cost".to_owned(),
        json!({
            "files_read": scan.cost.files_read,
            "bytes_read": scan.cost.bytes_read,
            "files_parsed": scan.cost.files_parsed,
            "bytes_parsed": scan.cost.bytes_parsed,
        }),
    );
    envelope.insert(
        "notices".to_owned(),
        json!(notices(
            report,
            shortfall,
            (view == View::Words).then_some(&called)
        )),
    );
    envelope.insert("duration_ms".to_owned(), json!(report.duration_ms));

    let mut line = Value::Object(envelope).to_string();
    line.push('\n');
    line
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
