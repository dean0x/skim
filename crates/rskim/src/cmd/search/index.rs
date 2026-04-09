//! Index directory management and build logic for `skim search`.

use std::path::{Path, PathBuf};

use rskim_core::Language;
use rskim_search::{fxhash_bytes, lexical::builder::LexicalLayerBuilder, LayerBuilder};

// ============================================================================
// Directory helpers
// ============================================================================

/// Return `true` if `path` is the root of a git repository (has a `.git` entry).
///
/// Lightweight predicate: does not invoke `gix` and does not walk up the tree.
pub(super) fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

/// Walk up directory tree to find a `.git` directory (repo root).
///
/// Falls back to CWD if no `.git` ancestor is found.
pub(super) fn find_repo_root() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let mut dir: &Path = cwd.as_path();
    loop {
        if dir.join(".git").exists() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return Ok(cwd),
        }
    }
}

/// Return the skim cache root directory.
///
/// Uses `SKIM_CACHE_DIR` environment variable if set; otherwise uses
/// the platform cache dir (`~/.cache/skim/` on Linux/macOS).
pub(super) fn skim_cache_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("SKIM_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine platform cache directory"))
        .map(|d| d.join("skim"))
}

/// Return the per-repo index directory under the skim cache.
pub(super) fn get_index_dir(repo_root: &Path) -> anyhow::Result<PathBuf> {
    let repo_hash = hash_path(repo_root);
    Ok(skim_cache_dir()?.join("search").join(repo_hash))
}

/// Delete the entire skim search cache directory.
pub(super) fn clear_search_cache() -> anyhow::Result<()> {
    let search_cache = skim_cache_dir()?.join("search");
    if search_cache.exists() {
        std::fs::remove_dir_all(&search_cache)?;
    }
    Ok(())
}

/// Stable hex hash of a path for use as a cache directory name.
///
/// Delegates to `rskim_search::fxhash_bytes` so the CLI and search
/// library share a single hash implementation. Changing the hash
/// function would invalidate existing indexes.
pub(super) fn hash_path(path: &Path) -> String {
    let path_str = path.to_string_lossy();
    let hash = fxhash_bytes(path_str.as_bytes());
    format!("{hash:016x}")
}

// ============================================================================
// Index build
// ============================================================================

/// Build a lexical index over `repo_root`, writing it to `index_dir`.
pub(super) fn build_index(repo_root: &Path, index_dir: &Path) -> anyhow::Result<()> {
    use ignore::WalkBuilder;

    std::fs::create_dir_all(index_dir)?;

    let mut builder = LexicalLayerBuilder::new(index_dir.to_path_buf(), repo_root.to_path_buf());
    let mut file_count: u64 = 0;

    let walker = WalkBuilder::new(repo_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();

        let language = match Language::from_path(path) {
            Some(lang) => lang,
            None => continue,
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // Skip binary or unreadable files.
        };

        // Use relative path from repo root so stored paths are portable.
        let rel_path = path.strip_prefix(repo_root).unwrap_or(path);

        if let Err(e) = builder.add_file(rel_path, &content, language) {
            eprintln!("warning: failed to index {}: {e}", rel_path.display());
            continue;
        }

        file_count += 1;
    }

    let _layer = Box::new(builder).build()?;
    eprintln!("Indexed {file_count} files.");
    Ok(())
}
