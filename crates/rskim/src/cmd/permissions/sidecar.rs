//! Per-agent `skim-permissions.json` sidecar manifest.
//!
//! Each agent that skim writes permissions for has a sidecar manifest at
//! `{config_dir}/skim-permissions.json`. The sidecar records:
//!
//! - The schema version (for forward-only migration).
//! - The permission tier (`seed | mirror | blanket`).
//! - Agent-native allowlist entries that skim wrote.
//! - Source→mirror provenance for non-seed tiers.
//! - A SHA-256 hash of the target config file at write time (computed by the
//!   caller via [`crate::cmd::integrity`]`::compute_file_hash`).
//!
//! ## API contract
//!
//! All public functions accept explicit `&Path` arguments.  They never read
//! environment variables — env-var resolution is the caller's responsibility
//! (use [`crate::cmd::init::DetectionEnv::resolve`] to obtain the config dir).
//!
//! [`load_sidecar`] returns a [`SidecarError::NotFound`] (not an empty
//! manifest) when the file is absent, so callers can distinguish "never
//! written" from "corrupt".  Any other failure (oversized, unparseable) is a
//! loud [`SidecarError`] with an actionable message — the sidecar is NEVER
//! silently ignored.

use std::collections::HashMap;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde::{Deserialize, Serialize};

use crate::cmd::init::MAX_SETTINGS_SIZE;

/// Filename of the per-agent sidecar manifest, relative to `config_dir`.
pub(super) const SIDECAR_FILENAME: &str = "skim-permissions.json";

// ============================================================================
// Schema
// ============================================================================

/// Per-agent sidecar manifest stored at `{config_dir}/skim-permissions.json`.
///
/// All fields are `pub(crate)` so callers can construct and inspect values
/// directly without accessor boilerplate — this is a plain data struct.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PermissionSidecar {
    /// Schema version (currently `1`). Forward-only migrations bump this.
    pub(crate) version: u32,
    /// Permission tier: `"seed"`, `"mirror"`, or `"blanket"`.
    pub(crate) tier: String,
    /// Agent-native allowlist entries that skim wrote.
    pub(crate) entries: Vec<String>,
    /// Source→mirror provenance.  Empty for the `seed` tier.
    /// Maps source config path to mirror config path.
    pub(crate) source_mirrors: HashMap<String, String>,
    /// SHA-256 hash of the target config file at write time.
    /// Computed by the caller via `integrity::compute_file_hash`.
    pub(crate) config_hash: String,
}

// ============================================================================
// Errors
// ============================================================================

/// Errors returned by [`load_sidecar`] and [`write_sidecar`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum SidecarError {
    /// The sidecar file does not exist (never written for this agent).
    ///
    /// This is a distinct variant — callers must NOT treat a missing sidecar
    /// the same as a corrupt one.
    #[error("sidecar not found: {0}")]
    NotFound(std::path::PathBuf),

    /// The sidecar file exceeds the byte cap.
    #[error(
        "sidecar file too large ({size} bytes, max {max}): {path}\n\
         hint: this does not look like a valid skim-permissions.json"
    )]
    Oversized {
        path: std::path::PathBuf,
        size: u64,
        max: u64,
    },

    /// The sidecar JSON is corrupt or unparseable.
    #[error(
        "sidecar JSON is corrupt or unparseable at {path}: {source}\nhint: delete {path} and re-run `skim init` to regenerate"
    )]
    Corrupt {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// An I/O error occurred reading or writing the sidecar.
    #[error("sidecar I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

// ============================================================================
// I/O
// ============================================================================

/// Load the sidecar manifest from `path`.
///
/// - **Missing file** → `Err(SidecarError::NotFound)`.  Callers that need to
///   distinguish "never written" from other failures must match this variant.
/// - **Oversized** (> [`MAX_SETTINGS_SIZE`]) → `Err(SidecarError::Oversized)`.
/// - **Unparseable JSON** → `Err(SidecarError::Corrupt)`.
/// - **I/O failure** → `Err(SidecarError::Io)`.
///
/// The sidecar is NEVER silently ignored or returned as an empty manifest on
/// failure.
pub(crate) fn load_sidecar(path: &Path) -> Result<PermissionSidecar, SidecarError> {
    if !path.exists() {
        return Err(SidecarError::NotFound(path.to_path_buf()));
    }

    let meta = std::fs::metadata(path).map_err(|e| SidecarError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let size = meta.len();
    if size > MAX_SETTINGS_SIZE {
        return Err(SidecarError::Oversized {
            path: path.to_path_buf(),
            size,
            max: MAX_SETTINGS_SIZE,
        });
    }

    let contents = std::fs::read_to_string(path).map_err(|e| SidecarError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    serde_json::from_str(&contents).map_err(|e| SidecarError::Corrupt {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Write the sidecar manifest to `path` atomically.
///
/// Uses a `.tmp`-sibling write + rename (crash-safe). On Unix the temporary
/// file is created with mode `0o600` (owner read/write only) — the rename
/// atomically grants that permission to the final path. The final file is
/// pretty-printed JSON terminated by a newline for readability.
pub(crate) fn write_sidecar(path: &Path, sidecar: &PermissionSidecar) -> anyhow::Result<()> {
    let pretty = serde_json::to_string_pretty(sidecar)?;
    let tmp_path = path.with_extension("json.tmp");

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&tmp_path, format!("{pretty}\n"))?;

    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o600);
        if let Err(e) = std::fs::set_permissions(&tmp_path, perms) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    }

    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sidecar() -> PermissionSidecar {
        PermissionSidecar {
            version: 1,
            tier: "seed".to_string(),
            entries: vec!["allow:Read".to_string(), "allow:Bash".to_string()],
            source_mirrors: HashMap::new(),
            config_hash: "abc123def456".to_string(),
        }
    }

    // ---- round-trip ----

    #[test]
    fn test_sidecar_round_trip_write_then_load() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-permissions.json");
        let original = sample_sidecar();

        write_sidecar(&path, &original).unwrap();
        let loaded = load_sidecar(&path).unwrap();

        assert_eq!(
            original, loaded,
            "loaded sidecar must equal the written sidecar"
        );
    }

    #[test]
    fn test_sidecar_round_trip_with_source_mirrors() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-permissions.json");
        let mut sidecar = sample_sidecar();
        sidecar.tier = "mirror".to_string();
        sidecar.source_mirrors.insert(
            "/src/config.json".to_string(),
            "/mirror/config.json".to_string(),
        );

        write_sidecar(&path, &sidecar).unwrap();
        let loaded = load_sidecar(&path).unwrap();

        assert_eq!(sidecar, loaded);
        assert_eq!(
            loaded
                .source_mirrors
                .get("/src/config.json")
                .map(String::as_str),
            Some("/mirror/config.json")
        );
    }

    // ---- missing file → NotFound ----

    #[test]
    fn test_load_sidecar_missing_file_returns_not_found() {
        let path = std::path::PathBuf::from("/nonexistent/__skim_perm_test_abc123__.json");
        let result = load_sidecar(&path);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SidecarError::NotFound(_)),
            "missing file must return NotFound variant"
        );
    }

    // ---- corrupt JSON → Err(Corrupt) ----

    #[test]
    fn test_load_sidecar_corrupt_json_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-permissions.json");
        std::fs::write(&path, b"{ not valid json !!!").unwrap();

        let result = load_sidecar(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SidecarError::Corrupt { .. }),
            "corrupt JSON must return Corrupt variant, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("corrupt") || msg.contains("unparseable"),
            "error message must mention corruption: {msg}"
        );
    }

    #[test]
    fn test_load_sidecar_wrong_schema_returns_corrupt() {
        // Valid JSON but wrong shape — serde will fail to deserialize.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-permissions.json");
        std::fs::write(&path, b"[\"not\", \"an\", \"object\"]").unwrap();

        let result = load_sidecar(&path);
        assert!(
            result.is_err(),
            "wrong-schema JSON must fail deserialization"
        );
        assert!(
            matches!(result.unwrap_err(), SidecarError::Corrupt { .. }),
            "wrong-schema must return Corrupt variant"
        );
    }

    // ---- oversized → Err(Oversized) ----

    #[test]
    fn test_load_sidecar_oversized_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-permissions.json");

        // Write a file that exceeds MAX_SETTINGS_SIZE using sparse file trick or
        // by setting the content length via metadata manipulation. Since we can't
        // easily create a real 10 MiB file in a unit test without I/O overhead,
        // we use the std::fs::File + seek trick to create a sparse file.
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::File::create(&path).unwrap();
            // Seek past MAX_SETTINGS_SIZE and write a single byte — creates a sparse
            // file (hole) on most Unix filesystems. The file metadata.len() will
            // report the full size, triggering the oversized guard.
            file.seek(SeekFrom::Start(MAX_SETTINGS_SIZE + 1)).unwrap();
            file.write_all(b"x").unwrap();
        }

        let result = load_sidecar(&path);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SidecarError::Oversized { .. }),
            "file exceeding MAX_SETTINGS_SIZE must return Oversized variant"
        );
    }

    // ---- write creates parent directories ----

    #[test]
    fn test_write_sidecar_creates_parent_directories() {
        let dir = tempfile::TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("skim-permissions.json");
        let sidecar = sample_sidecar();

        write_sidecar(&nested, &sidecar).unwrap();
        assert!(
            nested.exists(),
            "write_sidecar must create parent directories"
        );
    }

    // ---- write is atomic (tmp file cleaned up on success) ----

    #[test]
    fn test_write_sidecar_no_tmp_file_after_success() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-permissions.json");
        let sidecar = sample_sidecar();

        write_sidecar(&path, &sidecar).unwrap();

        let tmp_path = path.with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "tmp file must not remain after successful write"
        );
    }

    // ---- field preservation ----

    #[test]
    fn test_sidecar_version_preserved() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-permissions.json");
        let sidecar = PermissionSidecar {
            version: 1,
            tier: "blanket".to_string(),
            entries: vec![],
            source_mirrors: HashMap::new(),
            config_hash: "deadbeef".to_string(),
        };
        write_sidecar(&path, &sidecar).unwrap();
        let loaded = load_sidecar(&path).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.tier, "blanket");
        assert_eq!(loaded.config_hash, "deadbeef");
    }
}
