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
/// - The script does not exist in the `scripts` map.
///
/// Errors are logged to stderr when `SKIM_DEBUG` is enabled.
pub(super) fn resolve_script(start_dir: &Path, name: &str) -> Option<String> {
    let mut dir = start_dir.to_path_buf();
    for _ in 0..20 {
        let candidate = dir.join("package.json");
        if candidate.is_file() {
            let text = match std::fs::read_to_string(&candidate) {
                Ok(t) => t,
                Err(e) => {
                    if crate::debug::is_debug_enabled() {
                        eprintln!(
                            "skim: script_tool: failed to read {}: {e}",
                            candidate.display()
                        );
                    }
                    return None;
                }
            };
            let json: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    if crate::debug::is_debug_enabled() {
                        eprintln!(
                            "skim: script_tool: failed to parse {}: {e}",
                            candidate.display()
                        );
                    }
                    return None;
                }
            };
            return json
                .get("scripts")
                .and_then(|s| s.get(name))
                .and_then(|v| v.as_str())
                .map(str::to_string);
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

    // Split on compound shell operators.
    let segments: Vec<&str> = script
        .split(['&', '|', ';'])
        .filter(|s| !s.trim().is_empty())
        .collect();

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

            let tool = match stem {
                "vitest" => ScriptTool::Vitest,
                "jest" => ScriptTool::Jest,
                "eslint" => ScriptTool::Eslint,
                "biome" => ScriptTool::Biome,
                "prettier" => ScriptTool::Prettier,
                "oxlint" => ScriptTool::Oxlint,
                "tsc" => ScriptTool::Tsc,
                _ => ScriptTool::Unknown,
            };

            if tool != ScriptTool::Unknown {
                return tool;
            }
            // Unknown token — stop processing this segment (it's the actual command).
            break;
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
        assert_eq!(extract_tool("cross-env NODE_ENV=test vitest"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_env_assignment_skipped() {
        assert_eq!(extract_tool("NODE_ENV=test vitest"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_bin_path() {
        assert_eq!(extract_tool("node_modules/.bin/vitest --run"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_compound_and_second_segment() {
        // First segment is unknown, second segment has vitest.
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
        assert_eq!(extract_tool("node_modules/.bin/vitest.js --run"), ScriptTool::Vitest);
    }

    #[test]
    fn test_extract_tool_bunx_prefix() {
        assert_eq!(extract_tool("bunx vitest"), ScriptTool::Vitest);
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

        assert_eq!(
            resolve_script(&child, "test"),
            Some("jest".to_string())
        );
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
}
