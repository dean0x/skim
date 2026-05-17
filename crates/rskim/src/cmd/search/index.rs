//! `skim search index` — pipeline orchestration for the index builder.
//!
//! # Data flow
//!
//! **Streaming build**:
//! 1. `discover_project_root(cwd)` → walk up to `.git`, fall back to cwd
//! 2. Resolve cache dir: `~/.cache/skim/search/{sha256(canonical_root)[..16]}/`
//! 3. `walk_metadata(root, max_files)` → metadata-only WalkEntry list (sorted)
//! 4. Producer thread: for each entry, reads content, computes SHA, applies
//!    4-tier mtime/SHA cache, classifies; sends ProcessedFile on bounded channel
//! 5. Consumer thread: receives ProcessedFile, calls add_file_classified, inserts
//!    manifest entry, drops content → peak memory bounded by channel capacity
//! 6. `builder.build()` flushes index; manifest written after (marks coherence)
//! 7. Print summary to stderr
//!
//! **Incremental build** (manifest exists, no `--force`):
//! - SHA-256 match → reuse cached field_map (cache hit, no classify_source call).
//! - Always write a fresh manifest after build.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use clap::Parser;
use rskim_search::{FileId, LayerBuilder, NgramIndexBuilder, classify_source};

use super::manifest::{FileManifest, ManifestEntry, decode_field_map, encode_field_map};
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
fn build_index(config: &IndexConfig) -> anyhow::Result<IndexResult> {
    Pipeline::new(config)?.run()
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
    pub(super) fn run(self) -> anyhow::Result<IndexResult> {
        // Stage 1: Metadata-only walk (no content reading).
        let (walk_entries, walk_skips) =
            walk_metadata(&self.config.root, self.config.effective_max_files())?;
        let walk_skip_count = walk_skips.len();

        if walk_entries.is_empty() {
            // Nothing to index — write an empty manifest and return early.
            let manifest = FileManifest::new(self.config.root.clone(), self.cache_dir.clone());
            manifest.save()?;
            return Ok(IndexResult {
                file_count: 0,
                skipped: to_u32_capped(walk_skip_count),
                cache_hits: 0,
                duration: self.start.elapsed(),
            });
        }

        // Stage 2: Load the manifest for incremental builds.
        let manifest = if self.config.force {
            FileManifest::new(self.config.root.clone(), self.cache_dir.clone())
        } else {
            FileManifest::load(self.config.root.clone(), self.cache_dir.clone())?
        };

        // Stage 3: Streaming producer → consumer.
        //
        // The producer iterates the sorted walk_entries, reads each file,
        // applies 4-tier cache logic, and sends ProcessedFile on a bounded
        // channel.  The consumer (this thread) receives, indexes, and drops
        // content immediately — keeping peak memory bounded.
        let (tx, rx) = crossbeam_channel::bounded::<ProcessedFile>(CHANNEL_CAPACITY);

        // Producer-side skip counter (read errors, minification, size errors
        // discovered during content read).
        let producer_skips = Arc::new(AtomicU32::new(0));
        let producer_skips_clone = Arc::clone(&producer_skips);

        let debug_enabled = crate::debug::is_debug_enabled();
        let force = self.config.force;

        // Spawn producer thread.
        let producer_handle = std::thread::spawn(move || {
            for entry in &walk_entries {
                match read_and_classify(entry, &manifest, force, debug_enabled) {
                    Ok(pf) => {
                        // Send blocks when the channel is full — this is the
                        // backpressure mechanism that limits peak memory.
                        if tx.send(pf).is_err() {
                            // Consumer dropped the receiver (fatal error on consumer side).
                            // Break so the producer doesn't spin fruitlessly.
                            break;
                        }
                    }
                    Err(_reason) => {
                        // Count read/minification errors; continue with next file.
                        producer_skips_clone.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            // `tx` is dropped here, which closes the channel and signals EOF to consumer.
        });

        // Consumer: receives ProcessedFile, builds the index sequentially.
        //
        // `next_file_id` only increments after a successful `add_file_classified`,
        // preserving the builder's sequential FileId invariant even on errors.
        let mut builder = NgramIndexBuilder::new(self.cache_dir.clone())?;
        let mut new_manifest = FileManifest::new(self.config.root.clone(), self.cache_dir.clone());
        let mut next_file_id: u32 = 0;
        let mut cache_hits: u32 = 0;

        for pf in rx {
            // Fail-soft: a classify or builder error on one file must not abort
            // a 50 K-file build.
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
                continue;
            }

            // Success: advance counter and record in manifest.
            next_file_id = next_file_id.checked_add(1).ok_or_else(|| {
                anyhow::anyhow!("next_file_id overflows u32; too many files in index")
            })?;
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
            // `pf.content` is dropped here — memory released immediately.
        }

        // Wait for the producer to finish and propagate any panic.
        producer_handle.join().map_err(|e| {
            anyhow::anyhow!(
                "producer thread panicked: {:?}",
                e.downcast_ref::<String>()
                    .map(String::as_str)
                    .unwrap_or("<non-string panic>")
            )
        })?;

        // build() flushes index.skidx + index.skpost.
        let _layer = builder.build()?;
        // Manifest written last — marks index as coherent.
        new_manifest.save()?;

        let total_skipped =
            to_u32_capped(walk_skip_count).saturating_add(producer_skips.load(Ordering::Relaxed));

        Ok(IndexResult {
            file_count: next_file_id,
            skipped: total_skipped,
            cache_hits,
            duration: self.start.elapsed(),
        })
    }
}

// ============================================================================
// Streaming producer helper
// ============================================================================

/// Read a file's content, apply 4-tier mtime/SHA cache logic, and produce a
/// [`ProcessedFile`] — or a [`SkipReason`] if the file should be excluded.
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

    // 4-tier cache logic.
    let path_key = entry.rel_path.to_string_lossy().replace('\\', "/");

    if !force
        && let Some(cached) = manifest.lookup(&path_key)
        && cached.sha256 == sha
    {
        // SHA match → reuse field_map (cache hit).
        return Ok(ProcessedFile {
            rel_path: entry.rel_path.clone(),
            lang: entry.lang,
            content,
            sha256: sha,
            mtime: entry.mtime,
            field_map: decode_field_map(&cached.field_map),
            cache_hit: true,
        });
    }

    // Cache miss or --force → classify.
    let field_map = run_classify(&content, entry.lang, debug);

    Ok(ProcessedFile {
        rel_path: entry.rel_path.clone(),
        lang: entry.lang,
        content,
        sha256: sha,
        mtime: entry.mtime,
        field_map,
        cache_hit: false,
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
/// The caller hoists the env-var check once before the rayon worker pool so
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
fn resolve_search_cache_dir(root: &Path) -> anyhow::Result<PathBuf> {
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
