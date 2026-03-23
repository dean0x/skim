//! Claude Code session provider (#61)
//!
//! Parses Claude Code JSONL session files from `~/.claude/projects/<slug>/`.

use std::collections::HashMap;
use std::path::PathBuf;

use super::types::*;
use super::SessionProvider;

/// Claude Code session file provider.
pub(crate) struct ClaudeCodeProvider {
    projects_dir: PathBuf,
}

impl ClaudeCodeProvider {
    /// Detect Claude Code by checking if the projects directory exists.
    ///
    /// Uses `SKIM_PROJECTS_DIR` env var override for testability.
    pub(crate) fn detect() -> Option<Self> {
        let projects_dir = if let Ok(override_dir) = std::env::var("SKIM_PROJECTS_DIR") {
            PathBuf::from(override_dir)
        } else {
            dirs::home_dir()?.join(".claude").join("projects")
        };

        if projects_dir.is_dir() {
            Some(Self { projects_dir })
        } else {
            None
        }
    }
}

impl SessionProvider for ClaudeCodeProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>> {
        let mut sessions = Vec::new();

        // Canonicalize projects_dir to prevent symlink traversal outside boundary
        let canonical_root = self.projects_dir.canonicalize().unwrap_or_else(|_| self.projects_dir.clone());

        // Read project directories
        let entries = std::fs::read_dir(&self.projects_dir)?;
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            // Scan .jsonl files in each project dir
            if let Ok(files) = std::fs::read_dir(entry.path()) {
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }

                    // Verify resolved path stays within the projects directory (symlink traversal guard)
                    if let Ok(canonical_path) = path.canonicalize() {
                        if !canonical_path.starts_with(&canonical_root) {
                            eprintln!(
                                "warning: skipping file outside projects dir: {}",
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
                        agent: AgentKind::ClaudeCode,
                        session_id,
                    });
                }
            }
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
        const MAX_SESSION_SIZE: u64 = 100 * 1024 * 1024;
        let file_size = std::fs::metadata(&file.path)
            .map(|m| m.len())
            .unwrap_or(0);
        if file_size > MAX_SESSION_SIZE {
            anyhow::bail!(
                "session file too large ({:.1} MB, limit {:.0} MB): {}",
                file_size as f64 / (1024.0 * 1024.0),
                MAX_SESSION_SIZE as f64 / (1024.0 * 1024.0),
                file.path.display()
            );
        }

        let content = std::fs::read_to_string(&file.path)?;
        parse_claude_jsonl(&content, &file.session_id)
    }
}

/// Parse Claude Code JSONL content into tool invocations.
///
/// Correlates tool_use (in assistant messages) with tool_result (in user messages)
/// by matching tool_use.id to tool_result.tool_use_id.
fn parse_claude_jsonl(content: &str, session_id: &str) -> anyhow::Result<Vec<ToolInvocation>> {
    let mut invocations = Vec::new();
    // Map from tool_use_id to index in invocations vec for result correlation
    let mut pending: HashMap<String, usize> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines
        };

        let msg_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let timestamp = json
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        match msg_type {
            "assistant" => {
                // Extract tool_use from message.content[]
                if let Some(contents) = json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for item in contents {
                        if item.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                            continue;
                        }
                        let tool_id = item
                            .get("id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("")
                            .to_string();
                        let tool_name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input_json = item
                            .get("input")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);

                        let input = parse_tool_input(&tool_name, &input_json);

                        let idx = invocations.len();
                        invocations.push(ToolInvocation {
                            tool_name: tool_name.clone(),
                            input,
                            timestamp: timestamp.clone(),
                            session_id: session_id.to_string(),
                            agent: AgentKind::ClaudeCode,
                            result: None,
                        });

                        if !tool_id.is_empty() {
                            pending.insert(tool_id, idx);
                        }
                    }
                }
            }
            "user" => {
                // Extract tool_result from message.content[]
                if let Some(contents) = json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for item in contents {
                        if item.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                            continue;
                        }
                        let tool_use_id = item
                            .get("tool_use_id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("");

                        if let Some(&idx) = pending.get(tool_use_id) {
                            let result_content = match item.get("content") {
                                Some(serde_json::Value::String(s)) => s.clone(),
                                Some(serde_json::Value::Array(arr)) => {
                                    // Array of content blocks -- extract text
                                    arr.iter()
                                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                }
                                _ => String::new(),
                            };
                            let is_error = item
                                .get("is_error")
                                .and_then(|e| e.as_bool())
                                .unwrap_or(false);

                            invocations[idx].result = Some(ToolResult {
                                content: result_content,
                                is_error,
                            });
                            pending.remove(tool_use_id);
                        }
                    }
                }
            }
            _ => {} // skip "system", "summary" etc.
        }
    }

    Ok(invocations)
}

/// Parse tool input JSON into normalized ToolInput enum.
fn parse_tool_input(tool_name: &str, input: &serde_json::Value) -> ToolInput {
    match tool_name {
        "Read" => {
            let file_path = input
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Read { file_path }
        }
        "Write" => {
            let file_path = input
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Write { file_path }
        }
        "Edit" => {
            let file_path = input
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Edit { file_path }
        }
        "Bash" => {
            let command = input
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Bash { command }
        }
        "Glob" => {
            let pattern = input
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Glob { pattern }
        }
        "Grep" => {
            let pattern = input
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Grep { pattern }
        }
        _ => ToolInput::Other {
            tool_name: tool_name.to_string(),
            raw: input.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_use_read() {
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"/tmp/test.rs"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}"#;
        let invocations = parse_claude_jsonl(jsonl, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "Read");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Read { file_path } if file_path == "/tmp/test.rs"
        ));
    }

    #[test]
    fn test_parse_tool_use_bash() {
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_02","name":"Bash","input":{"command":"cargo test","description":"Run tests"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}"#;
        let invocations = parse_claude_jsonl(jsonl, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "Bash");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "cargo test"
        ));
    }

    #[test]
    fn test_correlate_tool_result() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"/tmp/test.rs"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"tool_use_id":"toolu_01","type":"tool_result","content":"fn main() {}"}]}}"#
        );
        let invocations = parse_claude_jsonl(jsonl, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.is_some());
        assert_eq!(
            invocations[0].result.as_ref().unwrap().content,
            "fn main() {}"
        );
    }

    #[test]
    fn test_skip_malformed_lines() {
        let jsonl = "not json\n{}\n";
        let invocations = parse_claude_jsonl(jsonl, "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    #[test]
    fn test_empty_input() {
        let invocations = parse_claude_jsonl("", "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    #[test]
    fn test_multiple_tools_in_one_message() {
        let jsonl = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"/a.rs"}},{"type":"tool_use","id":"toolu_02","name":"Read","input":{"file_path":"/b.rs"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}"#;
        let invocations = parse_claude_jsonl(jsonl, "sess1").unwrap();
        assert_eq!(invocations.len(), 2);
    }

    #[test]
    fn test_tool_result_with_array_content() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"/tmp/test.rs"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"tool_use_id":"toolu_01","type":"tool_result","content":[{"type":"text","text":"line 1"},{"type":"text","text":"line 2"}]}]}}"#
        );
        let invocations = parse_claude_jsonl(jsonl, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.is_some());
        assert_eq!(
            invocations[0].result.as_ref().unwrap().content,
            "line 1\nline 2"
        );
    }

    #[test]
    fn test_tool_result_is_error() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_01","name":"Read","input":{"file_path":"/tmp/test.rs"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"tool_use_id":"toolu_01","type":"tool_result","content":"file not found","is_error":true}]}}"#
        );
        let invocations = parse_claude_jsonl(jsonl, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_parse_tool_input_variants() {
        let write_input = serde_json::json!({"file_path": "/tmp/out.rs"});
        let result = parse_tool_input("Write", &write_input);
        assert!(matches!(result, ToolInput::Write { file_path } if file_path == "/tmp/out.rs"));

        let edit_input = serde_json::json!({"file_path": "/tmp/edit.rs"});
        let result = parse_tool_input("Edit", &edit_input);
        assert!(matches!(result, ToolInput::Edit { file_path } if file_path == "/tmp/edit.rs"));

        let glob_input = serde_json::json!({"pattern": "**/*.rs"});
        let result = parse_tool_input("Glob", &glob_input);
        assert!(matches!(result, ToolInput::Glob { pattern } if pattern == "**/*.rs"));

        let grep_input = serde_json::json!({"pattern": "fn main"});
        let result = parse_tool_input("Grep", &grep_input);
        assert!(matches!(result, ToolInput::Grep { pattern } if pattern == "fn main"));

        let other_input = serde_json::json!({"foo": "bar"});
        let result = parse_tool_input("UnknownTool", &other_input);
        assert!(matches!(result, ToolInput::Other { tool_name, .. } if tool_name == "UnknownTool"));
    }

    #[test]
    fn test_tool_input_file_path() {
        let read = ToolInput::Read {
            file_path: "/tmp/a.rs".to_string(),
        };
        assert_eq!(read.file_path(), Some("/tmp/a.rs"));

        let write = ToolInput::Write {
            file_path: "/tmp/b.rs".to_string(),
        };
        assert_eq!(write.file_path(), Some("/tmp/b.rs"));

        let edit = ToolInput::Edit {
            file_path: "/tmp/c.rs".to_string(),
        };
        assert_eq!(edit.file_path(), Some("/tmp/c.rs"));

        let bash = ToolInput::Bash {
            command: "ls".to_string(),
        };
        assert_eq!(bash.file_path(), None);

        let glob = ToolInput::Glob {
            pattern: "*.rs".to_string(),
        };
        assert_eq!(glob.file_path(), None);
    }
}
