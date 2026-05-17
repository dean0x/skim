//! `skim search index` — pipeline orchestration for the index builder.
//!
//! # Data flow
//!
//! **Full build** (no manifest, or `--force`):
//! 1. `discover_project_root(cwd)` → walk up to `.git`, fall back to cwd
//! 2. Resolve cache dir: `~/.cache/skim/search/{sha256(canonical_root)[..16]}/`
//! 3. `walk_and_read(root, max_files)` → per-file content + SHA-256
//! 4. Classify in parallel (rayon): `classify_source(content, lang)` → field_map
//! 5. Build (sequential): `NgramIndexBuilder::new()` + `add_file_classified()` + `build()`
//! 6. Write manifest atomically (last — marks index as coherent)
//! 7. Print summary to stderr
//!
//! **Incremental build** (manifest exists, no `--force`):
//! - Same walk+read (all files must be read for bigram extraction).
//! - Load manifest → if SHA-256 matches → reuse cached field_map (skip `classify_source`).
//! - Always write a fresh manifest after build.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use rskim_search::{FileId, LayerBuilder, NgramIndexBuilder, SearchField, classify_source};

use super::manifest::{FileManifest, ManifestEntry, decode_field_map, encode_field_map};
use super::types::{IndexConfig, IndexResult};
use super::walk::{discover_project_root, sha256_hex, walk_and_read};

// ============================================================================
// Internal type alias (avoids complex type in Vec)
// ============================================================================

/// Field map type: byte ranges mapped to their AST-derived search fields.
type FieldMap = Vec<(std::ops::Range<usize>, SearchField)>;

/// Classified file: SHA-256, field_map, and whether it was a manifest cache hit.
type ClassifiedFile = (String, FieldMap, bool);

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

// ============================================================================
// Core pipeline
// ============================================================================

/// Execute the full build or incremental build pipeline.
fn build_index(config: &IndexConfig) -> anyhow::Result<IndexResult> {
    let start = Instant::now();

    // 1. Resolve cache directory for this project root.
    let cache_dir = match &config.cache_dir_override {
        Some(dir) => dir.clone(),
        None => resolve_search_cache_dir(&config.root)?,
    };
    std::fs::create_dir_all(&cache_dir)?;

    // 2. Walk and read all source files.
    let max_files = config.effective_max_files();
    let (read_files, skipped_reasons) = walk_and_read(&config.root, max_files)?;
    let skipped_count = to_u32_capped(skipped_reasons.len());

    if read_files.is_empty() {
        // Nothing to index — write an empty manifest and return.
        let manifest = FileManifest::new(config.root.clone(), cache_dir.clone());
        manifest.save()?;
        return Ok(IndexResult {
            file_count: 0,
            skipped: skipped_count,
            cache_hits: 0,
            duration: start.elapsed(),
        });
    }

    // 3. Load manifest (for incremental builds).
    let manifest = if config.force {
        FileManifest::new(config.root.clone(), cache_dir.clone())
    } else {
        FileManifest::load(config.root.clone(), cache_dir.clone())?
    };

    // 4a. Pre-compute path keys once (avoids duplicate allocation in classify +
    //     manifest write phases — each key is a heap allocation).
    let path_keys: Vec<String> = read_files
        .iter()
        .map(|rf| rf.rel_path.to_string_lossy().replace('\\', "/"))
        .collect();

    // 4b. Classify in parallel: for each file, compute SHA-256, then apply the
    //     four-tier mtime/SHA cache logic:
    //
    //   Tier 1: mtime match + SHA match   → reuse field_map (cache hit)
    //   Tier 2: mtime match + SHA mismatch → fresh classify (rare – concurrent write)
    //   Tier 3: mtime mismatch + SHA match → reuse field_map (clock skew / copy)
    //   Tier 4: mtime mismatch + SHA mismatch → fresh classify
    //
    // SHA is *always* computed — mtime is only a fast pre-screening hint.
    // Results are in the same order as read_files.
    //
    // Hoist the debug flag once before entering the rayon worker pool to avoid
    // a syscall on every classify error across parallel workers.
    let debug_enabled = crate::debug::is_debug_enabled();
    let classified: Vec<ClassifiedFile> = read_files
        .par_iter()
        .zip(path_keys.par_iter())
        .map(|(rf, path_key)| {
            // Always compute SHA-256 — it is the correctness guarantee.
            let sha = sha256_hex(rf.content.as_bytes());

            if config.force {
                // --force: skip all cache logic.
                return (sha, run_classify(&rf.content, rf.lang, debug_enabled), false);
            }

            if let Some(entry) = manifest.lookup(path_key)
                && entry.sha256 == sha
            {
                // SHA matches → safe to reuse field_map regardless of mtime.
                return (sha, decode_field_map(&entry.field_map), true);
                // If SHA mismatches, fall through to fresh classify.
            }

            // No entry or SHA mismatch → classify.
            (sha, run_classify(&rf.content, rf.lang, debug_enabled), false)
        })
        .collect();

    let cache_hits = to_u32_capped(classified.iter().filter(|(_, _, hit)| *hit).count());

    // 5. Build the index sequentially (NgramIndexBuilder is not Sync).
    // 6. Accumulate manifest entries in the same pass (avoids a second enumerate loop).
    //
    // Use a manual `next_file_id` counter that only increments after a successful
    // `add_file_classified`. The previous `enumerate()` approach had a latent bug:
    // if `add_file_classified` failed and we `continue`d, the next iteration would
    // pass `idx+1` while `builder.file_count` was still at `idx`, causing the
    // builder's sequential-invariant check to reject ALL subsequent files.
    let mut builder = NgramIndexBuilder::new(cache_dir.clone())?;
    let mut new_manifest = FileManifest::new(config.root.clone(), cache_dir);
    let mut next_file_id: u32 = 0;
    for ((rf, (sha, field_map, _)), path_key) in
        read_files.iter().zip(classified).zip(path_keys)
    {
        // Guard against usize overflow into FileId(u32) on pathological inputs.
        let file_id = next_file_id;
        // Fail-soft: a single file that fails to index should not abort a
        // 50 K-file build. Log the error under debug and continue.
        // IMPORTANT: only advance `next_file_id` after a successful add so the
        // builder's sequential FileId invariant is never violated.
        if let Err(e) =
            builder.add_file_classified(FileId(file_id), &rf.content, rf.lang, &field_map)
        {
            if debug_enabled {
                eprintln!(
                    "skim search index [debug]: add_file_classified failed for {:?}: {e}",
                    rf.rel_path
                );
            }
            continue;
        }
        // Increment only on success.
        next_file_id = next_file_id.checked_add(1).ok_or_else(|| {
            anyhow::anyhow!("next_file_id overflows u32; too many files in index")
        })?;
        new_manifest.insert(ManifestEntry {
            path: path_key,
            sha256: sha,
            lang: rf.lang.as_str().to_string(),
            field_map: encode_field_map(&field_map),
            mtime: rf.mtime,
        });
    }
    // build() flushes index.skidx + index.skpost; manifest written after (marks coherence).
    let _layer = builder.build()?;
    new_manifest.save()?;

    let file_count = next_file_id;

    Ok(IndexResult {
        file_count,
        skipped: skipped_count,
        cache_hits,
        duration: start.elapsed(),
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

/// Compute a 16-char hex hash of the canonical project root path.
///
/// Used as a stable directory name in the search cache.
fn project_root_hash(canonical_root: &Path) -> String {
    let input = canonical_root.to_string_lossy();
    let digest = Sha256::digest(input.as_bytes());
    // Take first 8 bytes → 16 hex chars
    digest
        .iter()
        .take(8)
        .flat_map(|byte| {
            [
                b"0123456789abcdef"[(byte >> 4) as usize],
                b"0123456789abcdef"[(byte & 0x0f) as usize],
            ]
        })
        .map(|b| b as char)
        .collect()
}

// ============================================================================
// Tests (co-located in index_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
