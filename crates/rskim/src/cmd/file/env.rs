//! env/printenv parser with sensitive value redaction.
//!
//! Parses `env` / `printenv` output (KEY=value pairs) into structured `FileResult`.
//! Sensitive values are automatically redacted to protect secrets.
//!
//! Tiers:
//! - **Tier 1 (Full)**: KEY=value parsing with sensitive redaction
//! - **Tier 3 (Passthrough)**: Empty output or parse failure
//!
//! # Redaction rules
//!
//! Keys are redacted when:
//! - Key ends with `_TOKEN`, `_SECRET`, `_PASSWORD`, `_API_KEY`, `_SECRET_KEY`,
//!   `_PRIVATE_KEY`, `_ENCRYPTION_KEY`, `_SIGNING_KEY`, `_ACCESS_KEY`, `_HMAC_KEY`,
//!   `_CREDENTIAL`, or `_AUTH` (case-insensitive)
//! - Key matches the exact sensitive-key set (AWS_ACCESS_KEY_ID, GITHUB_TOKEN, etc.)
//!
//! URL credentials (scheme://user:pass@host) are replaced with `***:***` in values.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

/// CONFIG uses "printenv" as the binary name; `skim env` routes here via dispatch.
const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "printenv",
    env_overrides: &[],
    install_hint: "printenv is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    // SECURITY: Redaction is a security control, not a compression tier.  The
    // net-savings guard normally rejects compressed output that is no shorter
    // than the raw tool output and falls back to the verbatim raw value.  For
    // credential-bearing keys (GITHUB_TOKEN, NPM_TOKEN, …) whose secret is
    // only a few characters long, replacing the secret with `***` does NOT
    // shrink the line — the guard would therefore emit the raw secret verbatim,
    // defeating the entire purpose of redaction.  Setting this to `true` ensures
    // the redacted view always wins, regardless of byte arithmetic. (#317)
    skip_net_savings_guard: true,
    synthesize_success_line: None,
    injected_format_flag: None,
    raw_override: None,
    // SECURITY (PF-012): `never_passthrough = true` prevents SKIM_PASSTHROUGH=1
    // from bypassing the `parse_impl` redaction pass.  Credential redaction is a
    // non-negotiable safety property — it must hold on ALL paths, including the
    // SKIM_PASSTHROUGH=1 escape hatch.  Without this flag, execution.rs would
    // stream raw `printenv` output directly to stdout, leaking secrets.
    never_passthrough: true,
};

/// Regex to detect and redact URL credentials: scheme://user:pass@host
static RE_URL_CREDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(://)[^:@/]+:[^@/]+(@)").unwrap());

/// Exact set of well-known sensitive key names (uppercase).
const SENSITIVE_EXACT: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "DATABASE_URL",
    "NPM_TOKEN",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "STRIPE_SECRET_KEY",
    "SENTRY_DSN",
    "SENDGRID_API_KEY",
];

/// Key suffixes that indicate sensitive values (stored uppercase, matched case-insensitively).
///
/// `_KEY` was removed to avoid false positives on non-secret identifiers such as
/// `REGISTRY_KEY`, `PRIMARY_KEY`, and `SORT_KEY`. Specific compound suffixes
/// (`_API_KEY`, `_SECRET_KEY`, `_PRIVATE_KEY`, `_ENCRYPTION_KEY`, `_SIGNING_KEY`,
/// `_ACCESS_KEY`, `_HMAC_KEY`) are used instead to cover real crypto/signing secrets.
const SENSITIVE_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_API_KEY",
    "_SECRET_KEY",
    "_PRIVATE_KEY",
    "_ENCRYPTION_KEY",
    "_SIGNING_KEY",
    "_ACCESS_KEY",
    "_HMAC_KEY",
    "_CREDENTIAL",
    "_AUTH",
];

/// The same handler when `env` options or assignments come before the
/// child: the real `env` binary starts `printenv`, so the options and the
/// assignments apply to the listing.
const CONFIG_VIA_ENV: ToolRunConfig<'static> = ToolRunConfig {
    program: "env",
    install_hint: "env is typically pre-installed on Unix systems",
    ..CONFIG
};

/// Run `skim printenv [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_view(&EnvView::printenv(args.to_vec()), ctx)
}

/// Run one env view (see [`env_route`]).
pub(super) fn run_view(view: &EnvView, ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    let (config, args) = if view.env_words.is_empty() {
        (CONFIG, view.printenv_args.clone())
    } else {
        let mut args = view.env_words.clone();
        args.push(view.child.clone());
        args.extend_from_slice(&view.printenv_args);
        (CONFIG_VIA_ENV, args)
    };
    // The same decision as `run_tool` makes for these arguments.
    let reads_stdin = crate::cmd::should_read_stdin(&args);
    let known = KnownEnv::from_process().with_edits(&view.edits);
    let request = EnvRequest::new(&view.printenv_args, reads_stdin, known);
    run_tool(
        config,
        &args,
        ctx,
        |_| {},
        move |output| parse_impl(output, &request),
    )
}

/// What the env view masks, and what it knows about the text.
struct EnvRequest {
    /// The names given to `printenv` (empty for a listing).
    names: Vec<String>,
    /// The environment of the `printenv` child that skim starts.
    known: KnownEnv,
}

impl EnvRequest {
    /// Build the request.  When the handler can read stdin, the arguments
    /// are not `printenv` names (`run` is a routing word).
    ///
    /// The known records apply to stdin text too: the handler starts the
    /// child when stdin is empty, and on other stdin text an exact known
    /// record only sets an edge that the line rule would also accept, or
    /// hides more.
    fn new(printenv_args: &[String], reads_stdin: bool, known: KnownEnv) -> Self {
        let names = if reads_stdin {
            Vec::new()
        } else {
            requested_names(printenv_args)
        };
        Self { names, known }
    }
}

/// How `skim env ARGS` is served.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum EnvRoute {
    /// The masked env view.
    View(EnvView),
    /// The real `env` binary with the original arguments: it starts a child
    /// program that is not an env view (`env FOO=1 npm test`).
    Exec,
}

/// One env view: `[env ENV_WORDS] CHILD PRINTENV_ARGS`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct EnvView {
    /// The `env` options, assignments and nested `env` words before the
    /// child.  When there are some, the real `env` binary starts the child;
    /// when there are none, skim starts `printenv` directly.
    env_words: Vec<String>,
    /// The child program as written (`printenv`, or a path to it).
    child: String,
    /// The arguments for `printenv`.
    printenv_args: Vec<String>,
    /// The changes that `env_words` make to the environment of the child.
    edits: Vec<EnvEdit>,
}

impl EnvView {
    /// `printenv ARGS`, started directly.
    fn printenv(printenv_args: Vec<String>) -> Self {
        Self {
            env_words: Vec::new(),
            child: "printenv".to_string(),
            printenv_args,
            edits: Vec::new(),
        }
    }
}

/// One change that `env` makes to the environment, in order.
#[derive(Debug, PartialEq, Eq)]
enum EnvEdit {
    /// `-i`: start from an empty environment.
    Clear,
    /// `-u NAME`.
    Unset(String),
    /// `NAME=VALUE`.
    Set(String, String),
}

/// The maximum number of `-S` command lines that one `env` route expands.
const MAX_SPLIT_STRINGS: usize = 4;

/// `env` short options that take a value (GNU and BSD): the rest of the
/// cluster, or the next argument.
const ENV_SHORT_VALUE_OPTIONS: &[char] = &['u', 'P', 'C', 'S', 'a', 'L', 'U'];

/// `env` long options (without `--`), and whether each one takes a value.
const ENV_LONG_OPTIONS: &[(&str, bool)] = &[
    ("argv0", true),
    ("block-signal", false),
    ("chdir", true),
    ("debug", false),
    ("default-signal", false),
    ("help", false),
    ("ignore-environment", false),
    ("ignore-signal", false),
    ("list-signal-handling", false),
    ("null", false),
    ("split-string", true),
    ("unset", true),
    ("version", false),
];

/// Decide how `skim env ARGS` is served, from the `env` grammar: options,
/// then `NAME=VALUE` assignments, then the child program and its arguments.
///
/// A child whose file name is `env` or `printenv` (also as a path) is an env
/// view, so it gets the mask.  With no child, `env` prints the listing, so
/// that is the env view too, with `printenv` as the child.  The `-S` command
/// line is split with the GNU quote and escape rules.  A `-S` line that skim
/// cannot read (a `${VAR}` expansion, an open quote, more than
/// [`MAX_SPLIT_STRINGS`] lines) gives the plain listing view, which runs no
/// other program.  Only another child program goes to the real `env` binary.
///
/// Known limit: a child such as `sh -c env` runs a program that skim cannot
/// inspect, so it goes to the real `env` binary.
pub(super) fn env_route(args: &[String]) -> EnvRoute {
    let mut words: Vec<String> = args.to_vec();
    let mut i = 0;
    let mut grammar_used = false;
    let mut options_done = false;
    let mut null = false;
    let mut splits = 0;
    let mut edits: Vec<EnvEdit> = Vec::new();
    // Bound: each pass moves `i` forward, except a `-S` pass, and at most
    // MAX_SPLIT_STRINGS of those occur.
    while let Some(arg) = words.get(i) {
        let word = if options_done {
            plain_word(arg)
        } else {
            option_word(arg)
        };
        match word {
            EnvWord::Child => {
                let name = std::path::Path::new(arg)
                    .file_name()
                    .and_then(|name| name.to_str());
                match name {
                    Some("printenv") => {
                        let env_words = if grammar_used {
                            words[..i].to_vec()
                        } else {
                            Vec::new()
                        };
                        return EnvRoute::View(EnvView {
                            env_words,
                            child: arg.clone(),
                            printenv_args: words[i + 1..].to_vec(),
                            edits,
                        });
                    }
                    Some("env") => {
                        grammar_used = true;
                        options_done = false;
                        null = false;
                        i += 1;
                    }
                    // `skim env NAME` with no option or assignment: the
                    // handler reads it as `printenv NAME`, as before.
                    _ if !grammar_used => {
                        return EnvRoute::View(EnvView::printenv(words[i..].to_vec()));
                    }
                    _ => return EnvRoute::Exec,
                }
            }
            EnvWord::Option(option) => {
                grammar_used = true;
                null |= option.null;
                if option.clear {
                    edits.push(EnvEdit::Clear);
                }
                let Some((kind, inline)) = option.valued else {
                    i += 1;
                    continue;
                };
                let (value, next) = match inline {
                    Some(value) => (Some(value.to_string()), i + 1),
                    None => (words.get(i + 1).cloned(), i + 2),
                };
                match kind {
                    Valued::Unset => {
                        edits.extend(value.map(EnvEdit::Unset));
                        i = next;
                    }
                    Valued::Other => i = next,
                    Valued::SplitString { alone } => {
                        splits += 1;
                        let tokens = value.as_deref().and_then(split_string_words);
                        let Some(tokens) = tokens.filter(|_| alone && splits <= MAX_SPLIT_STRINGS)
                        else {
                            return EnvRoute::View(EnvView::printenv(Vec::new()));
                        };
                        let tail = words.split_off(next.min(words.len()));
                        words.truncate(i);
                        words.extend(tokens);
                        words.extend(tail);
                    }
                }
            }
            EnvWord::Assignment => {
                if let Some((name, value)) = arg.split_once('=') {
                    edits.push(EnvEdit::Set(name.to_string(), value.to_string()));
                }
                options_done = true;
                grammar_used = true;
                i += 1;
            }
            EnvWord::EndOfOptions => {
                options_done = true;
                grammar_used = true;
                i += 1;
            }
        }
    }

    // No child: `env` prints the listing.  `printenv` takes `-0`, and the
    // real `env` refuses `-0` with a child, so it moves.
    let env_words: Vec<String> = words
        .into_iter()
        .filter(|word| !(null && matches!(word.as_str(), "-0" | "--null")))
        .collect();
    EnvRoute::View(EnvView {
        env_words,
        child: "printenv".to_string(),
        printenv_args: if null {
            vec!["-0".to_string()]
        } else {
            Vec::new()
        },
        edits,
    })
}

/// The words of a `-S` command line, with the quote and escape rules of
/// GNU `env -S`.  `None` when skim cannot know the words: a `${VAR}`
/// expansion, an open quote, or an escape that `env` refuses.
fn split_string_words(line: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    // Bound: each pass reads at least one character.
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), '\\') => match chars.peek() {
                Some(&next @ ('\\' | '\'')) => {
                    word.push(next);
                    chars.next();
                }
                _ => word.push('\\'),
            },
            (Some('\''), _) => word.push(c),
            (_, '\\') => {
                let next = chars.next()?;
                let was_in_word = in_word;
                in_word = true;
                match next {
                    '_' if quote.is_none() => {
                        if was_in_word {
                            words.push(std::mem::take(&mut word));
                        }
                        in_word = false;
                    }
                    '_' => word.push(' '),
                    'c' => break,
                    'n' => word.push('\n'),
                    't' => word.push('\t'),
                    'r' => word.push('\r'),
                    'f' => word.push('\u{0c}'),
                    'v' => word.push('\u{0b}'),
                    '\\' | '\'' | '"' | '$' | '#' => word.push(next),
                    _ => return None,
                }
            }
            (_, '$') if chars.peek() == Some(&'{') => return None,
            (Some(_), _) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '#') if !in_word => break,
            (None, _) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, _) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

/// The kind of one `env` argument.
enum EnvWord<'a> {
    Option(OptionWord<'a>),
    /// `--`: the end of the options.
    EndOfOptions,
    /// `NAME=VALUE`.
    Assignment,
    /// The child program.
    Child,
}

/// One option argument (a long option, or a short cluster).
#[derive(Default)]
struct OptionWord<'a> {
    /// `-i`, `-` or `--ignore-environment`.
    clear: bool,
    /// `-0` or `--null`.
    null: bool,
    /// The option that takes a value, with its inline value (`None`: the
    /// value is the next argument).
    valued: Option<(Valued, Option<&'a str>)>,
}

/// An option that takes a value.
#[derive(Clone, Copy)]
enum Valued {
    Unset,
    /// `-S`; `alone` when no other option is in the same argument.
    SplitString {
        alone: bool,
    },
    Other,
}

/// The kind of an argument after the options.
fn plain_word(arg: &str) -> EnvWord<'_> {
    if arg.contains('=') {
        EnvWord::Assignment
    } else {
        EnvWord::Child
    }
}

/// The kind of an argument while options can still come.
fn option_word(arg: &str) -> EnvWord<'_> {
    if arg == "--" {
        return EnvWord::EndOfOptions;
    }
    // `-` alone is the old spelling of `-i`.
    if arg == "-" {
        return EnvWord::Option(OptionWord {
            clear: true,
            ..OptionWord::default()
        });
    }
    if let Some(long) = arg.strip_prefix("--") {
        return EnvWord::Option(long_option_word(long));
    }
    let Some(cluster) = arg.strip_prefix('-') else {
        return plain_word(arg);
    };
    let mut option = OptionWord::default();
    for (i, c) in cluster.char_indices() {
        option.clear |= c == 'i';
        option.null |= c == '0';
        if ENV_SHORT_VALUE_OPTIONS.contains(&c) {
            let attached = &cluster[i + c.len_utf8()..];
            let kind = match c {
                'u' => Valued::Unset,
                'S' => Valued::SplitString { alone: i == 0 },
                _ => Valued::Other,
            };
            option.valued = Some((kind, (!attached.is_empty()).then_some(attached)));
            break;
        }
    }
    EnvWord::Option(option)
}

/// One long option (`long` is the text after `--`).  A unique prefix of a
/// name is that option, as `getopt_long` reads it.  An unknown or ambiguous
/// name makes `env` stop with an error, so it runs nothing.
fn long_option_word(long: &str) -> OptionWord<'_> {
    let (name, inline) = match long.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (long, None),
    };
    let exact = ENV_LONG_OPTIONS.iter().find(|(option, _)| *option == name);
    let found = exact.or_else(|| {
        let mut matches = ENV_LONG_OPTIONS
            .iter()
            .filter(|(option, _)| !name.is_empty() && option.starts_with(name));
        let first = matches.next();
        if matches.next().is_none() {
            first
        } else {
            None
        }
    });
    let Some(&(option, takes_value)) = found else {
        return OptionWord::default();
    };
    let kind = match option {
        "unset" => Valued::Unset,
        "split-string" => Valued::SplitString { alone: true },
        _ => Valued::Other,
    };
    OptionWord {
        clear: option == "ignore-environment",
        null: option == "null",
        valued: takes_value.then_some((kind, inline)),
    }
}

/// The environment of the `printenv` child: skim's own environment, with the
/// `env` edits of the view applied (`CONFIG.env_overrides` is empty).  It gives
/// the exact records of the child's output: a value over several lines is one
/// record, whatever its lines hold.
struct KnownEnv {
    values: HashMap<String, String>,
}

impl KnownEnv {
    fn from_process() -> Self {
        // Lossy like the child's stdout (`runner` decodes it lossily), so a
        // non-UTF-8 value compares equal to the text that the parse sees.
        Self::from_pairs(std::env::vars_os().map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        }))
    }

    fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            values: pairs.into_iter().collect(),
        }
    }

    /// Apply the `env` edits, in order.
    fn with_edits(mut self, edits: &[EnvEdit]) -> Self {
        for edit in edits {
            match edit {
                EnvEdit::Clear => self.values.clear(),
                EnvEdit::Unset(name) => {
                    self.values.remove(name);
                }
                EnvEdit::Set(name, value) => {
                    self.values.insert(name.clone(), value.clone());
                }
            }
        }
        self
    }

    /// The known record `KEY=value` at the start of `text`, followed by
    /// `terminator` or by the end of the text.  Returns the key, the value
    /// and the length of the record with its terminator.
    fn record_at<'k>(&'k self, text: &str, terminator: char) -> Option<(&'k str, &'k str, usize)> {
        let key_end = text.find(['=', '\n', '\0'])?;
        if !text[key_end..].starts_with('=') {
            return None;
        }
        let (key, value) = self.values.get_key_value(&text[..key_end])?;
        let value_start = key_end + 1;
        let used = value_with_terminator(&text[value_start..], value, terminator)?;
        Some((key.as_str(), value.as_str(), value_start + used))
    }
}

/// The length of `value` and its terminator when `text` starts with exactly
/// `value`, followed by `terminator` or by the end of the text.
fn value_with_terminator(text: &str, value: &str, terminator: char) -> Option<usize> {
    let after = text.strip_prefix(value)?;
    match after.chars().next() {
        None => Some(value.len()),
        Some(c) if c == terminator => Some(value.len() + c.len_utf8()),
        Some(_) => None,
    }
}

/// The variable names that the caller asked `printenv` for (every argument
/// that is not a flag).  With names, `printenv` prints bare values, one after
/// the other, with no `KEY=` prefix, so the listing parse cannot see which
/// line belongs to which name.
fn requested_names(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect()
}

/// Three-tier parse function for env/printenv output.
///
/// Every path that can carry a value goes through a mask step.  For the
/// output of a `printenv` child, the records come from skim's own
/// environment, so each value is masked whole.  For text on stdin, a line
/// rule finds the records, and the lines it puts inside a masked value are
/// hidden with a counted footer.  Only an empty output, or a listing with no
/// `KEY=` record and no requested name, passes through.
///
/// Known limit (stdin only): a line inside a masked value that has the
/// shape of a new variable (`name=value`, with a portable name that is not
/// sensitive) reads as a new variable and shows.
fn parse_impl(output: &CommandOutput, request: &EnvRequest) -> ParseResult<FileResult> {
    let known = Some(&request.known);
    let result = if request.names.is_empty() {
        mask_listing(&output.stdout, known)
    } else {
        mask_named_values(&output.stdout, &request.names, known)
    };
    match result {
        Some(result) => ParseResult::Full(result),
        None => ParseResult::Passthrough(output.stdout.clone()),
    }
}

// ============================================================================
// Tier 1: env output parsing with redaction
// ============================================================================

/// Mask the bare values that `printenv NAME...` prints.
///
/// The output has no keys.  When the names are known, each value is matched
/// exactly, in the order of the names (a name that is not set prints
/// nothing), and masked by its own name.  Text that no known value accounts
/// for fails closed: every line of it is masked when any requested name is
/// sensitive, and gets the line mask otherwise.
fn mask_named_values(
    stdout: &str,
    requested: &[String],
    known: Option<&KnownEnv>,
) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let terminator = if stdout.contains('\0') { '\0' } else { '\n' };
    let mut entries: Vec<String> = Vec::new();
    let mut rest = stdout;
    if let Some(known) = known {
        for name in requested {
            let Some(value) = known.values.get(name) else {
                continue;
            };
            let Some(used) = value_with_terminator(rest, value, terminator) else {
                continue;
            };
            if is_sensitive_key(name) {
                entries.push("***".to_string());
            } else {
                entries.extend(value.split('\n').map(display_line));
            }
            rest = &rest[used..];
        }
    }

    let mask_all = requested.iter().any(|name| is_sensitive_key(name));
    entries.extend(
        rest.split(['\n', '\0'])
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                if mask_all {
                    "***".to_string()
                } else {
                    display_line(line)
                }
            }),
    );

    let count = entries.len();
    Some(FileResult::new(
        "env".to_string(),
        count,
        count,
        entries,
        None,
    ))
}

/// Parse a `KEY=value` listing and mask the sensitive values.
///
/// Records are NUL-separated when the output holds a NUL byte (`printenv -0`),
/// and newline-separated otherwise.  A known record (see [`KnownEnv`]) is
/// taken whole.  Other text goes through the line rule of [`ListingView`].
///
/// There is no line cap: every record goes through the mask, whatever the
/// size of the listing.  The listing is already in memory, and the parse is
/// one linear pass over it.
fn mask_listing(stdout: &str, known: Option<&KnownEnv>) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let terminator = if stdout.contains('\0') { '\0' } else { '\n' };
    let mut view = ListingView::default();
    let mut rest = stdout;
    // Bound: each pass removes at least one byte from `rest`.
    while !rest.is_empty() {
        if let Some((key, value, used)) = known.and_then(|k| k.record_at(rest, terminator)) {
            view.push_record(key, value);
            rest = &rest[used..];
            continue;
        }
        let (unit, tail) = rest.split_once(terminator).unwrap_or((rest, ""));
        if terminator == '\0' {
            view.push_nul_record(unit);
        } else {
            view.push_line(unit.strip_suffix('\r').unwrap_or(unit));
        }
        rest = tail;
    }
    view.finish()
}

/// The env view under construction.
#[derive(Default)]
struct ListingView {
    entries: Vec<String>,
    variables: usize,
    /// `None` before the first variable; `Some(masked)` for the current one.
    current_masked: Option<bool>,
    /// Lines that the line rule put inside a masked value, so not shown.
    hidden_lines: usize,
}

impl ListingView {
    /// A record with known edges: the whole value is masked, or shown line
    /// by line with the line mask.
    fn push_record(&mut self, key: &str, value: &str) {
        self.variables += 1;
        let masked = is_sensitive_key(key);
        self.current_masked = Some(masked);
        if masked {
            self.entries.push(format!("{key}=***"));
            return;
        }
        let mut lines = value.split('\n');
        let first = lines.next().unwrap_or_default();
        self.entries.push(format!("{key}={}", display_line(first)));
        self.entries.extend(lines.map(display_line));
    }

    /// One NUL-separated record whose edges are not known.  NUL marks the
    /// start of a variable, so the line state does not carry over.  Lines
    /// after the first go through the line rule, because a NUL byte on
    /// stdin does not prove that the text is `printenv -0` output.
    fn push_nul_record(&mut self, record: &str) {
        self.current_masked = None;
        if record.trim().is_empty() {
            return;
        }
        // The key comes from the first line only.
        let (first_line, more) = record.split_once('\n').unwrap_or((record, ""));
        let Some((key, first)) = first_line.split_once('=') else {
            // A record whose first line has no `=` has no key to judge it by:
            // fail closed.
            self.variables += 1;
            self.current_masked = Some(true);
            self.entries.push("***".to_string());
            return;
        };
        self.variables += 1;
        if is_sensitive_key(key) {
            self.current_masked = Some(true);
            self.entries.push(format!("{key}=***"));
            return;
        }
        self.current_masked = Some(false);
        self.entries.push(format!("{key}={}", display_line(first)));
        for line in more.split('\n') {
            self.push_line(line);
        }
    }

    /// One line whose record is not known.  A line that does not start with
    /// a portable name and `=` is a continuation of the previous value: it
    /// is hidden (and counted) when that value is masked, and shown with the
    /// line mask otherwise.
    fn push_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let inside_masked = self.current_masked == Some(true);
        let variable = match line.split_once('=') {
            // Inside a masked value, a line such as `abc==` (a padding
            // tail) is part of that value, not a variable.
            Some((key, value))
                if is_variable_name(key) && !(inside_masked && is_padding(value)) =>
            {
                Some((key, value))
            }
            // Outside a masked value, a sensitive key is always masked, also
            // when it is not a portable name (for example a non-ASCII key).
            Some((key, value)) if !inside_masked && is_sensitive_key(key) => Some((key, value)),
            _ => None,
        };

        match variable {
            Some((key, value)) => {
                self.variables += 1;
                let masked = is_sensitive_key(key);
                self.current_masked = Some(masked);
                let shown = if masked {
                    "***".to_string()
                } else {
                    display_line(value)
                };
                self.entries.push(format!("{key}={shown}"));
            }
            None if inside_masked => self.hidden_lines += 1,
            // Part of a value that is not masked, or text before the first
            // variable: shown, with the line mask.
            None => self.entries.push(display_line(line)),
        }
    }

    fn finish(self) -> Option<FileResult> {
        if self.variables == 0 {
            return None;
        }
        let footer = match self.hidden_lines {
            0 => None,
            1 => Some("1 line after a masked value is hidden".to_string()),
            n => Some(format!("{n} lines after a masked value are hidden")),
        };
        Some(FileResult::new(
            "env".to_string(),
            self.variables,
            self.variables,
            self.entries,
            footer,
        ))
    }
}

/// True when `value` is a non-empty run of `=` (the tail of base64 padding).
fn is_padding(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c == '=')
}

/// One line of a value that is not masked, as shown: a line `KEY=value`
/// with a sensitive `KEY` becomes `KEY=***`, and URL credentials are
/// replaced.
fn display_line(line: &str) -> String {
    match line.split_once('=') {
        Some((key, _)) if is_sensitive_key(key) => format!("{key}=***"),
        _ => redact_value(line),
    }
}

/// True when `key` is a portable variable name (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_variable_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The value as shown: URL credentials replaced, everything else unchanged.
fn redact_value(value: &str) -> String {
    if value.contains("://") && value.contains('@') {
        redact_url_credentials(value)
    } else {
        value.to_string()
    }
}

/// Return true if the key should have its value redacted.
///
/// Uses `eq_ignore_ascii_case` throughout to avoid `to_uppercase()` heap allocations
/// on every env variable (issue 3d).
///
/// [`SENSITIVE_EXACT`] / [`SENSITIVE_SUFFIXES`] are the canonical binary-facing
/// secret list (`tests/contract_secret_list_sync.rs` holds them in sync with
/// `rskim_contract::log`), so this predicate is also the single name
/// authority for `cmd::security`'s bare `NAME=VALUE` analytics scrubbing —
/// hence `pub(crate)` rather than private.
pub(crate) fn is_sensitive_key(key: &str) -> bool {
    // Exact match check — zero allocation
    if SENSITIVE_EXACT
        .iter()
        .any(|exact| key.eq_ignore_ascii_case(exact))
    {
        return true;
    }

    // Suffix check — zero allocation: compare trailing slice case-insensitively.
    // Guard with `is_char_boundary` before byte-slicing to avoid a panic when
    // `key.len() - suffix.len()` falls mid-UTF-8 codepoint (non-ASCII env keys).
    SENSITIVE_SUFFIXES.iter().any(|suffix| {
        let offset = key.len().wrapping_sub(suffix.len());
        key.len() >= suffix.len()
            && key.is_char_boundary(offset)
            && key[offset..].eq_ignore_ascii_case(suffix)
    })
}

/// Replace `scheme://user:pass@host` credentials with `***:***`.
fn redact_url_credentials(value: &str) -> String {
    RE_URL_CREDS
        .replace_all(value, "${1}***:***${2}")
        .into_owned()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::{load_fixture, make_output_full};

    /// A request for text on stdin: no names, no known records.
    fn stdin_request() -> EnvRequest {
        EnvRequest::new(&[], true, KnownEnv::from_pairs([]))
    }

    /// A request for `printenv NAME...` with no known records.
    fn named_request(names: &[String]) -> EnvRequest {
        EnvRequest {
            names: names.to_vec(),
            known: KnownEnv::from_pairs([]),
        }
    }

    /// A request for the output of a `printenv` child, with these records.
    fn child_request(args: &[&str], records: &[(&str, &str)]) -> EnvRequest {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let known =
            KnownEnv::from_pairs(records.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        EnvRequest::new(&args, false, known)
    }

    /// The listing parse for text on stdin.
    fn stdin_listing(input: &str) -> Option<FileResult> {
        mask_listing(input, None)
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn test_tier1_env_basic() {
        let input = load_fixture("file", "env_basic.txt");
        let result = stdin_listing(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 20, "20 variables in env_basic.txt");
        // Safe keys are preserved
        assert!(
            result.entries.iter().any(|e| e.starts_with("PATH=")),
            "PATH should appear unredacted"
        );
    }

    #[test]
    fn test_redacts_suffix_token() {
        let input = "MY_API_TOKEN=super-secret-value\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "MY_API_TOKEN=***");
    }

    #[test]
    fn test_redacts_suffix_secret() {
        let input = "APP_SECRET=very-secret\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "APP_SECRET=***");
    }

    #[test]
    fn test_redacts_suffix_password() {
        let input = "DB_PASSWORD=hunter2\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "DB_PASSWORD=***");
    }

    #[test]
    fn test_redacts_suffix_key() {
        // _KEY alone is no longer a sensitive suffix; _API_KEY and _SECRET_KEY are.
        let input = "SIGNING_API_KEY=abcdef1234\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "SIGNING_API_KEY=***");
    }

    #[test]
    fn test_redacts_secret_key_suffix() {
        let input = "MY_SECRET_KEY=abcdef1234\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "MY_SECRET_KEY=***");
    }

    #[test]
    fn test_redacts_private_key_suffix() {
        let input = "RSA_PRIVATE_KEY=-----BEGIN RSA PRIVATE KEY-----\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "RSA_PRIVATE_KEY=***");
    }

    #[test]
    fn test_no_false_positive_registry_key() {
        // REGISTRY_KEY ends with _KEY but NOT with _API_KEY, _SECRET_KEY, or _PRIVATE_KEY.
        // It must NOT be redacted.
        let input = "REGISTRY_KEY=HKLM\\\\Software\\\\Example\n";
        let result = stdin_listing(input).unwrap();
        assert!(
            result.entries[0].starts_with("REGISTRY_KEY=HKLM"),
            "REGISTRY_KEY should NOT be redacted, got: {}",
            result.entries[0]
        );
    }

    #[test]
    fn test_redacts_suffix_credential() {
        let input = "SERVICE_CREDENTIAL=cred-value\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "SERVICE_CREDENTIAL=***");
    }

    #[test]
    fn test_redacts_suffix_auth() {
        let input = "MY_CUSTOM_AUTH=bearer-token\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(result.entries[0], "MY_CUSTOM_AUTH=***");
    }

    #[test]
    fn test_redacts_exact_keys() {
        for key in SENSITIVE_EXACT {
            let input = format!("{key}=some-value\n");
            let result = stdin_listing(&input).unwrap();
            assert_eq!(
                result.entries[0],
                format!("{key}=***"),
                "Key {key} should be redacted"
            );
        }
    }

    #[test]
    fn test_redacts_url_credentials() {
        let input = load_fixture("file", "env_url_creds.txt");
        let result = stdin_listing(&input).unwrap();

        // DATABASE_URL is an exact key match → fully redacted
        let db_entry = result
            .entries
            .iter()
            .find(|e| e.starts_with("DATABASE_URL="))
            .expect("DATABASE_URL should be present");
        assert_eq!(
            db_entry, "DATABASE_URL=***",
            "DATABASE_URL exact match redacts entirely"
        );

        // REDIS_URL has URL credentials → partial redaction
        let redis_entry = result
            .entries
            .iter()
            .find(|e| e.starts_with("REDIS_URL="))
            .expect("REDIS_URL should be present");
        assert!(
            redis_entry.contains("***:***"),
            "REDIS_URL should have URL creds redacted, got: {redis_entry}"
        );
        assert!(
            !redis_entry.contains("redis_pass123"),
            "Password should not appear in output"
        );

        // SAFE_URL has no credentials — preserved
        let safe_entry = result
            .entries
            .iter()
            .find(|e| e.starts_with("SAFE_URL="))
            .expect("SAFE_URL should be present");
        assert!(
            safe_entry.contains("https://example.com"),
            "Safe URL should be preserved, got: {safe_entry}"
        );
    }

    #[test]
    fn test_preserves_safe_keys() {
        let input = "HOME=/home/dean\nSHELL=/bin/zsh\n";
        let result = stdin_listing(input).unwrap();
        assert!(result.entries.iter().any(|e| e == "HOME=/home/dean"));
        assert!(result.entries.iter().any(|e| e == "SHELL=/bin/zsh"));
    }

    #[test]
    fn test_no_false_positive_partial_match() {
        // MY_TOKEN_COUNT ends with digits after _COUNT, not _TOKEN directly
        // BUT: MY_TOKEN_COUNT ends with _COUNT which is not a sensitive suffix.
        // The key MY_TOKEN_COUNT does NOT end with _TOKEN.
        let input = "MY_TOKEN_COUNT=5\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.entries[0], "MY_TOKEN_COUNT=5",
            "MY_TOKEN_COUNT should NOT be redacted (does not end with _TOKEN)"
        );
    }

    #[test]
    fn test_case_insensitive_key_match() {
        // Keys should be matched case-insensitively
        let input = "github_token=ghp_test\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.entries[0], "github_token=***",
            "Lowercase github_token should still be redacted"
        );
    }

    #[test]
    fn test_case_insensitive_suffix_match() {
        // Verifies the suffix path (not the exact-match path) works case-insensitively.
        // "my_app_password" is not in SENSITIVE_EXACT, so it must be caught by the
        // suffix "_PASSWORD" even though the key is entirely lowercase.
        let input = "my_app_password=hunter2\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.entries[0], "my_app_password=***",
            "Lowercase suffix _password should be redacted via case-insensitive suffix match"
        );
    }

    #[test]
    fn test_no_panic_on_non_ascii_key() {
        // Non-ASCII env key: byte length of the key may not align with suffix byte
        // boundaries, which previously caused a panic in byte-slice indexing.
        // "clé_TOKEN" — "clé" contains a 2-byte UTF-8 sequence (é = U+00E9).
        let input = "cl\u{00e9}_TOKEN=secret\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.entries[0], "cl\u{00e9}_TOKEN=***",
            "Non-ASCII key ending with _TOKEN should be redacted without panic"
        );
    }

    #[test]
    fn test_redacts_new_compound_key_suffixes() {
        // Verify the four compound suffixes added for crypto/signing secrets.
        // These are PREFIXED keys ending with the compound suffix, e.g. APP_ENCRYPTION_KEY.
        // Bare names like ENCRYPTION_KEY (no prefix) would need to be in SENSITIVE_EXACT.
        let cases = [
            ("APP_ENCRYPTION_KEY=enc-val", "APP_ENCRYPTION_KEY=***"),
            ("SERVICE_SIGNING_KEY=sign-val", "SERVICE_SIGNING_KEY=***"),
            ("AWS_ACCESS_KEY=access-val", "AWS_ACCESS_KEY=***"),
            ("APP_HMAC_KEY=hmac-val", "APP_HMAC_KEY=***"),
        ];
        for (input_line, expected) in &cases {
            let result = stdin_listing(&format!("{input_line}\n")).unwrap();
            assert_eq!(
                result.entries[0], *expected,
                "Expected {expected}, got {}",
                result.entries[0]
            );
        }
    }

    // =========================================================================
    // Regression tests for fam-env-security fixes
    // =========================================================================

    // ISSUE 1 (SECURITY): Short-secret redaction must not be subject to byte
    // arithmetic.  Before the fix, `skip_net_savings_guard: false` caused the
    // net-savings guard to fall back to verbatim raw output when the redacted
    // form was no shorter — leaking secrets whose ciphertext is shorter than
    // "***".  These tests exercise the `mask_listing` layer directly; the
    // guard itself lives in execution.rs and is not reachable from a unit test.
    // The unit tests confirm that `mask_listing` always redacts regardless of
    // secret length.  An e2e test in crates/rskim/tests/ would be the honest
    // place to verify the guard is actually bypassed end-to-end — these unit
    // tests are the closest observable layer available without a running binary.
    #[test]
    fn test_redacts_short_github_token() {
        // 2-char secret: redacted form "***" is LONGER than "ab" in bytes.
        // Before the fix the guard rejected the compressed view and leaked "ab".
        let input = "GITHUB_TOKEN=ab\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.entries[0], "GITHUB_TOKEN=***",
            "Short GITHUB_TOKEN must be redacted regardless of byte length"
        );
    }

    #[test]
    fn test_redacts_short_npm_token() {
        // 2-char secret: same guard-bypass scenario as GITHUB_TOKEN=ab.
        let input = "NPM_TOKEN=xy\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.entries[0], "NPM_TOKEN=***",
            "Short NPM_TOKEN must be redacted regardless of byte length"
        );
    }

    #[test]
    fn test_redacts_medium_github_token() {
        // 8-char secret: previously verified to leak verbatim (issue evidence).
        let input = "GITHUB_TOKEN=abcd1234\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.entries[0], "GITHUB_TOKEN=***",
            "8-char GITHUB_TOKEN must be redacted"
        );
    }

    /// Render a parse result as the text that reaches stdout, for the
    /// "no fake value anywhere" assertions below.
    fn rendered(result: &ParseResult<FileResult>) -> String {
        match result {
            ParseResult::Full(r) | ParseResult::Degraded(r, _) => r.as_ref().to_string(),
            ParseResult::Passthrough(raw) => raw.clone(),
            ParseResult::RawPassthrough => panic!("the env handler never returns RawPassthrough"),
        }
    }

    // A listing above the shared input line cap is masked like any other
    // listing: there is no size at which the raw text is served.
    #[test]
    fn test_large_listing_is_masked() {
        let mut input = String::from("FAKE_TOKEN=fake-value-123\n");
        for i in 0..=crate::cmd::file::MAX_INPUT_LINES {
            input.push_str(&format!("VAR_{i}=x\n"));
        }
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output, &stdin_request());
        assert!(result.is_full(), "got {}", result.tier_name());
        let text = rendered(&result);
        assert!(text.contains("FAKE_TOKEN=***"));
        assert!(!text.contains("fake-value-123"));
        assert!(text.contains("VAR_0=x"), "plain values stay visible");
    }

    // A value over several lines: a continuation line that holds `=` stays
    // behind the mask of its variable.
    #[test]
    fn test_continuation_with_equals_sign_stays_masked() {
        let input = "FAKE_TOKEN=fake-part-one\nfake-part-two=fake-part-three\nPLAIN=ok\n";
        let result = stdin_listing(input).unwrap();
        let text = result.as_ref().to_string();
        assert!(text.contains("FAKE_TOKEN=***"));
        assert!(
            !text.contains("fake-part"),
            "no part of the value shows: {text}"
        );
        assert!(text.contains("PLAIN=ok"));
        assert_eq!(result.total_count, 2);
    }

    // A padding tail such as `abc==` inside a masked value is part of it.
    #[test]
    fn test_padding_tail_of_masked_value_stays_masked() {
        let input = "FAKE_PRIVATE_KEY=-----BEGIN FAKE-----\nZmFrZWZha2VmYWtl\nZmFrZQ==\n-----END FAKE-----\nPLAIN=ok\n";
        let text = stdin_listing(input).unwrap().as_ref().to_string();
        assert!(text.contains("FAKE_PRIVATE_KEY=***"));
        assert!(!text.contains("ZmFrZ"), "no body line shows: {text}");
        assert!(!text.contains("FAKE-----"), "no armour line shows: {text}");
        assert!(text.contains("PLAIN=ok"));
    }

    // A value over several lines that is not sensitive stays visible.
    #[test]
    fn test_continuation_of_plain_value_is_shown() {
        let input = "PLAIN=line-one\nline-two\nOTHER=ok\n";
        let text = stdin_listing(input).unwrap().as_ref().to_string();
        assert!(text.contains("PLAIN=line-one"));
        assert!(text.contains("line-two"));
        assert!(text.contains("OTHER=ok"));
    }

    // NUL-separated records (`printenv -0`): each record is masked by its key,
    // and a record with no `=` is masked whole.
    #[test]
    fn test_nul_records_are_masked() {
        let input = "PLAIN=1\0FAKE_TOKEN=fake-value-123\nfake-line-two\0fake-orphan\0";
        let output = make_output_full(input, "", Some(0));
        let result = parse_impl(&output, &stdin_request());
        assert!(result.is_full(), "got {}", result.tier_name());
        let text = rendered(&result);
        assert!(text.contains("PLAIN=1"));
        assert!(text.contains("FAKE_TOKEN=***"));
        assert!(!text.contains("fake-"), "no fake value shows: {text}");
    }

    // `printenv NAME`: bare values. A sensitive name masks every line.
    #[test]
    fn test_named_sensitive_value_is_masked() {
        let output = make_output_full("fake-value-123\nfake-line-two\n", "", Some(0));
        let requested = vec!["FAKE_TOKEN".to_string(), "PLAIN".to_string()];
        let result = parse_impl(&output, &named_request(&requested));
        assert!(result.is_full(), "got {}", result.tier_name());
        let text = rendered(&result);
        assert!(!text.contains("fake-"), "no fake value shows: {text}");
        assert!(text.contains("***"));
    }

    // `printenv NAME` for a name that is not sensitive: the value shows, with
    // URL credentials replaced.
    #[test]
    fn test_named_plain_value_is_shown_with_url_redaction() {
        let output = make_output_full("https://fakeuser:fakepass@example.invalid/x\n", "", Some(0));
        let requested = vec!["PLAIN_URL".to_string()];
        let text = rendered(&parse_impl(&output, &named_request(&requested)));
        assert!(text.contains("example.invalid/x"));
        assert!(!text.contains("fakeuser"));
        assert!(!text.contains("fakepass"));
    }

    #[test]
    fn test_requested_names_skip_flags() {
        let args: Vec<String> = ["-0", "FAKE_TOKEN", "--null", "PLAIN"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(requested_names(&args), vec!["FAKE_TOKEN", "PLAIN"]);
    }

    #[test]
    fn test_is_variable_name() {
        assert!(is_variable_name("A_1"));
        assert!(is_variable_name("_X"));
        assert!(!is_variable_name("fake-part"));
        assert!(!is_variable_name("1X"));
        assert!(!is_variable_name(""));
        assert!(!is_variable_name("cl\u{00e9}_TOKEN"));
    }

    // A key that is not a portable name but is sensitive is still masked,
    // after a plain value too.
    #[test]
    fn test_sensitive_key_with_unusual_name_is_masked_after_plain_value() {
        let input = "PLAIN=ok\nfake-app_TOKEN=fake-value-123\n";
        let text = stdin_listing(input).unwrap().as_ref().to_string();
        assert!(text.contains("fake-app_TOKEN=***"));
        assert!(!text.contains("fake-value-123"), "{text}");
    }

    // ISSUE 3: All variables must be shown when count <= MAX_INPUT_LINES.
    // Before the fix, entries past MAX_DISPLAY_ENTRIES (100) were silently dropped.
    #[test]
    fn test_no_silent_variable_drop_above_display_cap() {
        // Build an input with 159 variables (previously measured to drop 59 entries).
        let count = 159usize;
        let mut input = String::new();
        for i in 0..count {
            input.push_str(&format!("MY_VAR_{i:03}=value_{i}\n"));
        }
        let result = stdin_listing(&input).unwrap();
        assert_eq!(
            result.total_count, count,
            "total_count must reflect all {count} variables"
        );
        assert_eq!(
            result.entries.len(),
            count,
            "All {count} variables must appear in entries; none should be silently dropped"
        );
        // No elision footer when all variables are shown.
        assert!(
            result.footer.is_none(),
            "elision footer must be absent when all variables are shown"
        );
    }

    #[test]
    fn test_tier3_empty_passthrough() {
        let output = make_output_full("", "", Some(1));
        let result = parse_impl(&output, &stdin_request());
        assert!(
            result.is_passthrough(),
            "Empty output should be passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("file", "env_basic.txt");
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output, &stdin_request());
        assert!(
            result.is_full(),
            "parse_impl with exit code 0 and valid env output should return Full, got {}",
            result.tier_name()
        );
    }

    // ------------------------------------------------------------------
    // Child output: exact records from skim's own environment
    // ------------------------------------------------------------------

    // A masked value over several lines whose second line has the shape of
    // a variable: the known record holds both lines, so neither shows.
    #[test]
    fn test_child_value_with_name_shaped_line_stays_masked() {
        let value = "fake-part-one\nfakeparttwo=fake-part-three";
        let stdout = format!("FAKE_TOKEN={value}\nPLAIN=ok\n");
        let request = child_request(&[], &[("FAKE_TOKEN", value), ("PLAIN", "ok")]);
        let result = parse_impl(&make_output_full(&stdout, "", Some(0)), &request);
        let text = rendered(&result);
        assert!(text.contains("FAKE_TOKEN=***"), "{text}");
        assert!(
            !text.contains("fake-part"),
            "no part of the value shows: {text}"
        );
        assert!(text.contains("PLAIN=ok"), "{text}");
    }

    // The same, for a value that starts with a newline.
    #[test]
    fn test_child_value_that_starts_with_newline_stays_masked() {
        let value = "\nfakeparttwo=fake-part-three";
        let stdout = format!("FAKE_TOKEN={value}\nPLAIN=ok\n");
        let request = child_request(&[], &[("FAKE_TOKEN", value), ("PLAIN", "ok")]);
        let text = rendered(&parse_impl(
            &make_output_full(&stdout, "", Some(0)),
            &request,
        ));
        assert!(!text.contains("fake-part"), "{text}");
        assert!(text.contains("PLAIN=ok"), "{text}");
    }

    // A variable with a name that is not portable, and an exported shell
    // function, after a masked variable: both are known records, so both
    // show and are counted.
    #[test]
    fn test_child_variables_after_masked_value_are_shown() {
        let function = "() {  echo fake-body\n}";
        let stdout = format!(
            "FAKE_TOKEN=fake-value-123\nfake-name=fake-visible\nBASH_FUNC_fakefn%%={function}\n"
        );
        let request = child_request(
            &[],
            &[
                ("FAKE_TOKEN", "fake-value-123"),
                ("fake-name", "fake-visible"),
                ("BASH_FUNC_fakefn%%", function),
            ],
        );
        let result = parse_impl(&make_output_full(&stdout, "", Some(0)), &request);
        let ParseResult::Full(file) = &result else {
            panic!("got {}", result.tier_name());
        };
        let text = rendered(&result);
        assert!(!text.contains("fake-value-123"), "{text}");
        assert!(text.contains("fake-name=fake-visible"), "{text}");
        assert!(
            text.contains("BASH_FUNC_fakefn%%=() {  echo fake-body"),
            "{text}"
        );
        assert!(
            text.contains("\n }"),
            "the body of the function shows: {text}"
        );
        assert_eq!(file.total_count, 3);
        assert!(file.footer.is_none(), "nothing is hidden: {text}");
    }

    // A real `printenv` child: the known records match its output exactly,
    // also for a value over several lines and a name that is not portable.
    #[cfg(unix)]
    #[test]
    fn test_real_printenv_child_records_are_exact() {
        let pairs = [
            ("FAKE_TOKEN", "fake-part-one\nfakeparttwo=fake-part-three"),
            ("fake-name", "fake-visible"),
            ("PLAIN", "ok"),
        ];
        let output = std::process::Command::new("printenv")
            .env_clear()
            .envs(pairs)
            .output()
            .expect("printenv is installed on unix");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let known = KnownEnv::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let result = mask_listing(&stdout, Some(&known)).expect("a listing");
        let text = result.as_ref();
        assert!(!text.contains("fake-part"), "{text}");
        assert!(text.contains("FAKE_TOKEN=***"), "{text}");
        assert!(text.contains("fake-name=fake-visible"), "{text}");
        assert!(text.contains("PLAIN=ok"), "{text}");
        assert_eq!(result.total_count, 3, "{text}");
        assert!(result.footer.is_none(), "{text}");
    }

    // A line that no known record accounts for, after a masked record: it
    // is hidden, and the footer counts it.
    #[test]
    fn test_child_unknown_line_after_masked_value_is_counted() {
        let stdout = "FAKE_TOKEN=fake-value-123\nfake-unknown-line\nPLAIN=ok\n";
        let request = child_request(&[], &[("FAKE_TOKEN", "fake-value-123"), ("PLAIN", "ok")]);
        let result = parse_impl(&make_output_full(stdout, "", Some(0)), &request);
        let text = rendered(&result);
        assert!(!text.contains("fake-"), "{text}");
        assert!(
            text.contains("1 line after a masked value is hidden"),
            "{text}"
        );
        assert!(text.contains("PLAIN=ok"), "{text}");
    }

    // NUL records from a child: a plain value over lines shows whole.
    #[test]
    fn test_child_nul_records_are_exact() {
        let stdout = "PLAIN=line-one\nline-two\0FAKE_TOKEN=fake-a\nfake-b\0";
        let request = child_request(
            &["-0"],
            &[
                ("PLAIN", "line-one\nline-two"),
                ("FAKE_TOKEN", "fake-a\nfake-b"),
            ],
        );
        assert!(request.names.is_empty(), "-0 is a flag, not a name");
        let text = rendered(&parse_impl(
            &make_output_full(stdout, "", Some(0)),
            &request,
        ));
        assert!(text.contains("PLAIN=line-one"), "{text}");
        assert!(text.contains("line-two"), "{text}");
        assert!(text.contains("FAKE_TOKEN=***"), "{text}");
        assert!(!text.contains("fake-"), "{text}");
    }

    // `printenv NAME...` from a child: each value is matched and masked by
    // its own name, also over several lines.
    #[test]
    fn test_child_named_values_are_masked_by_name() {
        let stdout = "fake-plain\nfake-a\nfake-b\n";
        let request = child_request(
            &["PLAIN", "UNSET_NAME", "FAKE_TOKEN"],
            &[("PLAIN", "fake-plain"), ("FAKE_TOKEN", "fake-a\nfake-b")],
        );
        let result = parse_impl(&make_output_full(stdout, "", Some(0)), &request);
        let ParseResult::Full(file) = &result else {
            panic!("got {}", result.tier_name());
        };
        assert_eq!(file.entries, vec!["fake-plain", "***"]);
    }

    // A failed child (exit 1) still goes through the mask.
    #[test]
    fn test_child_named_value_with_exit_one_is_masked() {
        let request = child_request(
            &["FAKE_TOKEN", "UNSET_NAME"],
            &[("FAKE_TOKEN", "fake-value-123")],
        );
        let text = rendered(&parse_impl(
            &make_output_full("fake-value-123\n", "", Some(1)),
            &request,
        ));
        assert!(!text.contains("fake-value-123"), "{text}");
    }

    // A plain named value whose line has a sensitive key shape gets the
    // line mask.
    #[test]
    fn test_named_plain_value_with_sensitive_line_is_masked() {
        let value = "fake-head\nX_TOKEN=fake-inner";
        let request = child_request(&["PLAIN_BLOB"], &[("PLAIN_BLOB", value)]);
        let text = rendered(&parse_impl(
            &make_output_full(&format!("{value}\n"), "", Some(0)),
            &request,
        ));
        assert!(text.contains("fake-head"), "{text}");
        assert!(text.contains("X_TOKEN=***"), "{text}");
        assert!(!text.contains("fake-inner"), "{text}");
    }

    // ------------------------------------------------------------------
    // Text on stdin: the line rule with a counted footer
    // ------------------------------------------------------------------

    // With stdin, `run` is a routing word, not a `printenv` name: the text
    // is a listing and gets the key mask.
    #[test]
    fn test_run_word_with_stdin_is_a_listing() {
        let request = EnvRequest::new(&strings(&["run"]), true, KnownEnv::from_pairs([]));
        assert!(request.names.is_empty());
        let input = "FAKE_TOKEN=fake-value-123\nPLAIN=ok\n";
        let text = rendered(&parse_impl(&make_output_full(input, "", Some(0)), &request));
        assert!(text.contains("FAKE_TOKEN=***"), "{text}");
        assert!(!text.contains("fake-value-123"), "{text}");
    }

    // A padding line right after a masked variable is part of its value.
    #[test]
    fn test_padding_first_line_of_masked_value_stays_masked() {
        let input = "FAKE_PRIVATE_KEY=fake-part-one\nZmFrZQ==\nPLAIN=ok\n";
        let text = stdin_listing(input).unwrap().as_ref().to_string();
        assert!(!text.contains("ZmFrZQ"), "{text}");
        assert!(text.contains("PLAIN=ok"), "{text}");
        assert!(
            text.contains("1 line after a masked value is hidden"),
            "{text}"
        );
    }

    // One NUL byte in a newline listing: the line mask still applies inside
    // the record.
    #[test]
    fn test_stray_nul_keeps_the_line_mask() {
        let input = "PLAIN=1\nFAKE_TOKEN=fake-value-123\n\0";
        let text = rendered(&parse_impl(
            &make_output_full(input, "", Some(0)),
            &stdin_request(),
        ));
        assert!(text.contains("PLAIN=1"), "{text}");
        assert!(text.contains("FAKE_TOKEN=***"), "{text}");
        assert!(!text.contains("fake-value-123"), "{text}");
    }

    // A line hidden after a masked value on stdin is counted, never lost
    // with no marker.
    #[test]
    fn test_stdin_hidden_line_is_counted() {
        let input = "FAKE_TOKEN=fake-value-123\nfake-name=fake-visible\nfake-more\nPLAIN=ok\n";
        let result = stdin_listing(input).unwrap();
        assert_eq!(
            result.footer.as_deref(),
            Some("2 lines after a masked value are hidden")
        );
        assert!(result.as_ref().contains("PLAIN=ok"));
    }

    // An empty variable after a masked one is a variable, not padding.
    #[test]
    fn test_empty_variable_after_masked_value_is_shown() {
        let input = "FAKE_TOKEN=fake-value-123\nEMPTY_VAR=\n";
        let result = stdin_listing(input).unwrap();
        assert!(
            result.as_ref().contains("EMPTY_VAR="),
            "{}",
            result.as_ref()
        );
        assert_eq!(result.total_count, 2);
        assert!(result.footer.is_none());
    }

    // ------------------------------------------------------------------
    // The `env` grammar
    // ------------------------------------------------------------------

    /// The env view of a route.
    fn view(args: &[&str]) -> EnvView {
        match env_route(&strings(args)) {
            EnvRoute::View(view) => view,
            EnvRoute::Exec => panic!("env {args:?} must be an env view"),
        }
    }

    fn set(name: &str, value: &str) -> EnvEdit {
        EnvEdit::Set(name.to_string(), value.to_string())
    }

    fn unset(name: &str) -> EnvEdit {
        EnvEdit::Unset(name.to_string())
    }

    // No option or assignment: skim starts `printenv` directly.
    #[test]
    fn test_env_route_direct_printenv_views() {
        assert_eq!(view(&[]), EnvView::printenv(Vec::new()));
        assert_eq!(view(&["PATH"]), EnvView::printenv(strings(&["PATH"])));
        assert_eq!(
            view(&["printenv", "FAKE_TOKEN"]),
            EnvView::printenv(strings(&["FAKE_TOKEN"]))
        );
        assert_eq!(view(&["-0"]), EnvView::printenv(strings(&["-0"])));
    }

    // Options and assignments stay, so the real `env` applies them.
    #[test]
    fn test_env_route_views_through_env() {
        let v = view(&["-u", "FAKE_X", "FOO=1", "printenv"]);
        assert_eq!(v.env_words, strings(&["-u", "FAKE_X", "FOO=1"]));
        assert_eq!(v.child, "printenv");
        assert_eq!(v.edits, vec![unset("FAKE_X"), set("FOO", "1")]);

        let v = view(&["FOO=1", "/usr/bin/printenv", "FAKE_TOKEN"]);
        assert_eq!(v.env_words, strings(&["FOO=1"]));
        assert_eq!(v.child, "/usr/bin/printenv");
        assert_eq!(v.printenv_args, strings(&["FAKE_TOKEN"]));

        let v = view(&["-i", "FOO=fake-one", "printenv", "FOO"]);
        assert_eq!(v.edits, vec![EnvEdit::Clear, set("FOO", "fake-one")]);

        let v = view(&["FOO=1"]);
        assert_eq!(v.env_words, strings(&["FOO=1"]));
        assert_eq!(v.child, "printenv");
        assert!(v.printenv_args.is_empty());

        let v = view(&["-u", "FAKE_X", "--null"]);
        assert_eq!(v.env_words, strings(&["-u", "FAKE_X"]));
        assert_eq!(v.printenv_args, strings(&["-0"]));

        let v = view(&["FOO=1", "env", "-u", "FAKE_X", "printenv"]);
        assert_eq!(v.env_words, strings(&["FOO=1", "env", "-u", "FAKE_X"]));
        assert_eq!(v.edits, vec![set("FOO", "1"), unset("FAKE_X")]);

        assert_eq!(view(&["-uFAKE_X", "printenv"]).edits, vec![unset("FAKE_X")]);
        assert_eq!(
            view(&["--uns", "FAKE_X", "printenv"]).edits,
            vec![unset("FAKE_X")]
        );
        assert_eq!(
            view(&["--unset=FAKE_X", "printenv"]).edits,
            vec![unset("FAKE_X")]
        );
        assert_eq!(
            view(&["--", "FOO=1", "printenv"]).edits,
            vec![set("FOO", "1")]
        );
        assert_eq!(view(&["-i", "/usr/bin/env"]).edits, vec![EnvEdit::Clear]);
    }

    // `-S` lines are split with the quote and escape rules.
    #[test]
    fn test_env_route_split_string() {
        let v = view(&["-S", "FOO='fake a' printenv"]);
        assert_eq!(v.env_words, strings(&["FOO=fake a"]));
        assert_eq!(v.edits, vec![set("FOO", "fake a")]);
        assert_eq!(view(&["-S", "pri''ntenv"]).child, "printenv");
        assert_eq!(view(&["-S", "pri\"nt\"env"]).child, "printenv");
        assert_eq!(view(&["--split-string=printenv"]).child, "printenv");
        assert_eq!(view(&["-S'printenv'"]).child, "printenv");
        assert_eq!(
            view(&["-S", "printenv\\_FAKE_TOKEN"]).printenv_args,
            strings(&["FAKE_TOKEN"])
        );
        assert_eq!(view(&["-S", "-S printenv"]).child, "printenv");
    }

    // A `-S` line that skim cannot read gives the plain listing view, which
    // starts no other program.
    #[test]
    fn test_env_route_unreadable_split_string_is_the_listing() {
        let listing = EnvView::printenv(Vec::new());
        for line in ["${FAKE_CMD}", "'open", "printenv \\z"] {
            assert_eq!(view(&["-S", line]), listing, "-S {line:?}");
        }
        assert_eq!(view(&["-iSprintenv"]), listing);
        let nested = format!("{}printenv", "-S ".repeat(10));
        assert_eq!(view(&["-S", &nested]), listing);
    }

    #[test]
    fn test_env_route_other_children_exec() {
        let route = |args: &[&str]| env_route(&strings(args));
        assert_eq!(route(&["FOO=1", "npm", "test"]), EnvRoute::Exec);
        assert_eq!(route(&["-u", "printenv", "npm"]), EnvRoute::Exec);
        assert_eq!(route(&["-S", "npm test"]), EnvRoute::Exec);
        assert_eq!(route(&["-S", "FOO='fake a' npm"]), EnvRoute::Exec);
        assert_eq!(route(&["env", "FOO=1", "npm"]), EnvRoute::Exec);
        // Known limit: skim cannot inspect what `sh -c` prints.
        assert_eq!(route(&["FOO=1", "sh", "-c", "env"]), EnvRoute::Exec);
    }

    #[test]
    fn test_known_env_with_edits() {
        let pairs = [("A", "1"), ("B", "2")].map(|(k, v)| (k.to_string(), v.to_string()));
        let known = KnownEnv::from_pairs(pairs).with_edits(&[unset("A"), set("C", "3")]);
        let mut names: Vec<&str> = known.values.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["B", "C"]);
        assert!(known.with_edits(&[EnvEdit::Clear]).values.is_empty());
    }

    // ------------------------------------------------------------------
    // More stdin and child cases
    // ------------------------------------------------------------------

    // A NUL record takes its key from its first line only.
    #[test]
    fn test_nul_record_key_comes_from_first_line() {
        let input = "fake-head\nAWS_ACCESS_KEY_ID=fake-value-123\0PLAIN=ok\0";
        let text = rendered(&parse_impl(
            &make_output_full(input, "", Some(0)),
            &stdin_request(),
        ));
        assert!(!text.contains("fake-value-123"), "{text}");
        assert!(text.contains("PLAIN=ok"), "{text}");
    }

    // The handler starts the child when stdin is empty, so the known records
    // apply also when the handler could read stdin.
    #[test]
    fn test_known_records_apply_when_the_handler_could_read_stdin() {
        let value = "fake-part-one\nfakeparttwo=fake-part-three";
        let known = KnownEnv::from_pairs(
            [("FAKE_TOKEN", value), ("PLAIN", "ok")].map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let request = EnvRequest::new(&[], true, known);
        let stdout = format!("FAKE_TOKEN={value}\nPLAIN=ok\n");
        let result = parse_impl(&make_output_full(&stdout, "", Some(0)), &request);
        let ParseResult::Full(file) = &result else {
            panic!("got {}", result.tier_name());
        };
        let text = rendered(&result);
        assert!(!text.contains("fake-part"), "{text}");
        assert!(text.contains("PLAIN=ok"), "{text}");
        assert!(file.footer.is_none(), "{text}");
    }
}
