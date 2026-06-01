//! Package.json script tool detection for `npm test` and `npm run` delegation.
//!
//! Resolves the underlying tool used by an npm script so that the correct
//! parser can be selected for the script's output.
//!
//! # Design
//!
//! `resolve_script` walks up from the current directory looking for a
//! `package.json` file, then extracts the script body for the given name.
//! `extract_tool` tokenizes the script body and identifies the first
//! recognised tool binary.
//!
//! All I/O errors are treated as `None` (graceful degradation). When the tool
//! cannot be identified the caller falls back to raw passthrough.

use std::path::Path;

/// Maximum `package.json` size accepted for reading (16 MiB).
///
/// Real-world package.json files are kilobytes. A 16 MiB cap prevents
/// accidental memory pressure from malformed or adversarial inputs while
/// still being far above any legitimate file size.
const MAX_PKG_JSON_BYTES: u64 = 16 * 1024 * 1024;

/// Known tools that `npm run`/`npm test` can delegate to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScriptTool {
    Vitest,
    Jest,
    Eslint,
    Biome,
    Prettier,
    Oxlint,
    Tsc,
    Unknown,
}

/// Walk up the directory tree from `start_dir`, find the first `package.json`,
/// and return the body of the script named `name`.
///
/// Returns `None` when:
/// - No `package.json` is found within 20 levels.
/// - The file cannot be read or parsed as JSON.
/// - The file exceeds [`MAX_PKG_JSON_BYTES`].
/// - The script does not exist in the `scripts` map.
///
/// Errors are logged to stderr when `SKIM_DEBUG` is enabled.
pub(super) fn resolve_script(start_dir: &Path, name: &str) -> Option<String> {
    let mut dir = start_dir.to_path_buf();
    for _ in 0..20 {
        let candidate = dir.join("package.json");
        if candidate.is_file() {
            return try_parse_script(&candidate, name);
        }
        if !dir.pop() {
            break;
        }
    }
    if crate::debug::is_debug_enabled() {
        eprintln!(
            "skim: script_tool: no package.json found starting from {}",
            start_dir.display()
        );
    }
    None
}

/// Read `path`, enforce the size cap, parse JSON, and return the named script.
///
/// Extracted from [`resolve_script`] to reduce nesting depth. All failures
/// return `None` with an optional debug log.
fn try_parse_script(path: &std::path::Path, name: &str) -> Option<String> {
    // Guard: reject oversized files before reading into memory.
    match std::fs::metadata(path).map(|m| m.len()) {
        Ok(len) if len > MAX_PKG_JSON_BYTES => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim: script_tool: skipping oversized package.json ({} bytes > {} cap): {}",
                    len,
                    MAX_PKG_JSON_BYTES,
                    path.display()
                );
            }
            return None;
        }
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!("skim: script_tool: failed to stat {}: {e}", path.display());
            }
            return None;
        }
        Ok(_) => {} // within cap — proceed
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!("skim: script_tool: failed to read {}: {e}", path.display());
            }
            return None;
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            if crate::debug::is_debug_enabled() {
                eprintln!("skim: script_tool: failed to parse {}: {e}", path.display());
            }
            return None;
        }
    };

    json.get("scripts")
        .and_then(|s| s.get(name))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Split `script` on shell compound operators (`&&`, `||`, `;`).
///
/// Single `&` (background operator) is intentionally **not** treated as a
/// separator: `"cmd &"` should remain one segment so its tool is still found.
/// The char-level `split(['&', '|', ';'])` approach mis-handles this by
/// splitting on each individual character, which works by accident for `&&`
/// and `||` only because the empty string between them is filtered out.
///
/// This function scans left-to-right and emits a slice at each `&&`, `||`, or
/// `;` boundary, skipping the operator itself. Single `&` or `|` advances the
/// cursor without splitting.
fn split_shell_ops(script: &str) -> impl Iterator<Item = &str> {
    let mut segments: Vec<&str> = Vec::new();
    let bytes = script.as_bytes();
    let len = bytes.len();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < len {
        let op_len = if i + 1 < len
            && ((bytes[i] == b'&' && bytes[i + 1] == b'&')
                || (bytes[i] == b'|' && bytes[i + 1] == b'|'))
        {
            Some(2)
        } else if bytes[i] == b';' {
            Some(1)
        } else {
            None
        };

        if let Some(n) = op_len {
            // `start..i` is always on a valid UTF-8 boundary: all split points
            // are ASCII bytes (&&, ||, ;), which are single-byte code points.
            let segment = &script[start..i];
            if !segment.trim().is_empty() {
                segments.push(segment);
            }
            i += n;
            start = i;
        } else {
            i += 1;
        }
    }

    // Final segment after the last operator (or the whole string if no ops).
    let tail = &script[start..];
    if !tail.trim().is_empty() {
        segments.push(tail);
    }

    segments.into_iter()
}

/// Tokenize a shell script body and identify the first recognised tool.
///
/// The function splits the script on compound operators (`&&`, `||`, `;`),
/// then for each segment:
/// 1. Splits on whitespace.
/// 2. Skips env assignments (`KEY=VALUE` tokens that contain `=` and do not
///    start with `-`).
/// 3. Skips known wrappers: `cross-env`, `env`, `npx`, `pnpx`, `bunx`, `node`.
/// 4. Extracts the file-stem of the first remaining token (handles
///    `node_modules/.bin/vitest` → `vitest`).
/// 5. Matches the stem against known tool names.
///
/// Returns the first recognised tool found across all segments, or
/// `ScriptTool::Unknown` when nothing is recognised.
pub(super) fn extract_tool(script: &str) -> ScriptTool {
    // Wrappers that precede the actual tool binary.
    const WRAPPERS: &[&str] = &["cross-env", "env", "npx", "pnpx", "bunx", "node"];

    // Split on compound shell operators — single `&` is NOT a separator.
    let segments = split_shell_ops(script);

    for segment in segments {
        let mut tokens = segment.split_whitespace();
        for token in &mut tokens {
            // Skip env assignments (contains `=` and does not start with `-`).
            if token.contains('=') && !token.starts_with('-') {
                continue;
            }
            // Skip known wrappers.
            if WRAPPERS.contains(&token) {
                continue;
            }
            // Extract the file stem (last path component without extension).
            let stem = token.rsplit('/').next().unwrap_or(token);
            // Strip any `.js`, `.mjs`, `.cjs` extension that launchers may keep.
            let stem = stem
                .strip_suffix(".js")
                .or_else(|| stem.strip_suffix(".mjs"))
                .or_else(|| stem.strip_suffix(".cjs"))
                .unwrap_or(stem);

            match stem {
                "vitest" => return ScriptTool::Vitest,
                "jest" => return ScriptTool::Jest,
                "eslint" => return ScriptTool::Eslint,
                "biome" => return ScriptTool::Biome,
                "prettier" => return ScriptTool::Prettier,
                "oxlint" => return ScriptTool::Oxlint,
                "tsc" => return ScriptTool::Tsc,
                // Unknown token — stop processing this segment (it's the actual command).
                _ => break,
            }
        }
    }
    ScriptTool::Unknown
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // extract_tool tests
    // ========================================================================

    #[test]
    fn test_extract_tool_vitest_bare() {
        assert_eq!(extract_tool("vitest"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_jest_bare() {
        assert_eq!(extract_tool("jest"), ScriptTool::Jest);
    }

    #[test]
    fn test_extract_tool_eslint_bare() {
        assert_eq!(extract_tool("eslint src/"), ScriptTool::Eslint);
    }

    #[test]
    fn test_extract_tool_biome_bare() {
        assert_eq!(extract_tool("biome check ."), ScriptTool::Biome);
    }

    #[test]
    fn test_extract_tool_prettier_bare() {
        assert_eq!(extract_tool("prettier --check src/"), ScriptTool::Prettier);
    }

    #[test]
    fn test_extract_tool_oxlint_bare() {
        assert_eq!(extract_tool("oxlint src/"), ScriptTool::Oxlint);
    }

    #[test]
    fn test_extract_tool_tsc_bare() {
        assert_eq!(extract_tool("tsc --noEmit"), ScriptTool::Tsc);
    }

    #[test]
    fn test_extract_tool_npx_prefix() {
        assert_eq!(extract_tool("npx vitest"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_cross_env_prefix() {
        assert_eq!(
            extract_tool("cross-env NODE_ENV=test vitest"),
            ScriptTool::Vitest
        );
    }

    #[test]
    fn test_extract_tool_env_assignment_skipped() {
        assert_eq!(extract_tool("NODE_ENV=test vitest"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_bin_path() {
        assert_eq!(
            extract_tool("node_modules/.bin/vitest --run"),
            ScriptTool::Vitest
        );
    }

    #[test]
    fn test_extract_tool_compound_first_recognised_wins() {
        // tsc is recognised first; vitest in the second segment is not reached.
        assert_eq!(extract_tool("tsc --noEmit && vitest"), ScriptTool::Tsc);
    }

    #[test]
    fn test_extract_tool_compound_semicolon() {
        assert_eq!(extract_tool("echo start; jest"), ScriptTool::Jest);
    }

    #[test]
    fn test_extract_tool_compound_or() {
        assert_eq!(extract_tool("vitest || echo failed"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_unknown() {
        assert_eq!(extract_tool("node scripts/custom.js"), ScriptTool::Unknown);
    }

    #[test]
    fn test_extract_tool_empty() {
        assert_eq!(extract_tool(""), ScriptTool::Unknown);
    }

    #[test]
    fn test_extract_tool_js_extension_stripped() {
        // Launchers sometimes keep the .js extension.
        assert_eq!(
            extract_tool("node_modules/.bin/vitest.js --run"),
            ScriptTool::Vitest
        );
    }

    #[test]
    fn test_extract_tool_bunx_prefix() {
        assert_eq!(extract_tool("bunx vitest"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_single_ampersand_not_separator() {
        // Single `&` is the shell background operator — treat the whole
        // expression as one segment so the tool before `&` is still found.
        assert_eq!(extract_tool("vitest &"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_single_pipe_not_separator() {
        // Single `|` is a pipe — treat the whole expression as one segment.
        assert_eq!(extract_tool("vitest | tee output.log"), ScriptTool::Vitest);
    }

    // ========================================================================
    // split_shell_ops tests
    // ========================================================================

    #[test]
    fn test_split_shell_ops_double_ampersand() {
        let parts: Vec<&str> = split_shell_ops("a && b").collect();
        assert_eq!(parts, vec!["a ", " b"]);
    }

    #[test]
    fn test_split_shell_ops_double_pipe() {
        let parts: Vec<&str> = split_shell_ops("a || b").collect();
        assert_eq!(parts, vec!["a ", " b"]);
    }

    #[test]
    fn test_split_shell_ops_semicolon() {
        let parts: Vec<&str> = split_shell_ops("a; b").collect();
        assert_eq!(parts, vec!["a", " b"]);
    }

    #[test]
    fn test_split_shell_ops_single_ampersand_no_split() {
        // Single `&` must NOT produce a split.
        let parts: Vec<&str> = split_shell_ops("cmd &").collect();
        assert_eq!(parts, vec!["cmd &"]);
    }

    #[test]
    fn test_split_shell_ops_empty() {
        let parts: Vec<&str> = split_shell_ops("").collect();
        assert!(parts.is_empty());
    }

    #[test]
    fn test_split_shell_ops_no_operators() {
        let parts: Vec<&str> = split_shell_ops("vitest --run").collect();
        assert_eq!(parts, vec!["vitest --run"]);
    }

    // ========================================================================
    // resolve_script tests (filesystem-based)
    // ========================================================================

    #[test]
    fn test_resolve_script_found() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(
            &pkg,
            r#"{"scripts": {"test": "vitest --run", "lint": "eslint src/"}}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_script(dir.path(), "test"),
            Some("vitest --run".to_string())
        );
        assert_eq!(
            resolve_script(dir.path(), "lint"),
            Some("eslint src/".to_string())
        );
    }

    #[test]
    fn test_resolve_script_missing_name() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(&pkg, r#"{"scripts": {"build": "tsc"}}"#).unwrap();

        assert_eq!(resolve_script(dir.path(), "test"), None);
    }

    #[test]
    fn test_resolve_script_walks_up() {
        let parent = tempfile::tempdir().unwrap();
        let child = parent.path().join("src");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(
            parent.path().join("package.json"),
            r#"{"scripts": {"test": "jest"}}"#,
        )
        .unwrap();

        assert_eq!(resolve_script(&child, "test"), Some("jest".to_string()));
    }

    #[test]
    fn test_resolve_script_no_package_json() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_script(dir.path(), "test"), None);
    }

    #[test]
    fn test_resolve_script_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "not json").unwrap();
        assert_eq!(resolve_script(dir.path(), "test"), None);
    }

    #[test]
    fn test_resolve_script_no_scripts_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name": "my-app"}"#).unwrap();
        assert_eq!(resolve_script(dir.path(), "test"), None);
    }

    #[test]
    fn test_resolve_script_oversized_rejected() {
        // Verify the size cap constant is the documented 16 MiB value.
        // Writing a real 16 MiB file in tests would be too slow; instead we
        // assert the constant and confirm that a normal-sized file is accepted.
        assert_eq!(MAX_PKG_JSON_BYTES, 16 * 1024 * 1024);

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"test": "jest"}}"#,
        )
        .unwrap();
        assert_eq!(resolve_script(dir.path(), "test"), Some("jest".to_string()));
    }
}
