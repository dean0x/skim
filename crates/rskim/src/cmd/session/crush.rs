//! Crush session provider
//!
//! Parses Crush JSONL session files from `~/.crush/` directory.
//! Crush stores sessions similarly to Claude Code using JSONL format.

use std::path::PathBuf;

use super::types::*;
use super::SessionProvider;

/// Maximum session file size (100 MB) to prevent unbounded reads.
const MAX_SESSION_SIZE: u64 = 100 * 1024 * 1024;

/// Crush session provider.
///
/// Reads from `~/.crush/` directory. Uses `SKIM_CRUSH_DIR` env var for overrides.
pub(crate) struct CrushProvider {
    sessions_dir: PathBuf,
}

impl CrushProvider {
    /// Detect Crush by checking if the config directory exists.
    ///
    /// Uses `SKIM_CRUSH_DIR` env var override for testability.
    pub(crate) fn detect() -> Option<Self> {
        let sessions_dir = if let Ok(override_dir) = std::env::var("SKIM_CRUSH_DIR") {
            PathBuf::from(override_dir)
        } else {
            AgentKind::Crush.config_dir(&dirs::home_dir()?)
        };

        if sessions_dir.is_dir() {
            Some(Self { sessions_dir })
        } else {
            None
        }
    }
}

impl SessionProvider for CrushProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Crush
    }

    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>> {
        let mut sessions = Vec::new();

        let canonical_root = self
            .sessions_dir
            .canonicalize()
            .unwrap_or_else(|_| self.sessions_dir.clone());

        let entries = std::fs::read_dir(&self.sessions_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            // Symlink traversal guard
            if let Ok(canonical_path) = path.canonicalize() {
                if !canonical_path.starts_with(&canonical_root) {
                    eprintln!(
                        "warning: skipping file outside crush dir: {}",
                        path.display()
                    );
                    continue;
                }
            }

            let modified = match std::fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue,
            };

            if let Some(since) = filter.since {
                if modified < since {
                    continue;
                }
            }

            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            sessions.push(SessionFile {
                path,
                modified,
                agent: AgentKind::Crush,
                session_id,
            });
        }

        sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));

        if filter.latest_only {
            sessions.truncate(1);
        }

        Ok(sessions)
    }

    fn parse_session(&self, file: &SessionFile) -> anyhow::Result<Vec<ToolInvocation>> {
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
        let mut invocations = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let json: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Crush uses Claude Code-compatible JSONL format
            if let Some(inv) = parse_crush_line(&json, &file.session_id) {
                invocations.push(inv);
            }
        }

        Ok(invocations)
    }
}

/// Parse a single JSONL line from a Crush session file.
///
/// Crush uses a Claude Code-compatible format with tool_use and tool_result messages.
fn parse_crush_line(json: &serde_json::Value, session_id: &str) -> Option<ToolInvocation> {
    // Look for tool_use in message content blocks
    let content = json.get("message")?.get("content")?;
    let blocks = content.as_array()?;

    let timestamp = json
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }

        let tool_name = block
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();

        let input = block
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let tool_input = match tool_name.as_str() {
            "Bash" | "bash" | "shell" => {
                let command = input
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                ToolInput::Bash { command }
            }
            "Read" | "read_file" => {
                let file_path = input
                    .get("file_path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                ToolInput::Read { file_path }
            }
            "Write" | "write_file" => {
                let file_path = input
                    .get("file_path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                ToolInput::Write { file_path }
            }
            "Edit" | "edit_file" => {
                let file_path = input
                    .get("file_path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                ToolInput::Edit { file_path }
            }
            "Glob" | "glob" => {
                let pattern = input
                    .get("pattern")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                ToolInput::Glob { pattern }
            }
            "Grep" | "grep" => {
                let pattern = input
                    .get("pattern")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                ToolInput::Grep { pattern }
            }
            _ => ToolInput::Other {
                tool_name: tool_name.clone(),
                raw: input,
            },
        };

        return Some(ToolInvocation {
            tool_name,
            input: tool_input,
            timestamp,
            session_id: session_id.to_string(),
            agent: AgentKind::Crush,
            result: None,
        });
    }

    None
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crush_provider_detect_env_override_nonexistent() {
        std::env::set_var("SKIM_CRUSH_DIR", "/tmp/nonexistent-crush-test-dir");
        let provider = CrushProvider::detect();
        assert!(provider.is_none());
        std::env::remove_var("SKIM_CRUSH_DIR");
    }

    #[test]
    fn test_crush_provider_agent_kind() {
        let dir = tempfile::TempDir::new().unwrap();
        let provider = CrushProvider {
            sessions_dir: dir.path().to_path_buf(),
        };
        assert_eq!(provider.agent_kind(), AgentKind::Crush);
    }

    #[test]
    fn test_crush_provider_find_sessions_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let provider = CrushProvider {
            sessions_dir: dir.path().to_path_buf(),
        };
        let filter = TimeFilter {
            since: None,
            latest_only: false,
        };
        let sessions = provider.find_sessions(&filter).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_crush_provider_find_sessions_ignores_non_jsonl() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("session.txt"), "not jsonl").unwrap();
        std::fs::write(dir.path().join("session.json"), "{}").unwrap();
        let provider = CrushProvider {
            sessions_dir: dir.path().to_path_buf(),
        };
        let filter = TimeFilter {
            since: None,
            latest_only: false,
        };
        let sessions = provider.find_sessions(&filter).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_parse_crush_line_bash_tool() {
        let json = serde_json::json!({
            "timestamp": "2024-01-01T00:00:00Z",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Bash",
                    "input": { "command": "cargo test" }
                }]
            }
        });
        let inv = parse_crush_line(&json, "sess-1").unwrap();
        assert_eq!(inv.tool_name, "Bash");
        assert!(matches!(&inv.input, ToolInput::Bash { command } if command == "cargo test"));
        assert_eq!(inv.session_id, "sess-1");
        assert_eq!(inv.agent, AgentKind::Crush);
    }

    #[test]
    fn test_parse_crush_line_no_tool_use() {
        let json = serde_json::json!({
            "timestamp": "2024-01-01T00:00:00Z",
            "message": {
                "content": [{
                    "type": "text",
                    "text": "Hello"
                }]
            }
        });
        assert!(parse_crush_line(&json, "sess-1").is_none());
    }

    #[test]
    fn test_parse_crush_line_missing_message() {
        let json = serde_json::json!({ "timestamp": "2024-01-01T00:00:00Z" });
        assert!(parse_crush_line(&json, "sess-1").is_none());
    }
}
