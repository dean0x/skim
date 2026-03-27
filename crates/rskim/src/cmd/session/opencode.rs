//! OpenCode session provider
//!
//! Parses OpenCode SQLite session database from `.opencode/` directory.
//! OpenCode stores conversations and messages in a SQLite database with
//! tool_calls encoded as JSON in message rows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::types::*;
use super::SessionProvider;

/// Maximum SQLite database size (500 MB) to prevent unbounded reads.
///
/// SQLite databases are larger than JSON session files, so the limit is
/// higher than the 100 MB used by JSON-based providers.
const MAX_SESSION_SIZE: u64 = 500 * 1024 * 1024;

/// OpenCode session provider.
///
/// Reads from `.opencode/` directory containing a SQLite database with
/// `conversations` and `messages` tables.
pub(crate) struct OpenCodeProvider {
    db_path: PathBuf,
}

impl OpenCodeProvider {
    /// Detect OpenCode by walking up from cwd looking for `.opencode/` directory.
    ///
    /// Uses `SKIM_OPENCODE_DIR` env var override for testability.
    pub(crate) fn detect() -> Option<Self> {
        let opencode_dir = if let Ok(override_dir) = std::env::var("SKIM_OPENCODE_DIR") {
            PathBuf::from(override_dir)
        } else {
            walk_up_for_opencode()?
        };

        find_sqlite_db(&opencode_dir).map(|db_path| Self { db_path })
    }
}

/// Walk up from cwd looking for `.opencode/` directory.
fn walk_up_for_opencode() -> Option<PathBuf> {
    let mut current = std::env::current_dir().ok()?;
    loop {
        let candidate = current.join(".opencode");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Find a SQLite database file inside the given directory.
///
/// Looks for `.db` or `.sqlite` files; returns the first match.
fn find_sqlite_db(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext == "db" || ext == "sqlite" || ext == "sqlite3" {
                    return Some(path);
                }
            }
        }
    }
    None
}

impl SessionProvider for OpenCodeProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>> {
        let conn = rusqlite::Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(1000))?;

        let mut stmt = conn.prepare(
            "SELECT id, title, created_at, updated_at \
             FROM conversations \
             ORDER BY updated_at DESC \
             LIMIT 100",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(ConversationRow {
                id: row.get(0)?,
                _title: row.get::<_, Option<String>>(1)?,
                _created_at: row.get::<_, Option<String>>(2)?,
                updated_at: row.get::<_, Option<String>>(3)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            let conv = match row {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Parse updated_at to SystemTime for filtering
            let modified = parse_iso_timestamp(conv.updated_at.as_deref().unwrap_or(""))
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

            // Apply time filter
            if let Some(since) = filter.since {
                if modified < since {
                    continue;
                }
            }

            sessions.push(SessionFile {
                path: self.db_path.clone(),
                modified,
                agent: AgentKind::OpenCode,
                session_id: conv.id,
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
        // Guard against unbounded reads -- reject databases over 500 MB
        let file_size = std::fs::metadata(&self.db_path)?.len();
        if file_size > MAX_SESSION_SIZE {
            anyhow::bail!(
                "session database too large ({:.1} MB, limit {:.0} MB): {}",
                file_size as f64 / (1024.0 * 1024.0),
                MAX_SESSION_SIZE as f64 / (1024.0 * 1024.0),
                self.db_path.display()
            );
        }

        let conn = rusqlite::Connection::open_with_flags(
            &self.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(1000))?;

        let mut stmt = conn.prepare(
            "SELECT id, role, content, tool_calls, tool_call_id, created_at \
             FROM messages \
             WHERE conversation_id = ?1 \
             ORDER BY created_at ASC \
             LIMIT 10000",
        )?;

        let rows = stmt.query_map([&file.session_id], |row| {
            Ok(MessageRow {
                _id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                tool_calls: row.get(3)?,
                tool_call_id: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;

        let messages: Vec<MessageRow> = rows.filter_map(|r| r.ok()).collect();
        parse_opencode_messages(&messages, &file.session_id)
    }
}

// ============================================================================
// Internal types
// ============================================================================

struct ConversationRow {
    id: String,
    _title: Option<String>,
    _created_at: Option<String>,
    updated_at: Option<String>,
}

struct MessageRow {
    _id: String,
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    created_at: Option<String>,
}

// ============================================================================
// Message parsing (unit-testable without SQLite)
// ============================================================================

/// Parse OpenCode messages into tool invocations.
///
/// Assistant messages with `tool_calls` JSON produce invocations.
/// Tool messages with `tool_call_id` provide correlated results.
fn parse_opencode_messages(
    messages: &[MessageRow],
    session_id: &str,
) -> anyhow::Result<Vec<ToolInvocation>> {
    let mut invocations = Vec::new();
    // Map from tool_call_id to index in invocations for result correlation
    let mut pending: HashMap<String, usize> = HashMap::new();

    for msg in messages {
        let role = msg.role.as_deref().unwrap_or("");
        let timestamp = msg.created_at.as_deref().unwrap_or("").to_string();

        match role {
            "assistant" => {
                // Parse tool_calls JSON array
                if let Some(tool_calls_json) = &msg.tool_calls {
                    let tool_calls = parse_tool_calls_json(tool_calls_json);
                    for tc in tool_calls {
                        let input = map_opencode_tool(&tc.name, &tc.arguments);
                        let idx = invocations.len();
                        invocations.push(ToolInvocation {
                            tool_name: tc.name.clone(),
                            input,
                            timestamp: timestamp.clone(),
                            session_id: session_id.to_string(),
                            agent: AgentKind::OpenCode,
                            result: None,
                        });
                        if !tc.id.is_empty() {
                            pending.insert(tc.id, idx);
                        }
                    }
                }
            }
            "tool" => {
                // Correlate tool result by tool_call_id
                if let Some(call_id) = &msg.tool_call_id {
                    if let Some(&idx) = pending.get(call_id.as_str()) {
                        let content = msg.content.as_deref().unwrap_or("").to_string();
                        invocations[idx].result = Some(ToolResult {
                            content,
                            is_error: false,
                        });
                        pending.remove(call_id.as_str());
                    }
                }
            }
            _ => {} // skip "user", "system", etc.
        }
    }

    Ok(invocations)
}

/// A parsed tool call from the tool_calls JSON.
struct ParsedToolCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
}

/// Parse the tool_calls JSON string into structured tool calls.
///
/// Expected format:
/// ```json
/// [{"type": "function", "id": "call_123", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}]
/// ```
///
/// Gracefully handles malformed JSON by returning an empty vec.
fn parse_tool_calls_json(raw: &str) -> Vec<ParsedToolCall> {
    let arr: Vec<serde_json::Value> = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut calls = Vec::new();
    for item in &arr {
        let func = match item.get("function") {
            Some(f) => f,
            None => continue,
        };

        let id = item
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();

        let name = func
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();

        // arguments is a JSON-encoded string that needs double-parsing
        let arguments = func
            .get("arguments")
            .and_then(|a| {
                if let Some(s) = a.as_str() {
                    serde_json::from_str(s).ok()
                } else {
                    Some(a.clone())
                }
            })
            .unwrap_or(serde_json::Value::Null);

        calls.push(ParsedToolCall {
            id,
            name,
            arguments,
        });
    }

    calls
}

/// Map OpenCode tool names to normalized ToolInput.
///
/// OpenCode uses lowercase tool names: "bash"/"shell", "read_file", "write_file", etc.
fn map_opencode_tool(name: &str, args: &serde_json::Value) -> ToolInput {
    match name {
        "bash" | "shell" | "execute" => {
            let command = args
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Bash { command }
        }
        "read_file" | "read" => {
            let file_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Read { file_path }
        }
        "write_file" | "write" | "create_file" => {
            let file_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Write { file_path }
        }
        "edit_file" | "edit" | "patch" => {
            let file_path = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Edit { file_path }
        }
        "glob" | "list_files" => {
            let pattern = args
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Glob { pattern }
        }
        "grep" | "search" => {
            let pattern = args
                .get("pattern")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Grep { pattern }
        }
        _ => ToolInput::Other {
            tool_name: name.to_string(),
            raw: args.clone(),
        },
    }
}

/// Parse an ISO 8601 timestamp string to SystemTime.
///
/// Handles both `2024-01-01T00:00:00Z` and `2024-01-01T00:00:00.000Z` formats.
/// Returns None for unparseable timestamps.
fn parse_iso_timestamp(s: &str) -> Option<std::time::SystemTime> {
    // Simple ISO 8601 parser: extract year, month, day, hour, minute, second
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }

    let year: u64 = s.get(0..4)?.parse().ok()?;
    let month: u64 = s.get(5..7)?.parse().ok()?;
    let day: u64 = s.get(8..10)?.parse().ok()?;
    let hour: u64 = s.get(11..13)?.parse().ok()?;
    let minute: u64 = s.get(14..16)?.parse().ok()?;
    let second: u64 = s.get(17..19)?.parse().ok()?;

    // Approximate days from epoch (good enough for filtering)
    let days_in_year = 365;
    let leap_years = (year - 1970 + 1) / 4; // rough approximation
    let month_days: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut total_days: u64 = (year - 1970) * days_in_year + leap_years;
    for m in 0..(month.saturating_sub(1) as usize) {
        total_days += month_days.get(m).copied().unwrap_or(30);
    }
    total_days += day.saturating_sub(1);

    let total_secs = total_days * 86400 + hour * 3600 + minute * 60 + second;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(total_secs))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Tool call JSON parsing ----

    #[test]
    fn test_parse_messages_with_tool_calls() {
        let messages = vec![MessageRow {
            _id: "msg1".to_string(),
            role: Some("assistant".to_string()),
            content: None,
            tool_calls: Some(
                r#"[{"type":"function","id":"call_1","function":{"name":"bash","arguments":"{\"command\":\"cargo test\"}"}}]"#.to_string(),
            ),
            tool_call_id: None,
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
        }];

        let invocations = parse_opencode_messages(&messages, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "bash");
        assert!(matches!(
            &invocations[0].input,
            ToolInput::Bash { command } if command == "cargo test"
        ));
        assert_eq!(invocations[0].agent, AgentKind::OpenCode);
    }

    #[test]
    fn test_map_bash_tool() {
        let args = serde_json::json!({"command": "ls -la"});
        let input = map_opencode_tool("bash", &args);
        assert!(matches!(input, ToolInput::Bash { command } if command == "ls -la"));

        let input = map_opencode_tool("shell", &args);
        assert!(matches!(input, ToolInput::Bash { command } if command == "ls -la"));

        let input = map_opencode_tool("execute", &args);
        assert!(matches!(input, ToolInput::Bash { command } if command == "ls -la"));
    }

    #[test]
    fn test_map_read_file_tool() {
        let args = serde_json::json!({"file_path": "/tmp/test.rs"});
        let input = map_opencode_tool("read_file", &args);
        assert!(matches!(
            input,
            ToolInput::Read { file_path } if file_path == "/tmp/test.rs"
        ));

        // Also supports "path" key
        let args = serde_json::json!({"path": "/tmp/alt.rs"});
        let input = map_opencode_tool("read", &args);
        assert!(matches!(
            input,
            ToolInput::Read { file_path } if file_path == "/tmp/alt.rs"
        ));
    }

    #[test]
    fn test_correlate_tool_results_by_id() {
        let messages = vec![
            MessageRow {
                _id: "msg1".to_string(),
                role: Some("assistant".to_string()),
                content: None,
                tool_calls: Some(
                    r#"[{"type":"function","id":"call_42","function":{"name":"read_file","arguments":"{\"file_path\":\"/tmp/test.rs\"}"}}]"#.to_string(),
                ),
                tool_call_id: None,
                created_at: Some("2024-01-01T00:00:00Z".to_string()),
            },
            MessageRow {
                _id: "msg2".to_string(),
                role: Some("tool".to_string()),
                content: Some("fn main() {}".to_string()),
                tool_calls: None,
                tool_call_id: Some("call_42".to_string()),
                created_at: Some("2024-01-01T00:00:01Z".to_string()),
            },
        ];

        let invocations = parse_opencode_messages(&messages, "sess1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert!(invocations[0].result.is_some());
        let result = invocations[0].result.as_ref().unwrap();
        assert_eq!(result.content, "fn main() {}");
        assert!(!result.is_error);
    }

    #[test]
    fn test_empty_conversations() {
        let messages: Vec<MessageRow> = Vec::new();
        let invocations = parse_opencode_messages(&messages, "sess1").unwrap();
        assert!(invocations.is_empty());
    }

    #[test]
    fn test_malformed_tool_calls_graceful() {
        let messages = vec![MessageRow {
            _id: "msg1".to_string(),
            role: Some("assistant".to_string()),
            content: None,
            tool_calls: Some("not valid json".to_string()),
            tool_call_id: None,
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
        }];

        let invocations = parse_opencode_messages(&messages, "sess1").unwrap();
        assert!(invocations.is_empty());
    }

    #[test]
    fn test_walk_up_from_cwd() {
        // walk_up_for_opencode starts from real cwd, which won't have .opencode/
        // Just verify it returns None when directory not found (doesn't panic)
        let result = walk_up_for_opencode();
        // Could be Some or None depending on the system -- just ensure no crash
        let _ = result;
    }

    #[test]
    fn test_env_override_path() {
        // Temporarily set env var to a non-existent directory
        std::env::set_var("SKIM_OPENCODE_DIR", "/tmp/nonexistent-opencode-test-dir");
        let provider = OpenCodeProvider::detect();
        // Should return None because directory doesn't exist (or has no DB)
        assert!(provider.is_none());
        std::env::remove_var("SKIM_OPENCODE_DIR");
    }

    // ---- Additional tool mapping coverage ----

    #[test]
    fn test_map_write_file_tool() {
        let args = serde_json::json!({"file_path": "/tmp/out.rs"});
        let input = map_opencode_tool("write_file", &args);
        assert!(matches!(
            input,
            ToolInput::Write { file_path } if file_path == "/tmp/out.rs"
        ));

        let input = map_opencode_tool("create_file", &args);
        assert!(matches!(
            input,
            ToolInput::Write { file_path } if file_path == "/tmp/out.rs"
        ));
    }

    #[test]
    fn test_map_edit_file_tool() {
        let args = serde_json::json!({"file_path": "/tmp/edit.rs"});
        let input = map_opencode_tool("edit_file", &args);
        assert!(matches!(
            input,
            ToolInput::Edit { file_path } if file_path == "/tmp/edit.rs"
        ));

        let input = map_opencode_tool("patch", &args);
        assert!(matches!(
            input,
            ToolInput::Edit { file_path } if file_path == "/tmp/edit.rs"
        ));
    }

    #[test]
    fn test_map_glob_and_grep_tools() {
        let args = serde_json::json!({"pattern": "**/*.rs"});
        let input = map_opencode_tool("glob", &args);
        assert!(matches!(input, ToolInput::Glob { pattern } if pattern == "**/*.rs"));

        let input = map_opencode_tool("list_files", &args);
        assert!(matches!(input, ToolInput::Glob { pattern } if pattern == "**/*.rs"));

        let args = serde_json::json!({"pattern": "fn main"});
        let input = map_opencode_tool("grep", &args);
        assert!(matches!(input, ToolInput::Grep { pattern } if pattern == "fn main"));

        let input = map_opencode_tool("search", &args);
        assert!(matches!(input, ToolInput::Grep { pattern } if pattern == "fn main"));
    }

    #[test]
    fn test_map_unknown_tool() {
        let args = serde_json::json!({"foo": "bar"});
        let input = map_opencode_tool("custom_tool", &args);
        assert!(matches!(
            input,
            ToolInput::Other { tool_name, .. } if tool_name == "custom_tool"
        ));
    }

    #[test]
    fn test_parse_tool_calls_json_empty_array() {
        let calls = parse_tool_calls_json("[]");
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_tool_calls_json_multiple() {
        let json = r#"[
            {"type":"function","id":"call_1","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}},
            {"type":"function","id":"call_2","function":{"name":"read_file","arguments":"{\"file_path\":\"/tmp/a.rs\"}"}}
        ]"#;
        let calls = parse_tool_calls_json(json);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[1].name, "read_file");
    }

    #[test]
    fn test_parse_tool_calls_arguments_as_object() {
        // Some implementations pass arguments as a JSON object instead of a string
        let json = r#"[{"type":"function","id":"call_1","function":{"name":"bash","arguments":{"command":"cargo test"}}}]"#;
        let calls = parse_tool_calls_json(json);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(
            calls[0].arguments.get("command").and_then(|c| c.as_str()),
            Some("cargo test")
        );
    }

    #[test]
    fn test_parse_iso_timestamp_valid() {
        let ts = parse_iso_timestamp("2024-06-15T10:30:00Z");
        assert!(ts.is_some());
        assert!(ts.unwrap() > std::time::UNIX_EPOCH);
    }

    #[test]
    fn test_parse_iso_timestamp_with_millis() {
        let ts = parse_iso_timestamp("2024-06-15T10:30:00.123Z");
        assert!(ts.is_some());
    }

    #[test]
    fn test_parse_iso_timestamp_invalid() {
        assert!(parse_iso_timestamp("").is_none());
        assert!(parse_iso_timestamp("not-a-date").is_none());
        assert!(parse_iso_timestamp("2024").is_none());
    }

    #[test]
    fn test_multiple_tool_calls_in_one_message() {
        let messages = vec![MessageRow {
            _id: "msg1".to_string(),
            role: Some("assistant".to_string()),
            content: None,
            tool_calls: Some(
                r#"[
                    {"type":"function","id":"call_1","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}},
                    {"type":"function","id":"call_2","function":{"name":"read_file","arguments":"{\"file_path\":\"/tmp/a.rs\"}"}}
                ]"#
                .to_string(),
            ),
            tool_call_id: None,
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
        }];

        let invocations = parse_opencode_messages(&messages, "sess1").unwrap();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].tool_name, "bash");
        assert_eq!(invocations[1].tool_name, "read_file");
    }

    #[test]
    fn test_user_messages_ignored() {
        let messages = vec![MessageRow {
            _id: "msg1".to_string(),
            role: Some("user".to_string()),
            content: Some("Please help me with this code".to_string()),
            tool_calls: None,
            tool_call_id: None,
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
        }];

        let invocations = parse_opencode_messages(&messages, "sess1").unwrap();
        assert!(invocations.is_empty());
    }

    #[test]
    fn test_tool_result_without_matching_call() {
        // Tool result for a call_id that was never seen should be silently ignored
        let messages = vec![MessageRow {
            _id: "msg1".to_string(),
            role: Some("tool".to_string()),
            content: Some("some result".to_string()),
            tool_calls: None,
            tool_call_id: Some("call_nonexistent".to_string()),
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
        }];

        let invocations = parse_opencode_messages(&messages, "sess1").unwrap();
        assert!(invocations.is_empty());
    }

    #[test]
    fn test_session_id_propagated() {
        let messages = vec![MessageRow {
            _id: "msg1".to_string(),
            role: Some("assistant".to_string()),
            content: None,
            tool_calls: Some(
                r#"[{"type":"function","id":"call_1","function":{"name":"bash","arguments":"{\"command\":\"echo hi\"}"}}]"#.to_string(),
            ),
            tool_call_id: None,
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
        }];

        let invocations = parse_opencode_messages(&messages, "my-session-42").unwrap();
        assert_eq!(invocations[0].session_id, "my-session-42");
    }
}
