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

use rayon::prelude::*;
use sha2::{Digest, Sha256};

use rskim_search::{FileId, LayerBuilder, NgramIndexBuilder, SearchField, classify_source};

use super::manifest::{FileManifest, ManifestEntry, decode_field_map, encode_field_map};
use super::types::{IndexConfig, IndexResult};
use super::walk::{discover_project_root, walk_and_read};

// ============================================================================
// Internal type alias (avoids complex type in Vec)
// ============================================================================

/// Field map type: byte ranges mapped to their AST-derived search fields.
type FieldMap = Vec<(std::ops::Range<usize>, SearchField)>;

/// Classified file: field_map and whether it was a manifest cache hit.
type ClassifiedFile = (FieldMap, bool);

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search index` subcommand.
///
/// Accepted flags:
/// - `--root=<PATH>` or `--root <PATH>` — explicit project root (default: cwd)
/// - `--force` — skip manifest cache and re-classify every file
/// - `--max-files=<N>` — override the 50,000 file cap
/// - `-h` / `--help` — print help text and exit
///
/// # Errors
///
/// Returns `Err` only for fatal I/O failures. User-facing errors (unsupported
/// languages, too-large files) are counted and reported to stderr but do not
/// cause a non-zero exit code.
pub(super) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let config = parse_args(args)?;
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
// Argument parsing
// ============================================================================

fn parse_args(args: &[String]) -> anyhow::Result<IndexConfig> {
    let mut root: Option<PathBuf> = None;
    let mut force = false;
    let mut max_files: Option<usize> = None;
    let mut cache_dir_override: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--force" {
            force = true;
        } else if let Some(val) = next_value(args, &mut i, "--root")? {
            root = Some(PathBuf::from(val));
        } else if let Some(val) = next_value(args, &mut i, "--max-files")? {
            max_files = Some(
                val.parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("--max-files requires a positive integer"))?,
            );
        } else if let Some(val) = next_value(args, &mut i, "--index-dir")? {
            // Internal/test flag: override the cache directory.
            cache_dir_override = Some(PathBuf::from(val));
        } else {
            anyhow::bail!("skim search index: unknown argument: {arg}");
        }

        i += 1;
    }

    // Determine the project root.
    let effective_root = match root {
        Some(r) => r.canonicalize().unwrap_or(r),
        None => {
            let cwd = std::env::current_dir()?;
            discover_project_root(&cwd)?
        }
    };

    Ok(IndexConfig {
        root: effective_root,
        max_files,
        force,
        cache_dir_override,
    })
}

/// Extract the value for a `--flag=val` or `--flag val` argument pair.
///
/// Returns `Ok(Some(val))` if the current arg matches `flag`, advancing `i` for
/// the space-separated form. Returns `Ok(None)` if this arg is not `flag`.
fn next_value<'a>(
    args: &'a [String],
    i: &mut usize,
    flag: &str,
) -> anyhow::Result<Option<&'a str>> {
    let arg = &args[*i];
    let eq_prefix = format!("{flag}=");

    if let Some(val) = arg.strip_prefix(&eq_prefix) {
        return Ok(Some(val));
    }
    if arg == flag {
        *i += 1;
        let val = args
            .get(*i)
            .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))?;
        return Ok(Some(val.as_str()));
    }
    Ok(None)
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
    let skipped_count = u32::try_from(skipped_reasons.len()).unwrap_or(u32::MAX);

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

    // 4. Classify in parallel: for each file, either use cached field_map or call
    //    classify_source. Results are in the same order as read_files.
    let classified: Vec<ClassifiedFile> = read_files
        .par_iter()
        .map(|rf| {
            let path_key = rf.rel_path.to_string_lossy().replace('\\', "/");
            if let Some(entry) = manifest.lookup(&path_key)
                && entry.sha256 == rf.sha256
            {
                // Cache hit: reuse field_map
                return (decode_field_map(&entry.field_map), true);
            }
            // SHA mismatch or no entry: fresh classify
            (run_classify(&rf.content, rf.lang), false)
        })
        .collect();

    let cache_hits =
        u32::try_from(classified.iter().filter(|(_, hit)| *hit).count()).unwrap_or(u32::MAX);

    // 5. Build the index sequentially (NgramIndexBuilder is not Sync).
    let mut builder = NgramIndexBuilder::new(cache_dir.clone())?;
    for (idx, rf) in read_files.iter().enumerate() {
        let (ref field_map, _) = classified[idx];
        builder.add_file_classified(FileId(idx as u32), &rf.content, rf.lang, field_map)?;
    }
    // build() flushes index.skidx + index.skpost
    let _layer = builder.build()?;

    // 6. Write the manifest sidecar (written last — marks index as coherent).
    let mut new_manifest = FileManifest::new(config.root.clone(), cache_dir);
    for (idx, rf) in read_files.iter().enumerate() {
        let (ref field_map, _) = classified[idx];
        let path_key = rf.rel_path.to_string_lossy().replace('\\', "/");
        new_manifest.insert(ManifestEntry {
            path: path_key,
            sha256: rf.sha256.clone(),
            lang: format!("{:?}", rf.lang).to_lowercase(),
            field_map: encode_field_map(field_map),
        });
    }
    new_manifest.save()?;

    let file_count = u32::try_from(read_files.len()).unwrap_or(u32::MAX);

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

/// Call `classify_source` and return the field_map. On error, fall back to empty.
fn run_classify(
    content: &str,
    lang: rskim_core::Language,
) -> Vec<(std::ops::Range<usize>, rskim_search::SearchField)> {
    classify_source(content, lang).unwrap_or_default()
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
    use std::fmt::Write;
    let input = canonical_root.to_string_lossy();
    let digest = Sha256::digest(input.as_bytes());
    // Take first 8 bytes → 16 hex chars
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

// ============================================================================
// Help text
// ============================================================================

fn print_help() {
    println!(
        "\
Usage: skim search index [OPTIONS]

Build or update the search index for the current project.

Options:
  --root <PATH>       Project root to index (default: auto-discover via .git)
  --force             Rebuild from scratch, ignoring the manifest cache
  --max-files <N>     Maximum files to index (default: 50000)
  -h, --help          Print this help message

Index location:
  ~/.cache/skim/search/<project-hash>/
    index.skidx    Vocabulary + file metadata
    index.skpost   Posting lists
    index.skfiles  Manifest sidecar (for incremental updates)

Examples:
  skim search index
  skim search index --root /path/to/project
  skim search index --force"
    );
}

// ============================================================================
// Tests (co-located in index_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
