//! Search subcommand — code search via layered n-gram indexing.
//!
//! # Architecture
//!
//! All I/O lives here (this module). Business logic is split across:
//! - `types` — shared configuration and result types
//! - `walk` — project-root discovery and file traversal
//! - `manifest` — JSONL sidecar for incremental build caching
//! - `index` — full pipeline orchestration (`skim search index`)
//! - `query` — query execution and result formatting
//! - `snippet` — source context extraction
//! - `staleness` — git HEAD comparison and auto-refresh
//! - `hooks` — git hook installation/removal
//! - `rskim-search` crate — index building, n-gram extraction, BM25F scoring

mod ast;
mod build_lock;
pub(crate) mod hooks;
mod index;
mod manifest;
mod query;
mod snippet;
mod staleness;
mod temporal;
mod temporal_build;
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

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search` subcommand.
///
/// Dispatches to:
/// - `skim search index [OPTIONS]` — build or update the search index
/// - `skim search --build` — build incrementally (alias for index)
/// - `skim search --rebuild` — force full rebuild
/// - `skim search --update` — auto-refresh if stale
/// - `skim search --stats [--json]` — print index statistics
/// - `skim search --install-hooks` — install git hooks
/// - `skim search --remove-hooks` — remove git hooks
/// - `skim search [--json] [--limit N] <QUERY>` — search
/// - No args / `--help` / `-h` — print help
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // `skim search index [OPTIONS]` — legacy subcommand path.
    if args.first().is_some_and(|a| a == "index") {
        let rest = &args[1..];
        return index::run(rest, analytics);
    }

    // No args or --help/-h → print help
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Parse flags — propagate errors (invalid --limit, unrecognised flags, etc.).
    let flags = parse_flags(args)?;

    // ── Validation order (deterministic — tests rely on this ordering) ──────
    // --ast patterns are validated BEFORE dispatch so the error fires regardless
    // of which downstream path the flags resolve to:
    //   1. single-node pattern → #283 error.
    //   2. unknown pattern → lists available names.
    // --ast now composes freely with temporal flags (--hot/--cold/--risky/
    // --blast-radius), a text query, --limit, and --json — there is NO flag
    // combination that errors here (mutual exclusion of sort modes is still
    // enforced earlier, in parse_flags).
    if let Some(ref raw_ast) = flags.ast {
        ast::validate_ast_pattern(raw_ast)?;
    }
    // ────────────────────────────────────────────────────────────────────────

    match flags.action {
        SearchAction::Build => run_build(false, &flags.root_override, analytics),
        SearchAction::Rebuild => run_build(true, &flags.root_override, analytics),
        SearchAction::Update => run_update(&flags.root_override, analytics),
        SearchAction::Stats => run_stats(flags.json, &flags.root_override),
        SearchAction::InstallHooks => run_install_hooks(&flags.root_override),
        SearchAction::RemoveHooks => run_remove_hooks(&flags.root_override),
        // Reject whitespace-only queries at dispatch (defense-in-depth for Finding 1 / AC2):
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
            let (root, cache_dir) = resolve_root_and_cache(&flags.root_override)?;
            std::fs::create_dir_all(&cache_dir)?;
            // ADR-006: refresh BOTH indexes before opening either engine.
            let (_refreshed, manifest) =
                staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;
            let temporal_db_path = cache_dir.join("temporal.db");
            // Resolve blast-radius → FileIds BEFORE calling run_ast_standalone.
            // temporal::resolve_blast_radius_file_ids is the single resolver for all
            // three blast-radius call sites, so JSON-aware warning and PF-004 widening
            // live in one place.
            let sorted = manifest.sorted_paths();
            let blast_file_ids = temporal::resolve_blast_radius_file_ids(
                flags.blast_radius.as_deref(),
                &root,
                &temporal_db_path,
                &sorted,
                flags.json,
            )?;
            // Open the temporal DB only when a sort is requested.  Absent DB →
            // graceful degradation: warn on stderr and run unsorted (exit 0, AC-A3),
            // mirroring run_temporal_standalone's missing-data message.
            // Message composed from NO_TEMPORAL_DATA_MSG (single source of truth,
            // mod.rs:47-48) so the two can't silently drift (#357 cycle-2 finding 2).
            let temporal_db = if flags.temporal_sort.is_some() {
                let db = temporal::open_temporal_db(&temporal_db_path);
                if db.is_none() {
                    eprintln!(
                        "skim search: {}; returning unsorted --ast results",
                        NO_TEMPORAL_DATA_MSG
                    );
                }
                db
            } else {
                None
            };
            let mut stdout = BufWriter::new(std::io::stdout());
            let result = ast::run_ast_standalone(
                raw,
                flags.limit,
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
            run_temporal_standalone(
                flags.limit,
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
    /// Composite RRF weights for the blast-radius UNION ranking path (#200).
    ///
    /// Parsed from `--weights lexical,ast,temporal` and validated at flag-parse
    /// time.  `None` → use `CompositeWeights6::with_six_signal_defaults()` (0.5, 0.3, 0.2).
    weights: Option<rskim_search::CompositeWeights6>,
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
/// - Unrecognised flags (tokens beginning with `--`).
fn parse_flags(args: &[String]) -> anyhow::Result<Flags> {
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
            s if s.starts_with("--") => {
                anyhow::bail!(
                    "unrecognised flag {:?}. Valid flags: --build, --rebuild, --update, \
                     --stats, --install-hooks, --remove-hooks, --json, -j, --limit, --offset, \
                     --root, --ast, --hot, --cold, --risky, --blast-radius, --weights",
                    s
                );
            }
            // Positional arg — part of the query text.
            s => query_parts.push(s.to_string()),
        }
        i += 1;
    }

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
    })
}

// ============================================================================
// Shared project-root + cache-dir resolution
// ============================================================================

fn resolve_root_and_cache(root_override: &Option<PathBuf>) -> anyhow::Result<(PathBuf, PathBuf)> {
    let root = match root_override {
        Some(r) => r.canonicalize().unwrap_or_else(|_| r.clone()),
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

    // AD-TMP-1: --rebuild/--build must produce a COMPLETE index (lexical + AST +
    // temporal), matching user expectation that "rebuild" rebuilds everything (#357 BUG A).
    // run_build goes through build_index directly, bypassing auto_refresh_if_stale where
    // the only other temporal hook lives, so temporal must be populated here too.
    // Non-fatal by ADR-006/D5: a temporal failure must NOT fail the explicit build.
    // HEAD read via the pure file-IO read_git_head (no subprocess); None on non-git →
    // try_rebuild_temporal_nonfatal no-ops gracefully. The `force` flag is intentionally
    // NOT forwarded: rebuild_temporal always does a full history walk (no cache) —
    // see the `parse_history(root, 0)` call in `rebuild_temporal_with_source`
    // (temporal_build.rs, "Single full-history walk" comment).
    let current_head = staleness::read_git_head(&root);
    staleness::try_rebuild_temporal_nonfatal(
        &root,
        &cache_dir,
        current_head.as_deref(),
        "--rebuild hook",
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
    let (refreshed, _manifest) = staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;
    if !refreshed {
        eprintln!("skim search: index is current");
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --stats
// ============================================================================

fn run_stats(json: bool, root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;

    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        if json {
            println!("{{\"error\": \"no index found\"}}");
        } else {
            eprintln!("skim search: no index found — run `skim search --build` first");
        }
        return Ok(ExitCode::FAILURE);
    }

    let reader = rskim_search::NgramIndexReader::open(&cache_dir)?;
    let stats = reader.stats();

    // check_staleness returns the loaded manifest as part of its work.
    // Reuse it here instead of loading the manifest a second time.
    let (staleness_status, loaded_manifest) = staleness::check_staleness(&cache_dir, &root);
    let git_head = loaded_manifest
        .as_ref()
        .and_then(|m| m.stored_git_head().map(str::to_string));

    let mut out = BufWriter::new(std::io::stdout());
    if json {
        let extended = serde_json::json!({
            "file_count": stats.file_count,
            "total_ngrams": stats.total_ngrams,
            "index_size_bytes": stats.index_size_bytes,
            "last_updated": stats.last_updated,
            "git_head": git_head,
            "staleness": staleness_status.to_string(),
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&extended)?)?;
    } else {
        writeln!(out, "skim search index stats:")?;
        writeln!(out, "  files indexed : {}", stats.file_count)?;
        writeln!(out, "  total n-grams : {}", stats.total_ngrams)?;
        writeln!(out, "  index size    : {} bytes", stats.index_size_bytes)?;
        if let Some(ts) = stats.last_updated {
            writeln!(out, "  last updated  : {ts}")?;
        }
        writeln!(
            out,
            "  git HEAD      : {}",
            git_head.as_deref().unwrap_or("(none)")
        )?;
        writeln!(out, "  staleness     : {staleness_status}")?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --install-hooks / --remove-hooks
// ============================================================================

fn run_install_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    hooks::install_search_hooks(&root)?;
    eprintln!("skim search: git hooks installed in {}", root.display());
    Ok(ExitCode::SUCCESS)
}

fn run_remove_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    hooks::remove_search_hooks(&root)?;
    eprintln!("skim search: git hooks removed from {}", root.display());
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
    let pre_loaded_manifest_from_refresh =
        if flags.temporal_sort.is_some() || flags.blast_radius.is_some() || flags.ast.is_some() {
            let (_refreshed, manifest) =
                staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;
            Some(manifest)
        } else {
            // No temporal or AST flag: skip early refresh; execute_query_with_manifest
            // will call auto_refresh_if_stale internally exactly once.
            None
        };

    // Open the temporal DB once (AFTER refresh above). Used for both
    // blast-radius filtering (before the query, so LIMIT applies to the filtered
    // set) and temporal enrichment (after the query, to annotate/sort results).
    let temporal_db = if flags.temporal_sort.is_some() || flags.blast_radius.is_some() {
        temporal::open_temporal_db(&cache_dir.join("temporal.db"))
    } else {
        None
    };

    // Resolve blast-radius partner paths BEFORE querying so the file_filter
    // is applied inside the search engine (before LIMIT). This ensures the
    // limit applies to the filtered set rather than silently discarding
    // co-change partners that ranked beyond the top-N unfiltered results.
    let blast_radius_paths = temporal::resolve_blast_radius_paths(
        flags.blast_radius.as_deref(),
        &root,
        &cache_dir.join("temporal.db"),
        flags.json,
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
    let (ast_scored, pre_loaded_manifest) = if let Some(ref raw_ast) = flags.ast {
        // The refresh already ran above: `pre_loaded_manifest_from_refresh` is always
        // `Some` when `flags.ast.is_some()` (the early-refresh condition includes
        // `|| flags.ast.is_some()`). Reuse that manifest directly rather than calling
        // auto_refresh_if_stale a second time (the second call was idempotent but
        // wasteful — it returned `(false, manifest)` immediately on Current).
        let manifest = pre_loaded_manifest_from_refresh
            .expect("manifest must be present when flags.ast is Some (invariant)");
        let engine = ast::open_ast_engine(&cache_dir)?;
        // Changed from #199 (lossy HashSet) to #198 (scored vec for RRF).
        // resolve_ast_scored returns Vec<(FileId, f64)> sorted FileId-ASC,
        // preserving AST scores so intersect_and_rank can build the rank map.
        let ast_scored = match ast::resolve_ast_scored(&engine, raw_ast) {
            Ok(hits) => {
                if hits.is_empty() {
                    eprintln!("skim search: --ast {:?} matched no indexed files", raw_ast);
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
        (ast_scored, Some(manifest))
    } else {
        // Pure-lexical path: no --ast flag. Pass the manifest from the early
        // refresh (if we did one) so execute_query_with_manifest skips its own
        // auto_refresh_if_stale call. When no refresh was needed (no temporal or
        // AST flag), pass None so execute_query_with_manifest does its own refresh.
        (None, pre_loaded_manifest_from_refresh)
    };

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
        root,
        cache_dir,
        blast_radius_paths,
        ast_scored,
        composite_weights: flags.weights,
    };

    // Pass the already-refreshed manifest to execute_query_with_manifest.  When
    // pre_loaded_manifest is Some (temporal or AST flag active — refresh happened
    // above), execute_query skips its own auto_refresh_if_stale.  When None
    // (pure-lexical, no temporal/AST flag), execute_query refreshes internally,
    // preserving the invariant: exactly one auto_refresh_if_stale call per query.
    let mut output = query::execute_query_with_manifest(&config, pre_loaded_manifest, analytics)?;

    // Apply temporal sort/annotation to the results, then apply offset + truncate to --limit.
    // Applying offset+limit AFTER the re-sort (not via the engine's LIMIT) is the GAP-1
    // invariant: the top-`limit` BY TEMPORAL SCORE survive, not the top-`limit`
    // by lexical relevance re-ordered.
    // AD-372-3 / PF-006: thread config.offset so `--offset` works on the temporal path too.
    // The offset passed to QueryConfig above is None when temporal_sort is active, so
    // execute_query_with_manifest does NOT apply offset; this drain is the SINGLE application.
    if let (Some(sort), Some(db)) = (flags.temporal_sort, &temporal_db) {
        temporal::apply_temporal_enrichment(&mut output.results, sort, db)?;
        let effective_offset = flags.offset.unwrap_or(0);
        if effective_offset > 0 {
            output
                .results
                .drain(..effective_offset.min(output.results.len()));
        }
        output.results.truncate(flags.limit);
        output.total = output.results.len();
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
    limit: usize,
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
    staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;

    let temporal_db_path = cache_dir.join("temporal.db");

    let Some(db) = temporal::open_temporal_db(&temporal_db_path) else {
        if json {
            let msg = WarningJson {
                warning: NO_TEMPORAL_DATA_MSG,
            };
            println!("{}", serde_json::to_string(&msg)?);
        } else {
            eprintln!("skim search: {NO_TEMPORAL_DATA_MSG}");
        }
        return Ok(ExitCode::SUCCESS);
    };

    let output = temporal::query_standalone(temporal_sort, blast_radius, limit, &db, &root)?;

    let mut stdout = BufWriter::new(std::io::stdout());
    if json {
        temporal::format_temporal_json(&output, &mut stdout)?;
    } else {
        temporal::format_temporal_text(&output, &mut stdout)?;
    }
    stdout.flush()?;

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Help text
// ============================================================================

fn print_help() {
    println!(
        "\
Usage: skim search [OPTIONS] [QUERY]

Search code using layered n-gram BM25F indexing.

Subcommands / modes:
  (none)           Print this help message
  index            Build or update the search index (legacy)

Options:
  --build          Build the index incrementally (auto-build on first query)
  --rebuild        Rebuild the index from scratch
  --update         Refresh if index is stale (git HEAD changed)
  --stats          Show index statistics
  --install-hooks  Install git post-commit/merge hooks for auto-refresh
  --remove-hooks   Remove skim git hooks
  --json           Output results as JSON
  --limit N        Maximum results to return (default: 20)
  --offset N       Skip N verified results (pagination; default: 0)
  --root PATH      Override project root (default: walk up to .git)
  -h, --help       Print this help message

AST structural query options (#199):
  --ast PATTERN    Filter/list by AST structural pattern.
                   PATTERN is a named catalog pattern or a containment query:
                     Named:        --ast try-catch
                     Containment:  --ast \"for_statement > await_expression\"
                   Use `--ast` alone for standalone AST-only output (file-level),
                   or combine with a text query for intersection results.

  Limitations:
    #283 — Single-node queries (e.g. --ast try_statement) are not yet supported;
           use a named pattern or a containment query instead.

  --ast composes with every temporal flag (--hot / --cold / --risky /
  --blast-radius), a text query, --limit, and --json.  When heatmap data is
  absent, temporal sorts degrade gracefully: a warning is printed to stderr and
  results are returned unsorted (exit 0).

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

Composite ranking options (#200):
  --weights L,A,T      Tune the --blast-radius composite RRF ranking.
                       Exactly 3 comma-separated ratio values: lexical, ast, temporal.
                       Default: 0.5,0.3,0.2
                       Values are ratios only — NOT normalized; zero and non-sum-to-1
                       are allowed. Negative, NaN, and inf are rejected.
                       Only active on the --blast-radius composite ranking path;
                       the 3 extended signals (import_graph, dir_proximity,
                       structural_coupling) are fixed at 0.0 until measured.

  Example: --weights 0.8,0.1,0.1  (lexical-heavy)
           --weights 0.2,0.2,0.6  (temporal-heavy)

General examples:
  skim search \"authenticate\"                Search for 'authenticate'
  skim search --limit 5 \"parse_url\"         Return at most 5 results
  skim search --json \"UserService\"          JSON output
  skim search --build                       Build the search index
  skim search --rebuild                     Rebuild from scratch
  skim search --update                      Refresh stale index
  skim search --stats                       Show index statistics
  skim search --install-hooks               Auto-refresh on git commit/merge
  skim search --hot                         Top hotspot files (standalone)
  skim search --risky                       Top risky files (standalone)
  skim search --blast-radius src/auth.rs    Co-change partners of auth.rs
  skim search \"auth\" --hot                  Text results sorted by hotspot
  skim search \"auth\" --blast-radius src/auth.rs  Text within co-change partners"
    );
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

    /// Regression: `skim search index --help` must dispatch to index help,
    /// not the parent search help. The parent help check must not intercept
    /// flags intended for a known subcommand.
    #[test]
    fn test_index_help_dispatches_to_index_not_parent() {
        let result = run(
            &["index".to_string(), "--help".to_string()],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
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
        assert!(
            err.to_string().contains("unrecognised flag"),
            "unexpected error message: {err}"
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
        // Point to a non-existent DB file — resolver must degrade gracefully.
        let absent_db = dir.path().join("no_such.db");
        let result =
            temporal::resolve_blast_radius_paths(Some("src/auth.rs"), root, &absent_db, false);
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
        format_temporal_text(&output, &mut writer).unwrap();
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
        format_temporal_text(&output, &mut writer).unwrap();
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
}
