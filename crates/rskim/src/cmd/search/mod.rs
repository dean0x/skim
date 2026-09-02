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

mod ast;
mod build_lock;
mod gitdir;
pub(crate) mod hooks;
mod index;
mod manifest;
mod query;
mod snippet;
mod staleness;
mod temporal;
mod temporal_build;
mod temporal_state;
mod types;
mod walk;

use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;

// ============================================================================
// User-facing message constants
// ============================================================================

/// Warning message emitted (to stderr or JSON envelope) when a standalone
/// temporal query (`--hot`/`--cold`/`--risky`/`--blast-radius`) finds no
/// temporal data after the self-heal attempt.
///
/// Single source of truth for AC9 and for every other "no temporal data"
/// message in this module tree (used in run_temporal_standalone, the --ast arm,
/// and temporal.rs --blast-radius path via `super::NO_TEMPORAL_DATA_MSG`).
/// Changing the production message here immediately breaks the AC9 test,
/// preventing silent regression to the old manual-rebuild advice (#357 cycle-2).
pub(super) const NO_TEMPORAL_DATA_MSG: &str =
    "no temporal data — run 'skim search' on a git repo to auto-populate";

/// git HEAD is inside a repo but the symbolic-ref could not be resolved to a SHA.
///
/// Causes: (1) unborn branch — HEAD points to a branch that has no commits yet;
/// (2) unsupported ref backend (e.g. reftable — tracked in #481).
/// Named constant so tests can assert against it without hardcoding the text.
/// AD-413-8. Must NOT contain "run 'skim search' on a git repo" (AC18(a)/AC33(c)).
pub(super) const HEAD_UNRESOLVED_TEMPORAL_MSG: &str = "git HEAD could not be resolved to a SHA (unborn branch, or unsupported ref \
     backend such as reftable — tracked in #481); temporal data unavailable";

/// Emitted by [`temporal_unavailable_msg`] for any [`staleness::HeadState::Resolved`]
/// root that has no usable temporal data and no [`staleness::AnchorState::Differs`]
/// mismatch.  The three covered cases are:
///
/// 1. **Absent DB** — `temporal.db` was never built (first run, or after
///    `--rebuild`); `open_temporal_db` returns `None`.
/// 2. **Unopenable DB** — the file exists but is corrupt, schema-gated, or at an
///    incompatible `PRAGMA user_version`; `TemporalDb::open` returns `Err`.
/// 3. **Empty build** — `rebuild_temporal` ran successfully but the git log for
///    the root produced zero entries (e.g. a brand-new repo with one commit and
///    no file-change history, or a root whose entire history was ghost-filtered
///    by AD-413-17).  Re-running with `SKIM_DEBUG=1` surfaces the internal count.
///
/// AD-413-8.
pub(super) const TEMPORAL_BUILD_EMPTY_MSG: &str = "git HEAD resolved but the temporal build produced no data; \
     re-run with `SKIM_DEBUG=1`";

/// Anchor refusal prefix — temporal data on disk was built from a different
/// repository than the one this root now resolves to.
///
/// The full emitted message appends "(recorded: …, live: …); run …" so tests
/// can assert `stderr.contains(SUBDIR_ROOT_TEMPORAL_MSG)` for the stable prefix.
/// Must NOT contain "run 'skim search' on a git repo" (AC18(a)/AC33(c)).
/// AD-413-16.
pub(super) const SUBDIR_ROOT_TEMPORAL_MSG: &str =
    "temporal data on disk was built from a different repository";

/// Canonical enumeration of all recognised flags for `skim search`.
///
/// Single source of truth: used in the unknown-flag error message so that
/// adding or renaming a flag requires only one edit here rather than separate
/// edits to the error string and the doc comment.  The help text at
/// `print_help` is intentionally separate (different format and prose
/// descriptions).
const KNOWN_FLAGS: &str = "--build, --rebuild, --update, --stats, --install-hooks, \
    --remove-hooks, --json, -j, --limit, -n, --offset, --root, --ast, --hot, --cold, \
    --risky, --blast-radius, --weights, --phrase, --near, --lang, --help, -h";

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
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // AD-412-3: help detection (empty args, --help, -h before --) lives entirely in
    // parse_flags so the `--` end-of-flags boundary has a single owner.
    let flags = parse_flags(args)?;

    // ── Validation order (deterministic — tests rely on this ordering) ──────
    // --ast pattern validated before dispatch; composes freely with temporal flags
    // (mutual exclusion of sort modes is enforced earlier, in parse_flags).
    //   1. single-node pattern → #283 error.
    //   2. unknown pattern → lists available names.
    if let Some(ref raw_ast) = flags.ast {
        ast::validate_ast_pattern(raw_ast)?;
    }

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
        // Empty query + --ast → standalone AST dispatch.  This arm now also handles
        // --ast combined with a temporal sort (--hot/--cold/--risky) and/or
        // --blast-radius (the interim guard that blocked the combination was removed):
        //
        // - --blast-radius: temporal::resolve_blast_radius_file_ids resolves co-change
        //   peers to FileIds; run_ast_standalone intersects them with the AST result
        //   set BEFORE truncation (avoids PF-006 silent feature-drop).
        // - --hot/--cold/--risky: the opened temporal DB is threaded in; run_ast_standalone
        //   enriches + re-sorts the AST matches by temporal score, then truncates to --limit.
        //
        // Ordered BEFORE the temporal-only arm so `--ast --hot` lands here (the AST
        // filter is honoured), never silently in run_temporal_standalone (R1/GAP-6).
        SearchAction::Query(_) if let Some(ref raw) = flags.ast => {
            // AD-377-2 / PF-006: standalone --ast (empty text) runs no weighted RRF,
            // so any supplied --weights is wholly inert.  Emit the SAME fully-inert
            // notice as the execute_query_with_manifest paths via the single shared
            // helper/const (PF-008) so AC8 asserts an identical substring.  `has_text`
            // is false on this arm (a non-empty query routes to run_query above),
            // `has_ast` true.  stderr only — stdout (incl. --json) stays byte-identical
            // (AC9).
            if let Some(notice) = query::weights_inert_notice(
                flags.weights,
                /* has_text */ false,
                /* has_ast */ true,
                flags.blast_radius.is_some(),
            ) {
                eprintln!("{notice}");
            }
            // PF-006: --lang is only honored when a text query is present (the lexical
            // reader applies lang_filter at query time). On the standalone --ast path
            // there is no text query, so --lang is inert — emit a notice rather than
            // silently ignoring it. Use --ast with a text query to combine both filters.
            if flags.lang.is_some() {
                eprintln!(
                    "skim search: note: --lang has no effect on standalone --ast queries \
                     (no text term); to filter by language, add a text query: \
                     `skim search TERM --ast PATTERN --lang LANG`."
                );
            }
            let (root, cache_dir) = resolve_root_and_cache(&flags.root_override)?;
            std::fs::create_dir_all(&cache_dir)?;
            // ADR-006: refresh BOTH indexes before opening either engine.
            // Finding 2 fix: destructure the HeadState returned by auto_refresh_if_stale
            // so we do not re-call git_head_state on the temporal-consuming path below.
            let (_outcome, manifest, head_state) = staleness::auto_refresh_if_stale(
                &root,
                &cache_dir,
                analytics,
                staleness::ReanchorPolicy::Refuse,
            )?;
            // Step 7 wiring (d): advisory on the --ast temporal-consuming branch.
            // Finding 2 fix: use head_state from auto_refresh_if_stale instead of
            // a second git_head_state call.
            if flags.temporal_sort.is_some() || flags.blast_radius.is_some() {
                staleness::warn_if_temporal_unverifiable(&cache_dir, &head_state);
            }
            // Resolve blast-radius → FileIds BEFORE calling run_ast_standalone.
            // temporal::resolve_blast_radius_file_ids is the single resolver for all
            // three blast-radius call sites, so JSON-aware warning and PF-004 widening
            // live in one place.  Passes cache_dir (not db_path) so the funnel owns the
            // path construction (Finding 2 / AD-413-16).
            // Finding 1/3 fix: pass head_state so the resolver does not re-derive it.
            let sorted = manifest.sorted_paths();
            let blast_file_ids = temporal::resolve_blast_radius_file_ids(
                flags.blast_radius.as_deref(),
                &root,
                &cache_dir,
                &sorted,
                flags.json,
                &head_state,
            )?;
            // Open the temporal DB only when a sort is requested.  Absent DB →
            // graceful degradation: warn on stderr and run unsorted (exit 0, AC-A3),
            // mirroring run_temporal_standalone's missing-data message.
            // AD-414-1 / AD-414-15: open_temporal_state is the single funnel for all
            // temporal DB access on this arm.
            let temporal_db = if flags.temporal_sort.is_some() {
                match temporal::open_temporal_state(&root, &cache_dir, &head_state) {
                    temporal::TemporalOpen::Open(db) => Some(db),
                    temporal::TemporalOpen::Unavailable(u) => {
                        eprintln!(
                            "skim search: {}; returning unsorted --ast results",
                            temporal::degraded_notice(&u, "--ast", temporal::Fallback::Ast,)
                        );
                        None
                    }
                }
            } else {
                None
            };
            let mut stdout = BufWriter::new(std::io::stdout());
            // AD-404 (mod.rs): pass Page so run_ast_standalone honors --offset.
            // This is the call site the whole ticket is about (the P1 defect was
            // passing `flags.limit` here and silently losing `flags.offset`).
            let result = ast::run_ast_standalone(
                raw,
                types::Page::new(flags.limit, flags.offset),
                flags.json,
                &cache_dir,
                &manifest,
                blast_file_ids,
                flags.temporal_sort,
                temporal_db.as_ref(),
                &root,
                &mut stdout,
            );
            stdout.flush()?;
            result
        }
        // Empty query with temporal flags (no --ast) → standalone temporal dispatch.
        SearchAction::Query(_) if flags.temporal_sort.is_some() || flags.blast_radius.is_some() => {
            // AD-377-2 / PF-006 (blocking-review fix #1): the temporal-only and
            // blast-radius-only standalone paths (e.g. `--hot --weights x,y,z`,
            // `--blast-radius FILE --weights x,y,z` with NO text and NO --ast) run
            // no weighted RRF — `run_temporal_standalone` ranks purely by hotspot /
            // bug-fix / co-change score and never consumes `--weights`.  Without
            // this guard the flag was silently ignored on exactly the path this
            // ticket exists to fix.  `has_text` and `has_ast` are both false on this
            // arm (a non-empty query routes to run_query, --ast to the arm above), so
            // weights_inert_notice returns the SAME fully-inert notice (PF-008).
            // stderr only — JSON stdout stays byte-identical (AC9).
            if let Some(notice) = query::weights_inert_notice(
                flags.weights,
                /* has_text */ false,
                /* has_ast */ false,
                flags.blast_radius.is_some(),
            ) {
                eprintln!("{notice}");
            }
            // PF-006: --lang is only honored on text-query paths (lexical reader
            // applies lang_filter at query time). Standalone temporal queries
            // (--hot/--cold/--risky/--blast-radius with no text) have no lexical
            // layer, so --lang is inert — emit a notice rather than silently ignoring it.
            if flags.lang.is_some() {
                eprintln!(
                    "skim search: note: --lang has no effect on standalone temporal queries \
                     (no text term); to filter by language, add a text query: \
                     `skim search TERM --hot --lang LANG`."
                );
            }
            // AD-404 (mod.rs): pass Page so run_temporal_standalone honors --offset
            // on the --hot/--cold/--risky/--blast-radius-only paths.
            run_temporal_standalone(
                types::Page::new(flags.limit, flags.offset),
                flags.json,
                &flags.root_override,
                flags.temporal_sort,
                flags.blast_radius.as_deref(),
                analytics,
            )
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
    /// Sort mode for temporal queries — mutually exclusive.
    temporal_sort: Option<types::TemporalSort>,
    /// Raw path for blast-radius pre-filtering. Normalized later in run_query.
    blast_radius: Option<String>,
    /// Raw AST pattern string for structural pattern search (#199).
    ///
    /// Validated at dispatch time (before opening the index).  Space-separated
    /// `--ast try-catch` and equals form `--ast=try-catch` are both accepted.
    /// Whitespace-only values are rejected in `parse_flags`.
    ast: Option<String>,
    /// Composite RRF weights for the weighted-ranking query paths (#200, #377).
    ///
    /// Parsed from `--weights lexical,ast,temporal` and validated at flag-parse
    /// time.  `None` → use `CompositeWeights6::with_six_signal_defaults()` (0.5, 0.3, 0.2).
    ///
    /// AD-377-1/AD-377-3: honored on BOTH composite paths — the `--blast-radius`
    /// UNION ranking (all 3 weights) AND the text+`--ast` intersection ranking
    /// (lexical + ast only; the temporal weight is inert whenever `--ast` is
    /// present because the AST intersection fuses only lexical+ast).  On every
    /// other path (pure-lexical, standalone `--ast`, temporal-only, blast-only)
    /// the flag is inert and a one-line stderr notice fires (AD-377-2, PF-006).
    weights: Option<rskim_search::CompositeWeights6>,
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
            temporal_sort: None,
            blast_radius: None,
            ast: None,
            weights: None,
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
pub(super) fn parse_lang_value(raw: &str) -> anyhow::Result<rskim_core::Language> {
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

/// Parse a temporal flag arm (`--hot`, `--cold`, `--risky`, `--blast-radius`).
///
/// Returns `Ok(true)` when the flag consumed an extra token (i.e. the space-
/// separated `--blast-radius <path>` form), `Ok(false)` for single-token arms,
/// and `Err` on validation failure.
///
/// The caller is responsible for advancing `i` by one additional position when
/// this function returns `Ok(true)`.
fn parse_temporal_flag(
    arg: &str,
    next_arg: Option<&String>,
    temporal_sort: &mut Option<types::TemporalSort>,
    blast_radius: &mut Option<String>,
) -> anyhow::Result<bool> {
    match arg {
        "--hot" | "--cold" | "--risky" => {
            let new_sort = match arg {
                "--hot" => types::TemporalSort::Hot,
                "--cold" => types::TemporalSort::Cold,
                _ => types::TemporalSort::Risky,
            };
            if let Some(existing) = *temporal_sort {
                anyhow::bail!(
                    "{} and {} are mutually exclusive",
                    new_sort.flag_name(),
                    existing.flag_name()
                );
            }
            *temporal_sort = Some(new_sort);
            Ok(false)
        }
        "--blast-radius" => {
            let val =
                next_arg.ok_or_else(|| anyhow::anyhow!("--blast-radius requires a file path"))?;
            // AD-412-3: bail so --help/-h is never consumed as the file-path value
            // (parity with take_flag_value; e.g. `skim search --blast-radius --help`).
            // AD-412-4: {val:?} (Debug, quoted) neutralises ANSI-escape injection.
            if matches!(val.as_str(), "--help" | "-h") {
                anyhow::bail!(
                    "--blast-radius requires a file path (got {val:?}); \
                     to print help, run: `skim search --help`"
                );
            }
            *blast_radius = Some(val.clone());
            Ok(true)
        }
        s if s.starts_with("--blast-radius=") => {
            let val = s.trim_start_matches("--blast-radius=");
            if val.is_empty() {
                anyhow::bail!("--blast-radius requires a file path");
            }
            *blast_radius = Some(val.to_string());
            Ok(false)
        }
        _ => unreachable!("parse_temporal_flag called with non-temporal arg: {arg}"),
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
/// take_flag_value("--ast=try-catch", None, "--ast")           → Ok(("try-catch", false))
/// take_flag_value("--ast", Some("try-catch"), "--ast")         → Ok(("try-catch", true))
/// take_flag_value("--ast", None, "--ast")                      → Err(…missing…)
/// take_flag_value("--ast=  ", None, "--ast")                   → Err(…empty…)
/// take_flag_value("--ast", Some("--help"), "--ast")            → Err(…requires a value; to print help…)
/// take_flag_value("--ast", Some("-h"), "--ast")                → Err(…requires a value; to print help…)
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
    // AD-412-3: bail so --help/-h is never consumed as a flag value (e.g. `--ast --help`).
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
/// - `--ast` without a value or with a whitespace-only value.
/// - `--weights` without a value or with an invalid weight string.
/// - Unrecognised dash-leading flags — any `--foo` or `-x` (length >= 2) that is
///   not a known flag (see `KNOWN_FLAGS`). Bare `-` is a positional query token;
///   a literal dash-leading term is searchable after the `--` end-of-flags separator.
/// - Combining a text query with an action flag (`--build` / `--rebuild` /
///   `--update` / `--stats` / `--install-hooks` / `--remove-hooks`) — an
///   ambiguous mixed form.
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
    let mut temporal_sort: Option<types::TemporalSort> = None;
    let mut blast_radius: Option<String> = None;
    let mut ast: Option<String> = None;
    let mut weights: Option<rskim_search::CompositeWeights6> = None;
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
                // forms are handled by take_flag_value — same idiom as --root and --ast.
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
            s if s == "--ast" || s.starts_with("--ast=") => {
                // Space-separated (`--ast try-catch`) and equals (`--ast=try-catch`) forms.
                let (val, consumed) = take_flag_value(s, args.get(i + 1), "--ast")?;
                ast = Some(val);
                if consumed {
                    i += 1;
                }
            }
            s if s == "--weights" || s.starts_with("--weights=") => {
                // Composite RRF weights: `--weights l,a,t` or `--weights=l,a,t` (#200).
                // Parse and validate immediately so invalid values produce a clear CLI
                // error before any index I/O (AC5: non-zero exit with actionable message).
                let (raw, consumed) = take_flag_value(s, args.get(i + 1), "--weights")?;
                weights = Some(
                    rskim_search::CompositeWeights6::parse_weights_flag(&raw)
                        .map_err(|e| anyhow::anyhow!("--weights: {e}"))?,
                );
                if consumed {
                    i += 1;
                }
            }
            s if matches!(s, "--hot" | "--cold" | "--risky" | "--blast-radius")
                || s.starts_with("--blast-radius=") =>
            {
                let consumed_next =
                    parse_temporal_flag(s, args.get(i + 1), &mut temporal_sort, &mut blast_radius)?;
                if consumed_next {
                    i += 1;
                }
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
        temporal_sort,
        blast_radius,
        ast,
        weights,
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

    // AD-TMP-1: --rebuild/--build must produce a COMPLETE index (lexical + AST +
    // temporal), matching user expectation that "rebuild" rebuilds everything (#357 BUG A).
    // run_build goes through build_index directly, bypassing auto_refresh_if_stale where
    // the only other temporal hook lives, so temporal must be populated here too.
    // Non-fatal by ADR-006/D5: a temporal failure must NOT fail the explicit build.
    // Step 7 wiring (a): classify HEAD state once, emit the advisory, then rebuild.
    // The `force` flag is intentionally NOT forwarded: rebuild_temporal always does a
    // full history walk (no cache) — see `parse_history(root, 0)` in
    // `rebuild_temporal_with_source` (temporal_build.rs, "Single full-history walk").
    let head_state = staleness::git_head_state(&root);
    staleness::warn_if_temporal_unverifiable(&cache_dir, &head_state);
    staleness::try_rebuild_temporal_nonfatal(
        &root,
        &cache_dir,
        head_state.sha(),
        "--rebuild hook",
        staleness::ReanchorPolicy::Allow, // explicit build arm re-anchors (PF-017)
    );

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
    // Finding 2 fix: destructure HeadState from auto_refresh_if_stale.
    let (outcome, manifest, head_state) = staleness::auto_refresh_if_stale(
        &root,
        &cache_dir,
        analytics,
        staleness::ReanchorPolicy::Allow,
    )?;
    // Step 7 wiring (b): emit the HEAD-unresolvable advisory on --update (AC23).
    // Finding 2 fix: use head_state from above instead of a second git_head_state call.
    staleness::warn_if_temporal_unverifiable(&cache_dir, &head_state);
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

fn run_stats(json: bool, root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;

    // AD-381-1: surface the resolved search cache directory so the (otherwise
    // hidden) on-disk location is discoverable from `--stats` alone. Computed
    // once here and reused by both the no-index early-return and the populated
    // branch below, in both text and JSON modes.
    let cache_dir_display = cache_dir.display().to_string();

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

    // Step 7 wiring (c): emit HEAD-unresolvable advisory BEFORE the `if json` split
    // so both text and JSON modes see it (wiring it inside one branch loses the other,
    // F3; wiring it in the early-return above would fire even without an index, AC24).
    // Finding [reliability] fix: resolve HeadState ONCE here and thread it into both
    // warn_if_temporal_unverifiable and build_stats_json.  The previous code used
    // warn_if_temporal_unverifiable_at (which called git_head_state internally) and
    // then build_stats_json called git_head_state a second time — two unsynchronised
    // snapshots instead of one.  On a linked worktree each git_head_state traversal
    // is ~10 syscalls; on a repo that gets a commit between the two reads the advisory
    // and the JSON key would describe a different HEAD than the other.
    let head_state = staleness::git_head_state(&root);
    staleness::warn_if_temporal_unverifiable(&cache_dir, &head_state);

    let mut out = BufWriter::new(std::io::stdout());
    if json {
        // AC6/AC11: shared JSON construction via build_stats_json (same code path
        // as the test helper) — ensures AC11 back-compat tests guard production.
        // AD-395-6: `skipped` array and `skipped_by_reason` are additive keys.
        //
        // Gather-once: build_stats_json opens the reader, checks staleness, and
        // loads skip entries internally.  We do NOT pre-compute those values here
        // — doing so would run a second NgramIndexReader::open and a second
        // check_staleness (full working-tree metadata walk) before immediately
        // discarding the results.
        let extended = build_stats_json(&cache_dir, &root, &head_state)?;
        writeln!(out, "{}", serde_json::to_string_pretty(&extended)?)?;
    } else {
        // Text mode: gather stats once here (not needed by the JSON path above).
        let reader = rskim_search::NgramIndexReader::open(&cache_dir)?;
        let stats = reader.stats();

        // AD-380-4 (#380): the lexical-only `index_size_bytes` (skidx+skpost from
        // the reader) historically undercounted the TRUE on-disk footprint by
        // ~23 MB — it omitted the manifest, AST index/cache, and temporal DB.
        // Compute the real total here by summing metadata().len() over the fixed
        // set of index artifacts (AC-6). `index_size_bytes` is intentionally left
        // unchanged so the lexical-only figure remains available (AC-7).
        let total_on_disk = total_on_disk_bytes(&cache_dir);
        // AD-380-5: temporal.db scales with git history, not source size, so
        // report it as its own line — it is included in the total but distinguished.
        let temporal_db_bytes = artifact_len(&cache_dir, "temporal.db");

        // check_staleness returns the loaded manifest as part of its work.
        // Reuse it here instead of loading the manifest a second time.
        let (staleness_status, loaded_manifest) = staleness::check_staleness(&cache_dir, &root);
        let git_head = loaded_manifest
            .as_ref()
            .and_then(|m| m.stored_git_head().map(str::to_string));

        // AD-395-6: load skip section from the manifest for --stats display.
        // All pre-existing keys are unchanged; `skipped` / `skipped_by_reason`
        // are purely additive (AC11 back-compat).
        let skip_entries: Vec<_> = loaded_manifest
            .as_ref()
            .map(|m| m.skipped().collect::<Vec<_>>())
            .unwrap_or_default();

        writeln!(out, "skim search index stats:")?;
        writeln!(out, "  files indexed : {}", stats.file_count)?;
        writeln!(out, "  total n-grams : {}", stats.total_ngrams)?;
        writeln!(
            out,
            "  index size    : {} bytes (lexical)",
            stats.index_size_bytes
        )?;
        // AD-380-4: the TRUE total over all on-disk artifacts.
        writeln!(out, "  total on disk : {total_on_disk} bytes")?;
        // AD-380-5: temporal DB reported separately (scales with git history).
        writeln!(out, "  temporal db   : {temporal_db_bytes} bytes")?;
        if let Some(ts) = stats.last_updated {
            writeln!(out, "  last updated  : {ts}")?;
        }
        writeln!(
            out,
            "  git HEAD      : {}",
            git_head.as_deref().unwrap_or("(none)")
        )?;
        writeln!(out, "  staleness     : {staleness_status}")?;
        // AC4: resolved cache dir, in addition to the lines above.
        writeln!(out, "  cache dir     : {cache_dir_display}")?;
        // AD-395-6: skip counts by reason (text mode).
        // PF-012: use BTreeMap so reason keys iterate in stable sorted order.
        if !skip_entries.is_empty() {
            let mut by_reason: std::collections::BTreeMap<&str, u64> =
                std::collections::BTreeMap::new();
            for e in &skip_entries {
                *by_reason.entry(e.reason_label()).or_insert(0) += 1;
            }
            writeln!(
                out,
                "  skipped       : {} (content-skipped files)",
                skip_entries.len()
            )?;
            for (reason, count) in &by_reason {
                writeln!(out, "    {reason}: {count}")?;
            }
        }

        // AD-405-7 / AC-405-9 / AC-405-15: AST size-coverage section (D-4 cadence).
        // Omit when clean (is_clean() == true) — byte-identical to the pre-fix binary
        // on a corpus with zero excluded / zero undetermined files (AC-405-15).
        // Loaded manifest is already in memory from check_staleness — zero extra I/O.
        if let Some(ref m) = loaded_manifest {
            let coverage = m.ast_coverage();
            if !coverage.is_clean() {
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
const ON_DISK_ARTIFACTS: [&str; 7] = [
    "index.skidx",       // lexical n-gram index
    "index.skpost",      // lexical posting lists
    "index.skfiles",     // binary file manifest (this ticket)
    "ast_index.skidx",   // AST n-gram index header + metadata
    "ast_index.skpost",  // AST posting lists
    "ast_index.skcache", // AST extraction cache
    "temporal.db",       // hotspot / risk / co-change SQLite DB
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
/// or temporal ran) report exactly the sum of the files that exist (AC-7).
fn total_on_disk_bytes(cache_dir: &std::path::Path) -> u64 {
    ON_DISK_ARTIFACTS
        .iter()
        .map(|name| artifact_len(cache_dir, name))
        .sum()
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

fn run_query(
    text: &str,
    flags: &Flags,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(&flags.root_override)?;
    std::fs::create_dir_all(&cache_dir)?;

    // Self-heal ordering (#357 BUG B, cycle-2 finding 8): auto_refresh_if_stale
    // MUST run BEFORE opening temporal.db or resolving blast-radius paths, so that
    // a missing or HEAD-divergent temporal.db is rebuilt before we attempt to open
    // it.  This mirrors the ordering used by the two standalone arms:
    //   - run_temporal_standalone: refresh first, then open_temporal_db
    //   - standalone --ast arm:    refresh first, then open_temporal_db
    //
    // Previously, temporal_db was opened at the top of this function BEFORE
    // auto_refresh_if_stale fired, so a lexical-Current but temporal-stale DB was
    // consumed pre-heal by both blast-radius resolution and apply_temporal_enrichment.
    //
    // Fix: call auto_refresh_if_stale here unconditionally when temporal data is
    // needed, then open temporal_db with the now-fresh file.  The --ast subpath
    // reuses the manifest returned here directly (no second auto_refresh call).
    // The pure-lexical subpath passes the manifest to execute_query_with_manifest
    // so it skips its own internal refresh.
    //
    // ADR-006/D5: auto_refresh_if_stale propagates lexical errors as Err but
    // swallows temporal errors internally — callers only see lexical failures.
    // Finding 2 fix: destructure HeadState from auto_refresh_if_stale so the
    // temporal-consuming arms do not need a second git_head_state call.
    // The pure-lexical (no temporal/AST) path skips the early refresh; its
    // head_state is not needed because it cannot reach warn_if_temporal_unverifiable
    // (the guard below only fires when temporal_sort or blast_radius is set).
    let (pre_loaded_manifest_from_refresh, refresh_head_state) =
        if flags.temporal_sort.is_some() || flags.blast_radius.is_some() || flags.ast.is_some() {
            let (_outcome, manifest, head_state) = staleness::auto_refresh_if_stale(
                &root,
                &cache_dir,
                analytics,
                staleness::ReanchorPolicy::Refuse,
            )?;
            (Some(manifest), Some(head_state))
        } else {
            // No temporal or AST flag: skip early refresh; execute_query_with_manifest
            // will call auto_refresh_if_stale internally exactly once.
            (None, None)
        };

    // Step 7 wiring (d): emit the HEAD-unresolvable advisory on temporal-consuming
    // arms only (AC23). Plain lexical queries that do not request temporal data must
    // NOT produce advisory stderr output (A1 wiring correction, #414 SE-1/AC-30).
    // Finding 2 fix: use refresh_head_state from auto_refresh_if_stale instead of
    // a second git_head_state call (refresh_head_state is always Some on this path
    // because the guard above ensures auto_refresh_if_stale ran for temporal flags).
    if (flags.temporal_sort.is_some() || flags.blast_radius.is_some())
        && let Some(hs) = &refresh_head_state
    {
        staleness::warn_if_temporal_unverifiable(&cache_dir, hs);
    }

    // blast-radius and temporal enrichment each call open_temporal_state independently
    // so no shared temporal_db handle is needed here.  resolve_blast_radius_paths
    // (below) uses open_temporal_state internally; the enrichment arm (below the query
    // execution) opens it again to get fresh state post-refresh.  Two SQLite opens are
    // cheap and keep the two concerns separate (AD-414-1).

    // Resolve blast-radius partner paths BEFORE querying so the file_filter
    // is applied inside the search engine (before LIMIT). This ensures the
    // limit applies to the filtered set rather than silently discarding
    // co-change partners that ranked beyond the top-N unfiltered results.
    // Passes cache_dir (not db_path) so the funnel owns path construction
    // and the AD-413-16 guard is enforced inside resolve_blast_radius_paths
    // (Finding 2).
    // Finding 1/3 fix: pass head_state so the resolver does not re-derive it.
    let blast_radius_paths = temporal::resolve_blast_radius_paths(
        flags.blast_radius.as_deref(),
        &root,
        &cache_dir,
        flags.json,
        refresh_head_state
            .as_ref()
            .unwrap_or(&staleness::HeadState::NotARepo),
    )?;

    // Resolve AST file filter (#199): open the AST engine (already refreshed
    // above), execute the structural query, collect matching FileIds.
    // Applied at the FileId level inside execute_query (no path round-trip).
    //
    // IMPORTANT: auto_refresh_if_stale was already called above so the AST index
    // is fresh before we open it here (applies ADR-006: self-heal ordering is
    // load-bearing).  The manifest from that call is passed into execute_query so
    // it skips a redundant refresh+load — each query path refreshes exactly once.
    //
    // Missing index (after refresh) → fail loud (return Err, #199).
    // Query execution failure → degrade gracefully (warn, no AST filter).
    // AC-405-10 / AD-405-7: compute ast_coverage once here, before the match, so
    // the empty-hits branch and compound_ast_coverage share a single O(N) pass.
    // Carried forward in the tuple as `cached_ast_coverage` to avoid a second call
    // at the compound_ast_coverage site below (eliminating the redundant double pass
    // on the compound --ast empty-hits path).
    let (ast_scored, pre_loaded_manifest, cached_ast_coverage) =
        if let Some(ref raw_ast) = flags.ast {
            // The refresh already ran above: `pre_loaded_manifest_from_refresh` is always
            // `Some` when `flags.ast.is_some()` (the early-refresh condition includes
            // `|| flags.ast.is_some()`). Reuse that manifest directly rather than calling
            // auto_refresh_if_stale a second time (the second call was idempotent but
            // wasteful — it returned `(false, manifest)` immediately on Current).
            let manifest = pre_loaded_manifest_from_refresh
                .expect("manifest must be present when flags.ast is Some (invariant)");
            let engine = ast::open_ast_engine(&cache_dir)?;
            // Compute coverage once: reused by both the empty-hits message below and
            // the compound_ast_coverage binding after this block (no second pass).
            let coverage = manifest.ast_coverage();
            // Changed from #199 (lossy HashSet) to #198 (scored vec for RRF).
            // resolve_ast_scored returns Vec<(FileId, f64)> sorted FileId-ASC,
            // preserving AST scores so intersect_and_rank can build the rank map.
            let ast_scored = match ast::resolve_ast_scored(&engine, raw_ast) {
                Ok(hits) => {
                    if hits.is_empty() {
                        // AC-405-10: append excluded-file count when non-zero so the
                        // compound path mirrors the standalone path in output.rs.
                        let excluded = coverage.size_excluded_files;
                        if excluded > 0 {
                            eprintln!(
                                "skim search: --ast {:?} matched no indexed files \
                             ({excluded} file(s) excluded from AST indexing by size cap \
                             — run `skim search --stats --json`.)",
                                raw_ast
                            );
                        } else {
                            eprintln!("skim search: --ast {:?} matched no indexed files", raw_ast);
                        }
                    }
                    Some(hits)
                }
                Err(e) => {
                    // Query execution failure: degrade gracefully (warn, no AST filter).
                    // Warning always goes to stderr — even in --json mode — so it does
                    // not pollute the JSON stream (sibling warnings also go to stderr).
                    eprintln!("skim search: AST query warning: {e}");
                    None
                }
            };
            (ast_scored, Some(manifest), Some(coverage))
        } else {
            // Pure-lexical path: no --ast flag. Pass the manifest from the early
            // refresh (if we did one) so execute_query_with_manifest skips its own
            // auto_refresh_if_stale call. When no refresh was needed (no temporal or
            // AST flag), pass None so execute_query_with_manifest does its own refresh.
            (None, pre_loaded_manifest_from_refresh, None)
        };

    // AD-403-6: degenerate --near diagnostic (fail loud, never silently — ADR-001).
    // Emitted here on the text-query path ONLY (has_text is true by construction).
    // Case (a): single-word query + --near N (N cannot constrain anything).
    // Case (b): N < word_count - 1 (structurally unsatisfiable; returns empty results
    // silently without this notice).  stderr only; exit 0.
    if let Some(notice) = query::near_diagnostic_notice(flags.near, text) {
        eprintln!("{notice}");
    }

    // GAP-1: when a temporal sort is active, fetch a bounded candidate
    // window (limit*5 ≥ 100) so the re-sort can promote a temporally-hot file that
    // ranks beyond `--limit` in raw lexical/composite order; truncate to --limit
    // AFTER the sort (below). Without a sort, query exactly --limit (unchanged).
    let query_limit = if flags.temporal_sort.is_some() {
        temporal::resort_window(flags.limit)
    } else {
        flags.limit
    };

    let config = types::QueryConfig {
        text: text.to_string(),
        limit: query_limit,
        // AD-372-3 / RESOLVED Decision 3: offset is applied AFTER verification in
        // resolve_paths_and_snippets_verified (rank → verify → skip offset → take limit).
        // On the exact-symbol path query.rs sets sq.offset=None so the reader returns
        // the full ranked intersection; effective_offset from config.offset is then
        // passed to the post-verify skip.  On the multi-word path offset is also
        // applied post-verify (same code path).
        //
        // Double-offset guard (finding #372): when a temporal sort is active, offset
        // is applied ONCE post-temporal-sort (in the drain below), never inside
        // execute_query_with_manifest.  Pass None here so the pre-sort verify step
        // does not consume the offset; the correct single application is the drain.
        offset: if flags.temporal_sort.is_some() {
            None
        } else {
            flags.offset
        },
        json: flags.json,
        root: root.clone(),
        cache_dir: cache_dir.clone(),
        blast_radius_paths,
        ast_scored,
        composite_weights: flags.weights,
        phrase: flags.phrase,
        near: flags.near,
        lang: flags.lang,
    };

    // AD-405-7 / AC-405-17: AST coverage was already computed once in the block
    // above (carried in `cached_ast_coverage`) — no second manifest pass needed.
    // Pure-lexical paths carry None → ast_coverage key absent from JSON (D-5).
    let compound_ast_coverage = cached_ast_coverage;

    // Pass the already-refreshed manifest to execute_query_with_manifest.  When
    // pre_loaded_manifest is Some (temporal or AST flag active — refresh happened
    // above), execute_query skips its own auto_refresh_if_stale.  When None
    // (pure-lexical, no temporal/AST flag), execute_query refreshes internally,
    // preserving the invariant: exactly one auto_refresh_if_stale call per query.
    let mut output = query::execute_query_with_manifest(&config, pre_loaded_manifest, analytics)?;

    // AD-404-9 / AD-404-10: apply temporal enrichment + paginate on the text+temporal arm.
    //
    // Double-offset guard (AD-404-9): config.offset is None when temporal_sort is active
    // (mod.rs:1086-1090), so execute_query_with_manifest does NOT apply offset on this
    // path; the drain below is the SINGLE application (no double-offset bug).
    //
    // Guard drift fix (AD-404-10): the pagination (offset drain + limit truncate) MUST
    // run whenever temporal_sort is active, even when temporal_db is absent (degraded).
    // The old guard `if let (Some(sort), Some(db))` silently dropped --limit and --offset
    // on the degraded path (no temporal.db). Fix: bind the condition once and hoist
    // pagination out of the DB-presence branch so it runs unconditionally on temporal arms.
    let page = types::Page::new(flags.limit, flags.offset);
    if let Some(sort) = flags.temporal_sort {
        // AD-414-1: three-way match on open_temporal_state.  The arms are:
        //   Open(db) where dimension is non-empty → enrich + check coverage
        //   Open(_) where dimension is empty      → DB present but no rows
        //   Unavailable(u)                         → DB missing / corrupt / etc.
        // Produces a TemporalUnavailable when enrichment cannot be applied so
        // the fallback path (degraded_notice + DegradedJson) is shared.
        let head = refresh_head_state
            .as_ref()
            .unwrap_or(&staleness::HeadState::NotARepo);
        let maybe_unavail: Option<temporal::TemporalUnavailable> =
            match temporal::open_temporal_state(&root, &cache_dir, head) {
                temporal::TemporalOpen::Open(db) if !temporal::dimension_is_empty(&db, sort) => {
                    let cov = temporal::apply_temporal_enrichment(&mut output.results, sort, &db)?;
                    if cov.ranked == 0 {
                        // AD-414-13 zero-coverage: enrichment ran but no result received a
                        // temporal score (e.g. all files are untracked or newly added).
                        // Skip the re-sort; preserve lexical order; emit degraded signal.
                        let detail = format!(
                            "0 of {} result(s) have {} data ({} lookup error(s))",
                            cov.total,
                            sort.flag_name(),
                            cov.lookup_errors,
                        );
                        Some(temporal::TemporalUnavailable {
                            reason: temporal::DegradedReason::NoRankedRows,
                            detail,
                        })
                    } else {
                        None
                    }
                }
                temporal::TemporalOpen::Open(_) => {
                    // DB open but the requested dimension has zero rows — run --build.
                    Some(temporal::TemporalUnavailable {
                        reason: temporal::DegradedReason::Empty,
                        detail: String::new(),
                    })
                }
                temporal::TemporalOpen::Unavailable(u) => Some(u),
            };

        if let Some(ref u) = maybe_unavail {
            // degraded_notice is the SSOT for all stderr degradation messages (AD-414-1).
            // Goes to stderr so --json stdout stays byte-identical (PF-006 / AD-404-8).
            let msg = temporal::degraded_notice(u, sort.flag_name(), temporal::Fallback::Lexical);
            eprintln!("skim search: {msg}");
            output.degraded.push(temporal::DegradedJson {
                subsystem: "temporal",
                reason: u.reason.as_json_str(),
                requested: sort.flag_name().to_string(),
                applied: "lexical",
                message: msg,
                remediation: u.reason.remediation(),
            });
        }

        // Pagination applied regardless of DB presence (AD-404-10 guard drift fix).
        //
        // AD-404-11 / D-5: capture pre-page count BEFORE page.apply so we can emit
        // the sound `has_more` terminator — replaces the unsound `len < limit`
        // heuristic on this path.  `pre_page_len > page.depth()` is true when the
        // re-sorted resort window contains more results than the current page consumes.
        let pre_page_len = output.results.len();
        page.apply(&mut output.results);
        output.total = output.results.len();
        output.has_more = pre_page_len > page.depth();
    }

    // AD-404-11 / D-5: emit bounded-page notice on all text-query paths when
    // has_more is true (pure-text and text+temporal both reach here).
    // Goes to stderr so --json stdout stays byte-identical (PF-006 / AD-404-8).
    if output.has_more {
        eprintln!(
            "{}",
            temporal::bounded_page_notice(output.total, page.offset(), page.limit())
        );
    }

    // AD-405-7 / AC-405-17: emit AST coverage notice on compound --ast paths (D-4
    // cadence).  Notice goes to stderr so --json stdout stays byte-identical.
    // Wire coverage into output for the JSON key (D-5): omit when clean so the
    // key is absent on healthy repos (avoids noise for well-maintained codebases).
    if let Some(ref cov) = compound_ast_coverage {
        query::emit_ast_coverage_notice(cov);
    }
    output.ast_coverage = compound_ast_coverage.filter(|c| !c.is_clean());

    let mut stdout = BufWriter::new(std::io::stdout());
    if flags.json {
        query::format_json_output(&output, &mut stdout)?;
    } else {
        query::format_text_output(&output, &mut stdout)?;
    }
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

/// Typed JSON envelope for a warning-only response (no temporal data available).
#[derive(Serialize)]
struct WarningJson<'a> {
    warning: &'a str,
}

/// Execute a standalone temporal query (no text search term provided).
///
/// Opens the temporal DB from the resolved cache directory, ensures it is
/// fresh via `auto_refresh_if_stale` (mirrors the standalone `--ast` arm —
/// the `SearchAction::Query(_) if let Some(ref raw) = flags.ast` branch —
/// per the locked decision 2026-06-24, resolving the BLOCKER for #357),
/// dispatches the query (hotspot, cold, risky, or blast-radius), and writes
/// the result as JSON or plain text to stdout. Degrades gracefully when the
/// temporal DB is absent after self-heal — prints a warning and returns exit 0.
///
/// # False comment reconciled (mod.rs:737-740 in the old code)
///
/// The prior comment claimed "auto_refresh_if_stale guarantees freshness here"
/// but the function NEVER called auto_refresh_if_stale, so temporal.db was
/// never self-healed on the standalone --hot/--cold/--risky path.
/// The call below fixes that gap (#357 BLOCKER).
fn run_temporal_standalone(
    page: types::Page,
    json: bool,
    root_override: &Option<PathBuf>,
    temporal_sort: Option<types::TemporalSort>,
    blast_radius: Option<&str>,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;

    // Self-heal: ensure the lexical+AST+temporal index is fresh before querying.
    // This mirrors the standalone --ast arm (`SearchAction::Query(_) if let
    // Some(ref raw) = flags.ast`) and is the fix for the BLOCKER in #357 —
    // bare --hot/--cold/--risky/--blast-radius never called auto_refresh_if_stale,
    // so temporal.db was never self-healed on these paths even though the false
    // comment above claimed it was guaranteed.
    // ADR-006/D5: auto_refresh_if_stale propagates lexical errors as Err but
    // swallows temporal errors internally — callers only see lexical failures.
    // Finding 2 fix: destructure HeadState from auto_refresh_if_stale so we do
    // not re-call git_head_state for the advisory below.
    let (_outcome, _manifest, head_state) = staleness::auto_refresh_if_stale(
        &root,
        &cache_dir,
        analytics,
        staleness::ReanchorPolicy::Refuse,
    )?;

    // Step 7 wiring (d): temporal-consuming standalone arm (--hot/--cold/--risky/--blast-radius).
    // Finding 2 fix: use head_state from auto_refresh_if_stale above.
    staleness::warn_if_temporal_unverifiable(&cache_dir, &head_state);

    // AD-414-1: open_temporal_state replaces open_temporal_db_for; returns a typed
    // TemporalOpen so the caller matches on Open/Unavailable instead of Ok/Err.
    // RepositoryMismatch → wrong-repo rows rejected; Missing/Empty/Corrupt → no data.
    // Both arms degrade gracefully (exit 0, AC-F3).
    // degraded_notice is the SSOT for all stderr/JSON degradation messages.
    let db = match temporal::open_temporal_state(&root, &cache_dir, &head_state) {
        temporal::TemporalOpen::Open(db) => db,
        temporal::TemporalOpen::Unavailable(ref u) => {
            let msg_str = temporal::degraded_notice(u, "", temporal::Fallback::NoResults);
            if json {
                let msg = WarningJson { warning: &msg_str };
                println!("{}", serde_json::to_string(&msg)?);
            } else {
                eprintln!("skim search: {msg_str}");
            }
            return Ok(ExitCode::SUCCESS);
        }
    };

    let (output, has_more) =
        temporal::query_standalone(temporal_sort, blast_radius, page, &db, &root)?;

    // AD-404-8: bounded-page-notice — emitted on stderr when has_more is true
    // (more results exist beyond this page, or the temporal ranking window was
    // exceeded).  Goes to stderr (#377 seam, PF-006) so --json stdout stays
    // byte-identical.  The notice surfaces the pagination seam for agents that
    // detect the last page via has_more rather than the unsound `len < limit`
    // heuristic (D-5).
    if has_more {
        eprintln!(
            "{}",
            temporal::bounded_page_notice(output.result_count(), page.offset(), page.limit())
        );
    }

    let mut stdout = BufWriter::new(std::io::stdout());
    if json {
        temporal::format_temporal_json(&output, has_more, &mut stdout)?;
    } else {
        temporal::format_temporal_text(&output, page, &mut stdout)?;
    }
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Help text
// ============================================================================

/// Full `skim search` help text.
///
/// Extracted to a `const` (from the old inline `println!` literal) so AC10 can
/// assert its contents as a falsifiable unit test (PF-008 doc-drift guard): the
/// test verifies the `--weights` section names *both* composite paths and the
/// temporal-inert-on-`--ast` rule, and that the obsolete "Only active on the
/// `--blast-radius` composite ranking path" wording is gone.
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

  --phrase and --near are honored on any text query including text + --ast and
  text + --blast-radius. They are inert on all other arms (no text query).

Language filter option:
  --lang LANG      Filter results to files of a given language (e.g. --lang rust,
                   --lang python). Accepted as language name or extension.

AST structural query options (#199):
  --ast PATTERN    Filter/list by AST structural pattern.
                   PATTERN is a named catalog pattern or a containment query:
                     Named:        --ast try-catch
                     Containment:  --ast \"for_statement > await_expression\"
                   Use `--ast` alone for standalone AST-only output (file-level),
                   or combine with a text query for intersection results.

  Limitations:
    #283 -- Single-node queries (e.g. --ast try_statement) are not yet supported;
           use a named pattern or a containment query instead.
    Size cap -- Files larger than 1 MiB are excluded from AST indexing and will
           not appear in --ast results (they remain fully text-searchable).
           Run `skim search --stats` or `skim search --stats --json` to see
           which files are excluded (`ast_coverage` / ast eligible/excluded lines).
           The `--ast` JSON envelope includes an `ast_coverage` key when any
           files are excluded, listing per-language counts and a bounded sample.

  --ast composes with: text query, --phrase, --near, --lang, --hot/--cold/--risky,
  --blast-radius, --limit, --offset, and --json.  When heatmap data is absent,
  temporal sorts degrade gracefully: a warning is printed to stderr and results are
  returned unsorted (exit 0).

AST standalone examples:
  skim search --ast try-catch                   Files with try/catch blocks
  skim search --ast \"for_statement > await_expression\"  Async-in-loop pattern
  skim search \"error\" --ast try-catch           Text+AST intersection (lexical snippets preserved)
  skim search --ast try-catch --blast-radius src/auth.rs  AST ∩ co-change
  skim search --ast god-function --hot           AST matches sorted by hotspot score
  skim search \"error\" --ast try-catch --hot --blast-radius src/auth.rs --limit 20 --json
                                                 Full CLI surface: text + AST + temporal + co-change + JSON

Temporal query options (auto-populated by 'skim search' on a git repo):
  --hot                        Sort/list by hotspot score descending
  --cold                       Sort/list by hotspot score ascending
  --risky                      Sort/list by bug-fix density descending
  --blast-radius FILE          Restrict to co-change partners of FILE

Temporal flag composition:
  --hot and --cold/--risky are mutually exclusive (pick one sort mode).
  --blast-radius is composable with any sort mode and with text queries.

Composite ranking options (#200, #377):
  --weights L,A,T      Tune composite RRF ranking. Exactly 3 comma-separated ratio
                       values: lexical, ast, temporal.
                       Default: 0.5,0.3,0.2
                       Values are ratios only — NOT normalized; zero and non-sum-to-1
                       are allowed. Negative, NaN, and inf are rejected.
                       Active on TWO composite paths:
                         - --blast-radius (no --ast): all three weights apply
                           (lexical + ast + temporal).
                         - text + --ast (the intersection path): lexical and ast
                           apply. The temporal weight is INERT whenever --ast is
                           present, since the AST intersection fuses only the
                           lexical and ast signals.
                       On any other query (pure-lexical, standalone --ast,
                       --hot/--cold/--risky-only, --blast-radius-only) --weights is
                       inert; supplying it there prints a one-line notice to stderr
                       (#377).
                       The 3 extended signals (import_graph, dir_proximity,
                       structural_coupling) are fixed at 0.0 until measured.

  Example: --weights 0.8,0.1,0.1  (lexical-heavy)
           --weights 0.2,0.2,0.6  (temporal-heavy; needs --blast-radius, no --ast)

General examples:
  skim search \"authenticate\"                Search for 'authenticate'
  skim search --limit 5 \"parse_url\"         Return at most 5 results
  skim search --json \"UserService\"          JSON output
  skim search --build                       Build the search index
  skim search --rebuild                     Rebuild from scratch
  skim search --update                      Refresh stale index
  skim search --stats                       Show index statistics
  skim search --install-hooks               Install hooks (clone-wide in a linked worktree)
  skim search --hot                         Top hotspot files (standalone)
  skim search --hot --limit 5 --offset 5   Hotspot page 2 (items 6-10)
  skim search --risky                       Top risky files (standalone)
  skim search --blast-radius src/auth.rs    Co-change partners of auth.rs
  skim search --blast-radius src/auth.rs --offset 10  Co-change page 2
  skim search \"auth\" --hot                  Text results sorted by hotspot
  skim search \"auth\" --blast-radius src/auth.rs  Text within co-change partners";

fn print_help() {
    // AD-375-3: the `index` subcommand line was removed with #375 (avoids PF-008).
    // `skim search index` now runs a QUERY for the word "index"; builds go through
    // --build / --rebuild / --update (already documented in Options above).
    // Body lives in SEARCH_HELP_TEXT so AC10 can assert it without driving the CLI.
    println!("{SEARCH_HELP_TEXT}");
}

// ============================================================================
// Shared JSON stats builder
// ============================================================================

/// Build the `--stats --json` value used by both [`run_stats`] and the test helper.
///
/// Shared so that AC11 back-compat tests guard the actual production code path
/// rather than a hand-duplicated reimplementation.  `run_stats` calls this in its
/// JSON branch; `stats_json_for_test` delegates to it directly.
///
/// AD-413-13: `git_head` is the manifest's stored HEAD ("HEAD-at-last-build" from
/// [`FileManifest::stored_git_head`]) while `git_head_state` is the live HEAD state
/// resolved at call time.  The two keys can legitimately diverge — for example, a
/// frozen linked worktree before its first post-fix rebuild produces
/// `{"git_head": null, "git_head_state": "resolved"}`, which is a legal transient
/// state, not a contradiction.
///
/// `head_state` is resolved ONCE by the caller (`run_stats`) and passed in so the
/// same snapshot feeds both [`staleness::warn_if_temporal_unverifiable`] and the
/// `git_head_state` JSON key without a second `git_head_state` syscall sequence.
///
/// AD-395-6: `skipped` / `skipped_by_reason` are additive keys; all nine pre-existing
/// keys are unchanged (AC11 back-compat).  `skipped_by_reason` uses `BTreeMap` for
/// byte-stable key order consistent with the text-mode path (PF-012).
fn build_stats_json(
    cache_dir: &std::path::Path,
    root: &std::path::Path,
    head_state: &staleness::HeadState,
) -> anyhow::Result<serde_json::Value> {
    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        return Ok(serde_json::json!({ "error": "no index found" }));
    }
    let reader = rskim_search::NgramIndexReader::open(cache_dir)?;
    let stats = reader.stats();
    let total_on_disk = total_on_disk_bytes(cache_dir);
    let temporal_db_bytes = artifact_len(cache_dir, "temporal.db");
    let (staleness_status, loaded_manifest) = staleness::check_staleness(cache_dir, root);
    let git_head = loaded_manifest
        .as_ref()
        .and_then(|m| m.stored_git_head().map(str::to_string));
    let skip_entries: Vec<_> = loaded_manifest
        .as_ref()
        .map(|m| m.skipped().collect::<Vec<_>>())
        .unwrap_or_default();
    let skipped_arr: Vec<serde_json::Value> = skip_entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "path": e.path,
                "reason": e.reason_label(),
            })
        })
        .collect();
    // PF-012: BTreeMap gives deterministic (sorted) key order in JSON output,
    // consistent with the alphabetical sort used in the text-mode path.
    let mut skipped_by_reason: std::collections::BTreeMap<&str, u64> =
        std::collections::BTreeMap::new();
    for e in &skip_entries {
        *skipped_by_reason.entry(e.reason_label()).or_insert(0) += 1;
    }
    // NOTE for JSON consumers: `skipped` and `skipped_by_reason` reflect only
    // PERSISTED content-skips (Minified / NonUtf8 / TooLarge — OD-395-4).
    // They do NOT include UnsupportedLanguage or ReadError skips, which are
    // counted in the `run_build` headline ("N skipped") but not persisted.
    // A repo with 50 unsupported files + 1 minified bundle therefore shows
    // `"skipped": [<minified entry>]` here vs. "51 skipped" at build time.
    //
    // AD-405-9 / AC-405-9 / AC-405-15: `ast_coverage` is additive (never replaces
    // existing keys) and OMITTED when clean (is_clean() == true), matching the
    // same guard used on the standalone --ast and compound --ast surfaces.
    // Absent when no manifest is loaded OR when all files are within cap.
    // No-index early-return above keeps the error object as-is.
    let ast_coverage_val: Option<serde_json::Value> = loaded_manifest
        .as_ref()
        .and_then(|m| {
            let cov = m.ast_coverage();
            if cov.is_clean() { None } else { Some(cov) }
        })
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| anyhow::anyhow!("ast_coverage serialization error: {e}"))?;
    // AC20 (#413): additive `git_head_state` key with one of three string values.
    // Not in either error object (AC21 — the no-index early-return above is before
    // this computation). Owned by #413; #414 extends this with `temporal_state`.
    // Finding [reliability] fix: `head_state` is resolved ONCE by the caller
    // (run_stats) and passed in — no second git_head_state call here.
    let git_head_state_str = match head_state {
        staleness::HeadState::Resolved(_) => "resolved",
        staleness::HeadState::Unresolved => "unresolved",
        staleness::HeadState::NotARepo => "not_a_repo",
    };
    let mut result = serde_json::json!({
        "file_count": stats.file_count,
        "total_ngrams": stats.total_ngrams,
        "index_size_bytes": stats.index_size_bytes,
        "total_on_disk_bytes": total_on_disk,
        "temporal_db_bytes": temporal_db_bytes,
        "last_updated": stats.last_updated,
        "git_head": git_head,
        "git_head_state": git_head_state_str,
        "staleness": staleness_status.to_string(),
        "cache_dir": cache_dir.display().to_string(),
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

/// Delegate to [`build_stats_json`] so AC4/AC11 tests cover the production
/// code path rather than a hand-duplicated reimplementation.
///
/// Used by `index_tests.rs` for:
/// - AC4 (#395): assert `skipped` array / `skipped_by_reason` JSON fields.
/// - AC11 (#395): assert all nine pre-existing keys survive with unchanged types.
///
/// Resolves the live `HeadState` from `root` and passes it to `build_stats_json`,
/// matching the single-resolve contract the production `run_stats` path now enforces.
#[cfg(test)]
pub(crate) fn stats_json_for_test(
    cache_dir: &std::path::Path,
    root: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    let head_state = staleness::git_head_state(root);
    build_stats_json(cache_dir, root, &head_state)
}

// ============================================================================
// Tests
// ============================================================================

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

    /// Locate the `skim` binary for subprocess-level tests.
    ///
    /// Returns `CARGO_BIN_EXE_skim` when set by cargo test; falls back to walking
    /// up from `current_exe()` (deps/ → debug or release/).
    fn skim_bin_path() -> String {
        std::env::var("CARGO_BIN_EXE_skim").unwrap_or_else(|_| {
            let mut p = std::env::current_exe().unwrap();
            p.pop(); // deps/
            p.pop(); // debug/ or release/
            p.push("skim");
            p.to_string_lossy().to_string()
        })
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

    /// AC-6 (#380): when AST and temporal artifacts also exist, the total includes
    /// them — it MUST NOT report lexical-only.
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
            ("temporal.db", 100),
        ];
        for (name, n) in &sizes {
            std::fs::write(cache_dir.join(name), vec![0u8; *n as usize]).unwrap();
        }
        let expected: u64 = sizes.iter().map(|(_, n)| n).sum();
        let lexical_only: u64 = 10 + 20 + 5;

        let total = total_on_disk_bytes(cache_dir);
        assert_eq!(total, expected, "total must sum all 7 artifacts (AC-6)");
        assert!(
            total > lexical_only,
            "total ({total}) must exceed lexical-only ({lexical_only}) when AST+temporal exist (AC-6 negative)"
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
        assert_eq!(artifact_len(dir.path(), "temporal.db"), 0);
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

    /// Double-offset guard (#372): when `--hot`/`--cold`/`--risky` is active,
    /// the QueryConfig built inside `run_query` must carry `offset: None` so that
    /// `execute_query_with_manifest` (the pre-sort path) does NOT consume the
    /// offset.  The single correct application is the post-sort `drain` in
    /// `run_query`.
    ///
    /// This test exercises the config-building logic directly by checking the
    /// flags value and asserting that the temporal branch suppresses the offset
    /// in the config.  It is a whitebox unit test of the dispatch invariant, not
    /// an end-to-end integration (which would require a live temporal DB).
    ///
    /// PF-007 (discriminating): if `offset: if flags.temporal_sort.is_some() { None }
    /// else { flags.offset }` is removed, this test catches the regression by
    /// confirming the temporal flag was parsed (so the guard condition fires).
    #[test]
    fn test_double_offset_guard_temporal_sort_suppresses_config_offset() {
        // Parse flags that combine --offset and --hot.
        // We cannot call run_query directly (requires a real index), but we can
        // verify that the parsed flags correctly encode the pre-conditions for
        // the guard inside run_query.
        let flags = parse_flags(&[
            "authenticate".to_string(),
            "--hot".to_string(),
            "--offset".to_string(),
            "5".to_string(),
        ])
        .unwrap();
        // Offset is present in parsed flags.
        assert_eq!(
            flags.offset,
            Some(5),
            "offset must be parsed and stored in Flags"
        );
        // Temporal sort is set — this is the pre-condition for the double-offset guard.
        assert_eq!(
            flags.temporal_sort,
            Some(types::TemporalSort::Hot),
            "temporal_sort must be Hot when --hot is supplied"
        );
        // Verify the guard expression: when temporal_sort is Some, config.offset
        // should be None (suppressed for the pre-sort path).
        let config_offset = if flags.temporal_sort.is_some() {
            None
        } else {
            flags.offset
        };
        assert_eq!(
            config_offset, None,
            "QueryConfig.offset must be None when temporal_sort is active (double-offset guard)"
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

    /// `--ast --help`: `--help` must not be swallowed as the AST pattern value.
    ///
    /// Before the AD-412-3 fix, `take_flag_value("--ast", Some("--help"), "--ast")`
    /// returned Ok(("--help", true)), so `ast = Some("--help")` propagated into
    /// `validate_ast_pattern` which produced a confusing "unknown pattern" error
    /// instead of pointing the user at the help surface.  The fix bails early with
    /// a clear "requires a value; to print help, run: skim search --help" message.
    #[test]
    fn test_parse_flags_help_after_ast_is_not_consumed_as_value() {
        let err = parse_flags(&["--ast".to_string(), "--help".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--ast requires a value"),
            "AD-412-3: --help must not be silently consumed as the --ast value; got: {msg}"
        );
        assert!(
            msg.contains("skim search --help"),
            "error must point at the help surface; got: {msg}"
        );
    }

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

    /// Same as above but with the `-h` short form.
    #[test]
    fn test_parse_flags_short_h_after_ast_is_not_consumed_as_value() {
        let err = parse_flags(&["--ast".to_string(), "-h".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--ast requires a value"),
            "AD-412-3: -h must not be silently consumed as the --ast value; got: {msg}"
        );
    }

    /// `--blast-radius --help`: `--help` must not be swallowed as the file path.
    ///
    /// `--blast-radius` consumes its value via `parse_temporal_flag` rather than
    /// `take_flag_value`, so it needs the same AD-412-3 guard.  Without it,
    /// `blast_radius = Some("--help")` ran a degraded co-change search for a file
    /// literally named "--help" (exit 0) instead of honoring the help request —
    /// the same silent-wrong-behavior class AD-412-3 fixed for `--root`.
    ///
    /// PF-007 (discriminating): asserts the specific "--blast-radius requires a
    /// file path" message and the help-surface pointer, so reverting the guard
    /// (which yields Ok with a bogus blast_radius) makes `unwrap_err()` panic.
    #[test]
    fn test_parse_flags_help_after_blast_radius_is_not_consumed_as_value() {
        let err = parse_flags(&["--blast-radius".to_string(), "--help".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--blast-radius requires a file path"),
            "AD-412-3: --help must not be silently consumed as the --blast-radius value; got: {msg}"
        );
        assert!(
            msg.contains("skim search --help"),
            "error must point at the help surface; got: {msg}"
        );
    }

    /// Same as above but with the `-h` short form.
    #[test]
    fn test_parse_flags_short_h_after_blast_radius_is_not_consumed_as_value() {
        let err = parse_flags(&["--blast-radius".to_string(), "-h".to_string()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--blast-radius requires a file path"),
            "AD-412-3: -h must not be silently consumed as the --blast-radius value; got: {msg}"
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

    // ============================================================================
    // resolve_blast_radius_paths — None DB degradation path
    // ============================================================================

    /// When blast_radius is Some but temporal.db is absent (temporal data not yet
    /// auto-populated), the function must return Ok(None) without panicking.
    /// A stderr warning is expected but the caller handles the degradation.
    #[test]
    fn test_resolve_blast_radius_filter_no_db_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        // cache_dir has no temporal.db — open_temporal_db_for returns Err(Absent)
        // and the function degrades gracefully to Ok(None).
        let result = temporal::resolve_blast_radius_paths(
            Some("src/auth.rs"),
            root,
            dir.path(),
            false,
            &staleness::HeadState::NotARepo,
        );
        assert!(
            result.is_ok(),
            "must not error when temporal.db is absent, got: {:?}",
            result.unwrap_err()
        );
        assert_eq!(
            result.unwrap(),
            None,
            "must return None (graceful degradation) when temporal.db is absent"
        );
    }

    // ============================================================================
    // AD-413-16 / PF-016: AnchorDiffers must return Some(empty) not None.
    // ============================================================================

    /// When blast_radius is Some and temporal.db exists but belongs to a
    /// different repository (AnchorDiffers), `resolve_blast_radius_paths` must
    /// return `Ok(Some(empty_set))` — NOT `Ok(None)`.
    ///
    /// Returning `Ok(None)` overloads the "not requested" sentinel: callers'
    /// `.map()` produces `None → no file filter → full unfiltered index` (PF-016).
    /// An empty allowlist forces every blast-radius call site to emit zero results,
    /// matching the standalone arm (`run_temporal_standalone`) which also serves
    /// zero rows on `AnchorDiffers`.
    #[test]
    fn test_resolve_blast_radius_anchor_differs_returns_empty_set() {
        use staleness::HeadState;

        // Set up:
        //   parent_dir/.git/HEAD   ← fake git repo (so resolve_repo_toplevel
        //                            discovers parent_dir as the live toplevel)
        //   parent_dir/sub/        ← `root` (no own .git, so it is a subdir root)
        //   parent_dir/sub/temporal.db ← DB whose META_GIT_TOPLEVEL is a
        //                                 sentinel for a *different* repo path,
        //                                 triggering AnchorDiffers.
        let parent_dir = tempfile::TempDir::new().unwrap();
        let git_dir = parent_dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let sub_dir = parent_dir.path().join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();

        let db_path = sub_dir.join("temporal.db");

        // Write temporal.db whose git_toplevel is a sentinel that does NOT match
        // parent_dir — `anchor_state_on_db` will return AnchorDiffers.
        {
            let db = rskim_search::TemporalDb::open(&db_path).expect("must create temporal.db");
            db.set_meta(rskim_search::META_GIT_TOPLEVEL, "/other/repo/sentinel")
                .expect("must set toplevel");
        }

        // sub_dir has no .git → resolve_repo_toplevel walks up to parent_dir.
        // Stored toplevel ("/other/repo/sentinel") ≠ parent_dir → AnchorDiffers.
        let result = temporal::resolve_blast_radius_paths(
            Some("src/auth.rs"),
            &sub_dir, // root
            &sub_dir, // cache_dir (DB lives at sub_dir/temporal.db)
            false,
            &HeadState::NotARepo,
        );

        assert!(
            result.is_ok(),
            "must not return Err on AnchorDiffers, got: {:?}",
            result.unwrap_err()
        );
        let paths = result.unwrap();
        assert!(
            paths.is_some(),
            "must return Some(empty_set) on AnchorDiffers, not None \
             (None overloads the 'not requested' sentinel — PF-016 / AD-413-16)"
        );
        assert!(
            paths.unwrap().is_empty(),
            "empty allowlist forces zero results on all blast-radius arms"
        );
    }

    // ============================================================================
    // F12: Missing temporal.db must produce exit 0 (graceful degradation), not
    //      exit 1. AC says: "Missing temporal.db → warning on stderr, exit 0".
    // ============================================================================

    /// AC8: Standalone temporal mode (e.g. `skim search --hot`) with no temporal.db
    /// must return `ExitCode::SUCCESS` (not FAILURE) AND must not create a corrupt
    /// temporal.db in the cache directory.
    ///
    /// Discriminating: asserts both exit code AND absent/non-corrupt DB file.
    #[test]
    fn test_standalone_temporal_no_db_returns_exit_0() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        // No temporal.db exists in the temp dir's cache — standalone path should
        // degrade gracefully with exit 0.
        let result = run(
            &["--hot".to_string(), "--root".to_string(), root],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "missing temporal.db must be a warning (exit 0), not an error (exit 1)"
        );

        // AC8 postcondition: no corrupt temporal.db created as a side effect.
        // The cache dir is the auto-resolved .skim/search/ under the temp root.
        // We enumerate likely cache paths; if temporal.db was created anywhere,
        // it must be openable (not corrupt).
        // Most directly: verify it was not created at the root itself.
        let temporal_at_root = dir.path().join("temporal.db");
        if temporal_at_root.exists() {
            // If it somehow exists, it must at least be valid SQLite.
            assert!(
                rskim_search::TemporalDb::open(&temporal_at_root).is_ok(),
                "temporal.db at root must not be corrupt (AC8 postcondition)"
            );
        }
    }

    // ============================================================================
    // #413 / AD-413-8 — the degraded message is TRIAGED by HEAD state
    // ============================================================================

    /// AC18 — the unresolvable-HEAD message must not repeat the "not a git repo"
    /// lie, must name BOTH remaining causes, and must carry the real ticket number.
    ///
    /// AC18(c)/ADR-004: the `#481` literal is asserted against the constant itself,
    /// not against a regex for `#<n>`, so a placeholder ticket number or a renumber
    /// fails this test rather than shipping.
    #[test]
    fn test_head_unresolved_message_content_ac18() {
        assert!(
            !HEAD_UNRESOLVED_TEMPORAL_MSG.contains("run 'skim search' on a git repo"),
            "AC18(a): the unresolvable-HEAD message must NOT claim this is not a git repo"
        );
        assert!(
            HEAD_UNRESOLVED_TEMPORAL_MSG.contains("unborn branch"),
            "AC18(b): must name the unborn-branch cause"
        );
        assert!(
            HEAD_UNRESOLVED_TEMPORAL_MSG.contains("reftable"),
            "AC18(b): must name the unsupported-ref-backend (reftable) cause"
        );
        assert!(
            HEAD_UNRESOLVED_TEMPORAL_MSG.contains("#481"),
            "AC18(c)/ADR-004: must cite the filed reftable ticket #481 verbatim"
        );
        assert!(
            !SUBDIR_ROOT_TEMPORAL_MSG.contains("run 'skim search' on a git repo"),
            "AC33(c): the anchor-refusal message must NOT claim this is not a git repo"
        );
    }

    /// AC19 back-compat + AD-413-8 triage — `degraded_notice` returns the
    /// byte-identical `NO_TEMPORAL_DATA_MSG` for a genuine non-repo, and a DIFFERENT
    /// message for a repo whose HEAD cannot be resolved (AD-414-1).
    ///
    /// Finding 1/3 fix: the function is pure (takes pre-resolved head+anchor),
    /// so the test constructs states directly — no filesystem fixtures needed.
    #[test]
    fn test_degraded_notice_triage_ac18_ac19() {
        // Genuine non-repo → byte-identical legacy text (AC19).
        assert_eq!(
            temporal::degraded_notice(
                &temporal::TemporalUnavailable {
                    reason: temporal::DegradedReason::NotGitRepo,
                    detail: String::new(),
                },
                "",
                temporal::Fallback::Lexical
            ),
            NO_TEMPORAL_DATA_MSG,
            "AC19: NotARepo must keep the byte-identical legacy message"
        );

        // Repo whose HEAD exists but does not resolve → the triaged message (AC18).
        assert_eq!(
            temporal::degraded_notice(
                &temporal::TemporalUnavailable {
                    reason: temporal::DegradedReason::HeadUnresolved,
                    detail: String::new(),
                },
                "",
                temporal::Fallback::Lexical
            ),
            HEAD_UNRESOLVED_TEMPORAL_MSG,
            "AD-413-8: Unresolved HEAD must not get the non-repo message"
        );
    }

    /// AC33(f): `degraded_notice` Resolved+Differs branch returns a message that
    /// contains SUBDIR_ROOT_TEMPORAL_MSG and the recorded/live path snippets
    /// (AD-414-1).
    ///
    /// Finding 1/3 fix: the function is pure, so the test passes states directly.
    /// No filesystem or SQLite fixtures are needed for the dispatch logic.
    #[test]
    fn test_degraded_notice_resolved_differs_returns_subdir_msg() {
        let recorded = std::path::PathBuf::from("/wrong/repo/path");
        let live = std::path::PathBuf::from("/correct/repo/path");
        let msg = temporal::degraded_notice(
            &temporal::TemporalUnavailable {
                reason: temporal::DegradedReason::RepositoryMismatch,
                detail: format!("(recorded: {:?}, live: {:?})", recorded, live),
            },
            "",
            temporal::Fallback::Lexical,
        );

        assert!(
            msg.contains(SUBDIR_ROOT_TEMPORAL_MSG),
            "AC33(f): Resolved+Differs branch must contain SUBDIR_ROOT_TEMPORAL_MSG, got: {msg:?}"
        );
        assert!(
            msg.contains("/wrong/repo/path"),
            "AC33(f): message must include the recorded anchor path, got: {msg:?}"
        );
        assert!(
            !msg.contains(NO_TEMPORAL_DATA_MSG),
            "AC33(f): Resolved+Differs must NOT return the non-repo message, got: {msg:?}"
        );
        assert!(
            !msg.contains(HEAD_UNRESOLVED_TEMPORAL_MSG),
            "AC33(f): Resolved+Differs must NOT return the unresolved-HEAD message, got: {msg:?}"
        );
    }

    /// `degraded_notice` Resolved+other branch returns TEMPORAL_BUILD_EMPTY_MSG
    /// when the HEAD resolves and AnchorState is NOT Differs (AD-414-1).
    ///
    /// Finding 1/3 fix: the function is pure, so the test passes states directly.
    #[test]
    fn test_degraded_notice_resolved_other_returns_build_empty() {
        // NotAdopted → no Differs branch → TEMPORAL_BUILD_EMPTY_MSG.
        let msg = temporal::degraded_notice(
            &temporal::TemporalUnavailable {
                reason: temporal::DegradedReason::Empty,
                detail: String::new(),
            },
            "",
            temporal::Fallback::Lexical,
        );
        assert_eq!(
            msg, TEMPORAL_BUILD_EMPTY_MSG,
            "Resolved+NotAdopted branch must return TEMPORAL_BUILD_EMPTY_MSG, got: {msg:?}"
        );
        // Absent anchor also falls through to TEMPORAL_BUILD_EMPTY_MSG.
        let msg2 = temporal::degraded_notice(
            &temporal::TemporalUnavailable {
                reason: temporal::DegradedReason::Empty,
                detail: String::new(),
            },
            "",
            temporal::Fallback::Lexical,
        );
        assert_eq!(
            msg2, TEMPORAL_BUILD_EMPTY_MSG,
            "Resolved+Absent branch must return TEMPORAL_BUILD_EMPTY_MSG, got: {msg2:?}"
        );
        // Agrees anchor also falls through to TEMPORAL_BUILD_EMPTY_MSG.
        let msg3 = temporal::degraded_notice(
            &temporal::TemporalUnavailable {
                reason: temporal::DegradedReason::Empty,
                detail: String::new(),
            },
            "",
            temporal::Fallback::Lexical,
        );
        assert_eq!(
            msg3, TEMPORAL_BUILD_EMPTY_MSG,
            "Resolved+Agrees branch must return TEMPORAL_BUILD_EMPTY_MSG, got: {msg3:?}"
        );
    }

    // ============================================================================
    // AC9 — User-facing message accuracy: strings reference auto-refresh, not
    //        stale manual-refresh advice.
    // ============================================================================

    /// AC9: The no-temporal-data message for --hot/--cold/--risky must reference
    /// 'skim search' auto-populate, NOT the old 'skim heatmap' advice.
    ///
    /// PF-007 discriminating: asserts against the `NO_TEMPORAL_DATA_MSG` production
    /// constant (not a locally-declared copy), so changing the production string
    /// immediately breaks this test.
    ///
    /// Coverage note: this test guards the content of the production constant and
    /// verifies that run() exits 0 on a non-git dir with --json --hot (the exit-0
    /// contract of the degradation path).  The JSON emission path — that production
    /// stdout actually contains `{"warning": NO_TEMPORAL_DATA_MSG}` — requires
    /// subprocess spawning to capture stdout; that level of coverage is provided
    /// by `test_hot_json_warning_content_on_non_git_dir` below, which spawns the
    /// binary and asserts the parsed `warning` field equals the production constant.
    #[test]
    fn test_no_temporal_data_message_references_auto_refresh() {
        // Assert against the production constant — NOT a local string literal.
        // This is the single source of truth: if the production constant changes,
        // the assertions below break immediately (PF-007 fix, #357 cycle-2 finding 12).

        // AC9 guard: must NOT contain the old 'skim heatmap' advice.
        assert!(
            !NO_TEMPORAL_DATA_MSG.contains("skim heatmap"),
            "NO_TEMPORAL_DATA_MSG must NOT reference 'skim heatmap' (AC9 regression guard)"
        );
        // AC9 guard: must reference the auto-refresh path.
        assert!(
            NO_TEMPORAL_DATA_MSG.contains("skim search"),
            "NO_TEMPORAL_DATA_MSG must reference 'skim search' auto-refresh (AC9)"
        );
        assert!(
            NO_TEMPORAL_DATA_MSG.contains("auto-populate"),
            "NO_TEMPORAL_DATA_MSG must mention 'auto-populate' (AC9)"
        );

        // Exit-0 contract: --json --hot on a non-git dir must still exit SUCCESS.
        // (The warning is emitted to stdout as JSON; captured content is verified
        // in test_hot_json_warning_content_on_non_git_dir below.)
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let result = run(
            &[
                "--json".to_string(),
                "--hot".to_string(),
                "--root".to_string(),
                root,
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "--json --hot on non-git dir must exit 0 (AC9 degradation contract)"
        );
    }

    /// AC9 JSON path: the production code must emit
    /// `{"warning": NO_TEMPORAL_DATA_MSG}` on stdout when --json --hot is
    /// invoked on a dir with no temporal data.
    ///
    /// PF-007 discriminating: captures the actual binary's stdout via subprocess
    /// and asserts the JSON `warning` field equals the production constant — so a
    /// regression where the code emits a different string, or emits nothing, or
    /// emits plain text instead of JSON, fails this test (#357 cycle-2 finding 4).
    #[test]
    fn test_hot_json_warning_content_on_non_git_dir() {
        let bin = skim_bin_path();

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();

        let output = std::process::Command::new(&bin)
            .args(["search", "--json", "--hot", "--root", &root])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {bin}: {e}"));

        assert!(
            output.status.success(),
            "--json --hot on non-git dir must exit 0; got {:?}",
            output.status
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "stdout must be valid JSON; got {:?}\nparse error: {e}",
                stdout
            )
        });

        let warning = parsed
            .get("warning")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("JSON must have a 'warning' string field; got: {parsed:?}"));

        assert_eq!(
            warning, NO_TEMPORAL_DATA_MSG,
            "JSON 'warning' field must equal NO_TEMPORAL_DATA_MSG (AC9 JSON path, PF-007)"
        );
    }

    // ============================================================================
    // AC9 — format_temporal_text Hotspots/Coldspots header newline regression
    // ============================================================================

    /// Hotspots/Coldspots text header must NOT have a blank line between the
    /// header text and the column header row.
    ///
    /// Regression guard against the writeln!("...\n") double-newline introduced
    /// by a prior clippy refactor. The header must be immediately followed by the
    /// column header on the next line.
    #[test]
    fn test_format_temporal_text_hotspots_no_blank_line_after_header() {
        use std::io::BufWriter;

        use super::temporal::{TemporalQueryOutput, format_temporal_text};
        use rskim_search::HotspotRow;

        let rows = vec![HotspotRow {
            file_path: "src/hot.rs".to_string(),
            score: 0.8,
            changes_30d: 3,
            changes_90d: 5,
        }];
        let output = TemporalQueryOutput::Hotspots(rows);

        let mut buf = Vec::new();
        let mut writer = BufWriter::new(&mut buf);
        format_temporal_text(
            &output,
            crate::cmd::search::types::Page::first(10),
            &mut writer,
        )
        .unwrap();
        drop(writer);

        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        // Line 0: "Hotspots (top 1, 90-day decay):"
        // Line 1: "  Score  30d  90d  Path"  (column header — NOT a blank line)
        assert!(
            !lines.is_empty() && lines[0].contains("Hotspots"),
            "first line must contain 'Hotspots', got: {:?}",
            lines.first()
        );
        assert!(
            lines.len() >= 2 && !lines[1].trim().is_empty(),
            "second line must be the column header (not blank), got: {:?}",
            lines.get(1)
        );
        assert!(
            lines.get(1).map(|l| l.contains("Score")).unwrap_or(false),
            "second line must be the 'Score' column header (no blank line after header), \
             got: {:?}",
            lines.get(1)
        );
    }

    /// Coldspots text header must NOT have a blank line after it (same regression
    /// as Hotspots but for the --cold path).
    #[test]
    fn test_format_temporal_text_coldspots_no_blank_line_after_header() {
        use std::io::BufWriter;

        use super::temporal::{TemporalQueryOutput, format_temporal_text};
        use rskim_search::HotspotRow;

        let rows = vec![HotspotRow {
            file_path: "src/cold.rs".to_string(),
            score: 0.1,
            changes_30d: 0,
            changes_90d: 1,
        }];
        let output = TemporalQueryOutput::Coldspots(rows);

        let mut buf = Vec::new();
        let mut writer = BufWriter::new(&mut buf);
        format_temporal_text(
            &output,
            crate::cmd::search::types::Page::first(10),
            &mut writer,
        )
        .unwrap();
        drop(writer);

        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        assert!(
            !lines.is_empty() && lines[0].contains("Coldspots"),
            "first line must contain 'Coldspots'"
        );
        assert!(
            lines.get(1).map(|l| l.contains("Score")).unwrap_or(false),
            "second line must be the 'Score' column header (no blank line after header), \
             got: {:?}",
            lines.get(1)
        );
    }

    // ============================================================================
    // #357 BUG A — run_build (--rebuild / --build) must populate temporal.db
    // ============================================================================

    /// Shared git-repo helper — delegates to the canonical `staleness::create_real_git_repo`
    /// (#357 cycle-2 findings 9/14: removes the third near-verbatim copy, per plan step 6).
    /// Named identically to its counterpart in `staleness_tests.rs` and
    /// `temporal_build_tests.rs` so readers scanning the three test files see a
    /// single shared-helper relationship rather than three apparently-distinct helpers
    /// (#357 cycle-2 finding 3).
    fn create_real_git_repo(
        dir: &std::path::Path,
        commit_specs: &[(&str, &[(&str, &str)])],
    ) -> String {
        staleness::create_real_git_repo(dir, commit_specs)
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

    /// BUG A discriminating test: after `skim search --rebuild` on a git repo with
    /// ≥2 commits, temporal.db MUST exist, contain non-empty hotspots, and
    /// META_GIT_HEAD MUST equal the repo HEAD.
    ///
    /// PF-007: exit-0 alone is vacuous — this asserts the DISCRIMINATING observables
    /// (non-empty hotspots + exact HEAD match) so the test fails the moment BUG A
    /// returns (i.e. if the temporal hook were removed from run_build).
    #[test]
    fn test_rebuild_populates_temporal_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let head = create_real_git_repo(
            root,
            &[
                ("feat: add auth", &[("src/auth.rs", "fn authenticate() {}")]),
                ("feat: add parser", &[("src/parser.rs", "fn parse() {}")]),
                (
                    "fix: fix auth bug",
                    &[("src/auth.rs", "fn authenticate() { // fixed }")],
                ),
            ],
        );
        assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

        let root_str = root.to_string_lossy().to_string();
        let result = run(
            &["--rebuild".to_string(), "--root".to_string(), root_str],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS, "--rebuild must exit 0");

        // Locate the cache dir (resolves to <root>/.skim/search/).
        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let temporal_db_path = cache_dir.join("temporal.db");

        // Discriminating: temporal.db must exist.
        assert!(
            temporal_db_path.exists(),
            "temporal.db must exist after --rebuild on a git repo (#357 BUG A)"
        );

        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();

        // Discriminating: META_GIT_HEAD must equal the repo HEAD (exact match).
        let stored_head = db
            .get_meta(rskim_search::META_GIT_HEAD)
            .unwrap()
            .expect("META_GIT_HEAD must be set in temporal.db after --rebuild");
        assert_eq!(
            stored_head, head,
            "META_GIT_HEAD in temporal.db must match the repo HEAD after --rebuild (#357 BUG A)"
        );

        // Discriminating: hotspots must be non-empty (data was actually indexed).
        let hotspots = db.top_hotspots(20).unwrap();
        assert!(
            !hotspots.is_empty(),
            "temporal.db must contain non-empty hotspot data after --rebuild (#357 BUG A)"
        );
    }

    /// BUG A parity: `--build` (force=false) must populate temporal.db identically
    /// to `--rebuild` (force=true) on a fresh git repo with no prior index.
    ///
    /// PF-007: asserts META_GIT_HEAD equality between --build and --rebuild runs
    /// (both must have temporal data; comparing both to the same repo HEAD).
    #[test]
    fn test_build_populates_temporal_db_same_as_rebuild() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let head = create_real_git_repo(
            root,
            &[
                ("feat: first", &[("lib.rs", "pub fn foo() {}")]),
                ("feat: second", &[("main.rs", "fn main() {}")]),
            ],
        );
        assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

        let root_str = root.to_string_lossy().to_string();
        let result = run(
            &["--build".to_string(), "--root".to_string(), root_str],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS, "--build must exit 0");

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let temporal_db_path = cache_dir.join("temporal.db");

        assert!(
            temporal_db_path.exists(),
            "temporal.db must exist after --build on a git repo (#357 BUG A parity)"
        );

        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
        let stored_head = db
            .get_meta(rskim_search::META_GIT_HEAD)
            .unwrap()
            .expect("META_GIT_HEAD must be set in temporal.db after --build");
        assert_eq!(
            stored_head, head,
            "META_GIT_HEAD in temporal.db must match the repo HEAD after --build (#357 BUG A)"
        );

        let hotspots = db.top_hotspots(20).unwrap();
        assert!(
            !hotspots.is_empty(),
            "temporal.db must contain non-empty hotspot data after --build (#357 BUG A parity)"
        );
    }

    /// BUG A NEGATIVE: `--rebuild` on a non-git directory must succeed (exit 0),
    /// must NOT create temporal.db (no git history to index), and must create the
    /// lexical index (build still succeeds for lexical+AST).
    ///
    /// PF-007 discriminating: assert SUCCESS && !temporal.db.exists() && index.skidx exists.
    #[test]
    fn test_rebuild_non_git_dir_succeeds_no_temporal_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        // Write at least one indexable file so build_index has something to do.
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let root_str = root.to_string_lossy().to_string();
        let result = run(
            &["--rebuild".to_string(), "--root".to_string(), root_str],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "--rebuild on non-git dir must exit 0 (non-fatal temporal, ADR-006/D5)"
        );

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();

        // Discriminating: no temporal.db (no git history).
        let temporal_db_path = cache_dir.join("temporal.db");
        assert!(
            !temporal_db_path.exists(),
            "temporal.db must NOT be created on a non-git dir (no history to walk)"
        );

        // Discriminating: lexical index must still exist (build succeeded for lexical).
        let index_path = cache_dir.join("index.skidx");
        assert!(
            index_path.exists(),
            "index.skidx must exist after --rebuild even when temporal fails on non-git dir"
        );
    }

    // ============================================================================
    // #357 BUG B — temporal.db self-heals when lexical is Current but temporal stale
    // ============================================================================

    /// BUG B discriminating: when the lexical index is Current but temporal.db is
    /// deleted, a subsequent auto_refresh-routed query recreates temporal.db with
    /// META_GIT_HEAD == current HEAD and non-empty hotspots.
    ///
    /// Drive via `run()` with a text query (routes through auto_refresh_if_stale),
    /// not staleness::auto_refresh_if_stale directly — ensures the full dispatch
    /// path self-heals (PF-007: assert recreation + exact HEAD match).
    ///
    /// This test FAILS on the pre-fix code because auto_refresh_if_stale returned
    /// early on StalenessCheck::Current without checking temporal.db staleness.
    #[test]
    fn test_bug_b_temporal_db_self_heals_when_lexical_is_current() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let head = create_real_git_repo(
            root,
            &[
                ("feat: add module", &[("src/lib.rs", "pub fn greet() {}")]),
                (
                    "fix: fix greet",
                    &[("src/lib.rs", "pub fn greet() { // fixed }")],
                ),
            ],
        );
        assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

        let root_str = root.to_string_lossy().to_string();

        // First query: builds lexical+AST+temporal (NoIndex → refresh).
        run(
            &[
                "greet".to_string(),
                "--root".to_string(),
                root_str.clone(),
                "--limit".to_string(),
                "5".to_string(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let temporal_db_path = cache_dir.join("temporal.db");

        // Confirm temporal.db was created by the first query.
        assert!(
            temporal_db_path.exists(),
            "temporal.db must exist after first query (setup invariant for BUG B test)"
        );

        // Delete temporal.db — lexical stays Current (HEAD unchanged).
        std::fs::remove_file(&temporal_db_path).unwrap();
        assert!(
            !temporal_db_path.exists(),
            "temporal.db must be deleted (test setup)"
        );

        // Second query: lexical is Current (HEAD unchanged), but temporal.db is missing.
        // BUG B fix: auto_refresh_if_stale must self-heal temporal.db on the Current branch.
        let result = run(
            &[
                "greet".to_string(),
                "--root".to_string(),
                root_str,
                "--limit".to_string(),
                "5".to_string(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "second query must succeed after temporal.db deletion (#357 BUG B)"
        );

        // Discriminating: temporal.db must be recreated.
        assert!(
            temporal_db_path.exists(),
            "temporal.db must be recreated by the second query when lexical is Current (#357 BUG B)"
        );

        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();

        // Discriminating: META_GIT_HEAD must equal the current HEAD (not stale).
        let stored_head = db
            .get_meta(rskim_search::META_GIT_HEAD)
            .unwrap()
            .expect("META_GIT_HEAD must be set in recreated temporal.db");
        assert_eq!(
            stored_head, head,
            "META_GIT_HEAD in recreated temporal.db must match the current repo HEAD (#357 BUG B)"
        );

        // Discriminating: hotspots must be non-empty.
        let hotspots = db.top_hotspots(20).unwrap();
        assert!(
            !hotspots.is_empty(),
            "recreated temporal.db must contain non-empty hotspot data (#357 BUG B)"
        );
    }

    /// BUG B BLOCKER: `--hot` on a stale temporal.db (lexical Current) self-heals
    /// and returns populated hotspot results.
    ///
    /// Per locked decision 2026-06-24: run_temporal_standalone is wired to
    /// auto_refresh_if_stale so bare --hot self-heals a stale temporal.db.
    ///
    /// PF-007 discriminating observables (DB-inspection approach):
    /// - temporal.db is RECREATED by the self-heal (existence check).
    /// - META_GIT_HEAD in the recreated temporal.db equals the repo HEAD (exact
    ///   HEAD equality — fails if the wrong SHA or no SHA is written).
    /// - top_hotspots() returns a non-empty list (data was populated, not empty).
    ///
    /// Note: the test verifies the self-heal via direct DB inspection rather than
    /// stdout/stderr capture (stdout/stderr from run() cannot be reliably captured
    /// in a Rust unit test without process spawning). The DB-inspection assertions
    /// are discriminating: the test FAILS if temporal.db stays deleted (pre-fix
    /// behavior), if META_GIT_HEAD is wrong, or if hotspots are empty.
    /// The 'no temporal data' stderr message and ranked-row stdout guard are the
    /// natural follow-on once the DB is confirmed populated; they are not
    /// additionally asserted here since stdout is not capturable in unit tests.
    #[test]
    fn test_bug_b_hot_self_heals_stale_temporal_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let head = create_real_git_repo(
            root,
            &[
                ("feat: add auth", &[("src/auth.rs", "fn authenticate() {}")]),
                ("feat: add parser", &[("src/parser.rs", "fn parse() {}")]),
                (
                    "fix: fix auth",
                    &[("src/auth.rs", "fn authenticate() { // fixed }")],
                ),
            ],
        );
        assert_eq!(head.len(), 40);

        let root_str = root.to_string_lossy().to_string();

        // Build index first (NoIndex → full build including temporal).
        run(
            &[
                "auth".to_string(),
                "--root".to_string(),
                root_str.clone(),
                "--limit".to_string(),
                "5".to_string(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let temporal_db_path = cache_dir.join("temporal.db");

        // Confirm temporal.db was created.
        assert!(
            temporal_db_path.exists(),
            "temporal.db must exist after initial query (test setup for BUG B BLOCKER)"
        );

        // Delete temporal.db to simulate a stale/missing temporal.db while lexical is Current.
        std::fs::remove_file(&temporal_db_path).unwrap();

        // Run `--hot` on a stale temporal.db (lexical still Current).
        // Pre-fix: would print 'no temporal data' warning and exit 0 with NO rows.
        // Post-fix: auto_refresh_if_stale self-heals, --hot returns populated rows.
        let result = run(
            &[
                "--hot".to_string(),
                "--root".to_string(),
                root_str.clone(),
                "--limit".to_string(),
                "5".to_string(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "--hot after temporal.db deletion must exit 0 (#357 BUG B BLOCKER)"
        );

        // Discriminating: temporal.db must be recreated by the self-heal.
        assert!(
            temporal_db_path.exists(),
            "--hot must trigger temporal.db self-heal when lexical is Current (#357 BUG B BLOCKER)"
        );

        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
        let stored_head = db
            .get_meta(rskim_search::META_GIT_HEAD)
            .unwrap()
            .expect("META_GIT_HEAD must be set after --hot self-heals temporal.db");
        assert_eq!(
            stored_head, head,
            "META_GIT_HEAD must match repo HEAD after --hot self-heal (#357 BUG B BLOCKER)"
        );

        // Discriminating: hotspots must be non-empty (populated, not empty degradation).
        let hotspots = db.top_hotspots(20).unwrap();
        assert!(
            !hotspots.is_empty(),
            "--hot self-healed temporal.db must contain non-empty hotspot data (#357 BUG B BLOCKER)"
        );
    }

    /// BUG B BLOCKER — CLI-level discriminating test for `--hot` self-heal.
    ///
    /// Spawns the binary as a subprocess to capture real stdout/stderr so we can
    /// assert the TWO discriminating CLI observables the plan requires (plan lines
    /// 165 & 217, PF-007):
    ///   (a) at least one ranked hotspot row is present on stdout (data rendered),
    ///   (b) the 'no temporal data' degradation message is ABSENT from stderr
    ///       (self-heal took the render path, not the degradation path).
    ///
    /// The unit-level `test_bug_b_hot_self_heals_stale_temporal_db` proves the
    /// DB was populated; this test proves `run_temporal_standalone` actually USED
    /// that DB to render ranked rows instead of falling through to the degradation
    /// arm (#357 cycle-2 finding 5).
    #[test]
    fn test_hot_self_heal_renders_ranked_rows_not_degradation() {
        let bin = skim_bin_path();

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();

        // Build a git repo with enough commits that --hot has data to render.
        create_real_git_repo(
            root,
            &[
                ("feat: add auth", &[("src/auth.rs", "fn authenticate() {}")]),
                ("feat: add parser", &[("src/parser.rs", "fn parse() {}")]),
                (
                    "fix: fix auth",
                    &[("src/auth.rs", "fn authenticate() { // fixed }")],
                ),
            ],
        );

        // Phase 1: build the index (lexical+AST+temporal) via a text query.
        std::process::Command::new(&bin)
            .args(["search", "auth", "--root", &root_str, "--limit", "5"])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {bin} for setup: {e}"));

        // Phase 2: delete temporal.db so the lexical index is Current but temporal
        // is stale — this is the BUG B BLOCKER scenario.
        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let temporal_db_path = cache_dir.join("temporal.db");
        assert!(
            temporal_db_path.exists(),
            "temporal.db must exist after setup query (precondition for BUG B BLOCKER test)"
        );
        std::fs::remove_file(&temporal_db_path).unwrap();

        // Phase 3: run `--hot` as a subprocess — self-heal fires, then renders.
        let output = std::process::Command::new(&bin)
            .args(["search", "--hot", "--root", &root_str, "--limit", "5"])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {bin} for --hot: {e}"));

        assert!(
            output.status.success(),
            "--hot after temporal.db deletion must exit 0; got {:?}",
            output.status
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // (a) At least one ranked row must appear on stdout.
        // The text format emits hotspot rows as "  <score>  <file>" lines.
        // We check for a non-empty stdout that contains at least one non-header line
        // after the "Hotspots" header — any file path line is sufficient.
        assert!(
            !stdout.trim().is_empty(),
            "--hot must print ranked rows to stdout after self-heal (BUG B BLOCKER, \
             plan lines 165/217); got empty stdout. stderr={stderr:?}"
        );

        // (b) The degradation message must NOT appear on stderr.
        assert!(
            !stderr.contains(NO_TEMPORAL_DATA_MSG),
            "--hot must NOT emit the 'no temporal data' message after self-heal \
             (BUG B BLOCKER); got stderr={stderr:?}"
        );
    }

    /// BUG A BLOCKER — CLI-level discriminating test for `--rebuild` temporal population.
    ///
    /// Spawns the binary as a subprocess to drive the full CLI path, then spawns
    /// it again for `--hot`.  Asserts the TWO discriminating CLI observables (PF-007):
    ///   (a) at least one ranked hotspot row is present on stdout (temporal data populated),
    ///   (b) the 'no temporal data' degradation message is ABSENT from stderr
    ///       (--rebuild populated temporal.db; --hot rendered from it).
    ///
    /// The unit-level `test_rebuild_populates_temporal_db` proves temporal.db was
    /// written; this test proves the CLI `--hot` command actually USES that DB to
    /// render ranked rows instead of emitting the degradation message (#357 BUG A).
    #[test]
    fn test_rebuild_then_hot_renders_ranked_rows_not_degradation() {
        let bin = skim_bin_path();

        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();

        // Build a git repo with enough commits that --hot has hotspot data to render.
        create_real_git_repo(
            root,
            &[
                ("feat: add auth", &[("src/auth.rs", "fn authenticate() {}")]),
                ("feat: add parser", &[("src/parser.rs", "fn parse() {}")]),
                (
                    "fix: fix auth",
                    &[("src/auth.rs", "fn authenticate() { // fixed }")],
                ),
            ],
        );

        // Phase 1: build the index via `--rebuild` (this is the BUG A path).
        // Pre-fix: --rebuild did NOT populate temporal.db.
        // Post-fix: --rebuild calls try_rebuild_temporal_nonfatal (AD-TMP-1).
        let rebuild_out = std::process::Command::new(&bin)
            .args(["search", "--rebuild", "--root", &root_str])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {bin} for --rebuild: {e}"));
        assert!(
            rebuild_out.status.success(),
            "--rebuild must exit 0; got {:?}; stderr={}",
            rebuild_out.status,
            String::from_utf8_lossy(&rebuild_out.stderr)
        );

        // Phase 2: run `--hot` as a subprocess — temporal.db was populated by --rebuild.
        let output = std::process::Command::new(&bin)
            .args(["search", "--hot", "--root", &root_str, "--limit", "5"])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {bin} for --hot: {e}"));

        assert!(
            output.status.success(),
            "--hot after --rebuild must exit 0; got {:?}",
            output.status
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // (a) At least one ranked row must appear on stdout (temporal data was populated).
        assert!(
            !stdout.trim().is_empty(),
            "--hot must print ranked rows to stdout after --rebuild (BUG A BLOCKER, \
             AD-TMP-1); got empty stdout. stderr={stderr:?}"
        );

        // (b) The degradation message must NOT appear on stderr.
        assert!(
            !stderr.contains(NO_TEMPORAL_DATA_MSG),
            "--hot must NOT emit the 'no temporal data' message when --rebuild already \
             populated temporal.db (BUG A BLOCKER); got stderr={stderr:?}"
        );
    }

    // ========================================================================
    // #377 — inert-`--weights` notice at the CLI surface
    // ========================================================================

    /// AC6 (#377, API contract, NEGATIVE): invalid `--weights` MUST error at
    /// parse time on EVERY query shape — pure-lexical, text+--ast, and standalone
    /// --ast. Validation happens in `parse_flags` BEFORE any path dispatch, so the
    /// inert-weights path can never mask a validation error. A valid 3-tuple parses.
    #[test]
    fn test_parse_flags_invalid_weights_errors_on_every_path_ac6() {
        let s = |x: &str| x.to_string();
        // Path-shaping suffixes: pure-lexical (text only), text+--ast, standalone --ast.
        let shapes: [Vec<String>; 3] = [
            vec![s("token")],
            vec![s("token"), s("--ast"), s("try-catch")],
            vec![s("--ast"), s("try-catch")],
        ];
        // Each invalid weights string must be rejected regardless of path shape.
        for bad in ["nan,0,0", "-1,0,0", "inf,0,0", "0.5,0.3"] {
            for shape in &shapes {
                let mut args = vec![s("--weights"), s(bad)];
                args.extend(shape.iter().cloned());
                assert!(
                    parse_flags(&args).is_err(),
                    "AC6: invalid --weights {bad:?} must error at parse time for args {args:?}"
                );
            }
        }
        // Control: a valid 3-tuple parses on each shape.
        for shape in &shapes {
            let mut args = vec![s("--weights"), s("0.8,0.1,0.1")];
            args.extend(shape.iter().cloned());
            let flags = parse_flags(&args).unwrap_or_else(|e| {
                panic!("AC6 control: valid --weights must parse for args {args:?}: {e}")
            });
            assert!(
                flags.weights.is_some(),
                "AC6 control: valid --weights must populate flags.weights for args {args:?}"
            );
        }
    }

    /// Blocking-review fix #1 (CLI): the temporal-standalone dispatch arm
    /// (`--hot`/`--cold`/`--risky`/`--blast-radius` with NO text and NO --ast) must
    /// emit the fully-inert `--weights` notice on stderr — it previously called
    /// `run_temporal_standalone` and silently ignored the flag, the exact bug this
    /// ticket fixes. Driven as a subprocess so real stderr is captured.
    ///
    /// Discriminating (PF-007): the test FAILS if the dispatch-arm guard is removed
    /// (no notice on stderr). Both `--hot --weights` and `--blast-radius --weights`
    /// are covered.
    #[test]
    fn test_temporal_standalone_weights_emits_inert_notice_ac7_fix1() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();

        // Dedicated isolated cache dir so this subprocess test is immune to
        // concurrent SKIM_CACHE_DIR mutations from serial tests (e.g.
        // test_ac13_402_memo_cache_hit_and_miss in walk_tests.rs sets and
        // then drops SKIM_CACHE_DIR in the parent process; if the subprocess
        // inherits that value the cache dir is deleted under it mid-build).
        let cache_tmp = tempfile::TempDir::new().unwrap();
        let cache_dir_str = cache_tmp.path().to_string_lossy().to_string();

        // A git repo with commits so temporal data exists (the arm runs, not an error path).
        create_real_git_repo(
            root,
            &[
                ("feat: add auth", &[("src/auth.rs", "fn authenticate() {}")]),
                ("feat: add parser", &[("src/parser.rs", "fn parse() {}")]),
                (
                    "fix: fix auth",
                    &[("src/auth.rs", "fn authenticate() { // fixed }")],
                ),
            ],
        );

        // (1) --hot --weights (temporal-only standalone): notice on stderr, exit 0.
        let hot = std::process::Command::new(&bin)
            .args([
                "search",
                "--hot",
                "--weights",
                "0.5,0.3,0.2",
                "--root",
                &root_str,
            ])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", &cache_dir_str)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {bin}: {e}"));
        assert!(
            hot.status.success(),
            "--hot --weights must exit 0; stderr={}",
            String::from_utf8_lossy(&hot.stderr)
        );
        let hot_stderr = String::from_utf8_lossy(&hot.stderr);
        assert!(
            hot_stderr.contains("note: --weights"),
            "fix #1: `--hot --weights` MUST emit the inert-weights notice on stderr (the temporal \
             standalone arm previously ignored --weights silently); got stderr={hot_stderr:?}"
        );

        // (2) --blast-radius --weights (blast-only standalone): same notice on stderr.
        let blast = std::process::Command::new(&bin)
            .args([
                "search",
                "--blast-radius",
                "src/auth.rs",
                "--weights",
                "0.5,0.3,0.2",
                "--root",
                &root_str,
            ])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", &cache_dir_str)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn {bin}: {e}"));
        assert!(
            blast.status.success(),
            "--blast-radius --weights must exit 0; stderr={}",
            String::from_utf8_lossy(&blast.stderr)
        );
        let blast_stderr = String::from_utf8_lossy(&blast.stderr);
        assert!(
            blast_stderr.contains("note: --weights"),
            "fix #1: `--blast-radius --weights` (no text/--ast) MUST emit the inert-weights notice \
             on stderr; got stderr={blast_stderr:?}"
        );
    }

    /// AC9 (API contract / JSON purity, NEGATIVE): the inert-weights notice MUST go
    /// to stderr even in --json mode; stdout MUST be valid JSON byte-identical to
    /// the no-weights run. Driven via standalone `--ast --json --weights` (wholly
    /// inert) so the fully-inert notice fires while stdout stays pure JSON.
    #[test]
    fn test_inert_weights_notice_json_purity_ac9() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();

        // One file with a nested loop so `--ast rust-nested-loop` matches.
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join("src/loops.rs"),
            "fn f() { for i in 0..3 { for j in 0..3 { let _ = (i, j); } } }",
        )
        .unwrap();

        let run_json = |extra: &[&str]| {
            let mut args = vec!["search", "--ast", "rust-nested-loop", "--json"];
            args.extend_from_slice(extra);
            args.extend_from_slice(&["--root", &root_str]);
            std::process::Command::new(&bin)
                .args(&args)
                .env("SKIM_DISABLE_ANALYTICS", "1")
                .output()
                .unwrap_or_else(|e| panic!("failed to spawn {bin}: {e}"))
        };

        let base = run_json(&[]);
        assert!(
            base.status.success(),
            "baseline --ast --json must exit 0; stderr={}",
            String::from_utf8_lossy(&base.stderr)
        );
        let base_stdout = String::from_utf8_lossy(&base.stdout).to_string();
        serde_json::from_str::<serde_json::Value>(base_stdout.trim()).unwrap_or_else(|e| {
            panic!("AC9: baseline stdout must be valid JSON: {e}\n{base_stdout}")
        });

        let weighted = run_json(&["--weights", "0.8,0.1,0.1"]);
        assert!(
            weighted.status.success(),
            "--ast --json --weights must exit 0; stderr={}",
            String::from_utf8_lossy(&weighted.stderr)
        );
        let weighted_stdout = String::from_utf8_lossy(&weighted.stdout).to_string();
        let weighted_stderr = String::from_utf8_lossy(&weighted.stderr);

        // (1) stdout must STILL be valid JSON.
        serde_json::from_str::<serde_json::Value>(weighted_stdout.trim()).unwrap_or_else(|e| {
            panic!("AC9: --weights stdout must remain valid JSON: {e}\n{weighted_stdout}")
        });
        // (2) stdout must be byte-identical to the no-weights run.
        assert_eq!(
            weighted_stdout, base_stdout,
            "AC9: --weights must NOT change stdout — the inert notice goes to stderr only"
        );
        // (3) the notice MUST appear on stderr but NOT in stdout JSON.
        assert!(
            weighted_stderr.contains("note: --weights"),
            "AC9: the inert-weights notice must appear on stderr; got stderr={weighted_stderr:?}"
        );
        assert!(
            !weighted_stdout.contains("note: --weights"),
            "AC9: the inert-weights notice must NOT leak into stdout JSON"
        );
    }
}
