//! `skim search index` — pipeline orchestration for the index builder.
//!
//! # Data flow
//!
//! **Streaming build**:
//! 1. `discover_project_root(cwd)` → walk up to `.git`, fall back to cwd
//! 2. Resolve cache dir: `~/.cache/skim/search/{sha256(canonical_root)[..16]}/`
//! 3. `walk_metadata(root, max_files)` → metadata-only WalkEntry list (sorted)
//! 4. Producer thread: for each entry, reads content, computes SHA-256, applies
//!    2-tier SHA cache, classifies; sends ProcessedFile on bounded channel
//! 5. Consumer thread: receives ProcessedFile, calls add_file_classified, inserts
//!    manifest entry, drops content → peak memory bounded by channel capacity
//! 6. `builder.build()` flushes index; manifest written after (marks coherence)
//! 7. Print summary to stderr
//!
//! **Incremental build** (manifest exists, no `--force`):
//! - SHA-256 match → reuse cached field_map (cache hit, no classify_source call).
//! - SHA-256 mismatch → classify_source and write fresh field_map (cache miss).
//! - Mtime is stored in the manifest for potential future aggressive-mode
//!   optimisation (skip SHA on mtime match) but is not consulted for cache
//!   decisions in the current safe mode — SHA is the sole authority.
//! - Always write a fresh manifest after build.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use clap::Parser;
use rskim_search::{
    AstIndexBuilder, AstNgramCache, CachedAstEntry, FileId, LayerBuilder, NgramIndexBuilder,
    classify_source, extract_ast_ngrams_with_metrics, linearize_source,
};

use super::manifest::{FileManifest, ManifestEntry, decode_field_map, encode_field_map};
use super::staleness::read_git_head;
use super::types::{IndexConfig, IndexResult, ProcessedFile, SkipReason, WalkEntry};
use super::walk::{
    ReadOutcome, discover_project_root, is_minified, normalize_rel_path, open_and_read, sha256_hex,
    walk_metadata,
};

// ============================================================================
// Public entry point
// ============================================================================

/// Run the index builder.
///
/// Accepted flags:
/// - `--root=<PATH>` or `--root <PATH>` — explicit project root (default: cwd)
/// - `--force` — skip manifest cache and re-classify every file
/// - `--max-files=<N>` — override the 50,000 file cap (must be ≥ 1)
/// - `-h` / `--help` — print help text and exit
///
/// # AD-375-2 — `index::run` / `IndexCli` are retained, not deleted (applies ADR-001).
///
/// As of #375, `skim search index` as a positional subcommand was removed —
/// `index` is now treated as a query term, not a build trigger.  This function
/// is therefore no longer reachable from the `search::run` dispatcher.  It is
/// intentionally kept because:
///
/// 1. **`index_tests.rs` calls it directly** (`use super::run`) — deleting this
///    function or `IndexCli` would fail to compile the 37 builder tests.
/// 2. **`run_build`** (in `mod.rs`) delegates to `build_index()` (defined below),
///    which is the same build pipeline — `index::run` is the test seam for that
///    pipeline.
///
/// Do NOT delete this function or `IndexCli` as "dead code" — it is the primary
/// test entry point for the build pipeline.
///
/// # Errors
///
/// Returns `Err` only for fatal I/O failures. User-facing errors (unsupported
/// languages, too-large files) are counted and reported to stderr but do not
/// cause a non-zero exit code.
// AD-375-2: clippy's dead_code lint fires here because the only non-test caller
// (the `search::run` positional intercept) was removed by #375.  The function is
// live from `index_tests.rs` (`use super::run`) but that is a #[cfg(test)] module,
// which clippy-with-dead_code does not count as a live caller.  We suppress rather
// than delete (see the rustdoc above).
#[allow(dead_code)]
pub(super) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let cli = match IndexCli::try_parse_from(
        std::iter::once(&"skim search index".to_string()).chain(args),
    ) {
        Ok(cli) => cli,
        Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => {
            // `--help` / `-h` — clap already printed the help text to stdout.
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };

    let config = cli.into_config()?;
    let result = build_index(&config)?;

    eprintln!(
        "skim search: indexed {} files ({} skipped, {} field-map hits, \
         {} AST reused, {} AST re-extracted) in {:.1}s",
        result.file_count,
        result.skipped,
        result.cache_hits,
        result.ast_cache_hits,
        result.ast_reextracted,
        result.duration.as_secs_f64(),
    );

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Argument parsing (clap derive)
// ============================================================================

/// CLI arguments for `skim search index`.
#[derive(Parser, Debug)]
#[command(
    name = "skim search index",
    about = "Build or update the search index for the current project.",
    long_about = None,
    disable_version_flag = true,
)]
struct IndexCli {
    /// Project root to index (default: auto-discover via .git)
    #[arg(long)]
    root: Option<PathBuf>,

    /// Rebuild from scratch, ignoring the manifest cache
    #[arg(long)]
    force: bool,

    /// Maximum files to index (default: 50000; must be ≥ 1)
    #[arg(long, value_parser = parse_positive_usize)]
    max_files: Option<usize>,

    /// Internal/test flag: override the cache directory
    #[arg(long, hide = true)]
    index_dir: Option<PathBuf>,
}

impl IndexCli {
    // AD-375-2: same suppression as `run` above — used by index_tests.rs via
    // IndexCli::try_parse_from + into_config, invisible to dead_code lint.
    #[allow(dead_code)]
    fn into_config(self) -> anyhow::Result<IndexConfig> {
        let effective_root = match self.root {
            Some(r) => r.canonicalize().unwrap_or(r),
            None => {
                let cwd = std::env::current_dir()?;
                discover_project_root(&cwd)?
            }
        };
        Ok(IndexConfig {
            root: effective_root,
            max_files: self.max_files,
            force: self.force,
            cache_dir_override: self.index_dir,
        })
    }
}

/// Value parser that rejects zero — `--max-files` must be ≥ 1.
fn parse_positive_usize(s: &str) -> Result<usize, String> {
    let n = s
        .parse::<usize>()
        .map_err(|_| "--max-files requires a positive integer".to_string())?;
    if n == 0 {
        return Err("--max-files must be ≥ 1 (zero produces an empty index)".to_string());
    }
    Ok(n)
}

/// Execute the full build or incremental build pipeline.
///
/// Returns an [`IndexResult`] with counts and duration. Callers that need only
/// an exit code (e.g. [`run`]) wrap this; tests that need to inspect counts
/// call it directly.
///
/// # Concurrency
///
/// Acquires the shared advisory build lock via [`super::build_lock::acquire`]
/// before running the pipeline. Both `build_index` (this function) and
/// `rebuild_temporal` use the SAME lock so concurrent skim processes serialise
/// correctly against both the lexical/AST write and the temporal write. The
/// lock polls every 200 ms for up to 120 s, prints a one-time notice, then
/// returns an error if the deadline expires.
///
/// The lock is released when the returned [`IndexResult`] (or the `Err`) drops,
/// i.e. at the end of this function. The lock file itself is never deleted so
/// the OS can reuse it across processes.
pub(super) fn build_index(config: &IndexConfig) -> anyhow::Result<IndexResult> {
    let pipeline = Pipeline::new(config)?;

    // Acquire the shared advisory build lock. The single bounded implementation
    // lives in `build_lock::acquire` so that both the lexical build (here) and
    // the temporal rebuild (called from staleness.rs) use ONE lock loop with
    // consistent wait message and deadline. The lock is held for the duration of
    // `pipeline.run()` and released when `_lock` drops at function end.
    // (applies ADR-006: serialises concurrent skim processes)
    let _lock = super::build_lock::acquire("skim search index", &pipeline.cache_dir)?;

    pipeline.run()
}

/// Build the index, but re-check staleness AFTER acquiring the build lock and
/// SKIP the pipeline if `still_stale()` returns `false` (AD-379-8).
///
/// # Stampede collapse
///
/// Several concurrent `skim search` processes that all observe a dirty working
/// tree will queue on the advisory build lock. Without this re-check each would
/// rebuild in turn (a thundering herd). Here the FIRST waiter to acquire the
/// lock rebuilds; every subsequent waiter, upon acquiring the lock, calls
/// `still_stale()` — which re-runs the cheap staleness check against the
/// now-refreshed manifest — observes a Current index, and returns `Ok(None)`
/// WITHOUT running a second pipeline. This collapses N rebuilds into one.
///
/// The predicate is evaluated INSIDE the lock (after acquisition, before the
/// pipeline) so the re-check observes the committed state of any peer that
/// rebuilt before us. Acquiring the lock here and delegating to `pipeline.run()`
/// (which does NOT re-acquire) keeps a single lock hold for the whole critical
/// section — re-entering `build_index` would self-block on the advisory lock.
///
/// Returns `Ok(Some(result))` when a build ran, `Ok(None)` when it was skipped
/// because a peer already refreshed the index.
pub(super) fn build_index_rechecked(
    config: &IndexConfig,
    still_stale: impl FnOnce() -> bool,
) -> anyhow::Result<Option<IndexResult>> {
    let pipeline = Pipeline::new(config)?;

    // Single lock hold for the whole critical section (re-check + build).
    let _lock = super::build_lock::acquire("skim search index", &pipeline.cache_dir)?;

    // Post-lock re-check (AD-379-8): a peer may have rebuilt while we waited.
    if !still_stale() {
        return Ok(None);
    }

    pipeline.run().map(Some)
}

/// Orchestrates the index build pipeline as discrete, testable stages.
///
/// `run()` implements a bounded-channel streaming design:
/// - A producer thread walks + reads files, sending [`ProcessedFile`]s.
/// - The consumer (main thread) receives, indexes, and immediately drops content.
///
/// Peak memory is bounded by `CHANNEL_CAPACITY × avg_file_size` rather than
/// the total project size.
pub(super) struct Pipeline<'cfg> {
    config: &'cfg IndexConfig,
    cache_dir: PathBuf,
    start: Instant,
}

/// Bounded channel capacity: at most this many `ProcessedFile`s buffered in flight.
///
/// 64 × 5 MiB max file size = 320 MiB worst-case buffered in the channel.
const CHANNEL_CAPACITY: usize = 64;

/// Aggregated output from [`Pipeline::consume`].
#[derive(Debug)]
pub(super) struct ConsumeResult {
    /// Number of files successfully added to the index.
    pub(super) file_count: u32,
    /// Number of files whose cached `field_map` was reused (SHA match).
    pub(super) cache_hits: u32,
    /// Number of files whose AST n-grams were served from `ast_index.skcache`.
    pub(super) ast_cache_hits: u32,
    /// Number of files whose AST n-grams were freshly extracted.
    pub(super) ast_reextracted: u32,
}

impl<'cfg> Pipeline<'cfg> {
    /// Initialise the pipeline: resolve the cache directory and create it.
    ///
    /// # Errors
    ///
    /// Returns `Err` on I/O failure resolving or creating the cache directory.
    pub(super) fn new(config: &'cfg IndexConfig) -> anyhow::Result<Self> {
        let cache_dir = match &config.cache_dir_override {
            Some(dir) => dir.clone(),
            None => resolve_search_cache_dir(&config.root)?,
        };
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            config,
            cache_dir,
            start: Instant::now(),
        })
    }

    /// Run the streaming pipeline and return the final [`IndexResult`].
    ///
    /// Orchestrates three stages:
    /// 1. [`Self::walk`] — metadata-only directory walk.
    /// 2. [`Self::spawn_producer`] — producer thread that reads + classifies files.
    /// 3. [`Self::consume`] — consumer loop that indexes and builds the manifest.
    pub(super) fn run(self) -> anyhow::Result<IndexResult> {
        let debug_enabled = crate::debug::is_debug_enabled();

        // Stage 1: Metadata-only walk (no content reading).
        let (walk_entries, walk_skip_count) = self.walk()?;

        if walk_entries.is_empty() {
            return self.flush_empty(walk_skip_count);
        }

        // Stage 2: Load the manifest and the AST cache for incremental builds,
        // then spawn producer.
        let manifest = self.load_manifest()?;
        // Load the prior AST n-gram cache.  On --force, skip the cache entirely
        // (--force must re-extract everything, AC11).
        // `with_dir` on the force path creates an empty cache that knows where
        // to write its skcache — the same pattern as `FileManifest::new`.
        let ast_cache = if self.config.force {
            AstNgramCache::with_dir(&self.cache_dir)
        } else {
            AstNgramCache::load(&self.cache_dir)
        };
        let (producer_handle, rx, producer_skips) = Self::spawn_producer(
            walk_entries,
            manifest,
            ast_cache,
            self.config.force,
            debug_enabled,
        );

        // Stage 3: Consume processed files, build lexical + AST indexes.
        let mut builder = NgramIndexBuilder::new(self.cache_dir.clone())?;
        // AST index (#199 + #290): build alongside lexical so both share the same
        // FileId sequence (correctness-critical — see FileId contract in
        // ast_index/builder.rs).  Incremental cache (#290): unchanged files reuse
        // their cached AstNgramSet from ast_index.skcache instead of re-extracting.
        // NOTE: both builders retain posting lists until build(); memory scales with
        // file count (~tens of MB at 10k files) — tracked in #273 for chunked builds.
        let mut ast_builder = AstIndexBuilder::new(self.cache_dir.clone())
            .map_err(|e| anyhow::anyhow!("failed to create AST index builder: {e}"))?;
        let mut new_manifest = FileManifest::new(self.config.root.clone(), self.cache_dir.clone());
        // Capture consume's result rather than propagating with `?` immediately.
        // We MUST join the producer before propagating any error so that a worker-thread
        // panic is surfaced on BOTH the success path AND the ADR-006 abort path.
        // On the abort path, `rx` is consumed (dropped inside `consume`) before we reach
        // the join, so the producer's `tx.send()` has already returned `Err` and the
        // producer thread has already exited — no deadlock risk. (applies ADR-006)
        let mut new_ast_cache = AstNgramCache::with_dir(&self.cache_dir);
        let consume_result = Self::consume(
            &mut builder,
            &mut ast_builder,
            &mut new_manifest,
            &mut new_ast_cache,
            rx,
            debug_enabled,
        );

        // Always join the producer first so a worker-thread panic is surfaced
        // regardless of whether consume succeeded or aborted (ADR-006 desync).
        producer_handle.join().map_err(|e| {
            anyhow::anyhow!(
                "producer thread panicked: {:?}",
                e.downcast_ref::<String>()
                    .map(String::as_str)
                    .unwrap_or("<non-string panic>")
            )
        })?;

        // Now propagate the consume error (if any) — producer is already joined.
        let ConsumeResult {
            file_count,
            cache_hits,
            ast_cache_hits,
            ast_reextracted,
        } = consume_result?;

        // Commit ordering (crash-safety):
        // (1) Lexical build — index.skidx + index.skfiles written.
        // (2) AST build    — ast_index.skpost then ast_index.skidx written.
        // (3) Manifest save (git HEAD recorded) — the commit point.
        //
        // If the AST build fails, the manifest is NOT saved so the next query
        // sees the index as stale and triggers a full rebuild (self-heal path).
        // "HEAD recorded ⟹ both indexes coherent" is the invariant.
        // Commit-boundary invariant: both builders and the manifest must agree on
        // the file count before we write anything to disk. A mismatch here means
        // the "every file gets exactly one call" contract was broken somewhere in
        // the consume loop. Abort before any write so the old manifest survives
        // and the next query self-heals. (applies ADR-006)
        //
        // Why the comparison holds for realistic projects: `manifest_count` is the
        // number of unique BTreeMap keys (normalized rel-path strings produced by
        // normalize_rel_path), and `file_count` is the number of successful
        // `add_file_classified` calls. They agree when every successfully-indexed
        // file has a distinct normalized path key — the invariant upheld by
        // `walk_metadata`'s sort (AD-373-1: byte-wise normalized-string order,
        // matching the manifest BTreeMap<String> resolution side exactly).
        // Dedup is implicit in BTreeMap::insert (last writer wins); the walker
        // does NOT dedup entries — a duplicate walk entry would silently collapse
        // to one BTreeMap key, causing manifest_count < file_count and triggering
        // this guard. A mismatch would require two walk entries to normalize to the
        // same path key, which cannot happen on case-sensitive file-systems (two
        // distinct paths ⇒ two distinct keys) and is a data-corruption signal on
        // case-insensitive ones; hence this guard is intentionally defensive.
        let manifest_count = new_manifest.entry_count();
        if manifest_count != file_count as usize {
            return Err(anyhow::anyhow!(
                "index commit aborted: manifest entry count ({manifest_count}) != \
                 consume file count ({file_count}); FileId alignment is broken — \
                 the old manifest survives and the next query will trigger a full rebuild"
            ));
        }

        let _layer = builder.build()?;
        ast_builder
            .build()
            .map_err(|e| anyhow::anyhow!("AST index build failed: {e}"))?;

        // Commit ordering (applies ADR-006):
        // Write the AST n-gram cache AFTER ast_builder.build() and BEFORE
        // new_manifest.save().  A write failure here returns Err so the
        // manifest is never saved — the next query self-heals via full rebuild.
        // The skcache is advisory: a stale entry cannot be served because the
        // manifest SHA is the sole cache-key authority; a skcache entry present
        // without a matching manifest SHA is simply unused on the next build.
        new_ast_cache
            .save()
            .map_err(|e| anyhow::anyhow!("AST cache save failed: {e}"))?;

        // Record the current git HEAD in the manifest so staleness detection
        // can compare it on the next query without spawning a git subprocess.
        new_manifest.set_git_head(read_git_head(&self.config.root));
        new_manifest.save()?;

        // `producer_handle.join()` above is the happens-before edge: the atomic
        // load below is only valid after join() returns. Moving this load before
        // the join would make `producer_skips` racy.
        let total_skipped =
            to_u32_capped(walk_skip_count).saturating_add(producer_skips.load(Ordering::Relaxed));

        Ok(IndexResult {
            file_count,
            skipped: total_skipped,
            cache_hits,
            ast_cache_hits,
            ast_reextracted,
            duration: self.start.elapsed(),
        })
    }

    /// Stage 1: walk the project root and return `(entries, skip_count)`.
    fn walk(&self) -> anyhow::Result<(Vec<WalkEntry>, usize)> {
        let (entries, skips) = walk_metadata(&self.config.root, self.config.effective_max_files())?;
        Ok((entries, skips.len()))
    }

    /// Stage 2a: load or create the [`FileManifest`] based on `--force`.
    fn load_manifest(&self) -> anyhow::Result<FileManifest> {
        if self.config.force {
            Ok(FileManifest::new(
                self.config.root.clone(),
                self.cache_dir.clone(),
            ))
        } else {
            FileManifest::load(self.config.root.clone(), self.cache_dir.clone())
        }
    }

    /// Flush empty lexical + AST indexes, an empty skcache, and an empty manifest
    /// when the project has no indexable files.
    ///
    /// Called when `walk_entries` is empty so that `check_staleness` can find
    /// `index.skidx` and treat the project as indexed (rather than returning
    /// `NoIndex` on every query).  Writing an empty skcache preserves the
    /// "self-pruning, rebuilt from scratch each build" invariant — without it, a
    /// stale skcache from a prior non-empty build would persist on disk.
    fn flush_empty(self, walk_skip_count: usize) -> anyhow::Result<IndexResult> {
        let builder = NgramIndexBuilder::new(self.cache_dir.clone())?;
        let _layer = builder.build()?;
        // Build an empty AST index so self-heal doesn't trigger immediately.
        let ast_builder = AstIndexBuilder::new(self.cache_dir.clone())
            .map_err(|e| anyhow::anyhow!("failed to create AST index builder: {e}"))?;
        ast_builder
            .build()
            .map_err(|e| anyhow::anyhow!("AST index build failed: {e}"))?;
        // Write an empty skcache to maintain the self-pruning invariant.
        AstNgramCache::with_dir(&self.cache_dir)
            .save()
            .map_err(|e| anyhow::anyhow!("AST cache save failed: {e}"))?;
        let mut manifest = FileManifest::new(self.config.root.clone(), self.cache_dir.clone());
        manifest.set_git_head(read_git_head(&self.config.root));
        manifest.save()?;
        Ok(IndexResult {
            file_count: 0,
            skipped: to_u32_capped(walk_skip_count),
            cache_hits: 0,
            ast_cache_hits: 0,
            ast_reextracted: 0,
            duration: self.start.elapsed(),
        })
    }

    /// Stage 2b: spawn the producer thread.
    ///
    /// Returns a join handle, the receiving end of the bounded channel, and a
    /// shared skip counter that the producer increments on read/classify errors.
    ///
    /// `ast_cache` is moved into the producer thread and consulted for each
    /// file: a SHA match in the cache attaches the cached payload to the
    /// `ProcessedFile.ast_cached` field so the consumer skips `derive_ast_entry`.
    fn spawn_producer(
        walk_entries: Vec<WalkEntry>,
        manifest: FileManifest,
        ast_cache: AstNgramCache,
        force: bool,
        debug_enabled: bool,
    ) -> (
        std::thread::JoinHandle<()>,
        crossbeam_channel::Receiver<ProcessedFile>,
        Arc<AtomicU32>,
    ) {
        let (tx, rx) = crossbeam_channel::bounded::<ProcessedFile>(CHANNEL_CAPACITY);
        let producer_skips = Arc::new(AtomicU32::new(0));
        let skips = Arc::clone(&producer_skips);

        // `walk_entries`, `manifest`, and `ast_cache` are moved into the producer
        // thread.  All three must be `Send`; the compiler enforces this.
        let handle = std::thread::spawn(move || {
            for entry in &walk_entries {
                match read_and_classify(entry, &manifest, &ast_cache, force, debug_enabled) {
                    Ok(pf) => {
                        // Send blocks when channel is full — backpressure limits peak memory.
                        if tx.send(pf).is_err() {
                            // Consumer dropped receiver (fatal error on consumer side).
                            break;
                        }
                    }
                    Err(_reason) => {
                        skips.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            // `tx` dropped here closes the channel, signalling EOF to consumer.
        });

        (handle, rx, producer_skips)
    }

    /// Stage 3: consume [`ProcessedFile`]s from `rx`, index each one in BOTH
    /// the lexical and AST indexes, and build the new manifest.
    ///
    /// Returns aggregated counts. Per-file *lexical* errors are fail-soft: the
    /// file is skipped from BOTH indexes and indexing continues. On `FileId`
    /// overflow the loop breaks early and the partial result is returned — the
    /// caller's `builder.build()` + `ast_builder.build()` + `new_manifest.save()`
    /// still execute, preserving the fail-soft contract.
    ///
    /// # Errors
    ///
    /// Returns `Err` only when `add_file_ngrams` rejects an AST entry *after* the
    /// lexical entry for the same `FileId` was already accepted. That can only
    /// happen if the FileId-alignment invariant is already broken (e.g. a future
    /// regression in `extract_ast_ngrams` emitting a zero-count n-gram), at which
    /// point the two indexes are unrecoverably desynced. Aborting here propagates
    /// up through `run()` so `new_manifest.save()` is NEVER reached — the old
    /// manifest survives and the next query self-heals via a full rebuild. We do
    /// NOT silently `continue`, which would advance `next_file_id` and cascade the
    /// desync into a CRC-valid but corrupt index that gets committed.
    ///
    /// # FileId Invariants
    ///
    /// 1. `next_file_id` only advances after a successful `add_file_classified`.
    ///    A lexical-builder error causes a `continue` — the file is excluded from
    ///    BOTH indexes, keeping them in sync.
    /// 2. AST entries are ALWAYS inserted (even on linearization error or cache
    ///    hit) via exactly one `add_file_ngrams` call per file. This preserves
    ///    the AST builder's "every file gets exactly one call" contract and prevents
    ///    FileId desync between the lexical and AST indexes.
    /// 3. `new_ast_cache` accumulates payloads for all files in this build (hits
    ///    re-inserted from the prior cache, misses inserted after extraction).
    ///    The caller writes it atomically after `ast_builder.build()` and before
    ///    `new_manifest.save()`. (applies ADR-006)
    pub(super) fn consume(
        builder: &mut NgramIndexBuilder,
        ast_builder: &mut AstIndexBuilder,
        new_manifest: &mut FileManifest,
        new_ast_cache: &mut AstNgramCache,
        rx: crossbeam_channel::Receiver<ProcessedFile>,
        debug_enabled: bool,
    ) -> anyhow::Result<ConsumeResult> {
        let mut next_file_id: u32 = 0;
        let mut cache_hits: u32 = 0;
        let mut ast_cache_hits: u32 = 0;
        let mut ast_reextracted: u32 = 0;

        for pf in rx {
            // Fail-soft: a lexical builder error on one file must not abort a
            // 50 K-file build. Skip the file from BOTH indexes (keeps FileIds in sync).
            if let Err(e) = builder.add_file_classified(
                FileId(next_file_id),
                &pf.content,
                pf.lang,
                &pf.field_map,
            ) {
                if debug_enabled {
                    eprintln!(
                        "skim search [debug]: add_file_classified failed for {:?}: {e}",
                        pf.rel_path
                    );
                }
                // Do NOT increment next_file_id — invariant preserved.
                // Do NOT add an AST entry either — file excluded from both indexes.
                continue;
            }

            // AST index: resolve payload from cache (hit) or derive fresh (miss).
            // The invariant: EVERY file that passes the lexical stage gets exactly
            // ONE `add_file_ngrams` call — hit or miss. NEVER skip. (applies ADR-006)
            //
            // `pf.sha256` is borrowed here so it can be moved into the ManifestEntry
            // below without a redundant heap clone — the SHA is a 64-char hex string
            // that only needs to be allocated once per file. (applies ADR-003)
            let is_hit = pf.ast_cached.is_some();
            let entry = resolve_ast_entry(
                new_ast_cache,
                &pf.sha256,
                pf.ast_cached,
                &pf.content,
                pf.lang,
                &pf.rel_path,
                debug_enabled,
            );
            if is_hit {
                ast_cache_hits = ast_cache_hits.saturating_add(1);
            } else {
                ast_reextracted = ast_reextracted.saturating_add(1);
            }

            // Add the AST entry for this file. The lexical entry for the SAME
            // FileId was already accepted, so an error here means the indexes are
            // now desynced for `next_file_id`. This is unrecoverable: abort the
            // whole build (propagates to run() BEFORE new_manifest.save()), so the
            // old manifest survives and the next query self-heals via a full
            // rebuild. Silently continuing would advance next_file_id and cascade
            // the desync into a committed-but-corrupt index. See the FileId-
            // alignment invariant in the function doc. (applies ADR-006)
            if let Err(e) = ast_builder.add_file_ngrams(
                FileId(next_file_id),
                pf.lang,
                &entry.ngrams,
                entry.node_count,
                entry.metrics,
            ) {
                return Err(anyhow::anyhow!(
                    "AST index desync: add_file_ngrams failed for {:?} at FileId {}: {e} \
                     (lexical entry already written; aborting build so the manifest is \
                     not saved and the next query rebuilds from scratch)",
                    pf.rel_path,
                    next_file_id
                ));
            }

            // Success: advance counter.
            // On overflow (>4 billion files) break rather than abort — work already
            // indexed is flushed by the caller's builder.build() + new_manifest.save().
            let Some(next) = next_file_id.checked_add(1) else {
                if debug_enabled {
                    eprintln!(
                        "skim search [debug]: next_file_id overflows u32; \
                         flushing {} files and stopping",
                        next_file_id
                    );
                }
                break;
            };
            next_file_id = next;
            if pf.cache_hit {
                cache_hits = cache_hits.saturating_add(1);
            }

            // AD-373-2 (ref): use normalize_rel_path so the manifest key is
            // byte-identical to the walk sort key (walk.rs). Single source of truth.
            let path_key = normalize_rel_path(&pf.rel_path);
            new_manifest.insert(ManifestEntry {
                path: path_key,
                sha256: pf.sha256,
                lang: pf.lang.as_str().to_string(),
                field_map: encode_field_map(&pf.field_map),
                mtime: pf.mtime,
                size: pf.size,
            });
            // `pf.content` dropped here — memory released immediately.
        }

        Ok(ConsumeResult {
            file_count: next_file_id,
            cache_hits,
            ast_cache_hits,
            ast_reextracted,
        })
    }
}

// ============================================================================
// Streaming producer helper
// ============================================================================

/// Resolve the AST n-gram payload for one file into `new_ast_cache`, returning
/// a shared borrow of the stored entry.
///
/// # Responsibility
///
/// This helper extracts the multi-branch AST cache hit/miss logic from the
/// consume loop so `consume()` reads as a flat sequence of four steps:
/// lexical-add → resolve-ast → ast-add → advance.
///
/// # Cache semantics
///
/// - **Hit** (`cached.is_some()`): the owned `CachedAstEntry` arrived with the
///   `ProcessedFile`; insert it into `new_ast_cache` so it survives to the next
///   build, then return a borrow.  Uses `get_or_insert` (Entry API) so the SHA
///   key is hashed only once — no insert-then-re-probe double-hash. (applies ADR-003)
///
/// - **Miss** (`cached.is_none()`): run `derive_ast_entry` (fail-soft: always
///   returns a valid, possibly empty triple), construct a `CachedAstEntry`, insert
///   it, and return a borrow.  Empty entries for data-format files are valid cache
///   entries, not corrupt. (applies ADR-003)
///
/// # AC7 poison-check note
///
/// A zero-count entry from the cache reaching `add_file_ngrams` will trigger the
/// desync abort in `add_file_ngrams`'s `check_count_nonzero` guard (applies
/// ADR-006).  That path is not handled here — `resolve_ast_entry` is intentionally
/// unaware of it, keeping responsibilities separate.
fn resolve_ast_entry<'cache>(
    new_ast_cache: &'cache mut AstNgramCache,
    sha_key: &str,
    cached: Option<CachedAstEntry>,
    content: &str,
    lang: rskim_core::Language,
    rel_path: &std::path::Path,
    debug_enabled: bool,
) -> &'cache CachedAstEntry {
    // Cache miss: full extraction (fail-soft: always returns a valid entry).
    // Empty entries for data-format files are valid cache entries, not corrupt.
    let entry = cached.unwrap_or_else(|| derive_ast_entry(content, lang, rel_path, debug_enabled));
    // Entry API: hashes sha_key once, inserts if absent, returns &CachedAstEntry.
    // Eliminates the insert-then-lookup double-probe.
    // `sha_key` is borrowed from the caller's `ProcessedFile.sha256`; `.to_string()`
    // here allocates the HashMap key string only when an insertion is needed.
    // The caller can then move `pf.sha256` into the ManifestEntry without a clone.
    new_ast_cache.get_or_insert(sha_key.to_string(), entry)
}

/// Read a file's content, apply 2-tier SHA cache logic, and produce a
/// [`ProcessedFile`] — or a [`SkipReason`] if the file should be excluded.
///
/// Cache tiers:
/// - SHA match → reuse `field_map` from manifest (lexical cache hit, no
///   classify call); also attaches the AST payload from `ast_cache` when
///   available (AST cache hit, no `derive_ast_entry` call in consumer).
/// - SHA mismatch or `--force` → run `classify_source`; `ast_cached` is `None`
///   so the consumer calls `derive_ast_entry`.
///
/// Mtime is stored in the manifest for forward-looking aggressive-mode support
/// (where mtime mismatch could skip SHA entirely) but is not read here — SHA is
/// the sole cache authority in safe mode. (applies ADR-003)
///
/// Called by the producer thread for each [`WalkEntry`].
fn read_and_classify(
    entry: &WalkEntry,
    manifest: &FileManifest,
    ast_cache: &AstNgramCache,
    force: bool,
    debug: bool,
) -> Result<ProcessedFile, SkipReason> {
    // Read content (size check + UTF-8 validation).
    let content = match open_and_read(&entry.abs_path) {
        ReadOutcome::Content(c) => c,
        ReadOutcome::NonUtf8 => return Err(SkipReason::NonUtf8(entry.abs_path.clone())),
        ReadOutcome::TooLarge(size) => {
            return Err(SkipReason::TooLarge {
                path: entry.abs_path.clone(),
                size,
            });
        }
        ReadOutcome::Io(e) => {
            return Err(SkipReason::ReadError {
                path: entry.abs_path.clone(),
                error: e.to_string(),
            });
        }
    };

    // Minification check (tree-sitter languages only).
    if !entry.lang.is_serde_based() && is_minified(&content) {
        return Err(SkipReason::Minified(entry.abs_path.clone()));
    }

    // Always compute SHA — it is the correctness guarantee for both the
    // lexical and AST caches.  Content SHA-256 is the sole cache authority;
    // mtime is never consulted for cache decisions. (applies ADR-003)
    let sha = sha256_hex(content.as_bytes());

    // Lexical 2-tier SHA cache: SHA match → hit, mismatch/--force → miss.
    // AD-373-2 (ref): use normalize_rel_path so the lookup key is byte-identical
    // to the manifest key and the walk sort key. Single source of truth.
    let path_key = normalize_rel_path(&entry.rel_path);

    let (field_map, cache_hit) = if !force
        && let Some(cached) = manifest.lookup(&path_key)
        && cached.sha256 == sha
    {
        // SHA match → reuse field_map (lexical cache hit).
        (decode_field_map(&cached.field_map), true)
    } else {
        // Cache miss or --force → classify.
        (run_classify(&content, entry.lang, debug), false)
    };

    // AST cache lookup: independent of the lexical cache hit/miss.
    // On --force, ast_cache is empty so lookup always returns None (AC11).
    // A SHA-match-but-cache-absent case (e.g. first build after version bump,
    // or a corrupt entry) returns None here → consumer re-extracts. (AC5)
    let ast_cached = ast_cache.lookup(&sha).cloned();

    Ok(ProcessedFile {
        rel_path: entry.rel_path.clone(),
        lang: entry.lang,
        content,
        sha256: sha,
        mtime: entry.mtime,
        size: entry.size,
        field_map,
        cache_hit,
        ast_cached,
    })
}

// ============================================================================
// Private helpers
// ============================================================================

/// Saturating cast from `usize` to `u32`.
///
/// Returns `u32::MAX` on overflow — used for counters that only need
/// approximate values for display when the file count exceeds 4 billion.
fn to_u32_capped(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Call `classify_source` and return the field_map.
///
/// On error, falls back to an empty field map so indexing continues. The
/// failure is logged to stderr when `debug` is true (i.e. `SKIM_DEBUG` was
/// set), which matches the existing debug gate used throughout the codebase.
/// The caller hoists the env-var check once before the producer thread so
/// that this function never performs a syscall on the hot path.
fn run_classify(
    content: &str,
    lang: rskim_core::Language,
    debug: bool,
) -> Vec<(std::ops::Range<usize>, rskim_search::SearchField)> {
    match classify_source(content, lang) {
        Ok(fields) => fields,
        Err(e) => {
            if debug {
                eprintln!(
                    "skim search [debug]: classify_source failed for {:?}: {e}",
                    lang.as_str()
                );
            }
            Vec::new()
        }
    }
}

/// Derive the AST n-gram entry for one file.
///
/// Returns a [`CachedAstEntry`] ready to store in `new_ast_cache`.
///
/// # Error policy (fail-soft)
///
/// On ANY error (grammar load failure, linearization error, or an Ok-but-empty
/// result for non-tree-sitter languages / large files / empty content), this
/// function returns an empty-but-valid entry so the caller can still insert an
/// ALIGNED EMPTY ENTRY into the AST builder. It never panics and never propagates
/// an error — doing so would either abort the whole build (wrong for a per-file
/// parse error) or skip the AST call entirely (which desynchronises FileIds).
///
/// The fail-loud path lives in [`Pipeline::consume`]: once the lexical entry for
/// a FileId has been accepted, a failure from `add_file_ngrams` is unrecoverable
/// (the two indexes are now desynced). That is the only place where abort is
/// correct. This helper is deliberately infallible so the loop body reads as its
/// 4-step contract: lexical-add-or-continue → derive → ast-add-or-abort → advance.
fn derive_ast_entry(
    content: &str,
    lang: rskim_core::Language,
    rel_path: &Path,
    debug: bool,
) -> CachedAstEntry {
    let lin = match linearize_source(content, lang) {
        Ok(lin) if !lin.nodes.is_empty() => lin,
        Ok(_) => return CachedAstEntry::default(),
        Err(e) => {
            if debug {
                eprintln!(
                    "skim search [debug]: linearize_source failed for {:?}: {e}",
                    rel_path
                );
            }
            return CachedAstEntry::default();
        }
    };
    // Non-tree-sitter lang (JSON/YAML/TOML), file >100KiB, empty source,
    // parse-only-error, or grammar load failure returns early above —
    // only non-empty linearizations reach here. FileIds stay in sync with
    // the lexical index for all cases.
    let (ngrams, metrics) = extract_ast_ngrams_with_metrics(&lin.nodes, lang);
    // Applies PF-004: explicit try_from, not `as u32`.
    let node_count = u32::try_from(lin.nodes.len()).unwrap_or(u32::MAX);
    CachedAstEntry {
        ngrams,
        metrics,
        node_count,
    }
}

/// Resolve the per-project search cache directory.
///
/// Path: `{base_cache}/search/{sha256(canonical_root)[..16]}/`
///
/// The base cache dir is resolved via `SKIM_CACHE_DIR` (if set) or the platform
/// cache dir (`~/Library/Caches/skim` on macOS, `~/.cache/skim` on Linux).
///
/// For an existing on-disk root the path component is the truncated SHA-256 of
/// `root.canonicalize()`. For a NON-existent root (canonicalize fails) the path
/// is hashed from a pure-lexical normalization (see [`canonical_or_normalized`])
/// so that trailing-slash / `.`-segment spellings of the same missing root map
/// to a single index directory (AD-381-2).
pub(super) fn resolve_search_cache_dir(root: &Path) -> anyhow::Result<PathBuf> {
    let base = crate::cmd::resolve_cache_dir()
        .ok_or_else(|| anyhow::anyhow!("failed to resolve skim cache directory"))?;

    let canonical = canonical_or_normalized(root);
    let hash = project_root_hash(&canonical);

    Ok(base.join("search").join(hash))
}

/// Canonicalize `root`, falling back to a pure-lexical normalization when the
/// path does not exist on disk.
///
/// On the success path this is exactly `root.canonicalize()` — no extra work
/// for the common (existing-root) case. Only when `canonicalize()` errors (the
/// cold, non-existent-root branch) do we normalize lexically so that equivalent
/// spellings of the same missing root collapse to one directory.
///
/// The fallback normalization is **pure-lexical and side-effect-free**
/// (AD-381-N): it walks [`Path::components`], dropping `CurDir` (`.`) segments
/// and any trailing separator, with **no `..` resolution and no filesystem
/// calls**. This keeps the result deterministic and cross-platform (provable
/// without Windows CI).
///
/// Accepted trade-off: because `..` is NOT resolved, divergent `..` spellings of
/// a non-existent root (e.g. `foo/../bar` vs `bar`) deliberately remain distinct
/// — resolving them would require filesystem I/O on a path that does not exist.
/// Revisit only if a concrete duplicate-dir case for `..` is observed.
fn canonical_or_normalized(root: &Path) -> PathBuf {
    match root.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => lexically_normalize(root),
    }
}

/// Pure-lexical path normalization: strip trailing separators and collapse `.`
/// (current-dir) segments, WITHOUT resolving `..` and WITHOUT any syscalls.
///
/// Used only on the canonicalize-error (non-existent-root) path so that
/// `foo`, `foo/`, `./foo`, and `foo/./bar` vs `foo/bar` map to identical
/// `PathBuf`s. `..` segments are preserved verbatim (`Component::ParentDir`),
/// so `foo/../bar` stays distinct from `bar` by design (AD-381-N).
fn lexically_normalize(root: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in root.components() {
        match component {
            // Drop `.` segments — they are semantically inert.
            Component::CurDir => {}
            // Preserve everything else verbatim. `Path::components` already
            // collapses repeated and trailing separators, so re-pushing these
            // components yields a canonical-form spelling without `..` resolution.
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Compute a 16-char hex prefix of the SHA-256 of the canonical project root path.
///
/// Used as a stable directory name in the search cache.
fn project_root_hash(canonical_root: &Path) -> String {
    let input = canonical_root.to_string_lossy();
    sha256_hex(input.as_bytes())[..16].to_string()
}

// ============================================================================
// Tests (co-located in index_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
