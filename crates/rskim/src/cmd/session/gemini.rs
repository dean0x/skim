//! Gemini CLI session provider
//!
//! Parses Gemini CLI session files from `~/.gemini/tmp/`.
//! Supports dual format: legacy JSON array and current JSONL.

use std::collections::HashMap;
use std::path::PathBuf;

use super::types::*;
use super::SessionProvider;

/// Maximum session file size (100 MB) to prevent unbounded reads.
const MAX_SESSION_SIZE: u64 = 100 * 1024 * 1024;

/// Gemini CLI session file provider.
pub(crate) struct GeminiCliProvider {
    gemini_dir: PathBuf,
}

impl GeminiCliProvider {
    /// Detect Gemini CLI by checking if the session directory exists.
    ///
    /// Uses `SKIM_GEMINI_DIR` env var override for testability.
    pub(crate) fn detect() -> Option<Self> {
        let gemini_dir = if let Ok(override_dir) = std::env::var("SKIM_GEMINI_DIR") {
            PathBuf::from(override_dir)
        } else {
            dirs::home_dir()?.join(".gemini").join("tmp")
        };

        if gemini_dir.is_dir() {
            Some(Self { gemini_dir })
        } else {
            None
        }
    }
}

impl SessionProvider for GeminiCliProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::GeminiCli
    }

    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>> {
        let mut sessions = Vec::new();

        // Canonicalize gemini_dir to prevent symlink traversal outside boundary
        let canonical_root = self
            .gemini_dir
            .canonicalize()
            .unwrap_or_else(|_| self.gemini_dir.clone());

        let entries = std::fs::read_dir(&self.gemini_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            // Verify resolved path stays within the gemini directory (symlink traversal guard)
            if let Ok(canonical_path) = path.canonicalize() {
                if !canonical_path.starts_with(&canonical_root) {
                    eprintln!(
                        "warning: skipping file outside gemini dir: {}",
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
                agent: AgentKind::GeminiCli,
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
        parse_gemini_session(&content, &file.session_id)
    }
}

/// Detect format by first non-whitespace character and parse accordingly.
///
/// - First char `[` -> JSON array of messages (legacy format)
/// - Otherwise -> JSONL (one JSON object per line, current format)
fn parse_gemini_session(content: &str, session_id: &str) -> anyhow::Result<Vec<ToolInvocation>> {
    let trimmed = content.trim_start();
    if trimmed.starts_with('[') {
        parse_json_array_format(trimmed, session_id)
    } else {
        parse_jsonl_format(content, session_id)
    }
}

/// Parse Gemini CLI JSONL format (one JSON object per line).
///
/// Correlates tool_use events with tool_result events by matching
/// `id` to `tool_use_id`.
fn parse_jsonl_format(content: &str, session_id: &str) -> anyhow::Result<Vec<ToolInvocation>> {
    let mut invocations = Vec::new();
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

        process_gemini_event(&json, session_id, &mut invocations, &mut pending);
    }

    Ok(invocations)
}

/// Parse Gemini CLI JSON array format (legacy).
///
/// The file contains a single JSON array of message objects.
fn parse_json_array_format(content: &str, session_id: &str) -> anyhow::Result<Vec<ToolInvocation>> {
    let arr: Vec<serde_json::Value> = serde_json::from_str(content)?;
    let mut invocations = Vec::new();
    let mut pending: HashMap<String, usize> = HashMap::new();

    for json in &arr {
        process_gemini_event(json, session_id, &mut invocations, &mut pending);
    }

    Ok(invocations)
}

/// Process a single Gemini event (tool_use or tool_result).
///
/// Gemini CLI events have a top-level "type" field:
/// - `{ "type": "tool_use", "tool": "shell", "args": {"command": "..."}, "id": "tu-001" }`
/// - `{ "type": "tool_result", "tool_use_id": "tu-001", "content": "...", "is_error": false }`
fn process_gemini_event(
    json: &serde_json::Value,
    session_id: &str,
    invocations: &mut Vec<ToolInvocation>,
    pending: &mut HashMap<String, usize>,
) {
    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "tool_use" => {
            let tool_name = json
                .get("tool")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let tool_id = json
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_string();
            let args_json = json.get("args").cloned().unwrap_or(serde_json::Value::Null);

            let input = map_gemini_tool_input(&tool_name, &args_json);

            let idx = invocations.len();
            invocations.push(ToolInvocation {
                tool_name: tool_name.clone(),
                input,
                timestamp: String::new(),
                session_id: session_id.to_string(),
                agent: AgentKind::GeminiCli,
                result: None,
            });

            if !tool_id.is_empty() {
                pending.insert(tool_id, idx);
            }
        }
        "tool_result" => {
            let tool_use_id = json
                .get("tool_use_id")
                .and_then(|id| id.as_str())
                .unwrap_or("");

            if let Some(&idx) = pending.get(tool_use_id) {
                let result_content = match json.get("content") {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Array(arr)) => arr
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                let is_error = json
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
        _ => {} // skip unknown event types
    }
}

/// Map Gemini CLI tool names to normalized ToolInput enum.
///
/// Tool name mapping:
/// - "shell" / "bash" -> ToolInput::Bash
/// - "read_file" -> ToolInput::Read
/// - "write_file" -> ToolInput::Write
/// - "edit_file" -> ToolInput::Edit
/// - Everything else -> ToolInput::Other
fn map_gemini_tool_input(tool_name: &str, args: &serde_json::Value) -> ToolInput {
    match tool_name {
        "shell" | "bash" => {
            let command = args
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Bash { command }
        }
        "read_file" => {
            let file_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Read { file_path }
        }
        "write_file" => {
            let file_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Write { file_path }
        }
        "edit_file" => {
            let file_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
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

    #[test]
    fn test_parse_jsonl_format() {
        let content = concat!(
            r#"{"type":"tool_use","tool":"shell","args":{"command":"cargo test"},"id":"tu-001"}"#,
            "\n",
            r#"{"type":"tool_result","tool_use_id":"tu-001","content":"test result: ok","is_error":false}"#,
        );
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "shell");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "cargo test"
        ));
        assert!(invocations[0].result.is_some());
        assert_eq!(
            invocations[0].result.as_ref().unwrap().content,
            "test result: ok"
        );
        assert!(!invocations[0].result.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_parse_json_array_format() {
        let content = r#"[
            {"type":"tool_use","tool":"shell","args":{"command":"ls -la"},"id":"tu-001"},
            {"type":"tool_result","tool_use_id":"tu-001","content":"total 0\ndrwxr-xr-x","is_error":false}
        ]"#;
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "shell");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "ls -la"
        ));
        assert!(invocations[0].result.is_some());
        assert_eq!(
            invocations[0].result.as_ref().unwrap().content,
            "total 0\ndrwxr-xr-x"
        );
    }

    #[test]
    fn test_detect_format_by_first_char() {
        // JSON array format (starts with [)
        let array_content =
            r#"[{"type":"tool_use","tool":"shell","args":{"command":"echo hi"},"id":"tu-001"}]"#;
        let invocations = parse_gemini_session(array_content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);

        // JSONL format (starts with {)
        let jsonl_content =
            r#"{"type":"tool_use","tool":"shell","args":{"command":"echo hi"},"id":"tu-002"}"#;
        let invocations = parse_gemini_session(jsonl_content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);

        // Leading whitespace before [ should still detect array format
        let padded_array = format!(
            "  \n  {}",
            r#"[{"type":"tool_use","tool":"shell","args":{"command":"echo"},"id":"tu-003"}]"#
        );
        let invocations = parse_gemini_session(&padded_array, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
    }

    #[test]
    fn test_correlate_tool_result() {
        let content = concat!(
            r#"{"type":"tool_use","tool":"read_file","args":{"file_path":"/tmp/test.rs"},"id":"tu-001"}"#,
            "\n",
            r#"{"type":"tool_result","tool_use_id":"tu-001","content":"fn main() {}"}"#,
        );
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.is_some());
        assert_eq!(
            invocations[0].result.as_ref().unwrap().content,
            "fn main() {}"
        );
        assert!(!invocations[0].result.as_ref().unwrap().is_error);
    }

    #[test]
    fn test_skip_malformed_lines() {
        let content = "not json\n{}\n";
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    #[test]
    fn test_empty_input() {
        let invocations = parse_gemini_session("", "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }

    #[test]
    fn test_tool_result_with_error() {
        let content = concat!(
            r#"{"type":"tool_use","tool":"shell","args":{"command":"rm /protected"},"id":"tu-001"}"#,
            "\n",
            r#"{"type":"tool_result","tool_use_id":"tu-001","content":"permission denied","is_error":true}"#,
        );
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.as_ref().unwrap().is_error);
        assert_eq!(
            invocations[0].result.as_ref().unwrap().content,
            "permission denied"
        );
    }

    #[test]
    fn test_multiple_tools() {
        let content = concat!(
            r#"{"type":"tool_use","tool":"shell","args":{"command":"cargo test"},"id":"tu-001"}"#,
            "\n",
            r#"{"type":"tool_result","tool_use_id":"tu-001","content":"ok","is_error":false}"#,
            "\n",
            r#"{"type":"tool_use","tool":"read_file","args":{"file_path":"/src/main.rs"},"id":"tu-002"}"#,
            "\n",
            r#"{"type":"tool_result","tool_use_id":"tu-002","content":"fn main() {}","is_error":false}"#,
            "\n",
            r#"{"type":"tool_use","tool":"write_file","args":{"file_path":"/tmp/out.rs"},"id":"tu-003"}"#,
        );
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 3);

        // First: shell command
        assert_eq!(invocations[0].tool_name, "shell");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "cargo test"
        ));
        assert!(invocations[0].result.is_some());

        // Second: read_file
        assert_eq!(invocations[1].tool_name, "read_file");
        assert!(matches!(
            &invocations[1].input,
            ToolInput::Read { file_path } if file_path == "/src/main.rs"
        ));
        assert!(invocations[1].result.is_some());

        // Third: write_file (no result yet)
        assert_eq!(invocations[2].tool_name, "write_file");
        assert!(matches!(
            &invocations[2].input,
            ToolInput::Write { file_path } if file_path == "/tmp/out.rs"
        ));
        assert!(invocations[2].result.is_none());
    }

    #[test]
    fn test_tool_name_mapping() {
        // "bash" maps to ToolInput::Bash
        let input = map_gemini_tool_input("bash", &serde_json::json!({"command": "echo hi"}));
        assert!(matches!(input, ToolInput::Bash { command } if command == "echo hi"));

        // "shell" maps to ToolInput::Bash
        let input = map_gemini_tool_input("shell", &serde_json::json!({"command": "ls"}));
        assert!(matches!(input, ToolInput::Bash { command } if command == "ls"));

        // "read_file" maps to ToolInput::Read
        let input = map_gemini_tool_input("read_file", &serde_json::json!({"file_path": "/a.rs"}));
        assert!(matches!(input, ToolInput::Read { file_path } if file_path == "/a.rs"));

        // "read_file" with "path" key also works
        let input = map_gemini_tool_input("read_file", &serde_json::json!({"path": "/b.rs"}));
        assert!(matches!(input, ToolInput::Read { file_path } if file_path == "/b.rs"));

        // "edit_file" maps to ToolInput::Edit
        let input = map_gemini_tool_input("edit_file", &serde_json::json!({"file_path": "/c.rs"}));
        assert!(matches!(input, ToolInput::Edit { file_path } if file_path == "/c.rs"));

        // Unknown tools map to ToolInput::Other
        let input = map_gemini_tool_input("search", &serde_json::json!({"query": "test"}));
        assert!(matches!(input, ToolInput::Other { tool_name, .. } if tool_name == "search"));
    }

    #[test]
    fn test_agent_kind_is_gemini() {
        let content =
            r#"{"type":"tool_use","tool":"shell","args":{"command":"echo"},"id":"tu-001"}"#;
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations[0].agent, AgentKind::GeminiCli);
    }

    #[test]
    fn test_uncorrelated_result_ignored() {
        // tool_result with no matching tool_use should be silently ignored
        let content = r#"{"type":"tool_result","tool_use_id":"nonexistent","content":"orphan","is_error":false}"#;
        let invocations = parse_gemini_session(content, "sess1").unwrap();
        assert_eq!(invocations.len(), 0);
    }
}
