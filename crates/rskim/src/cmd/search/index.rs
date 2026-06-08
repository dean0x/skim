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

use anyhow::Context as _;
use clap::Parser;
use rskim_search::{
    AstIndexBuilder, AstNgramSet, FileId, LayerBuilder, NgramIndexBuilder, StructuralMetrics,
    classify_source, extract_ast_ngrams_with_metrics, linearize_source,
};

use super::manifest::{FileManifest, ManifestEntry, decode_field_map, encode_field_map};
use super::staleness::read_git_head;
use super::types::{IndexConfig, IndexResult, ProcessedFile, SkipReason, WalkEntry};
use super::walk::{
    ReadOutcome, discover_project_root, is_minified, open_and_read, sha256_hex, walk_metadata,
};

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search index` subcommand.
///
/// Accepted flags:
/// - `--root=<PATH>` or `--root <PATH>` — explicit project root (default: cwd)
/// - `--force` — skip manifest cache and re-classify every file
/// - `--max-files=<N>` — override the 50,000 file cap (must be ≥ 1)
/// - `-h` / `--help` — print help text and exit
///
/// # Errors
///
/// Returns `Err` only for fatal I/O failures. User-facing errors (unsupported
/// languages, too-large files) are counted and reported to stderr but do not
/// cause a non-zero exit code.
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
        "skim search index: indexed {} files ({} skipped, {} cache hits) in {:.1}s",
        result.file_count,
        result.skipped,
        result.cache_hits,
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
/// Acquires an exclusive advisory lock on `{cache_dir}/.skim-build.lock` before
/// running the pipeline. If another process holds the lock the call blocks until
/// that build completes and then proceeds with its own build. This serialises
/// all callers — `skim init` background spawn, git-hook `--update`, and direct
/// `--build` / `--rebuild` — protecting `index.skidx` and `index.skfiles` from
/// concurrent writes.
///
/// The lock is released when the returned [`IndexResult`] (or the `Err`) drops,
/// i.e. at the end of this function. The lock file itself is never deleted so
/// the OS can reuse it across processes.
pub(super) fn build_index(config: &IndexConfig) -> anyhow::Result<IndexResult> {
    let pipeline = Pipeline::new(config)?;

    // Acquire the advisory build lock before touching index files.
    let lock_path = pipeline.cache_dir.join(".skim-build.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open build lock: {}", lock_path.display()))?;
    lock_file
        .lock()
        .with_context(|| "failed to acquire exclusive build lock")?;

    // Lock is held for the duration of the build. `lock_file` drops (and
    // releases the lock) when this function returns.
    pipeline.run()
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
struct ConsumeResult {
    /// Number of files successfully added to the index.
    file_count: u32,
    /// Number of files whose cached `field_map` was reused (SHA match).
    cache_hits: u32,
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
            // Nothing to index — flush empty lexical + AST indexes and manifest
            // so that `check_staleness` can find `index.skidx` and treat the
            // project as indexed rather than returning `NoIndex` on every query.
            let builder = NgramIndexBuilder::new(self.cache_dir.clone())?;
            let _layer = builder.build()?;
            // Also build an empty AST index so self-heal doesn't trigger
            // immediately after an empty-project build.
            let ast_builder = AstIndexBuilder::new(self.cache_dir.clone())
                .map_err(|e| anyhow::anyhow!("failed to create AST index builder: {e}"))?;
            ast_builder
                .build()
                .map_err(|e| anyhow::anyhow!("AST index build failed: {e}"))?;
            let mut manifest = FileManifest::new(self.config.root.clone(), self.cache_dir.clone());
            manifest.set_git_head(read_git_head(&self.config.root));
            manifest.save()?;
            return Ok(IndexResult {
                file_count: 0,
                skipped: to_u32_capped(walk_skip_count),
                cache_hits: 0,
                duration: self.start.elapsed(),
            });
        }

        // Stage 2: Load the manifest for incremental builds, then spawn producer.
        let manifest = self.load_manifest()?;
        let (producer_handle, rx, producer_skips) =
            Self::spawn_producer(walk_entries, manifest, self.config.force, debug_enabled);

        // Stage 3: Consume processed files, build lexical + AST indexes.
        let mut builder = NgramIndexBuilder::new(self.cache_dir.clone())?;
        // AST index (#199): build alongside lexical so both share the same FileId
        // sequence (correctness-critical — see FileId contract in ast_index/builder.rs).
        // NOTE: both builders retain posting lists until build(); memory scales with
        // file count (~tens of MB at 10k files) — tracked in #273 for chunked builds.
        // Re-extract all files each refresh (no incremental cache) — tracked in #290.
        let mut ast_builder = AstIndexBuilder::new(self.cache_dir.clone())
            .map_err(|e| anyhow::anyhow!("failed to create AST index builder: {e}"))?;
        let mut new_manifest = FileManifest::new(self.config.root.clone(), self.cache_dir.clone());
        let ConsumeResult {
            file_count,
            cache_hits,
        } = Self::consume(
            &mut builder,
            &mut ast_builder,
            &mut new_manifest,
            rx,
            debug_enabled,
        );

        // Wait for the producer to finish and propagate any panic.
        producer_handle.join().map_err(|e| {
            anyhow::anyhow!(
                "producer thread panicked: {:?}",
                e.downcast_ref::<String>()
                    .map(String::as_str)
                    .unwrap_or("<non-string panic>")
            )
        })?;

        // Commit ordering (crash-safety):
        // (1) Lexical build — index.skidx + index.skfiles written.
        // (2) AST build    — ast_index.skpost then ast_index.skidx written.
        // (3) Manifest save (git HEAD recorded) — the commit point.
        //
        // If the AST build fails, the manifest is NOT saved so the next query
        // sees the index as stale and triggers a full rebuild (self-heal path).
        // "HEAD recorded ⟹ both indexes coherent" is the invariant.
        let _layer = builder.build()?;
        ast_builder
            .build()
            .map_err(|e| anyhow::anyhow!("AST index build failed: {e}"))?;

        // Record the current git HEAD in the manifest so staleness detection
        // can compare it on the next query without spawning a git subprocess.
        new_manifest.set_git_head(read_git_head(&self.config.root));
        new_manifest.save()?;

        let total_skipped =
            to_u32_capped(walk_skip_count).saturating_add(producer_skips.load(Ordering::Relaxed));

        Ok(IndexResult {
            file_count,
            skipped: total_skipped,
            cache_hits,
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

    /// Stage 2b: spawn the producer thread.
    ///
    /// Returns a join handle, the receiving end of the bounded channel, and a
    /// shared skip counter that the producer increments on read/classify errors.
    fn spawn_producer(
        walk_entries: Vec<WalkEntry>,
        manifest: FileManifest,
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

        // Both `walk_entries` and `manifest` are moved into the producer thread.
        // `Vec<WalkEntry>` and `FileManifest` must be `Send`; the compiler
        // enforces this at the `thread::spawn` call site.
        let handle = std::thread::spawn(move || {
            for entry in &walk_entries {
                match read_and_classify(entry, &manifest, force, debug_enabled) {
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
    /// Returns aggregated counts. Errors on individual files are fail-soft: the
    /// file is skipped and indexing continues. On `FileId` overflow the loop
    /// breaks early and the partial result is returned — the caller's
    /// `builder.build()` + `ast_builder.build()` + `new_manifest.save()` still
    /// execute, preserving the fail-soft contract.
    ///
    /// # FileId Invariants
    ///
    /// 1. `next_file_id` only advances after a successful `add_file_classified`.
    ///    A lexical-builder error causes a `continue` — the file is excluded from
    ///    BOTH indexes, keeping them in sync.
    /// 2. AST entries are ALWAYS inserted (even on linearization error) via an
    ///    empty `AstNgramSet` + zero node_count + default metrics. This preserves
    ///    the AST builder's "every file gets exactly one call" contract and prevents
    ///    FileId desync between the lexical and AST indexes.
    fn consume(
        builder: &mut NgramIndexBuilder,
        ast_builder: &mut AstIndexBuilder,
        new_manifest: &mut FileManifest,
        rx: crossbeam_channel::Receiver<ProcessedFile>,
        debug_enabled: bool,
    ) -> ConsumeResult {
        let mut next_file_id: u32 = 0;
        let mut cache_hits: u32 = 0;

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
                        "skim search index [debug]: add_file_classified failed for {:?}: {e}",
                        pf.rel_path
                    );
                }
                // Do NOT increment next_file_id — invariant preserved.
                // Do NOT add an AST entry either — file excluded from both indexes.
                continue;
            }

            // AST index: linearize + extract n-grams.
            // Fail-soft: on ANY error (grammar load failure, linearization error,
            // or Ok(empty) for non-tree-sitter langs / large files), insert an
            // EMPTY ALIGNED ENTRY so AST FileIds stay in sync with lexical FileIds.
            // NEVER `?`-propagate (would abort the whole build).
            // NEVER skip (would desync FileIds → silently mis-map results).
            let (ast_set, ast_metrics, ast_node_count) =
                match linearize_source(&pf.content, pf.lang) {
                    Ok(lin) if !lin.nodes.is_empty() => {
                        let (set, metrics) = extract_ast_ngrams_with_metrics(&lin.nodes, pf.lang);
                        let node_count = u32::try_from(lin.nodes.len()).unwrap_or(u32::MAX);
                        (set, metrics, node_count)
                    }
                    Ok(_empty) => {
                        // Non-tree-sitter lang (JSON/YAML/TOML), file >100KiB,
                        // empty source, or parse-only-error result — empty entry.
                        (AstNgramSet::default(), StructuralMetrics::default(), 0u32)
                    }
                    Err(e) => {
                        // Grammar load failure (SearchError::Ast) — only unrecoverable
                        // error from linearize_source. Still insert empty aligned entry.
                        if debug_enabled {
                            eprintln!(
                                "skim search index [debug]: linearize_source failed \
                                 for {:?}: {e}",
                                pf.rel_path
                            );
                        }
                        (AstNgramSet::default(), StructuralMetrics::default(), 0u32)
                    }
                };

            // Add the AST entry for this file. On error, log and continue — the
            // lexical entry was already added; both indexes share the same FileId.
            if let Err(e) = ast_builder.add_file_ngrams(
                FileId(next_file_id),
                pf.lang,
                &ast_set,
                ast_node_count,
                ast_metrics,
            ) && debug_enabled
            {
                eprintln!(
                    "skim search index [debug]: add_file_ngrams failed for {:?}: {e}",
                    pf.rel_path
                );
                // Continue — lexical entry is already written; best-effort AST.
            }

            // Success: advance counter.
            // On overflow (>4 billion files) break rather than abort — work already
            // indexed is flushed by the caller's builder.build() + new_manifest.save().
            let Some(next) = next_file_id.checked_add(1) else {
                if debug_enabled {
                    eprintln!(
                        "skim search index [debug]: next_file_id overflows u32; \
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

            let path_key = pf.rel_path.to_string_lossy().replace('\\', "/");
            new_manifest.insert(ManifestEntry {
                path: path_key,
                sha256: pf.sha256,
                lang: pf.lang.as_str().to_string(),
                field_map: encode_field_map(&pf.field_map),
                mtime: pf.mtime,
            });
            // `pf.content` dropped here — memory released immediately.
        }

        ConsumeResult {
            file_count: next_file_id,
            cache_hits,
        }
    }
}

// ============================================================================
// Streaming producer helper
// ============================================================================

/// Read a file's content, apply 2-tier SHA cache logic, and produce a
/// [`ProcessedFile`] — or a [`SkipReason`] if the file should be excluded.
///
/// Cache tiers:
/// - SHA match → reuse `field_map` from manifest (cache hit, no classify call).
/// - SHA mismatch or `--force` → run `classify_source` (cache miss).
///
/// Mtime is stored in the manifest for forward-looking aggressive-mode support
/// (where mtime mismatch could skip SHA entirely) but is not read here — SHA is
/// the sole cache authority in safe mode.
///
/// Called by the producer thread for each [`WalkEntry`].
fn read_and_classify(
    entry: &WalkEntry,
    manifest: &FileManifest,
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

    // Always compute SHA — it is the correctness guarantee.
    let sha = sha256_hex(content.as_bytes());

    // 2-tier SHA cache: SHA match → hit, mismatch/--force → miss.
    let path_key = entry.rel_path.to_string_lossy().replace('\\', "/");

    let (field_map, cache_hit) = if !force
        && let Some(cached) = manifest.lookup(&path_key)
        && cached.sha256 == sha
    {
        // SHA match → reuse field_map (cache hit).
        (decode_field_map(&cached.field_map), true)
    } else {
        // Cache miss or --force → classify.
        (run_classify(&content, entry.lang, debug), false)
    };

    Ok(ProcessedFile {
        rel_path: entry.rel_path.clone(),
        lang: entry.lang,
        content,
        sha256: sha,
        mtime: entry.mtime,
        field_map,
        cache_hit,
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
                    "skim search index [debug]: classify_source failed for {:?}: {e}",
                    lang.as_str()
                );
            }
            Vec::new()
        }
    }
}

/// Resolve the per-project search cache directory.
///
/// Path: `{base_cache}/search/{sha256(canonical_root)[..16]}/`
///
/// The base cache dir is resolved via `SKIM_CACHE_DIR` (if set) or
/// `~/.cache/skim/`.
pub(super) fn resolve_search_cache_dir(root: &Path) -> anyhow::Result<PathBuf> {
    let base = crate::cmd::resolve_cache_dir()
        .ok_or_else(|| anyhow::anyhow!("failed to resolve skim cache directory"))?;

    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let hash = project_root_hash(&canonical);

    Ok(base.join("search").join(hash))
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
