//! Search subcommand — code search via layered n-gram indexing.
//!
//! # Architecture
//!
//! All I/O lives here (this module). Business logic is split across:
//! - `types` — shared configuration and result types
//! - `walk` — project-root discovery and file traversal
//! - `manifest` — binary (v5) sidecar for incremental build caching
//! - `index` — full pipeline orchestration (invoked via `--build`/`--rebuild`)
//! - `query` — query execution and result formatting
//! - `snippet` — source context extraction
//! - `staleness` — git HEAD comparison and auto-refresh
//! - `hooks` — git hook installation/removal
//! - `rskim-search` crate — index building, n-gram extraction, BM25F scoring

mod build_lock;
// The classifier serves only the role agreement test until the scan engine
// reads it; the engine commit lifts the `cfg(test)` gate.
#[cfg(test)]
mod classifier;
#[cfg(test)]
mod field;
#[cfg(test)]
mod fields;
mod gitdir;
pub(crate) mod hooks;
mod index;
mod manifest;
mod query;
#[cfg(test)]
mod roles_tests;
mod snippet;
mod staleness;
mod types;
mod walk;

use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

// ============================================================================
// User-facing message constants
// ============================================================================

/// Canonical enumeration of all recognised flags for `skim search`.
///
/// Single source of truth: used in the unknown-flag error message so that
/// adding or renaming a flag requires only one edit here rather than separate
/// edits to the error string and the doc comment.  The help text at
/// `print_help` is intentionally separate (different format and prose
/// descriptions).
const KNOWN_FLAGS: &str = "--build, --rebuild, --update, --stats, --install-hooks, \
    --remove-hooks, --json, -j, --limit, -n, --offset, --root, --phrase, --near, --lang, \
    --help, -h";

/// Exit code for a flag that `skim search` no longer accepts.
const REMOVED_FLAG_EXIT_CODE: u8 = 2;

/// Flags that `skim search` no longer accepts, each with the advice that
/// replaces it.
///
/// A removed flag is refused during flag parsing, before any file is read or
/// written. The refusal prints one line on stderr and nothing on stdout.
const REMOVED_FLAGS: [(&str, &str); 6] = [
    ("--ast", "use ast-grep for structural search"),
    ("--hot", "use skim heatmap for churn and fix density"),
    ("--cold", "use skim heatmap for churn and fix density"),
    ("--risky", "use skim heatmap for churn and fix density"),
    (
        "--blast-radius",
        "use skim heatmap, or git log --name-only -- FILE, for co-change",
    ),
    ("--weights", "there is no fused ranking"),
];

/// The refusal message for `arg` when it names a removed flag, in the bare
/// (`--ast`) or the attached-value (`--ast=x`) form.
///
/// The message quotes the flag name from [`REMOVED_FLAGS`], never the argument
/// text, so an attached value cannot reach the terminal.
fn removed_flag_message(arg: &str) -> Option<String> {
    REMOVED_FLAGS.iter().find_map(|(flag, advice)| {
        let is_flag = arg == *flag
            || arg
                .strip_prefix(flag)
                .is_some_and(|rest| rest.starts_with('='));
        is_flag.then(|| format!("skim search: {flag} was removed in 3.0.0; {advice}"))
    })
}

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search` subcommand.
///
/// Dispatches to:
/// - `skim search --build` — build the index incrementally
/// - `skim search --rebuild` — force full rebuild
/// - `skim search --update` — auto-refresh if stale
/// - `skim search --stats [--json]` — print index statistics
/// - `skim search --install-hooks` — install git hooks
/// - `skim search --remove-hooks` — remove git hooks
/// - `skim search [--json] [--limit N] <QUERY>` — search
/// - A removed flag (`--ast`, `--hot`, `--cold`, `--risky`, `--blast-radius`,
///   `--weights`) — refuse with exit 2 and name the replacement
/// - No args / `--help` / `-h` — print help
///
/// # AD-375-1 — The `index` positional was removed (#375, avoids PF-006).
///
/// Prior to this change, a leading bareword `index` was intercepted and routed
/// to the index builder, making `skim search "index"` unsearchable regardless of
/// quoting (the shell strips quotes before argv dispatch). The word "index" appears
/// 193+ times in this repo, so it is a valid and useful query term.
///
/// The positional intercept shadowed the query path with a confusing error
/// (`unexpected argument '--limit' found`) whenever a user combined `skim search
/// index` with any query flag — the textbook PF-006 shape: a dispatch arm that
/// diverts an advertised/expected input to a different code path.
///
/// Builds now go exclusively through `--build` / `--rebuild` / `--update`, which
/// were already the recommended surface. A cold `skim search index` auto-builds
/// the index (via `auto_refresh_if_stale`) and then returns lexical results for
/// the word "index".
#[allow(clippy::disallowed_methods)] // Search output entry point; delegates to streaming BufWriter sinks in subordinate fns
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // AD-412-3: help detection (empty args, --help, -h before --) lives entirely in
    // parse_flags so the `--` end-of-flags boundary has a single owner.
    let flags = parse_flags(args)?;

    // AD-403-5 / PF-006: single pre-dispatch guard for positional-flag inert notice.
    // Fires before `match flags.action` so it covers every arm. A whitespace-only
    // query is treated as no-text (mirrors the `!text.trim().is_empty()` guard below).
    let has_text = matches!(&flags.action, SearchAction::Query(t) if !t.trim().is_empty());
    if let Some(notice) = query::positional_inert_notice(flags.phrase, flags.near, has_text) {
        eprintln!("{notice}");
    }

    match flags.action {
        SearchAction::Help => {
            print_help();
            Ok(ExitCode::SUCCESS)
        }
        SearchAction::Removed(ref message) => {
            eprintln!("{message}");
            Ok(ExitCode::from(REMOVED_FLAG_EXIT_CODE))
        }
        SearchAction::Build => run_build(false, &flags.root_override, analytics),
        SearchAction::Rebuild => run_build(true, &flags.root_override, analytics),
        SearchAction::Update => run_update(&flags.root_override, analytics),
        SearchAction::Stats => run_stats(flags.json, &flags.root_override),
        SearchAction::InstallHooks => run_install_hooks(&flags.root_override),
        SearchAction::RemoveHooks => run_remove_hooks(&flags.root_override),
        // Reject whitespace-only queries at dispatch (defense-in-depth / AC2):
        // query_substring_present uses split_whitespace which yields no tokens for "  ",
        // making the predicate vacuously true and letting the AD-355-7 all-files fallback
        // emit up to 100 arbitrary indexed files for a content-free query. Trimming here
        // prevents that path from being reached at all and gives a cleaner empty-result
        // response consistent with what is_empty() returns for a zero-length query.
        SearchAction::Query(ref text) if !text.trim().is_empty() => {
            run_query(text.trim(), &flags, analytics)
        }
        SearchAction::Query(_) => {
            // Empty query (no positional args and no action flag) → help.
            print_help();
            Ok(ExitCode::SUCCESS)
        }
    }
}

// ============================================================================
// Parsed flags
// ============================================================================

/// The action the user wants to perform, derived from CLI flags.
///
/// Encodes the mutually-exclusive mode flags as a single enum variant so that
/// dispatch is a `match` rather than a cascade of `if flags.X` checks.
#[derive(Debug, PartialEq, Eq)]
enum SearchAction {
    Build,
    Rebuild,
    Update,
    Stats,
    InstallHooks,
    RemoveHooks,
    /// Run a search query with the given text.
    Query(String),
    /// Print the help text and exit.
    ///
    /// AD-412-3: folded into `parse_flags` so the end-of-flags sentinel
    /// (`"--"`) lives in a single owner — the `"--"` match arm in
    /// `parse_flags`. Help detection (empty args, `--help`, `-h`) is
    /// handled entirely by `parse_flags`.
    Help,
    /// Refuse a removed flag: print the message on stderr and exit 2.
    ///
    /// Produced by `parse_flags` for the first removed flag it meets, so the
    /// refusal happens before any index or cache file is touched.
    Removed(String),
}

/// Parsed flags from the CLI args passed to `skim search`.
#[derive(Debug)]
struct Flags {
    action: SearchAction,
    json: bool,
    limit: usize,
    /// Pagination offset: skip this many verified results before collecting.
    ///
    /// Applied AFTER verification on the pure-lexical exact-symbol path
    /// (RESOLVED Decision 3 / AC#11): `rank → verify → skip offset → take limit`.
    /// `None` (the default) is equivalent to offset 0.
    offset: Option<usize>,
    root_override: Option<PathBuf>,
    /// v5 positional search: require contiguous, ordered phrase match (`--phrase`).
    phrase: bool,
    /// v5 positional search: max word-token distance for `--near N` (unordered).
    near: Option<u32>,
    /// Language filter from `--lang <name>` (e.g. `--lang rust`, `--lang py`).
    ///
    /// Accepts both language names (`rust`, `python`, `typescript`) and file
    /// extensions (`rs`, `py`, `ts`).  `None` means no language restriction.
    lang: Option<rskim_core::Language>,
}

/// Single source of truth for all `Flags` field defaults.
///
/// `parse_flags` initialises its local variables to these same values; if a
/// default changes, update both this impl and the corresponding `let mut …`
/// in `parse_flags` to keep them in sync.  `Flags::help()` delegates here so
/// adding a new field only requires one edit instead of two (the compiler
/// enforces exhaustiveness on struct literals in `Default::default`).
impl Default for Flags {
    fn default() -> Self {
        Flags {
            action: SearchAction::Help,
            json: false,
            limit: 20,
            offset: None,
            root_override: None,
            phrase: false,
            near: None,
            lang: None,
        }
    }
}

impl Flags {
    /// Construct a `Flags` value that signals "print help and exit."
    ///
    /// All fields other than `action` are at their defaults; they are never
    /// read on the `SearchAction::Help` dispatch path in `run()`.  Uses
    /// `Default::default()` so that adding a field only requires one edit
    /// (the `Default` impl above) rather than duplicating all defaults here.
    fn help() -> Self {
        Default::default()
    }

    /// Construct a `Flags` value that signals "refuse a removed flag."
    ///
    /// Like [`Flags::help`], every field other than `action` is at its default
    /// and is never read on the dispatch path.
    fn removed(message: String) -> Self {
        Flags {
            action: SearchAction::Removed(message),
            ..Default::default()
        }
    }
}

/// Parse and validate a `--limit` value string.
///
/// Accepts any positive (>= 1) `usize`. Returns an error for non-numeric
/// values or zero.
fn parse_limit_value(raw: &str) -> anyhow::Result<usize> {
    let parsed = raw
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("--limit value must be a positive integer, got {:?}", raw))?;
    if parsed == 0 {
        anyhow::bail!("--limit must be >= 1 (got 0)");
    }
    Ok(parsed)
}

/// Parse and validate a `--offset` value string.
///
/// Accepts any non-negative integer (`usize`). Returns an error for non-numeric
/// values. Parallel to `parse_limit_value` so both flag arms read identically.
/// Typed as `usize` to match `limit` and `SearchQuery::offset`, eliminating the
/// `as usize` casts that `u64` required at all consumption sites.
fn parse_offset_value(raw: &str) -> anyhow::Result<usize> {
    raw.parse::<usize>().map_err(|_| {
        anyhow::anyhow!(
            "--offset value must be a non-negative integer, got {:?}",
            raw
        )
    })
}

/// Parse and validate a `--near` value string as a positive word-token distance.
///
/// AD-393-9: `--near 0` is rejected with an actionable error because a span of
/// zero word-tokens is only satisfied by a single-word query (trivially true) or
/// an exact-adjacent match with no gap — both cases are better expressed as
/// `--phrase` (exact adjacent) or a bare term search.  Zero is structurally
/// allowed by the `u32` type but semantically meaningless for proximity search,
/// so we reject it early with a clear message rather than silently producing
/// unexpected results.
fn parse_near_value(raw: &str) -> anyhow::Result<u32> {
    let n = raw
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("--near value must be a positive integer, got {raw:?}"))?;
    if n == 0 {
        anyhow::bail!(
            "--near span must be > 0 for multiple words; \
             use --phrase for exact adjacent matching"
        );
    }
    Ok(n)
}

/// Parse a `--lang` value into a [`rskim_core::Language`].
///
/// Accepts both file extensions (`rs`, `py`, `ts`) and language display names
/// (`rust`, `python`, `typescript`); case-insensitive.  Returns an actionable
/// error listing accepted values when the input is unrecognised.
fn parse_lang_value(raw: &str) -> anyhow::Result<rskim_core::Language> {
    use rskim_core::Language;
    // Normalize to lowercase once so both the extension lookup and the name
    // match arm are case-insensitive.  Without this `--lang RS` (uppercase
    // extension) was rejected while `--lang rs` succeeded — an inconsistency
    // that surprised users.  Language::from_extension is case-sensitive, so
    // we must lower before calling it.
    let raw_lower = raw.to_ascii_lowercase();
    // Try file extension first so callers can pass "rs", "py", etc.
    if let Some(lang) = Language::from_extension(&raw_lower) {
        return Ok(lang);
    }
    // Names that from_extension does not handle (no matching file extension).
    // Everything else ("go", "java", "c", "cpp", "sql", "swift", "json", "yaml",
    // "toml", "markdown") is already returned above via from_extension.
    match raw_lower.as_str() {
        "rust" => Ok(Language::Rust),
        "python" => Ok(Language::Python),
        "typescript" => Ok(Language::TypeScript),
        "javascript" => Ok(Language::JavaScript),
        "c++" => Ok(Language::Cpp),
        "csharp" | "c#" => Ok(Language::CSharp),
        "ruby" => Ok(Language::Ruby),
        "kotlin" => Ok(Language::Kotlin),
        _ => Err(anyhow::anyhow!(
            "--lang: unknown language {:?}; accepted names: rust, python, typescript, \
             javascript, go, java, markdown, c, cpp, csharp, ruby, sql, kotlin, swift, \
             json, yaml, toml — or file extensions: rs, py, ts, js, md, c, cpp, cs, rb, kt",
            raw
        )),
    }
}

/// Extract a value from a flag that supports both space-separated and equals forms.
///
/// Handles `--flag value` (space-separated) and `--flag=value` (equals) forms.
/// Returns `(value, consumed_next)` where:
/// - `value` is the trimmed non-empty string value.
/// - `consumed_next` is `true` when the space-separated form consumed the next token
///   (the caller must advance `i` by one extra position).
///
/// # Errors
///
/// Returns `Err` when the value token is absent (space form) or empty/whitespace-only
/// (both forms).
///
/// # Examples
///
/// ```text
/// take_flag_value("--root=src", None, "--root")            → Ok(("src", false))
/// take_flag_value("--root", Some("src"), "--root")         → Ok(("src", true))
/// take_flag_value("--root", None, "--root")                → Err(…missing…)
/// take_flag_value("--root=  ", None, "--root")             → Err(…empty…)
/// take_flag_value("--root", Some("--help"), "--root")      → Err(…requires a value; to print help…)
/// take_flag_value("--root", Some("-h"), "--root")          → Err(…requires a value; to print help…)
/// ```
fn take_flag_value(
    arg: &str,
    next_arg: Option<&String>,
    flag: &str,
) -> anyhow::Result<(String, bool)> {
    let prefix = format!("{flag}=");
    if let Some(val) = arg.strip_prefix(&prefix) {
        let trimmed = val.trim();
        if trimmed.is_empty() {
            anyhow::bail!("{flag} value must not be empty or whitespace-only");
        }
        return Ok((trimmed.to_string(), false));
    }
    // Space-separated form: the value is in the next token.
    let val =
        next_arg.ok_or_else(|| anyhow::anyhow!("{flag} requires a value (e.g. {flag} <value>)"))?;
    // AD-412-3: bail so --help/-h is never consumed as a flag value (e.g. `--root --help`).
    if matches!(val.as_str(), "--help" | "-h") {
        anyhow::bail!(
            "{flag} requires a value (got {:?}); \
             to print help, run: `skim search --help`",
            val
        );
    }
    let trimmed = val.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{flag} value must not be empty or whitespace-only");
    }
    Ok((trimmed.to_string(), true))
}

/// Parse the flags from `args`.
///
/// # Errors
///
/// - `--limit` / `-n` without a following value.
/// - `--limit` / `-n` value that is not a valid `usize`.
/// - `--limit=<value>` with a non-numeric value.
/// - `--root` without a following value.
/// - Unrecognised dash-leading flags — any `--foo` or `-x` (length >= 2) that is
///   not a known flag (see `KNOWN_FLAGS`). Bare `-` is a positional query token;
///   a literal dash-leading term is searchable after the `--` end-of-flags separator.
/// - Combining a text query with an action flag (`--build` / `--rebuild` /
///   `--update` / `--stats` / `--install-hooks` / `--remove-hooks`) — an
///   ambiguous mixed form.
///
/// A removed flag (see `REMOVED_FLAGS`) is not an error here: the returned
/// `Flags` carries `SearchAction::Removed`, which `run` turns into exit 2.
fn parse_flags(args: &[String]) -> anyhow::Result<Flags> {
    // AD-412-3: empty args → help immediately. --help/-h before `--` → help via the
    // match arm below. Post-`--` tokens are drained as literal query text (AD-412-2).
    if args.is_empty() {
        return Ok(Flags::help());
    }

    let mut action_flag: Option<SearchAction> = None;
    let mut json = false;
    let mut limit: usize = 20;
    let mut offset: Option<usize> = None;
    let mut root_override: Option<PathBuf> = None;
    let mut query_parts: Vec<String> = Vec::new();
    let mut phrase = false;
    let mut near: Option<u32> = None;
    let mut lang: Option<rskim_core::Language> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--build" => action_flag = Some(SearchAction::Build),
            "--rebuild" => action_flag = Some(SearchAction::Rebuild),
            "--update" => action_flag = Some(SearchAction::Update),
            "--stats" => action_flag = Some(SearchAction::Stats),
            "--install-hooks" => action_flag = Some(SearchAction::InstallHooks),
            "--remove-hooks" => action_flag = Some(SearchAction::RemoveHooks),
            "--json" | "-j" => json = true,
            s if s == "--limit" || s == "-n" || s.starts_with("--limit=") => {
                // Both space-separated (`--limit 10`, `-n 10`) and equals (`--limit=10`)
                // forms are handled by take_flag_value — same idiom as --root.
                // `-n` is a short alias; errors always say "--limit" for consistency.
                // `-n` has no equals form so the "--limit=" prefix never fires for it.
                let (raw, consumed) = take_flag_value(s, args.get(i + 1), "--limit")?;
                limit = parse_limit_value(&raw)?;
                if consumed {
                    i += 1;
                }
            }
            s if s == "--offset" || s.starts_with("--offset=") => {
                // Pagination offset: skip N verified results before collecting.
                // Applied AFTER verification (RESOLVED Decision 3 / AC#11).
                // Space-separated (`--offset 5`) and equals (`--offset=5`) both accepted.
                let (raw, consumed) = take_flag_value(s, args.get(i + 1), "--offset")?;
                offset = Some(parse_offset_value(&raw)?);
                if consumed {
                    i += 1;
                }
            }
            s if s == "--root" || s.starts_with("--root=") => {
                let (val, consumed) = take_flag_value(s, args.get(i + 1), "--root")?;
                root_override = Some(PathBuf::from(val));
                if consumed {
                    i += 1;
                }
            }
            // A removed flag ends parsing at once: the refusal must come before
            // any later token can raise a different error or touch the index.
            s if let Some(message) = removed_flag_message(s) => {
                return Ok(Flags::removed(message));
            }
            // AD-403-6: When BOTH --phrase and --near are given, the composed semantic is
            // PhraseNear(n) — ordered, total span <= n — NOT just phrase.  See
            // verify_mode_for in query.rs (AD-403-1) for the exhaustive mapping.
            // v5 positional search (#392 / #380 Phase 2). Shell strips quotes, so
            // `skim search "alpha beta"` and `skim search alpha beta` both arrive as
            // text "alpha beta"; `--phrase` is the explicit contiguous-match signal.
            "--phrase" => phrase = true,
            s if s == "--near" || s.starts_with("--near=") => {
                let (raw, consumed) = take_flag_value(s, args.get(i + 1), "--near")?;
                near = Some(parse_near_value(&raw)?);
                if consumed {
                    i += 1;
                }
            }
            s if s == "--lang" || s.starts_with("--lang=") => {
                // Language filter: restrict results to files of a given language.
                // Accepts display names ("rust", "python") and extensions ("rs", "py").
                // D17 / AC16: honored on all search paths (positional + fallback + lexical).
                let (raw, consumed) = take_flag_value(s, args.get(i + 1), "--lang")?;
                lang = Some(parse_lang_value(&raw)?);
                if consumed {
                    i += 1;
                }
            }
            // AD-412-3: --help/-h before `--` triggers help; after `--` these tokens
            // are already drained as literal query text and never reach this arm.
            "--help" | "-h" => return Ok(Flags::help()),
            // AD-412-2: end-of-flags separator. Drains all remaining tokens verbatim into
            // the query and stops flag parsing. Escape hatch for dash-leading literals
            // (`-Werror`, `->`, `--rebuild`). Only the first `--` is special; a second
            // `--` becomes literal query text. Output flags must appear BEFORE `--`.
            "--" => {
                query_parts.extend(args[i + 1..].iter().cloned());
                break;
            }
            // AD-412-1: reject any unrecognised dash-prefixed token (both `--foo` and
            // `-x`), symmetric for short and long flags. Bare `-` (len == 1) falls
            // through to the positional catch-all. Matches on token shape only (no
            // sibling-flag-absent guard — avoids PF-006).
            // AD-412-4: {s:?} (Debug, quoted) prevents ANSI-escape injection in output.
            s if s.starts_with('-') && s.len() >= 2 => {
                anyhow::bail!(
                    "unrecognised flag {s:?}. \
                     To search a literal dash-leading term, use `--` (e.g. `skim search -- {s:?}`). \
                     Valid flags: {KNOWN_FLAGS}"
                );
            }
            // Positional arg — part of the query text.
            s => query_parts.push(s.to_string()),
        }
        i += 1;
    }

    // AD-412-5: error when an action flag and a text query appear together (mixed form).
    validate_no_mixed_form(action_flag.as_ref(), &query_parts)?;

    let action = action_flag.unwrap_or_else(|| SearchAction::Query(query_parts.join(" ")));

    Ok(Flags {
        action,
        json,
        limit,
        offset,
        root_override,
        phrase,
        near,
        lang,
    })
}

/// AD-412-5: Validate that an action flag and a text query are not combined.
///
/// Extracted from `parse_flags` to reduce that function's cyclomatic complexity.
/// Returns an error with an actionable message if both are present.
///
/// # AD-412-4 (security)
///
/// `{query:?}` uses Debug formatting (quoted, escaped) so ANSI-escape, newline,
/// or other control bytes in a crafted positional token cannot clear the terminal
/// or forge log lines in AI agent output.  The Debug-quoted form is still a
/// valid, copy-pasteable `skim search -- "..."` argument.
fn validate_no_mixed_form(
    action_flag: Option<&SearchAction>,
    query_parts: &[String],
) -> anyhow::Result<()> {
    // AD-412-5: use the same "is there a real query?" predicate as the `has_text`
    // binding in `run()` and the `SearchAction::Query` dispatch guard so
    // whitespace-only positional tokens (e.g. `skim search "  " --rebuild`) are
    // not treated as a genuine text query here while being silently ignored downstream.
    if action_flag.is_some() && query_parts.iter().any(|p| !p.trim().is_empty()) {
        let query = query_parts.join(" ");
        anyhow::bail!(
            "cannot combine a text query ({query:?}) with an action flag \
             (--build / --rebuild / --update / --stats / --install-hooks / --remove-hooks). \
             To search for the literal text, use: `skim search -- {query:?}`"
        );
    }
    Ok(())
}

// ============================================================================
// Shared project-root + cache-dir resolution
// ============================================================================

fn resolve_root_and_cache(root_override: &Option<PathBuf>) -> anyhow::Result<(PathBuf, PathBuf)> {
    let root = match root_override {
        // AD-400-1: `--root` is validated up-front at this single funnel so a
        // non-existent or non-directory value FAILS LOUD (skim's "fail loud with
        // actionable messages" invariant; #400) BEFORE resolve_search_cache_dir or
        // any create_dir_all runs — hence NO cache directory is created for a garbage
        // root. The former silent `.unwrap_or_else(|_| r.clone())` let a bogus root
        // reach resolve_search_cache_dir's AD-381-2 lexical fallback, index 0 files,
        // and return "no results" with exit 0. `canonicalize()` rejects a missing
        // path (its io::Error carries no path, so we prepend --root + the spelling);
        // the explicit `is_dir()` guard additionally rejects an existing *file* root,
        // since canonicalize() succeeds for files. Both bail → exit 1 (dispatch maps
        // anyhow::Err → ExitCode::FAILURE; exit 2 is reserved for the parse path).
        Some(r) => {
            let canonical = r.canonicalize().map_err(|e| {
                // AD-412-4: use {:?} (Debug, quoted) on the raw user-supplied path so
                // ANSI-escape or newline bytes in a crafted `--root` value cannot clear
                // the terminal or forge log lines in AI-agent output (same pattern as
                // the unrecognised-flag and mixed-form error paths).
                anyhow::anyhow!("--root {:?}: {e}. Pass the directory to index.", r)
            })?;
            anyhow::ensure!(
                canonical.is_dir(),
                // Canonical path is filesystem-derived (post-canonicalize), but apply
                // the same {:?} guard for consistency with the AD-412-4 hardening.
                "--root {:?} is not a directory. Pass the directory to index.",
                canonical
            );
            canonical
        }
        None => {
            let cwd = std::env::current_dir()?;
            walk::discover_project_root(&cwd)?
        }
    };
    let cache_dir = index::resolve_search_cache_dir(&root)?;
    Ok((root, cache_dir))
}

// ============================================================================
// --build / --rebuild
// ============================================================================

fn run_build(
    force: bool,
    root_override: &Option<PathBuf>,
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;
    let config = types::IndexConfig {
        root: root.clone(),
        max_files: None,
        force,
        cache_dir_override: Some(cache_dir.clone()),
    };
    let result = index::build_index(&config)?;
    eprintln!(
        "skim search: indexed {} files ({} skipped, {} cache hits) in {:.1}s",
        result.file_count,
        result.skipped,
        result.cache_hits,
        result.duration.as_secs_f64(),
    );

    // AD-395-6: emit the bounded, stable-key-sorted skip sample to stderr so
    // previously-silent skips are observable (PF-012 determinism: sample is
    // sorted by path string ascending, CapReached last, from build_skip_sample).
    if !result.skip_sample.is_empty() {
        // Show up to 10 named skip reasons; remainder surfaced as "...and N more".
        // Pass result.skipped (the exact uncapped total) so N reflects the true
        // remainder, not just the bounded sample length (AD-395-2).
        let sample_display =
            index::format_skip_sample(&result.skip_sample, 10, result.skipped as usize);
        eprintln!("{sample_display}");
    }

    // AD-405-7 / AC-405-17: emit AST coverage notice after an explicit build or
    // rebuild (D-4 cadence).  `result.ast_coverage` was computed in index.rs
    // before `new_manifest.save()` — zero extra I/O (AC-405-12).
    query::emit_ast_coverage_notice(&result.ast_coverage);

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --update
// ============================================================================

fn run_update(
    root_override: &Option<PathBuf>,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;
    let (outcome, manifest) = staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;
    if !outcome.refreshed() {
        eprintln!("skim search: index is current");
    } else {
        // AD-405-7 / AC-405-17: emit AST coverage notice after --update refreshes
        // the index (D-4 cadence).  Manifest is the post-refresh state.
        query::emit_ast_coverage_notice(&manifest.ast_coverage());
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --stats
// ============================================================================

#[allow(clippy::disallowed_methods)] // Search stats output; streaming BufWriter for potentially large index statistics
fn run_stats(json: bool, root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;

    // AD-381-1: surface the resolved search cache directory so the (otherwise
    // hidden) on-disk location is discoverable from `--stats` alone. Computed
    // once here and reused by both the no-index early-return and the populated
    // branch below, in both text and JSON modes.
    let cache_dir_display = cache_dir.display().to_string();

    // AD-414-23: `--stats` REPORTS on an index; it never creates one.
    //
    // The AC-14 self-heal below repairs an index that exists but is structurally
    // broken (F-Body-A/B/C) — a repair whose cost is bounded by work the user has
    // already paid for once. A cold start is a different thing: building from
    // scratch can take minutes on a large corpus, and doing it silently because
    // someone asked "what is in my index?" is the opposite of "fail loud with
    // actionable messages". So this arm keeps the #413 AC21 contract verbatim —
    // `{"error":"no index found","cache_dir":"<path>"}` on stdout in JSON mode,
    // the two guidance lines on stderr in text mode, exit 1 in both — which the
    // #414 plan (Step 11 / C11) treats as the reference shape when it normalises
    // `build_stats_json`'s own early return to include `cache_dir`.
    //
    // Consequence for the `.staleness` contract (F5): `check_staleness` returns
    // `StalenessCheck::NoIndex` on exactly this condition (`staleness.rs`, "Cold
    // start: no lexical index file"), so `--stats` can never report its `Display`
    // form `"no index"` — the gather never runs. The reportable value set is the
    // other four; CLAUDE.md and the CHANGELOG entry are corrected to match.
    // (A concurrent deleter racing between this probe and `gather_stats` could
    // still produce it; that is a TOCTOU window, not a contract.)
    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        if json {
            // AC7: single parseable object retaining `error`, plus `cache_dir`
            // (the "where would it go?" path). Exit FAILURE is unchanged.
            let no_index = serde_json::json!({
                "error": "no index found",
                "cache_dir": cache_dir_display,
            });
            println!("{}", serde_json::to_string(&no_index)?);
        } else {
            // AC5: print the resolved cache-dir path even with no index, in
            // addition to the existing guidance. Exit FAILURE is unchanged.
            eprintln!("skim search: no index found — run `skim search --build` first");
            eprintln!("  cache dir     : {cache_dir_display}");
        }
        return Ok(ExitCode::FAILURE);
    }

    let mut out = BufWriter::new(std::io::stdout());

    // AC6/AC11/AD-414-10: gather_stats performs the self-heal (AC-14), snapshots
    // pre-refresh state (AC-15, AC-22), and collects all index data in one pass.
    // Both the text branch and the JSON branch below read from the same snapshot,
    // eliminating the git_head divergence fixed by Finding [high/architecture]:
    // the previous code gave each branch its own copy of the 5-step gather sequence
    // and they diverged on git_head (text: post-heal manifest, JSON: pre-heal probe).
    //
    // AC-13 / T-13: if gather_stats returns Err (CRC-level corruption the structural
    // probe cannot detect), emit a structured JSON error and exit FAILURE in --json mode;
    // propagate the error in text mode.
    let snapshot = match gather_stats(&cache_dir, &root) {
        Ok(s) => s,
        Err(e) => {
            if json {
                let err_json = serde_json::json!({
                    "error": format!("{e:#}"),
                    "cache_dir": cache_dir_display,
                });
                writeln!(out, "{}", serde_json::to_string_pretty(&err_json)?)?;
                out.flush()?;
                return Ok(ExitCode::FAILURE);
            } else {
                return Err(e);
            }
        }
    };

    if json {
        // AC6/AC11: shared JSON serialisation via build_stats_json — same code path
        // as the test helper (stats_json_for_test) so AC11 back-compat tests guard
        // the production JSON shape. AD-395-6: `skipped`/`skipped_by_reason` additive.
        let extended = build_stats_json(&snapshot)?;
        writeln!(out, "{}", serde_json::to_string_pretty(&extended)?)?;
    } else {
        // Text rendering from the shared snapshot.
        // AD-380-4: the TRUE total over all on-disk artifacts (AC-6/AC-7).
        writeln!(out, "skim search index stats:")?;
        writeln!(out, "  files indexed : {}", snapshot.file_count)?;
        writeln!(out, "  total n-grams : {}", snapshot.total_ngrams)?;
        writeln!(
            out,
            "  index size    : {} bytes (lexical)",
            snapshot.index_size_bytes
        )?;
        writeln!(out, "  total on disk : {} bytes", snapshot.total_on_disk)?;
        if let Some(ts) = snapshot.last_updated {
            writeln!(out, "  last updated  : {ts}")?;
        }
        // AC-22 / AD-414-10: git_head is the PRE-REFRESH snapshot (same as JSON).
        writeln!(
            out,
            "  git HEAD      : {}",
            snapshot.pre_refresh_git_head.as_deref().unwrap_or("(none)")
        )?;
        writeln!(out, "  staleness     : {}", snapshot.staleness_status)?;
        // AC4: resolved cache dir.
        writeln!(out, "  cache dir     : {}", snapshot.cache_dir_display)?;
        // AD-395-6: skip counts by reason (text mode).
        // PF-012: BTreeMap gives stable sorted order consistent with JSON path.
        if !snapshot.skip_entries.is_empty() {
            let mut by_reason: std::collections::BTreeMap<&str, u64> =
                std::collections::BTreeMap::new();
            for e in &snapshot.skip_entries {
                *by_reason.entry(e.reason_label()).or_insert(0) += 1;
            }
            writeln!(
                out,
                "  skipped       : {} (content-skipped files)",
                snapshot.skip_entries.len()
            )?;
            for (reason, count) in &by_reason {
                writeln!(out, "    {reason}: {count}")?;
            }
        }
        // AD-405-7 / AC-405-9 / AC-405-15: AST size-coverage section (D-4 cadence).
        // Omit when clean (is_clean() == true) — byte-identical to the pre-fix binary
        // on a corpus with zero excluded / zero undetermined files (AC-405-15).
        if !snapshot.ast_coverage.is_clean() {
            let coverage = &snapshot.ast_coverage;
            writeln!(out, "  ast eligible  : {}", coverage.size_eligible_files)?;
            if coverage.size_excluded_files > 0 {
                writeln!(
                    out,
                    "  ast excluded  : {} (exceed 1 MiB cap)",
                    coverage.size_excluded_files
                )?;
                // PF-012: excluded_by_lang is a BTreeMap — already sorted.
                for (lang, count) in &coverage.excluded_by_lang {
                    writeln!(out, "    {lang}: {count}")?;
                }
            }
            if coverage.undetermined_files > 0 {
                // "ast no-size" (11 chars) + 3 spaces = 14-char label field;
                // colon at column 16 — aligned with all other stats lines.
                writeln!(out, "  ast no-size   : {}", coverage.undetermined_files)?;
            }
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The fixed set of on-disk index artifacts summed by [`total_on_disk_bytes`].
///
/// AD-380-4 (#380 / AC-6, AC-7): a FIXED, known filename list — `--stats` MUST
/// NOT recursively walk the cache directory. Each artifact is stat'd via
/// `metadata().len()` and a missing one counts as 0 bytes (fail-soft, AC-7).
/// Adding a new index artifact means extending this list (one source of truth).
const ON_DISK_ARTIFACTS: [&str; 6] = [
    "index.skidx",       // lexical n-gram index
    "index.skpost",      // lexical posting lists
    "index.skfiles",     // binary file manifest (this ticket)
    "ast_index.skidx",   // AST n-gram index header + metadata
    "ast_index.skpost",  // AST posting lists
    "ast_index.skcache", // AST extraction cache
];

/// Return the byte length of one artifact in `cache_dir`, or 0 when it is absent
/// or unreadable (fail-soft, AC-7).
fn artifact_len(cache_dir: &std::path::Path, name: &str) -> u64 {
    std::fs::metadata(cache_dir.join(name))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Sum the on-disk byte size of every present index artifact in `cache_dir`.
///
/// AD-380-4 (#380): reports the TRUE on-disk footprint of the search index. The
/// previous `--stats` "index size" line reported only the lexical skidx+skpost
/// pair, undercounting the real footprint. This iterates a FIXED filename list
/// via `metadata()` (O(1), no directory walk, AC-7) and treats a missing
/// artifact as 0 bytes so partial indexes (e.g. a lexical-only build before AST
/// ran) report exactly the sum of the files that exist (AC-7).
fn total_on_disk_bytes(cache_dir: &std::path::Path) -> u64 {
    ON_DISK_ARTIFACTS
        .iter()
        .map(|name| artifact_len(cache_dir, name))
        .sum()
}

// ============================================================================
// Stats snapshot (Finding [high/architecture] + [medium/architecture] fix)
// ============================================================================

/// All data needed to render `--stats` output, gathered in a single pass by
/// [`gather_stats`].
///
/// Both text and JSON branches of [`run_stats`] render from this snapshot,
/// ensuring they describe the same invocation state (Finding [high/architecture]:
/// `--stats` text and `--stats --json` previously reported different `git_head`
/// values for the same invocation because the text branch read git_head from the
/// post-heal manifest while the JSON branch snapshotted it pre-heal).
struct StatsGathered {
    /// git HEAD as stored in the manifest BEFORE any self-heal (AC-22 / AD-414-10).
    ///
    /// Derived from a header-only probe ([`FileManifest::load_git_head`]) so the
    /// second full manifest decode that `check_staleness` already performs inside
    /// `auto_refresh_if_stale` is avoided (Finding [medium/performance] fix).
    pre_refresh_git_head: Option<String>,
    /// Live git HEAD resolution state for the `git_head_state` JSON key (AC-20).
    git_head_state_str: &'static str,
    /// Staleness classification at INVOCATION TIME — the verdict from
    /// [`staleness::check_staleness`] run before `auto_refresh_if_stale` so
    /// `--stats --json` `.staleness` and the text `staleness` line report whether
    /// the index was stale when the user ran the command (AC-11 /
    /// Finding [medium/regression]: the post-heal value is always `Current` and
    /// therefore non-informative).
    staleness_status: staleness::StalenessCheck,
    // --- Index stats from the lexical reader (post-heal) ---
    file_count: u32,
    total_ngrams: u64,
    index_size_bytes: u64,
    last_updated: Option<u64>,
    total_on_disk: u64,
    // --- Manifest-derived data (post-heal) ---
    skip_entries: Vec<types::SkippedEntry>,
    ast_coverage: rskim_search::AstCoverage,
    // --- Display string (avoids threading cache_dir into build_stats_json) ---
    cache_dir_display: String,
}

/// Gather all data needed by [`run_stats`] in one pass (repair → collect).
///
/// Implements the repair-then-report pattern (Finding [medium/architecture] CQS fix):
/// the self-heal (`auto_refresh_if_stale`) and the data collection live here so
/// [`build_stats_json`] is a pure serialiser with no I/O side effects.
fn gather_stats(
    cache_dir: &std::path::Path,
    root: &std::path::Path,
) -> anyhow::Result<StatsGathered> {
    let cache_dir_display = cache_dir.display().to_string();

    // Finding [medium/performance] fix: header-only probe for the pre-refresh HEAD.
    // `auto_refresh_if_stale` internally calls `check_staleness → FileManifest::load`
    // which fully decodes the manifest (60 k-node BTreeMap + 60 k String allocations
    // for a max-size index). Decoding a second full copy here — as the previous code
    // did in `build_stats_json` — doubles that cost for one header field. The probe
    // reads the same bytes but decodes only the fixed header block via `decode_header`
    // and returns just the git_head string; the entry table is never materialised.
    let pre_refresh_git_head = manifest::FileManifest::load_git_head(root, cache_dir);

    let git_head_state_str = match staleness::git_head_state(root) {
        staleness::HeadState::Resolved(_) => "resolved",
        staleness::HeadState::Unresolved => "unresolved",
        staleness::HeadState::NotARepo => "not_a_repo",
    };

    // AC-14: self-heal structural corruption before opening the reader.
    // Finding [medium/architecture] CQS fix: repair lives here, not in
    // `build_stats_json`, so the reporting function has no I/O side effects.
    let noop_analytics = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };
    // Pre-heal staleness: snapshot the invocation-time verdict BEFORE
    // auto_refresh_if_stale runs, so --stats --json `.staleness` and the text
    // `staleness` line reflect whether the index WAS stale when the user ran the
    // command (AC-11 / Finding [medium/regression]).  auto_refresh_if_stale calls
    // check_staleness internally; the extra call here is acceptable for the
    // infrequent --stats path.  The Option<FileManifest> returned by check_staleness
    // is discarded — auto_refresh_if_stale loads the authoritative post-heal manifest.
    let (staleness_status, _pre_check_manifest) = staleness::check_staleness(cache_dir, root);

    let (_, loaded_manifest) = staleness::auto_refresh_if_stale(root, cache_dir, &noop_analytics)?;

    // AD-414-7: opens successfully for F-Body-A/B/C after the self-heal above.
    // Only CRC-level corruption not detectable by the structural probe propagates
    // as Err here (T-13); the caller (`run_stats`) maps it to a structured error.
    let reader = query::open_lexical_reader(cache_dir)?;
    let stats = reader.stats();

    let total_on_disk = total_on_disk_bytes(cache_dir);

    // Collect skip and coverage data from the post-heal manifest.
    let skip_entries: Vec<types::SkippedEntry> = loaded_manifest.skipped().cloned().collect();
    let ast_coverage = loaded_manifest.ast_coverage();

    Ok(StatsGathered {
        pre_refresh_git_head,
        git_head_state_str,
        staleness_status,
        file_count: stats.file_count,
        total_ngrams: stats.total_ngrams,
        index_size_bytes: stats.index_size_bytes,
        last_updated: stats.last_updated,
        total_on_disk,
        skip_entries,
        ast_coverage,
        cache_dir_display,
    })
}

// ============================================================================
// --install-hooks / --remove-hooks
// ============================================================================

fn run_install_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    // AD-413-15: install_search_hooks resolves the shared hooks dir and emits
    // the AC34(b) shared-scope notice automatically; the returned `dir` names
    // what was actually touched (may differ from `<root>/.git/hooks`).
    let outcome = hooks::install_search_hooks(&root)?;
    // Normalize to remove any `..` components that arise from a relative `gitdir:`
    // pointer (submodules write e.g. `gitdir: ../.git/modules/sub`).
    let display_dir = outcome
        .dir
        .canonicalize()
        .unwrap_or_else(|_| outcome.dir.clone());
    eprintln!("skim search: git hooks installed in {:?}", display_dir);
    Ok(ExitCode::SUCCESS)
}

fn run_remove_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    // AD-413-15 / AC31(a): remove_search_hooks resolves the shared dir,
    // emits the AC34(b) shared-scope notice when applicable, and returns the
    // resolved dir.  Only print "removed from" when a marker block was actually
    // removed (`outcome.changed`).
    let outcome = hooks::remove_search_hooks(&root)?;
    if outcome.changed {
        // Normalize to remove any `..` components (submodule gitdir pointers).
        let display_dir = outcome
            .dir
            .canonicalize()
            .unwrap_or_else(|_| outcome.dir.clone());
        eprintln!("skim search: git hooks removed from {:?}", display_dir);
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Query execution
// ============================================================================

#[allow(clippy::disallowed_methods)] // Search result streaming; result set may be large, BufWriter for throughput
fn run_query(
    text: &str,
    flags: &Flags,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(&flags.root_override)?;
    std::fs::create_dir_all(&cache_dir)?;

    // AD-403-6: degenerate --near diagnostic (fail loud, never silently — SEARCH-ADR-001).
    // Emitted here on the text-query path ONLY (has_text is true by construction).
    // Case (a): single-word query + --near N (N cannot constrain anything).
    // Case (b): N < word_count - 1 (structurally unsatisfiable; returns empty results
    // silently without this notice).  stderr only; exit 0.
    if let Some(notice) = query::near_diagnostic_notice(flags.near, text) {
        eprintln!("{notice}");
    }

    let config = types::QueryConfig {
        text: text.to_string(),
        limit: flags.limit,
        // AD-372-3 / RESOLVED Decision 3: offset is applied AFTER verification in
        // resolve_paths_and_snippets_verified (rank → verify → skip offset → take limit).
        // On the exact-symbol path query.rs sets sq.offset=None so the reader returns
        // the full ranked intersection; effective_offset from config.offset is then
        // passed to the post-verify skip.  On the multi-word path offset is also
        // applied post-verify (same code path).
        offset: flags.offset,
        json: flags.json,
        root,
        cache_dir,
        phrase: flags.phrase,
        near: flags.near,
        lang: flags.lang,
    };

    let output = query::execute_query(&config, analytics)?;

    // AD-404-11 / D-5: emit bounded-page notice when has_more is true.
    // Goes to stderr so --json stdout stays byte-identical (PF-006 / AD-404-8).
    if output.has_more {
        let page = config.page();
        eprintln!(
            "{}",
            query::bounded_page_notice(output.total, page.offset(), page.limit())
        );
    }

    let mut stdout = BufWriter::new(std::io::stdout());
    if flags.json {
        query::format_json_output(&output, &mut stdout)?;
    } else {
        query::format_text_output(&output, &mut stdout)?;
    }
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Help text
// ============================================================================

/// Full `skim search` help text.
///
/// Extracted to a `const` (from the old inline `println!` literal) so tests can
/// assert its contents as a falsifiable unit test (PF-008 doc-drift guard).
pub(super) const SEARCH_HELP_TEXT: &str = "\
Usage: skim search [OPTIONS] [QUERY]

Search code using layered n-gram BM25F indexing.

Subcommands / modes:
  (none)           Print this help message

Options:
  --build          Build the index incrementally (auto-build on first query)
  --rebuild        Rebuild the index from scratch
  --update         Refresh if index is stale (git HEAD changed)
  --stats          Show index statistics
  --install-hooks  Install git post-commit/merge hooks for auto-refresh.
                   In a linked worktree, writes to the shared hooks dir
                   (applies to all worktrees in the clone).
  --remove-hooks   Remove skim git hooks. In a linked worktree, removes
                   from the shared hooks dir (applies clone-wide).
  --json, -j       Output results as JSON
  --limit N, -n N  Maximum results to return (default: 20)
  --offset N       Skip N verified results (pagination; default: 0)
  --root PATH      Override project root (default: walk up to .git)
  --               End of flags. All tokens after `--` are literal query text,
                   even if they look like flags. Use this to search for
                   dash-leading terms (e.g. `skim search -- -Werror`).
                   Output flags (--json, --limit) must be placed BEFORE `--`.
  -h, --help       Print this help message

Positional query options:
  --phrase         Require query words in order, adjacent (no gaps between tokens).
                   Matching is case-sensitive and byte-exact. Punctuation is a word
                   separator, so --phrase \"foo bar\" matches foo::bar() and foo bar.
                   A single-word --phrase is a whole-word-exact search: --phrase alpha
                   does NOT match 'alphabet'. Inert without a text query.

  --near N         Require all query words within a window of N word tokens, in any
                   order. N counts word tokens (not characters or lines). N >= 1.
                   Example: --near 5 means the matched words span at most 5 positions.
                   Inert without a text query.

  --phrase --near N  Require query words in order (strictly ascending positions) AND
                   total span ≤ N word tokens (same N as bare --near). Narrows
                   --near N by additionally enforcing query word order; never grows
                   the result set versus bare --near N.
                   Identity: --phrase --near (k-1) == --phrase for a k-word query.
                   Example: \"alpha beta gamma\" --phrase --near 4 matches if alpha,
                   beta, and gamma appear in that order within 4 word-token positions.

Language filter option:
  --lang LANG      Filter results to files of a given language (e.g. --lang rust,
                   --lang python). Accepted as language name or extension.

Removed flags (each exits 2 and names its replacement):
  --ast                        Use ast-grep for structural search
  --hot, --cold, --risky       Use skim heatmap for churn and fix density
  --blast-radius               Use skim heatmap, or git log --name-only -- FILE
  --weights                    There is no fused ranking

General examples:
  skim search \"authenticate\"                Search for 'authenticate'
  skim search --limit 5 \"parse_url\"         Return at most 5 results
  skim search --json \"UserService\"          JSON output
  skim search --build                       Build the search index
  skim search --rebuild                     Rebuild from scratch
  skim search --update                      Refresh stale index
  skim search --stats                       Show index statistics
  skim search --install-hooks               Install hooks (clone-wide in a linked worktree)";

fn print_help() {
    // AD-375-3: the `index` subcommand line was removed with #375 (avoids PF-008).
    // `skim search index` now runs a QUERY for the word "index"; builds go through
    // --build / --rebuild / --update (already documented in Options above).
    // Body lives in SEARCH_HELP_TEXT so tests can assert it without driving the CLI.
    println!("{SEARCH_HELP_TEXT}");
}

// ============================================================================
// Shared JSON stats builder
// ============================================================================

/// Build the `--stats --json` value used by both [`run_stats`] and the test helper.
///
/// Pure serialiser (Finding [medium/architecture] CQS fix): all data is pre-gathered
/// by [`gather_stats`], which performs the self-heal and takes the pre-refresh
/// snapshots.  This function has **no I/O side effects**.
///
/// Shared so that AC11 back-compat tests guard the actual production code path
/// rather than a hand-duplicated reimplementation.  `run_stats` calls this in its
/// JSON branch; `stats_json_for_test` runs the same `gather_stats` + serialise pair.
///
/// AD-413-13: `git_head` is the manifest's stored HEAD ("HEAD-at-last-build", read
/// by [`manifest::FileManifest::load_git_head`]) while `git_head_state` is the live
/// HEAD state resolved at invocation time.  The two keys can legitimately diverge —
/// a frozen linked worktree before its first post-fix rebuild produces
/// `{"git_head": null, "git_head_state": "resolved"}`, which is a legal transient
/// state, not a contradiction.
///
/// AD-395-6: `skipped` / `skipped_by_reason` are additive keys (AC11 back-compat).
/// `skipped_by_reason` uses `BTreeMap` for byte-stable key order (PF-012).
///
/// **Snapshot asymmetry (AD-414-10 / AC-22):** `git_head` and `staleness` are
/// PRE-heal snapshots from `snapshot`; all other fields reflect the post-heal state.
fn build_stats_json(snapshot: &StatsGathered) -> anyhow::Result<serde_json::Value> {
    let skipped_arr: Vec<serde_json::Value> = snapshot
        .skip_entries
        .iter()
        .map(|e| serde_json::json!({ "path": e.path, "reason": e.reason_label() }))
        .collect();
    // PF-012: BTreeMap gives deterministic (sorted) key order in JSON output,
    // consistent with the alphabetical sort used in the text-mode path.
    let mut skipped_by_reason: std::collections::BTreeMap<&str, u64> =
        std::collections::BTreeMap::new();
    for e in &snapshot.skip_entries {
        *skipped_by_reason.entry(e.reason_label()).or_insert(0) += 1;
    }
    // AD-405-9 / AC-405-9 / AC-405-15: `ast_coverage` is additive (never replaces
    // existing keys) and OMITTED when clean (is_clean() == true).
    let ast_coverage_val: Option<serde_json::Value> = if snapshot.ast_coverage.is_clean() {
        None
    } else {
        Some(
            serde_json::to_value(&snapshot.ast_coverage)
                .map_err(|e| anyhow::anyhow!("ast_coverage serialization error: {e}"))?,
        )
    };
    let mut result = serde_json::json!({
        "file_count": snapshot.file_count,
        "total_ngrams": snapshot.total_ngrams,
        "index_size_bytes": snapshot.index_size_bytes,
        "total_on_disk_bytes": snapshot.total_on_disk,
        "last_updated": snapshot.last_updated,
        "git_head": snapshot.pre_refresh_git_head,
        "git_head_state": snapshot.git_head_state_str,
        "staleness": snapshot.staleness_status.to_string(),
        "cache_dir": snapshot.cache_dir_display,
        "skipped": skipped_arr,
        "skipped_by_reason": skipped_by_reason,
    });
    // Insert ast_coverage only when non-clean (omit-when-clean AC-405-9/15).
    if let Some(val) = ast_coverage_val {
        result["ast_coverage"] = val;
    }
    Ok(result)
}

// ============================================================================
// Test helpers (cfg(test) only — not compiled into production builds)
// ============================================================================

/// Delegate to [`gather_stats`] + [`build_stats_json`] so AC4/AC11 tests cover
/// the production code path rather than a hand-duplicated reimplementation.
///
/// Used by `index_tests.rs` for:
/// - AC4 (#395): assert `skipped` array / `skipped_by_reason` JSON fields.
/// - AC11 (#395): assert all pre-existing keys survive with unchanged types.
///
/// Runs the same gather+serialise sequence as the production `run_stats --json`
/// path.
#[cfg(test)]
pub(crate) fn stats_json_for_test(
    cache_dir: &std::path::Path,
    root: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    let snapshot = gather_stats(cache_dir, root)?;
    build_stats_json(&snapshot)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Stub analytics config for tests — analytics disabled, no cost override.
    const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };

    /// Locate the built `skim` binary for subprocess-level tests.
    ///
    /// Cargo sets `CARGO_BIN_EXE_skim` only for integration tests, so these
    /// in-source unit tests walk up from `current_exe()` to the first ancestor
    /// that holds `skim`. The walk covers both cargo layouts for the test binary:
    /// `target/<profile>/deps/skim-<hash>` and, from cargo 1.100,
    /// `target/<profile>/build/rskim/<hash>/out/skim-<hash>`. It stops at the
    /// cargo target dir (the folder with `CACHEDIR.TAG`), so a `skim` file
    /// outside the build output is never used.
    ///
    /// PRECONDITION: the `skim` binary is built from the current tree
    /// (`cargo test -p rskim --bins`, `--all-targets`, or `cargo build -p rskim`
    /// before nextest). A stale binary runs old code and can give a false pass.
    pub(super) fn skim_bin_path() -> String {
        if let Ok(bin) = std::env::var("CARGO_BIN_EXE_skim") {
            return bin;
        }
        let exe = std::env::current_exe().expect("test binary path must resolve");
        let name = format!("skim{}", std::env::consts::EXE_SUFFIX);
        for dir in exe.ancestors().skip(1) {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
            if dir.join("CACHEDIR.TAG").is_file() {
                break;
            }
        }
        panic!(
            "no built `{name}` between {} and the cargo target dir; \
             run `cargo build -p rskim` first",
            exe.display()
        )
    }

    // ========================================================================
    // --stats total-on-disk size (#380, AC-6 / AC-7)
    // ========================================================================

    /// AC-7 (#380): with ONLY the three lexical files present, the reported total
    /// equals EXACTLY their summed sizes (falsifiable) — missing artifacts count
    /// as 0, no error.
    #[test]
    fn test_total_on_disk_lexical_only_sums_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        std::fs::write(cache_dir.join("index.skidx"), vec![0u8; 100]).unwrap();
        std::fs::write(cache_dir.join("index.skpost"), vec![0u8; 250]).unwrap();
        std::fs::write(cache_dir.join("index.skfiles"), vec![0u8; 30]).unwrap();
        // No AST/temporal artifacts present.

        let total = total_on_disk_bytes(cache_dir);
        assert_eq!(
            total, 380,
            "total must equal the sum of the three present files (100+250+30), missing=0 (AC-7)"
        );
    }

    /// AC-6 (#380): when AST artifacts also exist, the total includes them — it
    /// MUST NOT report lexical-only.
    #[test]
    fn test_total_on_disk_includes_all_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        let sizes = [
            ("index.skidx", 10u64),
            ("index.skpost", 20),
            ("index.skfiles", 5),
            ("ast_index.skidx", 7),
            ("ast_index.skpost", 11),
            ("ast_index.skcache", 13),
        ];
        for (name, n) in &sizes {
            std::fs::write(cache_dir.join(name), vec![0u8; *n as usize]).unwrap();
        }
        let expected: u64 = sizes.iter().map(|(_, n)| n).sum();
        let lexical_only: u64 = 10 + 20 + 5;

        let total = total_on_disk_bytes(cache_dir);
        assert_eq!(total, expected, "total must sum all 6 artifacts (AC-6)");
        assert!(
            total > lexical_only,
            "total ({total}) must exceed lexical-only ({lexical_only}) when AST artifacts exist (AC-6 negative)"
        );
    }

    /// AC-7 (#380): an empty cache dir (no artifacts) reports 0, never an error.
    #[test]
    fn test_total_on_disk_empty_dir_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            total_on_disk_bytes(dir.path()),
            0,
            "no artifacts → 0 bytes (AC-7)"
        );
    }

    /// AC-7 (#380): `artifact_len` fail-soft — a missing file is 0 bytes.
    #[test]
    fn test_artifact_len_missing_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(artifact_len(dir.path(), "index.skidx"), 0);
    }

    #[test]
    fn test_search_help_returns_success() {
        let result = run(&[], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_help_flag_returns_success() {
        let result = run(&["--help".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_short_help_flag_returns_success() {
        let result = run(&["-h".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    /// AC6 (#375): `skim search index --help` now prints PARENT search help and
    /// exits 0.  After removing the `index` positional intercept (AD-375-1, avoids
    /// PF-006), the `--help` short-circuit at the top of `run()` fires before any
    /// query dispatch, printing the parent help and returning SUCCESS.
    ///
    /// The deleted predecessor test (`test_index_help_dispatches_to_index_not_parent`)
    /// asserted the opposite: that the positional intercept routes `index --help` to
    /// `index::run` (which printed IndexCli help).  That premise is gone.
    ///
    /// Discriminating assertion (PF-007): `index --help` must exit SUCCESS AND the
    /// parent-help marker "layered n-gram BM25F" must be present.  If the intercept
    /// were restored, `index::run` would print IndexCli help (which does NOT contain
    /// "layered n-gram BM25F") — the stdout assertion would fail.
    #[test]
    fn test_index_help_token_prints_parent_help() {
        // Capture stdout by redirecting inside the test.  The easiest approach is to
        // drive the subprocess surface (via skim_bin_path) so stdout is truly captured.
        // The in-process `run()` call prints to stdout directly, so we use the binary.
        let output = std::process::Command::new(skim_bin_path())
            .args(["search", "index", "--help"])
            .output()
            .expect("skim binary must be invocable in test");

        assert!(
            output.status.success(),
            "`skim search index --help` must exit 0; got: {:?}",
            output.status
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parent-only marker: present in mod.rs print_help but absent from IndexCli
        // clap help (IndexCli about = "Build or update the search index for the
        // current project.", no BM25F or n-gram language).
        assert!(
            stdout.contains("layered n-gram BM25F"),
            "`skim search index --help` must print PARENT search help (containing \
             'layered n-gram BM25F'), not IndexCli help; got stdout:\n{stdout}"
        );
        // Confirm IndexCli's about string is NOT present (proves parent help, not
        // index-builder help, was rendered).
        assert!(
            !stdout.contains("Build or update the search index for the current project."),
            "`skim search index --help` must NOT print IndexCli help; got stdout:\n{stdout}"
        );
    }

    /// AC8 (#375): bareword 'index' is parsed as a query term, not a build action.
    ///
    /// Verifies that after removing the positional intercept (AD-375-1), `index`
    /// in argv is accumulated into `query_parts` and dispatched as
    /// `SearchAction::Query("index")`.  This is the parse_flags-layer discriminator:
    /// if the intercept were restored the intercept fires before parse_flags and
    /// this arm is unreachable via run() — but here we call parse_flags directly to
    /// assert the dispatch-mapping invariant.
    #[test]
    fn test_bareword_index_is_parsed_as_query() {
        let flags = parse_flags(&["index".to_string()]).unwrap();
        // Discriminating assertion (PF-007): must be a Query action, not Build.
        // If parse_flags somehow mapped "index" to SearchAction::Build, this fails.
        assert_eq!(
            flags.action,
            SearchAction::Query("index".to_string()),
            "bareword 'index' must be accumulated into a query, not routed to the builder"
        );
    }

    // ============================================================================
    // parse_flags — action dispatch
    // ============================================================================

    #[test]
    fn test_parse_flags_build() {
        let flags = parse_flags(&["--build".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::Build);
    }

    #[test]
    fn test_parse_flags_rebuild() {
        let flags = parse_flags(&["--rebuild".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::Rebuild);
    }

    #[test]
    fn test_stats_flag_parsed_correctly() {
        let flags = parse_flags(&["--stats".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::Stats);
    }

    #[test]
    fn test_install_hooks_flag_parsed() {
        let flags = parse_flags(&["--install-hooks".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::InstallHooks);
    }

    #[test]
    fn test_remove_hooks_flag_parsed() {
        let flags = parse_flags(&["--remove-hooks".to_string()]).unwrap();
        assert_eq!(flags.action, SearchAction::RemoveHooks);
    }

    // ============================================================================
    // parse_flags — modifier flags
    // ============================================================================

    #[test]
    fn test_parse_flags_limit() {
        let flags = parse_flags(&["--limit".to_string(), "5".to_string()]).unwrap();
        assert_eq!(flags.limit, 5);
    }

    #[test]
    fn test_parse_flags_limit_equals() {
        let flags = parse_flags(&["--limit=10".to_string()]).unwrap();
        assert_eq!(flags.limit, 10);
    }

    #[test]
    fn test_parse_flags_short_n() {
        let flags = parse_flags(&["-n".to_string(), "3".to_string()]).unwrap();
        assert_eq!(flags.limit, 3);
    }

    #[test]
    fn test_parse_flags_json() {
        let flags = parse_flags(&["--json".to_string()]).unwrap();
        assert!(flags.json);
    }

    #[test]
    fn test_parse_flags_offset_space() {
        let flags = parse_flags(&["--offset".to_string(), "5".to_string()]).unwrap();
        assert_eq!(flags.offset, Some(5));
    }

    #[test]
    fn test_parse_flags_offset_equals() {
        let flags = parse_flags(&["--offset=10".to_string()]).unwrap();
        assert_eq!(flags.offset, Some(10));
    }

    #[test]
    fn test_parse_flags_offset_zero() {
        let flags = parse_flags(&["--offset".to_string(), "0".to_string()]).unwrap();
        assert_eq!(flags.offset, Some(0));
    }

    #[test]
    fn test_parse_flags_offset_default_is_none() {
        let flags = parse_flags(&["--limit".to_string(), "5".to_string()]).unwrap();
        assert_eq!(
            flags.offset, None,
            "offset must default to None when not supplied"
        );
    }

    #[test]
    fn test_parse_flags_offset_invalid_is_error() {
        let err = parse_flags(&["--offset".to_string(), "abc".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--offset"),
            "error message must mention '--offset'; got: {err}"
        );
    }

    #[test]
    fn test_parse_flags_root_space() {
        let flags = parse_flags(&["--root".to_string(), "/tmp/proj".to_string()]).unwrap();
        assert_eq!(flags.root_override, Some(PathBuf::from("/tmp/proj")));
    }

    #[test]
    fn test_parse_flags_root_equals() {
        let flags = parse_flags(&["--root=/tmp/other".to_string()]).unwrap();
        assert_eq!(flags.root_override, Some(PathBuf::from("/tmp/other")));
    }

    // ============================================================================
    // parse_flags — query text
    // ============================================================================

    #[test]
    fn test_parse_flags_query_text() {
        let flags = parse_flags(&["fn".to_string(), "parse_url".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("fn parse_url".to_string())
        );
    }

    #[test]
    fn test_parse_flags_combined_json_limit_query() {
        let flags = parse_flags(&[
            "--json".to_string(),
            "--limit".to_string(),
            "5".to_string(),
            "authenticate".to_string(),
        ])
        .unwrap();
        assert!(flags.json);
        assert_eq!(flags.limit, 5);
        assert_eq!(
            flags.action,
            SearchAction::Query("authenticate".to_string())
        );
    }

    // ============================================================================
    // parse_flags — error cases
    // ============================================================================

    #[test]
    fn test_parse_flags_limit_missing_value_is_error() {
        let err = parse_flags(&["--limit".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit requires a value"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_non_numeric_is_error() {
        let err = parse_flags(&["--limit".to_string(), "abc".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_equals_non_numeric_is_error() {
        let err = parse_flags(&["--limit=abc".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn test_parse_flags_root_missing_value_is_error() {
        let err = parse_flags(&["--root".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--root requires"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn test_parse_flags_unrecognised_flag_is_error() {
        let err = parse_flags(&["--unknown-flag".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unrecognised flag"),
            "unexpected error message: {msg}"
        );
        // KNOWN_FLAGS completeness: the error message embeds KNOWN_FLAGS verbatim,
        // so `-n` (the --limit short alias added in #412) must appear in the
        // "Valid flags" listing.  A regression removing `-n` from KNOWN_FLAGS would
        // make this test fail while all other assertions still pass.
        assert!(
            msg.contains("-n"),
            "unrecognised-flag error must list -n in the valid-flags help text; got: {msg}"
        );
        // --help/-h must appear in the valid-flags listing so users see a complete
        // picture of recognised tokens when they mistype a flag.
        assert!(
            msg.contains("--help"),
            "unrecognised-flag error must list --help in the valid-flags help text; got: {msg}"
        );
    }

    // ============================================================================
    // AD-412-3: --help/-h must not be silently consumed as a flag value
    // ============================================================================

    /// `--limit --help`: `--help` must not be swallowed as the limit value.
    #[test]
    fn test_parse_flags_help_after_limit_is_not_consumed_as_value() {
        let err = parse_flags(&["--limit".to_string(), "--help".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--limit requires a value"),
            "AD-412-3: --help must not be silently consumed as the --limit value; got: {msg}"
        );
        assert!(
            msg.contains("skim search --help"),
            "error must point at the help surface; got: {msg}"
        );
    }

    /// `--root --help`: `--help` must not be silently accepted as a directory path.
    ///
    /// Before the fix `--root --help` ran a real search rooted at the bogus
    /// path "--help" with exit 0, which was the worst outcome (silent wrong
    /// behavior).  After the fix it is a clear error.
    #[test]
    fn test_parse_flags_help_after_root_is_not_consumed_as_value() {
        let err = parse_flags(&["--root".to_string(), "--help".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--root requires a value"),
            "AD-412-3: --help must not be silently consumed as the --root path; got: {msg}"
        );
        assert!(
            msg.contains("skim search --help"),
            "error must point at the help surface; got: {msg}"
        );
    }

    // ============================================================================
    // AD-412-3: help-vs-error precedence (finding 5) — pin sequential behavior
    // ============================================================================

    /// `--badflag --help` → error, NOT help.
    ///
    /// Pre-delta (ad7b590), `run()` pre-scanned argv for `--help`/`-h` BEFORE
    /// calling `parse_flags`, so a help request anywhere in argv — even after an
    /// invalid flag — printed help and exited 0.
    ///
    /// Post-AD-412-3, `parse_flags` processes tokens sequentially: `--badflag`
    /// hits the unrecognised-flag arm and errors before `--help` is reached.
    /// This is consistent with #412's strict left-to-right direction and with
    /// getopt semantics.  The test pins the new behaviour so any future regression
    /// (restoring a pre-scan) is immediately detected (PF-007: discriminating).
    #[test]
    fn test_parse_flags_bad_flag_before_help_errors_not_help() {
        let err = parse_flags(&["--badflag".to_string(), "--help".to_string()]).unwrap_err();
        let msg = err.to_string();
        // Must error on the bad flag — NOT silently print help.
        assert!(
            msg.contains("unrecognised flag"),
            "AD-412-3: --badflag before --help must produce an error (not help); got: {msg}"
        );
        // The error must name the offending token (PF-007: discriminating observable).
        assert!(
            msg.contains("--badflag"),
            "error must name the unrecognised flag '--badflag'; got: {msg}"
        );
    }

    /// `--limit xyz --help` → error (bad value wins, not help).
    ///
    /// The invalid value `xyz` triggers `parse_limit_value` to error before
    /// `--help` is reached (another sequential-error-wins case).
    #[test]
    fn test_parse_flags_bad_limit_value_before_help_errors_not_help() {
        let err = parse_flags(&[
            "--limit".to_string(),
            "xyz".to_string(),
            "--help".to_string(),
        ])
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("positive integer"),
            "AD-412-3: bad --limit value before --help must produce a parse error; got: {msg}"
        );
    }

    // ============================================================================
    // Unknown single-dash flags rejected symmetrically (AC1, AC11)
    // ============================================================================

    /// AC1: -i is rejected with "unrecognised flag" (not folded into the query).
    /// AC11: error message mentions the `--` escape hatch.
    #[test]
    fn test_parse_flags_unknown_short_flag_i_is_error() {
        let err = parse_flags(&["-i".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unrecognised flag"),
            "AC1: -i must produce 'unrecognised flag'; got: {msg}"
        );
        assert!(
            msg.contains("--"),
            "AC11: error message must mention the '--' escape hatch; got: {msg}"
        );
    }

    /// AC1: -w is rejected symmetrically with long flags.
    #[test]
    fn test_parse_flags_unknown_short_flag_w_is_error() {
        let err = parse_flags(&["-w".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("unrecognised flag"),
            "AC1: -w must produce 'unrecognised flag'"
        );
    }

    /// AC1: -C is rejected symmetrically with long flags.
    #[test]
    fn test_parse_flags_unknown_short_flag_c_is_error() {
        let err = parse_flags(&["-C".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("unrecognised flag"),
            "AC1: -C must produce 'unrecognised flag'"
        );
    }

    // ============================================================================
    // `--` end-of-flags separator (AC2, AC3, AC10)
    // ============================================================================

    /// AC2: `['--', '-i']` yields Query("-i") — separator drains following tokens verbatim.
    #[test]
    fn test_parse_flags_dashdash_before_short_flag_becomes_query() {
        let flags = parse_flags(&["--".to_string(), "-i".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("-i".to_string()),
            "AC2: ['--', '-i'] must yield Query(\"-i\")"
        );
    }

    /// AC2: `['foo', '--', '--limit']` yields Query("foo --limit").
    #[test]
    fn test_parse_flags_query_then_dashdash_drains_flag_as_literal() {
        let flags =
            parse_flags(&["foo".to_string(), "--".to_string(), "--limit".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("foo --limit".to_string()),
            "AC2: ['foo', '--', '--limit'] must yield Query(\"foo --limit\")"
        );
    }

    /// AC2/AC3: `['--', '--rebuild']` yields Query("--rebuild") — no rebuild action fired.
    #[test]
    fn test_parse_flags_dashdash_before_action_flag_yields_query() {
        let flags = parse_flags(&["--".to_string(), "--rebuild".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("--rebuild".to_string()),
            "AC2/AC3: ['--', '--rebuild'] must yield Query(\"--rebuild\"); no rebuild action"
        );
    }

    // ============================================================================
    // AC10: bare `-` stays positional; dash-leading literals searchable via `--`
    // ============================================================================

    /// AC10: bare `-` (len == 1) is a valid query token, not a flag.
    #[test]
    fn test_parse_flags_bare_dash_is_positional() {
        let flags = parse_flags(&["-".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("-".to_string()),
            "AC10: bare '-' must be treated as a positional query token"
        );
    }

    /// AC10: `['--', '->']` yields Query("->").
    #[test]
    fn test_parse_flags_dashdash_before_arrow_becomes_query() {
        let flags = parse_flags(&["--".to_string(), "->".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("->".to_string()),
            "AC10: ['--', '->'] must yield Query(\"->\")"
        );
    }

    /// AC10: `['--', '-Werror']` yields Query("-Werror").
    #[test]
    fn test_parse_flags_dashdash_before_werror_becomes_query() {
        let flags = parse_flags(&["--".to_string(), "-Werror".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("-Werror".to_string()),
            "AC10: ['--', '-Werror'] must yield Query(\"-Werror\")"
        );
    }

    /// AC10: `['--', '-5']` yields Query("-5").
    #[test]
    fn test_parse_flags_dashdash_before_negative_number_becomes_query() {
        let flags = parse_flags(&["--".to_string(), "-5".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("-5".to_string()),
            "AC10: ['--', '-5'] must yield Query(\"-5\")"
        );
    }

    /// Only the first `--` is special; a second `--` becomes literal query text.
    /// `['--', '--', 'x']` must yield Query("-- x") — the second `--` is drained
    /// verbatim as a query token, not re-interpreted as a separator.
    #[test]
    fn test_parse_flags_second_dashdash_is_literal_query_text() {
        let flags = parse_flags(&["--".to_string(), "--".to_string(), "x".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("-- x".to_string()),
            "second '--' must be literal query text, not a separator; expected Query(\"-- x\")"
        );
    }

    /// Trailing `--` with nothing after it drains an empty slice — must not panic
    /// and must preserve any query tokens accumulated before the separator.
    /// `['foo', '--']` must yield Query("foo").
    #[test]
    fn test_parse_flags_trailing_dashdash_yields_preceding_query() {
        let flags = parse_flags(&["foo".to_string(), "--".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Query("foo".to_string()),
            "trailing '--' with nothing after must yield Query(\"foo\"), not panic"
        );
    }

    // ============================================================================
    // Mixed-form hard error: action flag + query text (AC14)
    // ============================================================================

    /// AC14: `['foo', '--rebuild']` is rejected — cannot combine query and action flag.
    #[test]
    fn test_parse_flags_mixed_form_query_and_rebuild_is_error() {
        let err = parse_flags(&["foo".to_string(), "--rebuild".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot combine"),
            "AC14: mixed query+action must produce 'cannot combine'; got: {msg}"
        );
        assert!(
            msg.contains("--"),
            "AC14: error message must mention the '--' escape hatch; got: {msg}"
        );
    }

    /// AC14: `['myterm', '--build']` is also rejected as mixed form.
    #[test]
    fn test_parse_flags_mixed_form_query_and_build_is_error() {
        let err = parse_flags(&["myterm".to_string(), "--build".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("cannot combine"),
            "AC14: query + --build must be rejected as mixed form"
        );
    }

    /// AD-412-5: a whitespace-only positional combined with an action flag MUST
    /// succeed (not error as mixed form).
    ///
    /// `validate_no_mixed_form` uses `.any(|p| !p.trim().is_empty())` so a
    /// positional whose only content is whitespace is not counted as a real text
    /// query.  `skim search "  " --rebuild` therefore dispatches to `Rebuild` and
    /// silently discards the whitespace token — consistent with every downstream
    /// dispatch site that trims before use.
    ///
    /// Discriminating coverage (avoids PF-007): a regression reverting the predicate
    /// to `!query_parts.is_empty()` would restore the old `Err("cannot combine")`
    /// and make this test fail, while the two non-whitespace tests above (`foo`,
    /// `myterm`) continue to pass — so the stale guard cannot hide behind them.
    #[test]
    fn test_parse_flags_whitespace_only_positional_with_action_flag_succeeds() {
        let flags = parse_flags(&["  ".to_string(), "--rebuild".to_string()]).unwrap();
        assert_eq!(
            flags.action,
            SearchAction::Rebuild,
            "AD-412-5: whitespace-only positional + --rebuild must yield Rebuild, \
             not a mixed-form error"
        );
    }

    #[test]
    fn test_parse_flags_short_n_missing_value_is_error() {
        let err = parse_flags(&["-n".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit requires a value"),
            "unexpected error message: {err}"
        );
    }

    // ============================================================================
    // Regression: -j short alias for --json (issue mod.rs:136)
    // ============================================================================

    #[test]
    fn test_parse_flags_short_j_sets_json() {
        let flags = parse_flags(&["-j".to_string()]).unwrap();
        assert!(flags.json, "-j must set json=true");
    }

    #[test]
    fn test_parse_flags_short_j_combined_with_query() {
        let flags = parse_flags(&["-j".to_string(), "authenticate".to_string()]).unwrap();
        assert!(flags.json);
        assert_eq!(
            flags.action,
            SearchAction::Query("authenticate".to_string())
        );
    }

    // ============================================================================
    // Regression: --limit 0 must be rejected (issue mod.rs:142)
    // ============================================================================

    #[test]
    fn test_parse_flags_limit_zero_space_is_error() {
        let err = parse_flags(&["--limit".to_string(), "0".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit must be >= 1"),
            "expected rejection of 0, got: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_zero_equals_is_error() {
        let err = parse_flags(&["--limit=0".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("--limit must be >= 1"),
            "expected rejection of 0, got: {err}"
        );
    }

    #[test]
    fn test_parse_flags_limit_one_is_valid() {
        let flags = parse_flags(&["--limit".to_string(), "1".to_string()]).unwrap();
        assert_eq!(flags.limit, 1);
    }

    // ── Removed flags ─────────────────────────────────────────────────────────

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// Each removed flag yields a one-line message that names the flag and the
    /// replacement, so a reader knows what to run instead.
    #[test]
    fn removed_flag_message_names_flag_and_replacement() {
        for (flag, advice) in REMOVED_FLAGS {
            let message = removed_flag_message(flag)
                .unwrap_or_else(|| panic!("{flag} must be refused as a removed flag"));
            assert!(
                message.starts_with("skim search: "),
                "message must carry the program prefix: {message:?}"
            );
            assert!(
                message.contains(flag) && message.contains(advice),
                "message must name {flag} and its replacement {advice:?}: {message:?}"
            );
            assert!(!message.contains('\n'), "message must be one line");
        }
    }

    /// The attached-value form (`--ast=x`) is refused too, and the attached text
    /// never reaches the message.
    #[test]
    fn removed_flag_message_covers_attached_value_without_echoing_it() {
        let message = removed_flag_message("--ast=try-catch\u{1b}[31m").unwrap();
        assert!(message.contains("--ast was removed"), "got {message:?}");
        assert!(
            !message.contains("try-catch") && !message.contains('\u{1b}'),
            "attached text must not reach the message: {message:?}"
        );
    }

    /// Flags and words that only start like a removed flag are not refused.
    #[test]
    fn removed_flag_message_ignores_lookalikes() {
        for arg in [
            "--ast-grep",
            "--hotfix",
            "--weightsx",
            "--root",
            "ast",
            "-hot",
        ] {
            assert_eq!(
                removed_flag_message(arg),
                None,
                "{arg} is not a removed flag"
            );
        }
    }

    /// A removed flag ends parsing at once: a later bad value must not replace
    /// the refusal with a different error.
    #[test]
    fn parse_flags_removed_flag_wins_over_later_tokens() {
        let flags = parse_flags(&args(&["--ast", "try-catch", "--limit", "abc"])).unwrap();
        assert!(
            matches!(&flags.action, SearchAction::Removed(m) if m.contains("--ast")),
            "expected a removal refusal, got {:?}",
            flags.action
        );
    }

    /// A removed flag after a text query still refuses; the query is not run.
    #[test]
    fn parse_flags_removed_flag_after_text_query_is_refused() {
        let flags = parse_flags(&args(&["needle", "--hot"])).unwrap();
        assert!(
            matches!(&flags.action, SearchAction::Removed(m) if m.contains("--hot")),
            "expected a removal refusal, got {:?}",
            flags.action
        );
    }

    /// After `--`, a removed flag name is query text, not a flag.
    #[test]
    fn parse_flags_removed_flag_name_after_double_dash_is_query_text() {
        let flags = parse_flags(&args(&["--", "--ast"])).unwrap();
        assert_eq!(flags.action, SearchAction::Query("--ast".to_string()));
    }

    /// The refusal exits 2, the code for a refused command line.
    #[test]
    fn removed_flag_exit_code_is_two() {
        assert_eq!(REMOVED_FLAG_EXIT_CODE, 2);
    }

    /// Shared git-repo helper — delegates to the canonical `staleness::create_real_git_repo`.
    /// Named identically to its counterpart in `staleness_tests.rs` so readers
    /// scanning the two test files see a single shared-helper relationship rather
    /// than two apparently-distinct helpers.
    fn create_real_git_repo(
        dir: &std::path::Path,
        commit_specs: &[(&str, &[(&str, &str)])],
    ) -> String {
        staleness::create_real_git_repo(dir, commit_specs)
    }

    /// Locate the single per-root search cache directory beneath an isolated
    /// `SKIM_CACHE_DIR`, without re-reading the env var in this process.
    fn only_search_cache_dir(cache_root: &std::path::Path) -> std::path::PathBuf {
        let search_dir = cache_root.join("search");
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&search_dir)
            .unwrap_or_else(|e| panic!("no search cache dir at {search_dir:?}: {e}"))
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one per-root cache dir under {search_dir:?}, got {entries:?}"
        );
        entries.pop().unwrap()
    }

    /// Shared helper for #402 unit tests: create a real git repo with a
    /// tracked-but-.gitignored file, used by both `walk_tests` and `index_tests`
    /// (eliminates the two near-byte-identical per-module copies that matched the
    /// per-module helper convention but lived in the SAME rskim crate).
    ///
    /// Layout after init + commit:
    /// ```text
    /// root/
    ///   .gitignore       <- contains "secretdoc.md"
    ///   secretdoc.md     <- tracked via `git add -f`; content = "ZZUNIQUETOKEN"
    ///   src/a.rs         <- regular tracked file
    /// ```
    pub(crate) fn make_tracked_ignored_repo() -> tempfile::TempDir {
        use std::process::Command as StdCmd;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        StdCmd::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");
        StdCmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .output()
            .expect("git config email");
        StdCmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .output()
            .expect("git config name");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "secretdoc.md\n").unwrap();
        std::fs::write(root.join("secretdoc.md"), "ZZUNIQUETOKEN\n").unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        StdCmd::new("git")
            .args(["add", "-f", "secretdoc.md", "src/a.rs", ".gitignore"])
            .current_dir(root)
            .output()
            .expect("git add");
        StdCmd::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("git commit");
        dir
    }

    // =========================================================================
    // Step 11: T-13/T-14 — actionable lexical error and stats self-heal
    // (AD-414-7, AD-414-10, AC-13, AC-14)
    // =========================================================================

    /// T-13 / AC-13: a size-preserving byte flip in `index.skpost` (with
    /// `index.skverify` deleted to force the full CRC32 path) MUST cause a query
    /// to exit 1 with an error message that:
    ///
    /// (1) names the artifact directory (the cache-dir path),
    /// (2) names the failing artifact (`index.skpost`),
    /// (3) contains `skim search --rebuild`,
    /// (4) contains `No such file or directory` at most once.
    ///
    /// PF-007: asserts observable error message content, not just exit code.
    /// PF-018: byte-flip preserves file length so the structural size probe passes;
    ///         only the full CRC32 detects the corruption.
    /// SKIM_CACHE_DIR isolation: #[serial] prevents concurrent env-var mutations
    /// from other in-process tests flipping the resolved cache dir between the
    /// --rebuild call and the subsequent resolve_search_cache_dir probe.
    #[serial_test::serial]
    #[test]
    fn t13_crc_mismatch_names_artifact_and_rebuild_hint() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();

        create_real_git_repo(
            root,
            &[
                ("feat: alpha", &[("src/alpha.rs", "fn alpha() {}")]),
                ("feat: beta", &[("src/beta.rs", "fn beta() {}")]),
            ],
        );

        // Build a full index so index.skpost exists with a valid CRC.
        let result = run(
            &[
                "--rebuild".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-13 setup: --rebuild must succeed before byte-flip"
        );

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let post_path = cache_dir.join("index.skpost");
        let verify_path = cache_dir.join("index.skverify");

        // Byte-flip the last byte of index.skpost, preserving file length (PF-018).
        // The structural size probe compares lengths only, so it passes; the full
        // CRC32 (which covers post_mmap + idx_mmap body) detects the corruption.
        {
            let mut bytes = std::fs::read(&post_path).expect("T-13: index.skpost must exist");
            assert!(
                !bytes.is_empty(),
                "T-13: index.skpost must be non-empty to flip a byte"
            );
            let last = bytes.last_mut().unwrap();
            *last = last.wrapping_add(1);
            std::fs::write(&post_path, &bytes).expect("T-13: rewriting index.skpost must succeed");
        }

        // Delete index.skverify so the fast-path (cached CRC signature) is bypassed
        // and the full CRC32 computation runs.
        let _ = std::fs::remove_file(&verify_path);

        let cache_dir_display = cache_dir.display().to_string();

        // A query must fail with an actionable error (AC-13).
        let result = run(
            &[
                "zebra_widget".to_string(),
                "--root".to_string(),
                root_str.clone(),
                "--limit".to_string(),
                "2".to_string(),
            ],
            &TEST_ANALYTICS,
        );

        // AC-13: exit 1 (run() returns Err for unrecoverable open failures).
        let err = result.unwrap_err();
        let msg = format!("{err:#}");

        // AC-13 clause 1: error names the artifact directory.
        assert!(
            msg.contains(&cache_dir_display),
            "T-13/AC-13 clause 1: error must name the artifact directory; got: {msg:?}"
        );
        // AC-13 clause 2: error names the failing artifact.
        assert!(
            msg.contains("index.skpost"),
            "T-13/AC-13 clause 2: error must name the failing artifact (index.skpost); got: {msg:?}"
        );
        // AC-13 clause 3: error contains the rebuild hint.
        assert!(
            msg.contains("skim search --rebuild"),
            "T-13/AC-13 clause 3: error must contain 'skim search --rebuild'; got: {msg:?}"
        );
        // AC-13 clause 4: OS error phrase is not duplicated.
        let no_such_file_count = msg.matches("No such file or directory").count();
        assert!(
            no_such_file_count <= 1,
            "T-13/AC-13 clause 4: 'No such file or directory' must appear at most once; \
             got {no_such_file_count} times in: {msg:?}"
        );
    }

    /// T-14 / AC-14: `--stats --json` on F-Body-A (truncated `index.skpost`) MUST
    /// exit 0 after self-healing through `auto_refresh_if_stale` → rebuild, and the
    /// returned JSON must be a complete stats object (not an error object).
    ///
    /// Covers the G-4 fix: previously `build_stats_json` opened the reader BEFORE
    /// `check_staleness`, so body-corrupt indexes never reached the self-heal path
    /// and `--stats` always exited 1 with no JSON on stdout.
    ///
    /// PF-007: asserts observable output, not just exit code.
    /// SKIM_CACHE_DIR isolation: #[serial] prevents concurrent env-var mutations
    /// from other in-process tests racing the resolve_search_cache_dir call.
    #[serial_test::serial]
    #[test]
    fn t14_stats_json_self_heals_on_fbody_a_truncated_skpost() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        // Need a real git repo so the HEAD-based staleness check also runs.
        create_real_git_repo(
            root,
            &[
                ("feat: first", &[("src/a.rs", "fn alpha() {}")]),
                ("feat: second", &[("src/b.rs", "fn beta() {}")]),
            ],
        );

        // Build the index.
        let result = run(
            &[
                "--rebuild".to_string(),
                "--root".to_string(),
                root.to_string_lossy().to_string(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-14 setup: --rebuild must succeed"
        );

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let skpost_path = cache_dir.join("index.skpost");

        // Corrupt: truncate skpost to half its size (F-Body-A).
        let original_len = std::fs::metadata(&skpost_path).unwrap().len();
        let half_len = (original_len / 2).max(1);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&skpost_path)
            .unwrap();
        file.set_len(half_len).unwrap();

        // --stats --json must self-heal and return a complete stats object.
        let stats = stats_json_for_test(&cache_dir, root)
            .expect("T-14: stats_json_for_test must succeed after self-heal on F-Body-A");

        // Must NOT be an error object (PF-007).
        assert!(
            stats.get("error").is_none(),
            "T-14: stats after self-heal must not contain 'error'; got: {stats:?}"
        );
        assert!(
            stats.get("file_count").is_some(),
            "T-14: stats after self-heal must contain 'file_count'; got: {stats:?}"
        );

        // index.skpost must be restored to a consistent length.
        let restored_len = std::fs::metadata(&skpost_path).unwrap().len();
        assert!(
            restored_len != half_len,
            "T-14: skpost must be restored (not still half-length) after self-heal"
        );
    }

    /// T-14(b) / AC-14: `--stats` text mode on F-Body-C (removed `index.skpost`)
    /// must exit 0 after self-healing (same route as T-14 but text mode).
    /// SKIM_CACHE_DIR isolation: #[serial] prevents concurrent env-var mutations
    /// from other in-process tests racing the resolve_search_cache_dir call.
    #[serial_test::serial]
    #[test]
    fn t14b_stats_text_self_heals_on_fbody_c_removed_skpost() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        create_real_git_repo(
            root,
            &[
                ("feat: x", &[("src/x.rs", "fn x() {}")]),
                ("feat: y", &[("src/y.rs", "fn y() {}")]),
            ],
        );

        let root_str = root.to_string_lossy().to_string();
        let result = run(
            &[
                "--rebuild".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-14b setup: --rebuild must succeed"
        );

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        // F-Body-C: remove skpost entirely.
        std::fs::remove_file(cache_dir.join("index.skpost")).unwrap();

        // --stats text mode: exit 0 after self-heal.
        let result = run(
            &["--stats".to_string(), "--root".to_string(), root_str],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-14b: --stats on F-Body-C (removed skpost) must exit 0 after self-heal (AC-14)"
        );

        // skpost must be restored.
        assert!(
            cache_dir.join("index.skpost").exists(),
            "T-14b: index.skpost must be restored after self-heal"
        );
    }
    // ========================================================================
    // T-16 / F5 / AD-414-23 — the `--stats` staleness contract, via the CLI
    // ========================================================================

    /// Run `skim search <args>` as a subprocess against an isolated cache dir.
    ///
    /// Returns `(exit_success, stdout, stderr)`. Driving the real binary is what
    /// makes these tests CLI-entry tests: `--stats`' no-index early return lives
    /// in `run_stats` and is invisible to the in-process `stats_json_for_test`
    /// helper, which starts at `gather_stats` (PF-007 — the earlier revisions of
    /// T-16/T-16b asserted a value that the CLI cannot produce precisely because
    /// they entered below the arm that decides it).
    fn run_skim_search_capture(bin: &str, cache: &str, args: &[&str]) -> (bool, String, String) {
        let out = std::process::Command::new(bin)
            .arg("search")
            .args(args)
            .env("SKIM_CACHE_DIR", cache)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .env_remove("SKIM_DEBUG")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn skim search {args:?}: {e}"));
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// T-16 / F5 / AD-414-10: `--stats --json` reports `staleness` from the
    /// PRE-self-heal state while `file_count` comes from the POST-heal state.
    ///
    /// F-Body-C (remove `index.skpost`, keep `index.skidx`) is the reachable
    /// demonstration of that asymmetry through the CLI: `check_staleness` sees a
    /// size-inconsistent lexical index and returns `NoStoredHead`, the self-heal
    /// rebuilds, and the reported `file_count` counts the rebuilt corpus.
    ///
    /// The expected string comes from `StalenessCheck`'s own `Display`, never a
    /// hand-written literal, so a reworded variant fails here rather than drifting.
    ///
    /// `#[serial]` matches its T-13/T-14/T-15 siblings; the child process is given
    /// an explicit `SKIM_CACHE_DIR`, so here it is defence in depth rather than a
    /// strict requirement.
    #[serial_test::serial]
    #[test]
    fn t16_f5_stats_json_staleness_is_the_pre_heal_snapshot() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();
        let cache = tempfile::TempDir::new().unwrap();
        let cache_str = cache.path().to_string_lossy().to_string();

        create_real_git_repo(
            root,
            &[
                ("feat: add a", &[("src/a.rs", "fn alpha() {}")]),
                ("feat: add b", &[("src/b.rs", "fn beta() {}")]),
            ],
        );

        let (ok, _, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--rebuild", "--root", &root_str]);
        assert!(ok, "T-16 setup: --rebuild must succeed; stderr:\n{stderr}");

        let search_cache = only_search_cache_dir(cache.path());

        // Healthy precondition: staleness == "current".
        let (ok, stdout, stderr) = run_skim_search_capture(
            &bin,
            &cache_str,
            &["--stats", "--json", "--root", &root_str],
        );
        assert!(
            ok,
            "T-16 setup: --stats --json must exit 0; stderr:\n{stderr}"
        );
        let healthy: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("T-16 setup: stdout must be one JSON object ({e}): {stdout}")
        });
        assert_eq!(
            healthy["staleness"].as_str().unwrap_or(""),
            staleness::StalenessCheck::Current.to_string(),
            "T-16 pre-condition: a healthy index must report staleness == 'current'"
        );

        // F-Body-C: remove the posting body, keep the header.
        let skpost = search_cache.join("index.skpost");
        std::fs::remove_file(&skpost).unwrap();

        let (ok, stdout, stderr) = run_skim_search_capture(
            &bin,
            &cache_str,
            &["--stats", "--json", "--root", &root_str],
        );
        assert!(
            ok,
            "T-16/AC-14: --stats --json must exit 0 after self-healing; stderr:\n{stderr}"
        );
        let stats: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("T-16: stdout must be one JSON object ({e}): {stdout}"));
        assert!(
            stats.get("error").is_none(),
            "T-16: a self-healed --stats must not return an error object; got {stats}"
        );
        assert_eq!(
            stats["staleness"].as_str().unwrap_or(""),
            staleness::StalenessCheck::NoStoredHead.to_string(),
            "T-16/AD-414-10: staleness is the PRE-heal verdict; got {}",
            stats["staleness"]
        );
        assert!(
            stats["file_count"].as_u64().unwrap_or(0) > 0,
            "T-16/AD-414-10: file_count is the POST-heal state and must be > 0 even \
             though staleness reports the pre-heal verdict; got {stats}"
        );
        assert!(
            skpost.exists(),
            "T-16/AC-14: index.skpost must be restored by the self-heal"
        );
    }

    /// T-16(b) / F5: the same pre-heal `staleness` verdict on the **text** arm.
    ///
    /// `run_stats`' text branch renders from the same snapshot as the JSON branch;
    /// this pins that they cannot diverge. The expected line is assembled from the
    /// production `Display` impl, not a copied literal.
    #[serial_test::serial]
    #[test]
    fn t16b_f5_stats_text_staleness_line_is_the_pre_heal_snapshot() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();
        let cache = tempfile::TempDir::new().unwrap();
        let cache_str = cache.path().to_string_lossy().to_string();

        create_real_git_repo(
            root,
            &[
                ("feat: add x", &[("src/x.rs", "fn ex() {}")]),
                ("feat: add y", &[("src/y.rs", "fn why() {}")]),
            ],
        );

        let (ok, _, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--rebuild", "--root", &root_str]);
        assert!(ok, "T-16b setup: --rebuild must succeed; stderr:\n{stderr}");

        let search_cache = only_search_cache_dir(cache.path());
        std::fs::remove_file(search_cache.join("index.skpost")).unwrap();

        let (ok, stdout, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--stats", "--root", &root_str]);
        assert!(
            ok,
            "T-16b/AC-14: --stats (text) must exit 0 after self-healing; stderr:\n{stderr}"
        );
        let expected = format!(
            "staleness     : {}",
            staleness::StalenessCheck::NoStoredHead
        );
        assert!(
            stdout.contains(&expected),
            "T-16b: --stats text output must carry {expected:?}; got stdout:\n{stdout}"
        );
    }

    /// T-16(d) / AD-414-23: on a root that was **never** indexed, `--stats` does
    /// not build one — it prints the `{error, cache_dir}` object and exits 1.
    ///
    /// This is #413's AC21 contract, unchanged by #414, and it is why
    /// `StalenessCheck::NoIndex`'s `Display` form `"no index"` is not among the
    /// values the `staleness` key can take: `run_stats` short-circuits the exact
    /// condition that produces it. CLAUDE.md, the CHANGELOG and the cmd-search
    /// knowledge base previously documented `"no index"` as observable here;
    /// measured false and corrected alongside this test.
    ///
    /// Discriminating in both directions: if `--stats` ever starts building a
    /// missing index, the exit code, the object shape and the "no index.skidx was
    /// created" assertion all fail together.
    #[serial_test::serial]
    #[test]
    fn t16d_ad414_23_cold_start_stats_reports_no_index_error_and_builds_nothing() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();
        let cache = tempfile::TempDir::new().unwrap();
        let cache_str = cache.path().to_string_lossy().to_string();

        // A healthy repository — the ONLY thing missing is the index, so the
        // no-index arm is the sole possible cause of the response.
        create_real_git_repo(root, &[("feat: add a", &[("src/a.rs", "fn alpha() {}")])]);

        // --- JSON mode ---
        let (ok, stdout, stderr) = run_skim_search_capture(
            &bin,
            &cache_str,
            &["--stats", "--json", "--root", &root_str],
        );
        assert!(
            !ok,
            "T-16d/#413 AC21: cold-start --stats --json must exit 1; stdout:\n{stdout}"
        );
        let obj: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("T-16d: stdout must be one JSON object ({e}): {stdout}"));
        let map = obj
            .as_object()
            .expect("T-16d: stdout must be a JSON object");
        assert_eq!(
            map.len(),
            2,
            "T-16d/#413 AC21: the object is exactly {{error, cache_dir}}; got {obj}"
        );
        assert_eq!(
            map.get("error").and_then(|v| v.as_str()),
            Some("no index found"),
            "T-16d: the error string is part of the pinned contract; got {obj}"
        );
        assert!(
            map.contains_key("cache_dir"),
            "T-16d: cache_dir answers 'where would it go?'; got {obj}"
        );
        assert!(
            map.get("staleness").is_none() && map.get("file_count").is_none(),
            "T-16d/AD-414-23: no stats are gathered on this arm, so neither \
             'staleness' nor 'file_count' may appear; got {obj}"
        );
        assert!(
            !stderr.contains("no index found"),
            "T-16d/AC-23: in --json mode the single object carries the error on \
             STDOUT; it must not be duplicated onto stderr. Got stderr:\n{stderr}"
        );

        // --- text mode ---
        let (ok, _, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--stats", "--root", &root_str]);
        assert!(
            !ok,
            "T-16d/#413 AC21: cold-start --stats (text) must exit 1"
        );
        assert!(
            stderr.contains("no index found") && stderr.contains("skim search --build"),
            "T-16d: text mode must name the condition and the remedy; got stderr:\n{stderr}"
        );

        // AD-414-23: --stats reports on an index, it never creates one.
        let search_dir = cache.path().join("search");
        let built: Vec<std::path::PathBuf> = std::fs::read_dir(&search_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten())
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.file_name().is_some_and(|n| n == "index.skidx"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            built.is_empty(),
            "T-16d/AD-414-23: --stats must not build a missing index; found {built:?}"
        );
    }
}
