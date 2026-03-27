//! Copilot CLI session provider.
//!
//! Parses Copilot CLI timeline JSONL session files from `~/.copilot/session-state/`.
//! Session files may optionally contain a YAML metadata header (delimited by `---`)
//! followed by JSONL tool events.

use std::collections::HashMap;
use std::path::PathBuf;

use super::types::{AgentKind, SessionFile, TimeFilter, ToolInput, ToolInvocation, ToolResult};
use super::SessionProvider;

/// Maximum session file size: 100 MB.
const MAX_SESSION_SIZE: u64 = 100 * 1024 * 1024;

/// Copilot CLI session file provider.
pub(crate) struct CopilotCliProvider {
    sessions_dir: PathBuf,
}

impl CopilotCliProvider {
    /// Detect Copilot CLI by checking if the session directory exists.
    ///
    /// Uses `SKIM_COPILOT_DIR` env var override for testability.
    pub(crate) fn detect() -> Option<Self> {
        let sessions_dir = if let Ok(override_dir) = std::env::var("SKIM_COPILOT_DIR") {
            PathBuf::from(override_dir)
        } else {
            dirs::home_dir()?.join(".copilot").join("session-state")
        };

        if sessions_dir.is_dir() {
            Some(Self { sessions_dir })
        } else {
            None
        }
    }
}

impl SessionProvider for CopilotCliProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::CopilotCli
    }

    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>> {
        let mut sessions = Vec::new();

        // Canonicalize sessions_dir to prevent symlink traversal outside boundary
        let canonical_root = self
            .sessions_dir
            .canonicalize()
            .unwrap_or_else(|_| self.sessions_dir.clone());

        let entries = std::fs::read_dir(&self.sessions_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();

            // Accept .jsonl files
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            // Verify resolved path stays within the session directory (symlink traversal guard)
            if let Ok(canonical_path) = path.canonicalize() {
                if !canonical_path.starts_with(&canonical_root) {
                    eprintln!(
                        "warning: skipping file outside session dir: {}",
                        path.display()
                    );
                    continue;
                }
            }

            let modified = match std::fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!(
                        "warning: could not read metadata for {}: {}",
                        path.display(),
                        e
                    );
                    continue;
                }
            };

            // Apply time filter
            if let Some(since) = filter.since {
                if modified < since {
                    continue;
                }
            }

            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            sessions.push(SessionFile {
                path,
                modified,
                agent: AgentKind::CopilotCli,
                session_id,
            });
        }

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
        parse_copilot_jsonl(&content, &file.session_id)
    }
}

/// Skip optional YAML header, returning only the JSONL body.
///
/// If the first non-empty line is `---`, scans forward until the closing
/// `---` delimiter and returns the content after it. Otherwise returns
/// the original content unchanged.
fn skip_yaml_header(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }

    // Find the first `---` line
    let after_first = match trimmed.strip_prefix("---") {
        Some(rest) => rest.trim_start_matches(['\r', ' ', '\t']),
        None => return content,
    };

    // Skip leading newline after first ---
    let after_first = after_first.strip_prefix('\n').unwrap_or(after_first);

    // Find the closing `---`
    if let Some(end_idx) = after_first.find("\n---") {
        let rest_start = end_idx + 4; // skip "\n---"
        if rest_start < after_first.len() {
            &after_first[rest_start..]
        } else {
            ""
        }
    } else {
        // No closing `---` found; treat entire content as JSONL (no valid header)
        content
    }
}

/// Parse Copilot CLI JSONL content into tool invocations.
///
/// Handles optional YAML header, then parses timeline events:
/// - `tool_use` events create invocations
/// - `tool_result` events are correlated by `toolUseId` -> `id`
fn parse_copilot_jsonl(content: &str, session_id: &str) -> anyhow::Result<Vec<ToolInvocation>> {
    let jsonl_body = skip_yaml_header(content);

    let mut invocations = Vec::new();
    // Map from tool id to index in invocations vec for result correlation
    let mut pending: HashMap<String, usize> = HashMap::new();

    for line in jsonl_body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines
        };

        let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let timestamp = json
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        match event_type {
            "tool_use" => {
                let tool_id = json
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = json
                    .get("toolName")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_args = json
                    .get("toolArgs")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                let input = parse_copilot_tool_input(&tool_name, &tool_args);

                let idx = invocations.len();
                invocations.push(ToolInvocation {
                    tool_name: tool_name.clone(),
                    input,
                    timestamp,
                    session_id: session_id.to_string(),
                    agent: AgentKind::CopilotCli,
                    result: None,
                });

                if !tool_id.is_empty() {
                    pending.insert(tool_id, idx);
                }
            }
            "tool_result" => {
                let tool_use_id = json
                    .get("toolUseId")
                    .and_then(|id| id.as_str())
                    .unwrap_or("");

                if let Some(&idx) = pending.get(tool_use_id) {
                    let result_content = json
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    let result_type = json
                        .get("resultType")
                        .and_then(|r| r.as_str())
                        .unwrap_or("success");
                    let is_error = result_type == "error";

                    invocations[idx].result = Some(ToolResult {
                        content: result_content,
                        is_error,
                    });
                    pending.remove(tool_use_id);
                }
            }
            _ => {} // skip unknown event types
        }
    }

    Ok(invocations)
}

/// Map Copilot CLI tool names to normalized ToolInput enum.
fn parse_copilot_tool_input(tool_name: &str, args: &serde_json::Value) -> ToolInput {
    match tool_name {
        "bash" => {
            let command = args
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Bash { command }
        }
        "readFile" => {
            let file_path = args
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Read { file_path }
        }
        "writeFile" => {
            let file_path = args
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Write { file_path }
        }
        "editFile" => {
            let file_path = args
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Edit { file_path }
        }
        _ => ToolInput::Other {
            tool_name: tool_name.to_string(),
            raw: args.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- JSONL parsing without YAML header ----

    #[test]
    fn test_parse_jsonl_without_yaml_header() {
        let content = concat!(
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "cargo test"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#,
            "\n",
            r#"{ "type": "tool_result", "toolUseId": "t-001", "resultType": "success", "content": "ok", "timestamp": "2024-06-15T10:01:05Z" }"#,
        );
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "bash");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "cargo test"
        ));
        assert!(invocations[0].result.is_some());
        assert_eq!(invocations[0].result.as_ref().unwrap().content, "ok");
        assert!(!invocations[0].result.as_ref().unwrap().is_error);
    }

    // ---- JSONL parsing with YAML header ----

    #[test]
    fn test_parse_jsonl_with_yaml_header() {
        let content = concat!(
            "---\n",
            "model: gpt-4o\n",
            "session_start: \"2024-06-15T10:00:00Z\"\n",
            "---\n",
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "ls"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#,
            "\n",
        );
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "bash");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "ls"
        ));
    }

    // ---- Tool result correlation ----

    #[test]
    fn test_correlate_tool_result() {
        let content = concat!(
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "echo hi"}, "id": "t-010", "timestamp": "2024-06-15T10:01:00Z" }"#,
            "\n",
            r#"{ "type": "tool_result", "toolUseId": "t-010", "resultType": "success", "content": "hi", "timestamp": "2024-06-15T10:01:01Z" }"#,
        );
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.is_some());
        assert_eq!(invocations[0].result.as_ref().unwrap().content, "hi");
        assert!(!invocations[0].result.as_ref().unwrap().is_error);
    }

    // ---- Error result type ----

    #[test]
    fn test_result_type_error() {
        let content = concat!(
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "false"}, "id": "t-020", "timestamp": "2024-06-15T10:01:00Z" }"#,
            "\n",
            r#"{ "type": "tool_result", "toolUseId": "t-020", "resultType": "error", "content": "command failed", "timestamp": "2024-06-15T10:01:01Z" }"#,
        );
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.as_ref().unwrap().is_error);
        assert_eq!(
            invocations[0].result.as_ref().unwrap().content,
            "command failed"
        );
    }

    // ---- Skip malformed lines ----

    #[test]
    fn test_skip_malformed_lines() {
        let content = "not json\n{}\n";
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    // ---- Empty input ----

    #[test]
    fn test_empty_input() {
        let invocations = parse_copilot_jsonl("", "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    // ---- Multiple tools ----

    #[test]
    fn test_multiple_tools() {
        let content = concat!(
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "cargo test"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#,
            "\n",
            r#"{ "type": "tool_result", "toolUseId": "t-001", "resultType": "success", "content": "ok", "timestamp": "2024-06-15T10:01:05Z" }"#,
            "\n",
            r#"{ "type": "tool_use", "toolName": "readFile", "toolArgs": {"path": "/tmp/main.rs"}, "id": "t-002", "timestamp": "2024-06-15T10:02:00Z" }"#,
            "\n",
            r#"{ "type": "tool_result", "toolUseId": "t-002", "resultType": "success", "content": "fn main() {}", "timestamp": "2024-06-15T10:02:01Z" }"#,
            "\n",
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "git status"}, "id": "t-003", "timestamp": "2024-06-15T10:03:00Z" }"#,
        );
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 3);

        // First: bash with result
        assert_eq!(invocations[0].tool_name, "bash");
        assert!(invocations[0].result.is_some());

        // Second: readFile mapped to Read
        assert_eq!(invocations[1].tool_name, "readFile");
        assert!(
            matches!(&invocations[1].input, ToolInput::Read { file_path } if file_path == "/tmp/main.rs")
        );
        assert!(invocations[1].result.is_some());

        // Third: bash without result (no matching tool_result)
        assert_eq!(invocations[2].tool_name, "bash");
        assert!(invocations[2].result.is_none());
    }

    // ---- YAML header skipping ----

    #[test]
    fn test_skip_yaml_header() {
        let content = concat!(
            "---\n",
            "model: gpt-4o\n",
            "session_start: \"2024-06-15T10:00:00Z\"\n",
            "project: \"/home/user/myproject\"\n",
            "---\n",
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "echo test"}, "id": "t-100", "timestamp": "2024-06-15T10:05:00Z" }"#,
        );

        let body = skip_yaml_header(content);
        // Body should contain the JSONL events, not the YAML header
        assert!(!body.is_empty());
        assert!(!body.contains("model: gpt-4o"));

        // Full parse from original content works
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
    }

    // ---- Tool input mapping ----

    #[test]
    fn test_tool_input_bash() {
        let args = serde_json::json!({"command": "cargo build"});
        let input = parse_copilot_tool_input("bash", &args);
        assert!(matches!(input, ToolInput::Bash { command } if command == "cargo build"));
    }

    #[test]
    fn test_tool_input_read_file() {
        let args = serde_json::json!({"path": "/tmp/test.rs"});
        let input = parse_copilot_tool_input("readFile", &args);
        assert!(matches!(input, ToolInput::Read { file_path } if file_path == "/tmp/test.rs"));
    }

    #[test]
    fn test_tool_input_write_file() {
        let args = serde_json::json!({"path": "/tmp/out.rs"});
        let input = parse_copilot_tool_input("writeFile", &args);
        assert!(matches!(input, ToolInput::Write { file_path } if file_path == "/tmp/out.rs"));
    }

    #[test]
    fn test_tool_input_edit_file() {
        let args = serde_json::json!({"path": "/tmp/edit.rs"});
        let input = parse_copilot_tool_input("editFile", &args);
        assert!(matches!(input, ToolInput::Edit { file_path } if file_path == "/tmp/edit.rs"));
    }

    #[test]
    fn test_tool_input_unknown() {
        let args = serde_json::json!({"foo": "bar"});
        let input = parse_copilot_tool_input("unknownTool", &args);
        assert!(matches!(input, ToolInput::Other { tool_name, .. } if tool_name == "unknownTool"));
    }

    // ---- Agent kind ----

    #[test]
    fn test_agent_kind_is_copilot() {
        let content = r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "ls"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#;
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].agent, AgentKind::CopilotCli);
    }

    // ---- Session ID propagation ----

    #[test]
    fn test_session_id_propagation() {
        let content = r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "ls"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#;
        let invocations = parse_copilot_jsonl(content, "my-session-42").unwrap();
        assert_eq!(invocations[0].session_id, "my-session-42");
    }

    // ---- Timestamp propagation ----

    #[test]
    fn test_timestamp_propagation() {
        let content = r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "ls"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#;
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations[0].timestamp, "2024-06-15T10:01:00Z");
    }

    // ---- No closing YAML delimiter ----

    #[test]
    fn test_yaml_header_no_closing_delimiter() {
        // If there's no closing `---`, treat entire content as JSONL
        let content = concat!(
            "---\n",
            "model: gpt-4o\n",
            r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "ls"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#,
        );
        let body = skip_yaml_header(content);
        // Without closing delimiter, returns original content
        assert_eq!(body, content);

        // Full parse should still attempt to parse lines (malformed YAML lines will be skipped)
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        // The `---` and `model:` lines are not valid JSON, so they get skipped.
        // The tool_use line is valid JSON and should parse.
        assert_eq!(invocations.len(), 1);
    }

    // ---- Uncorrelated result is ignored ----

    #[test]
    fn test_uncorrelated_result_ignored() {
        let content = r#"{ "type": "tool_result", "toolUseId": "nonexistent", "resultType": "success", "content": "orphan", "timestamp": "2024-06-15T10:01:00Z" }"#;
        let invocations = parse_copilot_jsonl(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }
}
