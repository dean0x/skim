//! Flag parsing of `skim search`.
//!
//! The flag set is closed. A flag outside it, a removed flag and a flag of
//! another tool (`rg`) are refused with exit 2 and are never forwarded to a
//! program. Parsing reads no file and starts no process: every usage error is
//! found before the scan starts.

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

use super::matcher::CaseMode;
use super::rank::MAX_WORDS;

/// Largest `-C` value.
pub(super) const MAX_CONTEXT: usize = 100;

/// Largest `--limit` value.
pub(super) const MAX_LIMIT: usize = 1_000_000;

/// Most `-g` and `-t` values together.
pub(super) const MAX_FILTERS: usize = 64;

/// Exit code of every usage error.
pub(super) const USAGE_EXIT_CODE: u8 = 2;

/// Flags that `skim search` no longer accepts, each with the advice that
/// replaces it.
///
/// A removed flag is refused during flag parsing, before any file is read or
/// written. The refusal prints one line on stderr and nothing on stdout.
const REMOVED_FLAGS: [(&str, &str); 15] = [
    ("--ast", "use ast-grep for structural search"),
    ("--hot", "use skim heatmap for churn and fix density"),
    ("--cold", "use skim heatmap for churn and fix density"),
    ("--risky", "use skim heatmap for churn and fix density"),
    (
        "--blast-radius",
        "use skim heatmap, or git log --name-only -- FILE, for co-change",
    ),
    ("--weights", "there is no fused ranking"),
    ("--phrase", "use -F 'exact words' or -E for a regex"),
    ("--near", "use -F 'exact words' or -E for a regex"),
    ("--build", NO_INDEX_ADVICE),
    ("--rebuild", NO_INDEX_ADVICE),
    ("--update", NO_INDEX_ADVICE),
    ("--stats", NO_INDEX_ADVICE),
    ("--install-hooks", OLD_HOOKS_ADVICE),
    ("--remove-hooks", OLD_HOOKS_ADVICE),
    ("--lang", "use -t TYPE"),
];

/// The advice for the commands of the old index.
const NO_INDEX_ADVICE: &str = "skim search has no index; each query reads the files";

/// The advice for the commands that managed the old search git hooks.
const OLD_HOOKS_ADVICE: &str = "skim init and skim init --uninstall remove old search hooks";

/// The refusal for `-j`, which the old query form read as `--json`.
const REMOVED_SHORT_JSON: &str = "-j was removed in 3.0.0; use --json";

/// The refusal message for `arg` when it names a removed flag, in the bare
/// (`--ast`) or the attached-value (`--ast=x`) form, without the program
/// prefix.
///
/// The message quotes the flag name from the table, never the argument text, so
/// an attached value cannot reach the terminal.
pub(super) fn removed_flag_message(arg: &str) -> Option<String> {
    if arg == "-j" {
        return Some(REMOVED_SHORT_JSON.to_owned());
    }
    REMOVED_FLAGS.iter().find_map(|(flag, advice)| {
        let is_flag = arg == *flag
            || arg
                .strip_prefix(flag)
                .is_some_and(|rest| rest.starts_with('='));
        is_flag.then(|| format!("{flag} was removed in 3.0.0; {advice}"))
    })
}

/// A usage error. Its text is the message without the program prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UsageError(String);

impl UsageError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        UsageError(message.into())
    }

    /// The message, without the `skim search: ` prefix.
    pub(super) fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skim search: {}", self.0)
    }
}

impl std::error::Error for UsageError {}

/// How the pattern argument is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QueryShape {
    /// One pattern: the labelled view.
    Pattern {
        text: String,
        /// A bare identifier: a hit inside a longer identifier is `partial`,
        /// and the default view lists the calls and refs only.
        identifier: bool,
    },
    /// Several plain words: the word view. No word repeats.
    Words(Vec<String>),
}

/// What `-l` and `-c` ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListMode {
    /// The lines (the default).
    Lines,
    /// `-l`: the paths.
    Files,
    /// `-c`: `path:count`.
    Counts,
}

/// The flags of one query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueryFlags {
    pub(super) shape: QueryShape,
    pub(super) case: CaseMode,
    pub(super) word: bool,
    pub(super) regex: bool,
    pub(super) globs: Vec<String>,
    pub(super) types: Vec<String>,
    pub(super) list: ListMode,
    pub(super) context: usize,
    pub(super) all: bool,
    pub(super) full: bool,
    pub(super) json: bool,
    pub(super) limit: Option<usize>,
    pub(super) offset: usize,
    pub(super) root: Option<PathBuf>,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SearchAction {
    /// Print the help text.
    Help,
    /// Refuse a removed flag: print the message on stderr and exit 2. The
    /// message has no program prefix.
    Removed(String),
    /// Search.
    Query(Box<QueryFlags>),
}

/// Whether the arguments ask for JSON output, whatever else is wrong with
/// them. A usage error then prints a JSON error object.
pub(super) fn wants_json(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--json")
}

/// A flag that takes a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueFlag {
    Glob,
    Type,
    Context,
    Limit,
    Offset,
    Root,
}

impl ValueFlag {
    fn from_long(name: &str) -> Option<Self> {
        match name {
            "glob" => Some(ValueFlag::Glob),
            "type" => Some(ValueFlag::Type),
            "context" => Some(ValueFlag::Context),
            "limit" => Some(ValueFlag::Limit),
            "offset" => Some(ValueFlag::Offset),
            "root" => Some(ValueFlag::Root),
            _ => None,
        }
    }

    fn from_short(letter: char) -> Option<Self> {
        match letter {
            'g' => Some(ValueFlag::Glob),
            't' => Some(ValueFlag::Type),
            'C' => Some(ValueFlag::Context),
            _ => None,
        }
    }

    /// The spelling the messages use.
    fn name(self) -> &'static str {
        match self {
            ValueFlag::Glob => "-g",
            ValueFlag::Type => "-t",
            ValueFlag::Context => "-C",
            ValueFlag::Limit => "--limit",
            ValueFlag::Offset => "--offset",
            ValueFlag::Root => "--root",
        }
    }
}

/// A flag that takes no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchFlag {
    IgnoreCase,
    SmartCase,
    Word,
    Fixed,
    Regex,
    Files,
    Count,
    LineNumber,
    All,
    Full,
    Json,
}

impl SwitchFlag {
    fn from_long(name: &str) -> Option<Self> {
        match name {
            "ignore-case" => Some(SwitchFlag::IgnoreCase),
            "smart-case" => Some(SwitchFlag::SmartCase),
            "word-regexp" => Some(SwitchFlag::Word),
            "fixed-strings" => Some(SwitchFlag::Fixed),
            "regex" => Some(SwitchFlag::Regex),
            "files-with-matches" => Some(SwitchFlag::Files),
            "count" => Some(SwitchFlag::Count),
            "line-number" => Some(SwitchFlag::LineNumber),
            "all" => Some(SwitchFlag::All),
            "full" => Some(SwitchFlag::Full),
            "json" => Some(SwitchFlag::Json),
            _ => None,
        }
    }

    fn from_short(letter: char) -> Option<Self> {
        match letter {
            'i' => Some(SwitchFlag::IgnoreCase),
            'S' => Some(SwitchFlag::SmartCase),
            'w' => Some(SwitchFlag::Word),
            'F' => Some(SwitchFlag::Fixed),
            'E' => Some(SwitchFlag::Regex),
            'l' => Some(SwitchFlag::Files),
            'c' => Some(SwitchFlag::Count),
            'n' => Some(SwitchFlag::LineNumber),
            _ => None,
        }
    }
}

/// The flags read so far.
#[derive(Debug, Default)]
struct Draft {
    pattern: Option<String>,
    ignore_case: bool,
    smart_case: bool,
    word: bool,
    fixed: bool,
    regex: bool,
    files: bool,
    count: bool,
    all: bool,
    full: bool,
    json: bool,
    context: Option<usize>,
    limit: Option<usize>,
    offset: usize,
    root: Option<PathBuf>,
    globs: Vec<String>,
    types: Vec<String>,
}

/// Parse the arguments after `skim search`.
///
/// Flags are read from left to right and the first problem ends the parse: a
/// removed flag, an unknown flag, a bad value, a second pattern. `-h` and
/// `--help` end it with the help action. Everything after a bare `--` is the
/// pattern.
///
/// # Errors
///
/// [`UsageError`] for an unknown flag, a missing or bad value, a second
/// pattern, a missing pattern, an empty pattern, more than [`MAX_WORDS`] words,
/// more than [`MAX_FILTERS`] filters, or a conflict between flags.
pub(super) fn parse_flags(args: &[String]) -> Result<SearchAction, UsageError> {
    if args.is_empty() {
        return Ok(SearchAction::Help);
    }
    let mut draft = Draft::default();
    let mut flags_open = true;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if !flags_open || !arg.starts_with('-') || arg == "-" {
            let previous = i.checked_sub(1).map(|p| args[p].as_str());
            draft.positional(arg, previous)?;
            i += 1;
        } else if arg == "--" {
            flags_open = false;
            i += 1;
        } else if arg == "-h" || arg == "--help" {
            return Ok(SearchAction::Help);
        } else if let Some(message) = removed_flag_message(arg) {
            return Ok(SearchAction::Removed(message));
        } else {
            i += draft.flag(arg, args.get(i + 1).map(String::as_str))?;
        }
    }
    draft.finish()
}

/// The error of a flag that this tool does not know.
fn unknown_flag(arg: &str) -> UsageError {
    UsageError::new(format!(
        "unknown flag {arg:?}; skim search --help lists the flags"
    ))
}

/// Parse a whole number of a flag, within `0..=max`.
fn parse_number(flag: &str, raw: &str, min: u64, max: u64) -> Result<u64, UsageError> {
    match raw.parse::<u64>() {
        Ok(n) if (min..=max).contains(&n) => Ok(n),
        _ => Err(UsageError::new(format!(
            "{flag} needs a whole number from {min} to {max}, got {raw:?}"
        ))),
    }
}

impl Draft {
    /// Record a positional argument: the pattern.
    fn positional(&mut self, arg: &str, previous: Option<&str>) -> Result<(), UsageError> {
        if self.pattern.is_none() {
            self.pattern = Some(arg.to_owned());
            return Ok(());
        }
        if previous == Some("-n") && !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
            return Err(UsageError::new(
                "-n no longer sets the limit; use --limit N",
            ));
        }
        Err(UsageError::new(
            "one pattern only; quote words as one argument; use -g to filter paths",
        ))
    }

    /// Read the flag `arg` (and its value, which may be `next`). Returns how
    /// many arguments the flag used.
    fn flag(&mut self, arg: &str, next: Option<&str>) -> Result<usize, UsageError> {
        if let Some(body) = arg.strip_prefix("--") {
            return self.long_flag(arg, body, next);
        }
        self.short_group(arg, next)
    }

    fn long_flag(
        &mut self,
        arg: &str,
        body: &str,
        next: Option<&str>,
    ) -> Result<usize, UsageError> {
        let (name, attached) = match body.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (body, None),
        };
        if let Some(flag) = ValueFlag::from_long(name) {
            return match attached {
                Some(value) => self.set_value(flag, value).map(|()| 1),
                None => {
                    let value = take_value(flag, next)?;
                    self.set_value(flag, value).map(|()| 2)
                }
            };
        }
        if let Some(flag) = SwitchFlag::from_long(name) {
            if attached.is_some() {
                return Err(UsageError::new(format!("{arg:?} takes no value")));
            }
            self.set_switch(flag);
            return Ok(1);
        }
        Err(unknown_flag(arg))
    }

    /// A group of short flags such as `-iwF` or `-C3`. A value flag ends the
    /// group: the rest of the group is its value, else the next argument.
    fn short_group(&mut self, group: &str, next: Option<&str>) -> Result<usize, UsageError> {
        let letters = &group[1..];
        for (at, letter) in letters.char_indices() {
            if let Some(flag) = SwitchFlag::from_short(letter) {
                self.set_switch(flag);
            } else if let Some(flag) = ValueFlag::from_short(letter) {
                let rest = &letters[at + letter.len_utf8()..];
                if rest.is_empty() {
                    let value = take_value(flag, next)?;
                    self.set_value(flag, value)?;
                    return Ok(2);
                }
                self.set_value(flag, rest)?;
                return Ok(1);
            } else {
                return Err(unknown_flag(group));
            }
        }
        Ok(1)
    }

    fn set_switch(&mut self, flag: SwitchFlag) {
        match flag {
            SwitchFlag::IgnoreCase => self.ignore_case = true,
            SwitchFlag::SmartCase => self.smart_case = true,
            SwitchFlag::Word => self.word = true,
            SwitchFlag::Fixed => self.fixed = true,
            SwitchFlag::Regex => self.regex = true,
            SwitchFlag::Files => self.files = true,
            SwitchFlag::Count => self.count = true,
            SwitchFlag::LineNumber => {}
            SwitchFlag::All => self.all = true,
            SwitchFlag::Full => self.full = true,
            SwitchFlag::Json => self.json = true,
        }
    }

    fn set_value(&mut self, flag: ValueFlag, value: &str) -> Result<(), UsageError> {
        match flag {
            ValueFlag::Glob | ValueFlag::Type => {
                if value.is_empty() {
                    return Err(UsageError::new(format!(
                        "{} needs a value that is not empty",
                        flag.name()
                    )));
                }
                if self.globs.len() + self.types.len() >= MAX_FILTERS {
                    return Err(UsageError::new(format!(
                        "at most {MAX_FILTERS} -g/-t filters"
                    )));
                }
                let list = if flag == ValueFlag::Glob {
                    &mut self.globs
                } else {
                    &mut self.types
                };
                list.push(value.to_owned());
            }
            ValueFlag::Context => {
                self.context = Some(to_usize(parse_number("-C", value, 0, MAX_CONTEXT as u64)?));
            }
            ValueFlag::Limit => {
                self.limit = Some(to_usize(parse_number(
                    "--limit",
                    value,
                    1,
                    MAX_LIMIT as u64,
                )?));
            }
            ValueFlag::Offset => {
                self.offset = to_usize(parse_number("--offset", value, 0, u64::from(u32::MAX))?);
            }
            ValueFlag::Root => {
                if value.is_empty() {
                    return Err(UsageError::new("--root needs a value that is not empty"));
                }
                self.root = Some(PathBuf::from(value));
            }
        }
        Ok(())
    }

    /// Check the conflicts and build the action.
    fn finish(self) -> Result<SearchAction, UsageError> {
        let Some(pattern) = self.pattern.as_deref() else {
            return Err(UsageError::new(
                "no pattern given; skim search --help lists the flags",
            ));
        };
        if self.files && self.count {
            return Err(UsageError::new("-l and -c cannot be combined"));
        }
        if self.fixed && self.regex {
            return Err(UsageError::new("-F and -E cannot be combined"));
        }
        let list = match (self.files, self.count) {
            (true, _) => ListMode::Files,
            (_, true) => ListMode::Counts,
            _ => ListMode::Lines,
        };
        if list != ListMode::Lines {
            let flag = if list == ListMode::Files { "-l" } else { "-c" };
            let with = [
                ("-C", self.context.is_some()),
                ("--all", self.all),
                ("--full", self.full),
                ("--json", self.json),
            ];
            if let Some((other, _)) = with.iter().find(|(_, set)| *set) {
                return Err(UsageError::new(format!(
                    "{flag} cannot be combined with {other}"
                )));
            }
        }
        let shape = query_shape(pattern, self.fixed || self.regex)?;
        if list != ListMode::Lines && matches!(shape, QueryShape::Words(_)) {
            return Err(UsageError::new(
                "-l and -c take one pattern, not a list of words; \
                 use -F for exact words or -E for a regex",
            ));
        }
        let case = if self.ignore_case {
            CaseMode::Insensitive
        } else if self.smart_case {
            CaseMode::Smart
        } else {
            CaseMode::Sensitive
        };
        Ok(SearchAction::Query(Box::new(QueryFlags {
            shape,
            case,
            word: self.word,
            regex: self.regex,
            globs: self.globs,
            types: self.types,
            list,
            context: self.context.unwrap_or(0),
            all: self.all,
            full: self.full,
            json: self.json,
            limit: self.limit,
            offset: self.offset,
            root: self.root,
        })))
    }
}

/// The value of `flag` in the next argument.
fn take_value(flag: ValueFlag, next: Option<&str>) -> Result<&str, UsageError> {
    match next {
        None => Err(UsageError::new(format!("{} needs a value", flag.name()))),
        Some("-h" | "--help") => Err(UsageError::new(format!(
            "{} needs a value; to print help, run: skim search --help",
            flag.name()
        ))),
        Some(value) => Ok(value),
    }
}

/// A small number as `usize`. The numbers are bounded by the callers, so the
/// conversion cannot lose a bit on a 32-bit or 64-bit target.
fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// A bare identifier: the labelled view without `--all`.
static BARE_IDENTIFIER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^[\p{XID_Start}_$][\p{XID_Continue}$]*$").ok());

/// Whether `text` is a bare identifier.
pub(super) fn is_bare_identifier(text: &str) -> bool {
    BARE_IDENTIFIER.as_ref().is_some_and(|re| re.is_match(text))
}

/// Decide how the pattern argument is read.
///
/// A pattern given with `-E` or `-F` is one pattern as written. Otherwise an
/// argument with ASCII whitespace is a list of words: split on whitespace, drop
/// empty parts, keep the first copy of each. One word left is a pattern.
fn query_shape(pattern: &str, explicit: bool) -> Result<QueryShape, UsageError> {
    if pattern.is_empty() {
        return Err(UsageError::new("the pattern is empty"));
    }
    if explicit || !pattern.bytes().any(|b| b.is_ascii_whitespace()) {
        return Ok(QueryShape::Pattern {
            text: pattern.to_owned(),
            identifier: !explicit && is_bare_identifier(pattern),
        });
    }
    let mut words: Vec<String> = Vec::new();
    for part in pattern.split(|c: char| c.is_ascii_whitespace()) {
        if !part.is_empty() && !words.iter().any(|w| w == part) {
            if words.len() == MAX_WORDS {
                return Err(UsageError::new(format!(
                    "at most {MAX_WORDS} words; use -E for a longer pattern"
                )));
            }
            words.push(part.to_owned());
        }
    }
    match words.len() {
        0 => Err(UsageError::new("the pattern is empty")),
        1 => {
            let text = words.remove(0);
            let identifier = is_bare_identifier(&text);
            Ok(QueryShape::Pattern { text, identifier })
        }
        _ => Ok(QueryShape::Words(words)),
    }
}

/// Full `skim search` help text.
///
/// A `const` so a test can pin each flag against it.
pub(super) const SEARCH_HELP_TEXT: &str = "\
Usage: skim search [OPTIONS] PATTERN

Find where a name is defined and where it is used. Each query reads the files:
there is no index to build.

The PATTERN is a literal. It gives one of two views.
  One bare identifier  The labelled view. The definition comes first, with its
                       line range. The calls follow, each with the function that
                       holds it. Imports, strings, comments and other lines are
                       counted (--all lists them).
  Several words in     The word view. The files that hold all the words come
  one argument         first, ranked, with their best lines. If fewer than 3
                       files hold all of them, files with fewer words follow.
Any other pattern, and every -E pattern, gives the labelled view with --all.

Match options:
  -i, --ignore-case       Ignore case
  -S, --smart-case        Ignore case when the pattern has no uppercase letter
                          (-i wins over -S). The default is case sensitive.
  -w, --word-regexp       Match whole words only
  -F, --fixed-strings     Read the pattern as a literal (the default)
  -E, --regex             Read the pattern as a regular expression

Which files:
  -g, --glob GLOB         Keep the files whose repo-relative path fits GLOB.
                          Repeat the flag; a leading ! leaves the files out.
  -t, --type TYPE         Keep the files of a type (as rg: rust, py, ts, ...)
  --root DIR              Search DIR (default: the repository that holds the
                          current folder, else the current folder)
  Every tracked text file is searched, and so is every untracked file that the
  ignore files allow. Tracked files are searched also when they are hidden or
  may hold secrets (.env, key files): use -g '!NAME' to leave them out.
  A later release can change this default. One function and one constant in
  the source set it.
  A file that is binary, not UTF-8, above 5 MiB, unreadable, missing, a link,
  or outside the root is counted under \"left out\" in the last line.

Output:
  -l, --files-with-matches  Print the paths, in path order
  -c, --count             Print path:count, in path order
                          -l and -c list every file: --limit and --offset do
                          not apply. They cannot be used with -C, --all, --full
                          or --json.
  -C, --context N         Print N lines around each line (0 to 100)
  --all                   List every section
  --full                  Print the text of every line, also in a long section
  --json                  Print one JSON object on one line
  --limit N               List the first N files in rank order (1 to 1000000)
  --offset N              Skip the first N files
  -n, --line-number       Accepted and ignored: line numbers always print
  -h, --help              Print this help
  --                      End of flags. The next argument is the pattern.

Exit codes: 0 a line matched, 1 no line matched, 2 an error.

Removed flags (each exits 2 and names its replacement):
  --ast                       Use ast-grep for structural search
  --hot, --cold, --risky      Use skim heatmap for churn and fix density
  --blast-radius              Use skim heatmap, or git log --name-only -- FILE
  --weights                   There is no fused ranking
  --phrase, --near            Use -F 'exact words' or -E for a regex
  --build, --rebuild,         There is no index; each query reads the files
  --update, --stats
  --install-hooks,            Use skim init and skim init --uninstall to remove
  --remove-hooks              old search hooks
  --lang                      Use -t TYPE
  -j                          Use --json

Examples:
  skim search launchWorker                 Definition, calls, counts
  skim search 'temporal staleness check'   Files that hold all three words
  skim search -E 'fn \\w+_test' -t rust    A regex, Rust files only
  skim search -l TODO -g '!vendor'         Paths only, without vendor";

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
