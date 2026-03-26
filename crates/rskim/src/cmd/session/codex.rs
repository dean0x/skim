//! Codex CLI session provider.
//!
//! Parses Codex CLI event-stream JSONL session files from `~/.codex/sessions/`.
//! Directory structure: `YYYY/MM/DD/rollout-*.jsonl`.

use std::collections::HashMap;
use std::path::PathBuf;

use super::types::*;
use super::SessionProvider;

/// Codex CLI session file provider.
pub(crate) struct CodexCliProvider {
    sessions_dir: PathBuf,
}

impl CodexCliProvider {
    /// Detect Codex CLI by checking if the sessions directory exists.
    ///
    /// Uses `SKIM_CODEX_SESSIONS_DIR` env var override for testability.
    pub(crate) fn detect() -> Option<Self> {
        let sessions_dir = if let Ok(override_dir) = std::env::var("SKIM_CODEX_SESSIONS_DIR") {
            PathBuf::from(override_dir)
        } else {
            AgentKind::CodexCli.config_dir(&dirs::home_dir()?).join("sessions")
        };

        if sessions_dir.is_dir() {
            Some(Self { sessions_dir })
        } else {
            None
        }
    }
}

/// Depth of the Codex YYYY/MM/DD/files directory structure.
const CODEX_DIR_DEPTH: usize = 4;

/// Recursively collect `rollout-*.jsonl` files from the YYYY/MM/DD directory structure.
///
/// At `depth < CODEX_DIR_DEPTH`, recurses into subdirectories.
/// At `depth == CODEX_DIR_DEPTH`, collects matching files with symlink guard.
fn collect_codex_files(
    dir: &std::path::Path,
    depth: usize,
    canonical_root: &std::path::Path,
) -> Vec<(PathBuf, std::time::SystemTime)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();

        if depth < CODEX_DIR_DEPTH {
            // Intermediate level — recurse into subdirectories only
            if path.is_dir() {
                results.extend(collect_codex_files(&path, depth + 1, canonical_root));
            }
        } else {
            // Leaf level — collect rollout-*.jsonl files
            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name,
                None => continue,
            };
            if !file_name.starts_with("rollout-")
                || path.extension().and_then(|e| e.to_str()) != Some("jsonl")
            {
                continue;
            }

            // Symlink traversal guard
            if let Ok(canonical_path) = path.canonicalize() {
                if !canonical_path.starts_with(canonical_root) {
                    continue;
                }
            }

            if let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                results.push((path, modified));
            }
        }
    }
    results
}

impl SessionProvider for CodexCliProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::CodexCli
    }

    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>> {
        if !self.sessions_dir.is_dir() {
            return Ok(Vec::new());
        }

        // Canonicalize sessions_dir to prevent symlink traversal outside boundary
        let canonical_root = self
            .sessions_dir
            .canonicalize()
            .unwrap_or_else(|_| self.sessions_dir.clone());

        // Collect all matching files from YYYY/MM/DD structure
        let files = collect_codex_files(&self.sessions_dir, 1, &canonical_root);

        // Filter by time, map to SessionFile, sort, truncate
        let mut sessions: Vec<SessionFile> = files
            .into_iter()
            .filter(|(_, modified)| filter.since.is_none_or(|since| *modified >= since))
            .map(|(path, modified)| {
                let session_id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                SessionFile {
                    path,
                    modified,
                    agent: AgentKind::CodexCli,
                    session_id,
                }
            })
            .collect();

        // Sort by modification time (newest first)
        sessions.sort_by(|a, b| b.modified.cmp(&a.modified));

        // Apply latest_only filter
        if filter.latest_only {
            sessions.truncate(1);
        }

        Ok(sessions)
    }

    fn parse_session(&self, file: &SessionFile) -> anyhow::Result<Vec<ToolInvocation>> {
        // Guard against unbounded reads -- reject files over 100 MB
        const MAX_SESSION_SIZE: u64 = 100 * 1024 * 1024;
        let file_size = std::fs::metadata(&file.path)?.len();
        if file_size > MAX_SESSION_SIZE {
            anyhow::bail!(
                "session file too large ({:.1} MB, limit {:.0} MB): {}",
                file_size as f64 / (1024.0 * 1024.0),
                MAX_SESSION_SIZE as f64 / (1024.0 * 1024.0),
                file.path.display()
            );
        }

        let content = std::fs::read_to_string(&file.path)?;
        parse_codex_jsonl(&content, &file.session_id)
    }
}

/// Parse Codex CLI JSONL content into tool invocations.
///
/// Correlates `codex.tool_decision` events with `codex.tool_result` events
/// by matching `tool_decision_id` fields.
fn parse_codex_jsonl(content: &str, session_id: &str) -> anyhow::Result<Vec<ToolInvocation>> {
    let mut invocations = Vec::new();
    // Map from tool_decision_id to index in invocations vec for result correlation
    let mut pending: HashMap<String, usize> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines gracefully
        };

        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let timestamp = json
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        match event_type {
            "codex.tool_decision" => {
                let tool_name = json
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = json.get("args").cloned().unwrap_or(serde_json::Value::Null);
                let tool_decision_id = json
                    .get("tool_decision_id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("")
                    .to_string();

                let input = parse_codex_tool_input(&tool_name, &args);

                let idx = invocations.len();
                invocations.push(ToolInvocation {
                    tool_name: tool_name.clone(),
                    input,
                    timestamp,
                    session_id: session_id.to_string(),
                    agent: AgentKind::CodexCli,
                    result: None,
                });

                if !tool_decision_id.is_empty() {
                    pending.insert(tool_decision_id, idx);
                }
            }
            "codex.tool_result" => {
                let tool_decision_id = json
                    .get("tool_decision_id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("");

                if let Some(&idx) = pending.get(tool_decision_id) {
                    let result_content = json
                        .get("result")
                        .and_then(|r| r.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    let is_error = json
                        .get("result")
                        .and_then(|r| r.get("is_error"))
                        .and_then(|e| e.as_bool())
                        .unwrap_or(false);

                    invocations[idx].result = Some(ToolResult {
                        content: result_content,
                        is_error,
                    });
                    pending.remove(tool_decision_id);
                }
            }
            _ => {} // skip unknown event types
        }
    }

    Ok(invocations)
}

/// Map Codex CLI tool names to normalized ToolInput enum.
///
/// Codex uses lowercase tool names: "bash", "read", "write", "edit", "glob", "grep".
fn parse_codex_tool_input(tool_name: &str, args: &serde_json::Value) -> ToolInput {
    match tool_name {
        "bash" => {
            let command = args
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Bash { command }
        }
        "read" => {
            let file_path = args
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Read { file_path }
        }
        "write" => {
            let file_path = args
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Write { file_path }
        }
        "edit" => {
            let file_path = args
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Edit { file_path }
        }
        "glob" => {
            let pattern = args
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Glob { pattern }
        }
        "grep" => {
            let pattern = args
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Grep { pattern }
        }
        _ => ToolInput::Other {
            tool_name: tool_name.to_string(),
            raw: args.clone(),
        },
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_decision_bash() {
        let jsonl = r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"cargo test"},"timestamp":"2026-03-01T10:00:00Z","session_id":"sess-abc","tool_decision_id":"td-001"}"#;
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "bash");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "cargo test"
        ));
        assert_eq!(invocations[0].agent, AgentKind::CodexCli);
        assert_eq!(invocations[0].timestamp, "2026-03-01T10:00:00Z");
    }

    #[test]
    fn test_parse_tool_decision_read() {
        let jsonl = r#"{"type":"codex.tool_decision","tool":"read","args":{"file_path":"/tmp/main.rs"},"timestamp":"2026-03-01T10:00:02Z","session_id":"sess-abc","tool_decision_id":"td-002"}"#;
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "read");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Read { file_path } if file_path == "/tmp/main.rs"
        ));
    }

    #[test]
    fn test_correlate_tool_result() {
        let jsonl = concat!(
            r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"cargo test"},"timestamp":"2026-03-01T10:00:00Z","session_id":"sess-abc","tool_decision_id":"td-001"}"#,
            "\n",
            r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"test result: ok","is_error":false},"timestamp":"2026-03-01T10:00:01Z","session_id":"sess-abc","tool_decision_id":"td-001"}"#
        );
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.is_some());
        let result = invocations[0].result.as_ref().unwrap();
        assert_eq!(result.content, "test result: ok");
        assert!(!result.is_error);
    }

    #[test]
    fn test_skip_malformed_lines() {
        let jsonl = "not json\n{}\n";
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    #[test]
    fn test_empty_input() {
        let invocations = parse_codex_jsonl("", "sess-abc").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    #[test]
    fn test_multiple_tools_in_session() {
        let jsonl = concat!(
            r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"cargo test"},"timestamp":"2026-03-01T10:00:00Z","session_id":"sess-abc","tool_decision_id":"td-001"}"#,
            "\n",
            r#"{"type":"codex.tool_decision","tool":"read","args":{"file_path":"/tmp/main.rs"},"timestamp":"2026-03-01T10:00:02Z","session_id":"sess-abc","tool_decision_id":"td-002"}"#,
            "\n",
            r#"{"type":"codex.tool_decision","tool":"write","args":{"file_path":"/tmp/out.rs"},"timestamp":"2026-03-01T10:00:04Z","session_id":"sess-abc","tool_decision_id":"td-003"}"#
        );
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 3);
        assert_eq!(invocations[0].tool_name, "bash");
        assert_eq!(invocations[1].tool_name, "read");
        assert_eq!(invocations[2].tool_name, "write");
    }

    #[test]
    fn test_tool_result_with_error() {
        let jsonl = concat!(
            r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"git diff"},"timestamp":"2026-03-01T10:00:04Z","session_id":"sess-abc","tool_decision_id":"td-003"}"#,
            "\n",
            r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"error: not a git repository","is_error":true},"timestamp":"2026-03-01T10:00:05Z","session_id":"sess-abc","tool_decision_id":"td-003"}"#
        );
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.is_some());
        let result = invocations[0].result.as_ref().unwrap();
        assert_eq!(result.content, "error: not a git repository");
        assert!(result.is_error);
    }

    #[test]
    fn test_uncorrelated_result_ignored() {
        // A tool_result with no matching tool_decision should not crash
        let jsonl = r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"orphan","is_error":false},"timestamp":"2026-03-01T10:00:05Z","session_id":"sess-abc","tool_decision_id":"td-999"}"#;
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    #[test]
    fn test_parse_codex_tool_input_variants() {
        let write_args = serde_json::json!({"file_path": "/tmp/out.rs"});
        let result = parse_codex_tool_input("write", &write_args);
        assert!(matches!(result, ToolInput::Write { file_path } if file_path == "/tmp/out.rs"));

        let edit_args = serde_json::json!({"file_path": "/tmp/edit.rs"});
        let result = parse_codex_tool_input("edit", &edit_args);
        assert!(matches!(result, ToolInput::Edit { file_path } if file_path == "/tmp/edit.rs"));

        let glob_args = serde_json::json!({"pattern": "**/*.rs"});
        let result = parse_codex_tool_input("glob", &glob_args);
        assert!(matches!(result, ToolInput::Glob { pattern } if pattern == "**/*.rs"));

        let grep_args = serde_json::json!({"pattern": "fn main"});
        let result = parse_codex_tool_input("grep", &grep_args);
        assert!(matches!(result, ToolInput::Grep { pattern } if pattern == "fn main"));

        let other_args = serde_json::json!({"foo": "bar"});
        let result = parse_codex_tool_input("unknown_tool", &other_args);
        assert!(
            matches!(result, ToolInput::Other { tool_name, .. } if tool_name == "unknown_tool")
        );
    }

    #[test]
    fn test_decision_without_id_skips_correlation() {
        // A tool_decision without tool_decision_id should still be parsed,
        // but results won't correlate (the empty-string key won't match).
        let jsonl = concat!(
            r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"echo hi"},"timestamp":"2026-03-01T10:00:00Z","session_id":"sess-abc"}"#,
            "\n",
            r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"hi","is_error":false},"timestamp":"2026-03-01T10:00:01Z","session_id":"sess-abc","tool_decision_id":"td-001"}"#
        );
        let invocations = parse_codex_jsonl(jsonl, "sess-abc").unwrap();
        assert_eq!(invocations.len(), 1);
        // Result should NOT be correlated since the decision had no tool_decision_id
        // (empty string key won't match "td-001")
        assert!(invocations[0].result.is_none());
    }

    // ========================================================================
    // collect_codex_files recursive helper (TD-1)
    // ========================================================================

    #[test]
    fn test_collect_codex_files_date_structure() {
        let dir = tempfile::TempDir::new().unwrap();
        // Canonicalize to handle macOS /var -> /private/var symlink
        let root = dir.path().canonicalize().unwrap();
        // Create YYYY/MM/DD structure with a rollout file
        let day_dir = root.join("2026").join("03").join("26");
        std::fs::create_dir_all(&day_dir).unwrap();
        std::fs::write(day_dir.join("rollout-abc.jsonl"), "{}").unwrap();
        // Also add a non-matching file
        std::fs::write(day_dir.join("other.txt"), "nope").unwrap();

        let files = collect_codex_files(&root, 1, &root);
        assert_eq!(files.len(), 1);
        assert!(files[0].0.ends_with("rollout-abc.jsonl"));
    }

    #[test]
    fn test_collect_codex_files_ignores_wrong_depth() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // File at depth 2 (YYYY/rollout-*.jsonl) — should NOT be collected
        let year_dir = root.join("2026");
        std::fs::create_dir_all(&year_dir).unwrap();
        std::fs::write(year_dir.join("rollout-orphan.jsonl"), "{}").unwrap();

        let files = collect_codex_files(&root, 1, &root);
        assert!(files.is_empty(), "files at wrong depth should be ignored");
    }
}
