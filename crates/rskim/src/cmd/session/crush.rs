//! Crush session provider
//!
//! Parses Crush JSONL session files from `~/.crush/` directory.
//! Crush stores sessions similarly to Claude Code using JSONL format.

use std::io::{BufRead, BufReader};
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

        Self::detect_with_dir(sessions_dir)
    }

    /// Inner detection helper — checks whether `sessions_dir` is an existing
    /// directory and wraps it in `CrushProvider` if so.
    ///
    /// Extracted to allow testing the detection logic directly with a
    /// constructed path, avoiding `std::env::set_var` in tests.
    ///
    /// TODO: The other five providers (Claude, Gemini, Codex, Copilot, Cursor)
    /// inline this check inside `detect()` and rely on env-var mutation for
    /// testing.  Extracting an equivalent `detect_with_dir` helper to all of
    /// them would make tests race-condition-free and is tracked as a follow-up
    /// consistency improvement.
    fn detect_with_dir(sessions_dir: PathBuf) -> Option<Self> {
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

        // BufReader is used here (rather than read_to_string) so that large
        // sessions near the 100 MB limit are read one line at a time instead
        // of being loaded entirely into memory.  Other providers use
        // read_to_string; that is acceptable for now because their content
        // tends to be smaller, but Crush sessions can saturate the limit.
        // See: intentional divergence from Claude/Gemini/Codex parse_session.
        let reader = BufReader::new(std::fs::File::open(&file.path)?);
        let mut invocations = Vec::new();

        for line in reader.lines() {
            let line = line?;
            // BufReader::lines() strips the line terminator (\n / \r\n).
            // trim() is still needed to remove any leading whitespace that
            // some editors or tooling may prepend.
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

        let str_field = |key: &str| -> String {
            input
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        let tool_input = match tool_name.as_str() {
            "Bash" | "bash" | "shell" => ToolInput::Bash { command: str_field("command") },
            "Read" | "read_file" => ToolInput::Read { file_path: str_field("file_path") },
            "Write" | "write_file" => ToolInput::Write { file_path: str_field("file_path") },
            "Edit" | "edit_file" => ToolInput::Edit { file_path: str_field("file_path") },
            "Glob" | "glob" => ToolInput::Glob { pattern: str_field("pattern") },
            "Grep" | "grep" => ToolInput::Grep { pattern: str_field("pattern") },
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
    fn test_crush_provider_detect_nonexistent_dir() {
        // Test detection logic directly — no env mutation, race-condition-free.
        let provider =
            CrushProvider::detect_with_dir(PathBuf::from("/tmp/nonexistent-crush-test-dir"));
        assert!(provider.is_none());
    }

    #[test]
    fn test_crush_provider_detect_existing_dir() {
        // Happy path: detect_with_dir returns Some when the directory exists.
        let dir = tempfile::TempDir::new().unwrap();
        let provider = CrushProvider::detect_with_dir(dir.path().to_path_buf());
        assert!(
            provider.is_some(),
            "detect_with_dir must return Some when the directory exists"
        );
        assert_eq!(
            provider.unwrap().sessions_dir,
            dir.path(),
            "sessions_dir must match the provided path"
        );
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

    // ---- parse_session BufReader tests ----------------------------------------

    /// parse_session reads valid JSONL via BufReader and returns the correct
    /// tool invocations — exercises the happy path of the line-by-line reader.
    #[test]
    fn test_parse_session_bufreader_valid_jsonl() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sess.jsonl");
        let jsonl = concat!(
            "{\"timestamp\":\"2024-01-01T00:00:00Z\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"cargo test\"}}]}}\n",
            "{\"timestamp\":\"2024-01-01T00:00:01Z\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{\"file_path\":\"/tmp/a.rs\"}}]}}\n",
        );
        std::fs::write(&path, jsonl).unwrap();

        let provider = CrushProvider {
            sessions_dir: dir.path().to_path_buf(),
        };
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let session_file = SessionFile {
            path,
            modified,
            agent: AgentKind::Crush,
            session_id: "sess".to_string(),
        };

        let invocations = provider.parse_session(&session_file).unwrap();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].tool_name, "Bash");
        assert!(matches!(&invocations[0].input, ToolInput::Bash { command } if command == "cargo test"));
        assert_eq!(invocations[1].tool_name, "Read");
        assert!(matches!(&invocations[1].input, ToolInput::Read { file_path } if file_path == "/tmp/a.rs"));
    }

    /// parse_session skips malformed lines and blank lines — the BufReader path
    /// must be as tolerant as the read_to_string path used in other providers.
    #[test]
    fn test_parse_session_bufreader_skips_malformed_and_blank_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sess.jsonl");
        let jsonl = concat!(
            "not-json\n",
            "\n",
            "{\"timestamp\":\"2024-01-01T00:00:00Z\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"echo hi\"}}]}}\n",
        );
        std::fs::write(&path, jsonl).unwrap();

        let provider = CrushProvider {
            sessions_dir: dir.path().to_path_buf(),
        };
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let session_file = SessionFile {
            path,
            modified,
            agent: AgentKind::Crush,
            session_id: "sess".to_string(),
        };

        let invocations = provider.parse_session(&session_file).unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "Bash");
    }

    /// parse_session rejects files exceeding MAX_SESSION_SIZE — the size guard
    /// runs before the BufReader is opened.
    #[test]
    fn test_parse_session_rejects_oversized_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("big.jsonl");

        // Write a sparse file whose reported size exceeds the 100 MB limit.
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_SESSION_SIZE + 1).unwrap();

        let provider = CrushProvider {
            sessions_dir: dir.path().to_path_buf(),
        };
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let session_file = SessionFile {
            path,
            modified,
            agent: AgentKind::Crush,
            session_id: "big".to_string(),
        };

        let result = provider.parse_session(&session_file);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("too large"), "expected 'too large' in: {msg}");
    }
}
