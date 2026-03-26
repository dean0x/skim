//! Cursor session provider.
//!
//! Parses Cursor's SQLite-backed session data from `state.vscdb`.
//! Cursor stores composer conversations in a `cursorDiskKV` table
//! with JSON-encoded values keyed by `composer.*`.

use std::path::PathBuf;

use super::types::*;
use super::SessionProvider;

/// Maximum database file size: 100 MB.
const MAX_DB_SIZE: u64 = 100 * 1024 * 1024;

/// Cursor session file provider.
///
/// Reads from Cursor's `state.vscdb` SQLite database. Access is always
/// read-only with a 1-second busy timeout to avoid hanging when Cursor
/// has a write lock.
pub(crate) struct CursorProvider {
    db_path: PathBuf,
}

impl CursorProvider {
    /// Detect Cursor by checking if the state database exists.
    ///
    /// Uses `SKIM_CURSOR_DB_PATH` env var override for testability.
    pub(crate) fn detect() -> Option<Self> {
        let db_path = if let Ok(override_path) = std::env::var("SKIM_CURSOR_DB_PATH") {
            PathBuf::from(override_path)
        } else {
            default_db_path()?
        };

        if db_path.is_file() {
            Some(Self { db_path })
        } else {
            None
        }
    }
}

/// Platform-specific default path for Cursor's state database.
fn default_db_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // Windows uses a different base directory (AppData), not covered by config_dir()
        dirs::data_dir().map(|d| d.join("Cursor/User/globalStorage/state.vscdb"))
    }

    #[cfg(not(target_os = "windows"))]
    {
        dirs::home_dir().map(|h| {
            AgentKind::Cursor
                .config_dir(&h)
                .join("User/globalStorage/state.vscdb")
        })
    }
}

impl SessionProvider for CursorProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Cursor
    }

    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>> {
        let rows = match query_composer_keys(&self.db_path) {
            Ok(rows) => rows,
            Err(e) => {
                // Graceful degradation: if the database is locked or
                // otherwise inaccessible, return empty rather than fail.
                eprintln!("warning: could not query Cursor database: {e}");
                return Ok(Vec::new());
            }
        };

        let file_modified = std::fs::metadata(&self.db_path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::now());

        // Apply time filter against the database file's mtime (we cannot
        // reliably get per-session timestamps from the KV table).
        if let Some(since) = filter.since {
            if file_modified < since {
                return Ok(Vec::new());
            }
        }

        let mut sessions: Vec<SessionFile> = rows
            .into_iter()
            .map(|(key, _value)| SessionFile {
                path: self.db_path.clone(),
                modified: file_modified,
                agent: AgentKind::Cursor,
                session_id: key,
            })
            .collect();

        // Sort by session_id for deterministic output
        sessions.sort_by(|a, b| b.session_id.cmp(&a.session_id));

        if filter.latest_only {
            sessions.truncate(1);
        }

        Ok(sessions)
    }

    fn parse_session(&self, file: &SessionFile) -> anyhow::Result<Vec<ToolInvocation>> {
        // Guard against oversized databases (consistent with other providers)
        let db_size = std::fs::metadata(&self.db_path)?.len();
        if db_size > MAX_DB_SIZE {
            anyhow::bail!(
                "database too large ({:.1} MB, limit {:.0} MB): {}",
                db_size as f64 / (1024.0 * 1024.0),
                MAX_DB_SIZE as f64 / (1024.0 * 1024.0),
                self.db_path.display()
            );
        }

        let value = match query_single_key(&self.db_path, &file.session_id) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(Vec::new()),
            Err(e) => {
                eprintln!(
                    "warning: could not read Cursor session {}: {e}",
                    file.session_id
                );
                return Ok(Vec::new());
            }
        };

        parse_cursor_json_value(&value, &file.session_id)
    }
}

// ============================================================================
// SQLite queries (thin layer)
// ============================================================================

/// Query all composer session keys and their values from the database.
///
/// Opens read-only with a 1-second busy timeout. Uses a SQL LIMIT to
/// prevent unbounded reads on large databases.
fn query_composer_keys(db_path: &std::path::Path) -> anyhow::Result<Vec<(String, String)>> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(1000))?;

    let mut stmt =
        conn.prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composer.%' LIMIT 1000")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rows)
}

/// Query a single key's value from the database.
fn query_single_key(db_path: &std::path::Path, key: &str) -> anyhow::Result<Option<String>> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(1000))?;

    let mut stmt = conn.prepare("SELECT value FROM cursorDiskKV WHERE key = ?1 LIMIT 1")?;
    let result = stmt
        .query_row(rusqlite::params![key], |row| row.get::<_, String>(0))
        .ok();

    Ok(result)
}

// ============================================================================
// JSON parsing (business logic, fully testable without SQLite)
// ============================================================================

/// Parse a Cursor composer JSON value into tool invocations.
///
/// The JSON structure has `composerData.conversations[].messages[]`
/// where assistant messages may contain `tool_calls` and tool messages
/// contain results correlated by `tool_call_id`.
pub(super) fn parse_cursor_json_value(
    json_str: &str,
    session_id: &str,
) -> anyhow::Result<Vec<ToolInvocation>> {
    let root: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("invalid JSON in Cursor session: {e}"))?;

    let conversations = match root
        .get("composerData")
        .and_then(|cd| cd.get("conversations"))
        .and_then(|c| c.as_array())
    {
        Some(convs) => convs,
        None => return Ok(Vec::new()),
    };

    let mut invocations = Vec::new();
    // Map from tool_call_id to index in invocations for result correlation
    let mut pending: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for conversation in conversations {
        let messages = match conversation.get("messages").and_then(|m| m.as_array()) {
            Some(msgs) => msgs,
            None => continue,
        };

        for message in messages {
            let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("");

            match role {
                "assistant" => {
                    if let Some(tool_calls) = message.get("tool_calls").and_then(|tc| tc.as_array())
                    {
                        process_cursor_tool_calls(
                            tool_calls,
                            session_id,
                            &mut invocations,
                            &mut pending,
                        );
                    }
                }
                "tool" => {
                    let tool_call_id = message
                        .get("tool_call_id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("");

                    if let Some(&idx) = pending.get(tool_call_id) {
                        let content = message
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();

                        invocations[idx].result = Some(ToolResult {
                            content,
                            is_error: false,
                        });
                        pending.remove(tool_call_id);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(invocations)
}

/// Extract tool invocations from Cursor's `tool_calls` array.
///
/// Each tool call has `type: "function"`, a `function` object with `name`
/// and `arguments` (JSON-encoded string), and an `id` for result correlation.
fn process_cursor_tool_calls(
    tool_calls: &[serde_json::Value],
    session_id: &str,
    invocations: &mut Vec<ToolInvocation>,
    pending: &mut std::collections::HashMap<String, usize>,
) {
    for tool_call in tool_calls {
        let tc_type = tool_call.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if tc_type != "function" {
            continue;
        }

        let function = match tool_call.get("function") {
            Some(f) => f,
            None => continue,
        };

        let tool_name = function
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();

        let arguments_str = function
            .get("arguments")
            .and_then(|a| a.as_str())
            .unwrap_or("{}");

        let arguments: serde_json::Value =
            serde_json::from_str(arguments_str).unwrap_or_default();

        let input = map_cursor_tool(&tool_name, &arguments);

        let tc_id = tool_call
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("")
            .to_string();

        let idx = invocations.len();
        invocations.push(ToolInvocation {
            tool_name: tool_name.clone(),
            input,
            timestamp: String::new(),
            session_id: session_id.to_string(),
            agent: AgentKind::Cursor,
            result: None,
        });

        if !tc_id.is_empty() {
            pending.insert(tc_id, idx);
        }
    }
}

/// Map Cursor tool names to normalized ToolInput variants.
fn map_cursor_tool(tool_name: &str, arguments: &serde_json::Value) -> ToolInput {
    match tool_name {
        "run_terminal_command" => {
            let command = arguments
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Bash { command }
        }
        "read_file" => {
            let file_path = arguments
                .get("file_path")
                .or_else(|| arguments.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Read { file_path }
        }
        "write_file" => {
            let file_path = arguments
                .get("file_path")
                .or_else(|| arguments.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Write { file_path }
        }
        "edit_file" => {
            let file_path = arguments
                .get("file_path")
                .or_else(|| arguments.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            ToolInput::Edit { file_path }
        }
        _ => ToolInput::Other {
            tool_name: tool_name.to_string(),
            raw: arguments.clone(),
        },
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- JSON parsing tests (no SQLite needed) ----

    fn sample_json() -> &'static str {
        r#"{
            "composerData": {
                "conversations": [{
                    "id": "conv-001",
                    "messages": [
                        {
                            "role": "assistant",
                            "tool_calls": [{
                                "id": "tc-001",
                                "type": "function",
                                "function": {
                                    "name": "run_terminal_command",
                                    "arguments": "{\"command\":\"cargo test\"}"
                                }
                            }]
                        },
                        {
                            "role": "tool",
                            "tool_call_id": "tc-001",
                            "content": "test result: ok"
                        }
                    ]
                }]
            }
        }"#
    }

    #[test]
    fn test_parse_cursor_json_value() {
        let invocations = parse_cursor_json_value(sample_json(), "sess-1").unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].tool_name, "run_terminal_command");
        assert_eq!(invocations[0].agent, AgentKind::Cursor);
        assert_eq!(invocations[0].session_id, "sess-1");
    }

    #[test]
    fn test_map_run_terminal_command_to_bash() {
        let args = serde_json::json!({"command": "cargo test --nocapture"});
        let input = map_cursor_tool("run_terminal_command", &args);
        assert!(matches!(
            &input,
            ToolInput::Bash { command } if command == "cargo test --nocapture"
        ));
    }

    #[test]
    fn test_map_read_file_to_read() {
        let args = serde_json::json!({"file_path": "/tmp/src/main.rs"});
        let input = map_cursor_tool("read_file", &args);
        assert!(matches!(
            &input,
            ToolInput::Read { file_path } if file_path == "/tmp/src/main.rs"
        ));

        // Also supports "path" key variant
        let args_alt = serde_json::json!({"path": "/tmp/alt.rs"});
        let input_alt = map_cursor_tool("read_file", &args_alt);
        assert!(matches!(
            &input_alt,
            ToolInput::Read { file_path } if file_path == "/tmp/alt.rs"
        ));
    }

    #[test]
    fn test_map_write_file_to_write() {
        let args = serde_json::json!({"file_path": "/tmp/out.rs"});
        let input = map_cursor_tool("write_file", &args);
        assert!(matches!(
            &input,
            ToolInput::Write { file_path } if file_path == "/tmp/out.rs"
        ));
    }

    #[test]
    fn test_map_edit_file_to_edit() {
        let args = serde_json::json!({"file_path": "/tmp/edit.rs"});
        let input = map_cursor_tool("edit_file", &args);
        assert!(matches!(
            &input,
            ToolInput::Edit { file_path } if file_path == "/tmp/edit.rs"
        ));
    }

    #[test]
    fn test_map_unknown_tool_to_other() {
        let args = serde_json::json!({"foo": "bar"});
        let input = map_cursor_tool("custom_tool", &args);
        assert!(matches!(
            &input,
            ToolInput::Other { tool_name, .. } if tool_name == "custom_tool"
        ));
    }

    #[test]
    fn test_correlate_tool_result() {
        let invocations = parse_cursor_json_value(sample_json(), "sess-1").unwrap();
        assert_eq!(invocations.len(), 1);
        let result = invocations[0].result.as_ref().expect("should have result");
        assert_eq!(result.content, "test result: ok");
        assert!(!result.is_error);
    }

    #[test]
    fn test_empty_conversations() {
        let json = r#"{"composerData": {"conversations": []}}"#;
        let invocations = parse_cursor_json_value(json, "sess-1").unwrap();
        assert!(invocations.is_empty());
    }

    #[test]
    fn test_missing_composer_data() {
        let json = r#"{"otherKey": "value"}"#;
        let invocations = parse_cursor_json_value(json, "sess-1").unwrap();
        assert!(invocations.is_empty());
    }

    #[test]
    fn test_malformed_json_graceful() {
        let result = parse_cursor_json_value("not valid json {{{", "sess-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid JSON"));
    }

    #[test]
    fn test_malformed_arguments_graceful() {
        // Arguments is not valid JSON -- should default to empty object
        let json = r#"{
            "composerData": {
                "conversations": [{
                    "id": "conv-001",
                    "messages": [{
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "tc-001",
                            "type": "function",
                            "function": {
                                "name": "run_terminal_command",
                                "arguments": "not valid json"
                            }
                        }]
                    }]
                }]
            }
        }"#;
        let invocations = parse_cursor_json_value(json, "sess-1").unwrap();
        assert_eq!(invocations.len(), 1);
        // Should produce Bash with empty command (arguments parsed as null)
        assert!(matches!(&invocations[0].input, ToolInput::Bash { command } if command.is_empty()));
    }

    #[test]
    fn test_multiple_tool_calls_in_message() {
        let json = r#"{
            "composerData": {
                "conversations": [{
                    "id": "conv-001",
                    "messages": [{
                        "role": "assistant",
                        "tool_calls": [
                            {
                                "id": "tc-001",
                                "type": "function",
                                "function": {
                                    "name": "read_file",
                                    "arguments": "{\"file_path\":\"/a.rs\"}"
                                }
                            },
                            {
                                "id": "tc-002",
                                "type": "function",
                                "function": {
                                    "name": "read_file",
                                    "arguments": "{\"file_path\":\"/b.rs\"}"
                                }
                            }
                        ]
                    }]
                }]
            }
        }"#;
        let invocations = parse_cursor_json_value(json, "sess-1").unwrap();
        assert_eq!(invocations.len(), 2);
    }

    #[test]
    fn test_multiple_conversations() {
        let json = r#"{
            "composerData": {
                "conversations": [
                    {
                        "id": "conv-001",
                        "messages": [{
                            "role": "assistant",
                            "tool_calls": [{
                                "id": "tc-001",
                                "type": "function",
                                "function": {
                                    "name": "run_terminal_command",
                                    "arguments": "{\"command\":\"cargo build\"}"
                                }
                            }]
                        }]
                    },
                    {
                        "id": "conv-002",
                        "messages": [{
                            "role": "assistant",
                            "tool_calls": [{
                                "id": "tc-002",
                                "type": "function",
                                "function": {
                                    "name": "run_terminal_command",
                                    "arguments": "{\"command\":\"cargo test\"}"
                                }
                            }]
                        }]
                    }
                ]
            }
        }"#;
        let invocations = parse_cursor_json_value(json, "sess-1").unwrap();
        assert_eq!(invocations.len(), 2);
    }

    #[test]
    fn test_platform_path_detection() {
        // Verify default_db_path returns a path (platform-specific)
        let path = default_db_path();
        // On CI or containers without a home dir this may be None, which is fine
        if let Some(p) = path {
            let path_str = p.to_string_lossy();
            #[cfg(target_os = "macos")]
            assert!(
                path_str.contains("Library/Application Support/Cursor"),
                "macOS path should contain Cursor app support dir, got: {path_str}"
            );
            #[cfg(target_os = "linux")]
            assert!(
                path_str.contains(".config/Cursor"),
                "Linux path should contain .config/Cursor, got: {path_str}"
            );
        }
    }

    #[test]
    fn test_env_override_path() {
        // Use a temp path that does not exist -- detect() should return None
        std::env::set_var("SKIM_CURSOR_DB_PATH", "/tmp/nonexistent_skim_test.vscdb");
        let provider = CursorProvider::detect();
        assert!(
            provider.is_none(),
            "detect() should return None for non-existent file"
        );
        std::env::remove_var("SKIM_CURSOR_DB_PATH");
    }

    #[test]
    fn test_non_function_tool_calls_skipped() {
        let json = r#"{
            "composerData": {
                "conversations": [{
                    "id": "conv-001",
                    "messages": [{
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "tc-001",
                            "type": "code_interpreter",
                            "function": {
                                "name": "run_terminal_command",
                                "arguments": "{\"command\":\"ls\"}"
                            }
                        }]
                    }]
                }]
            }
        }"#;
        let invocations = parse_cursor_json_value(json, "sess-1").unwrap();
        assert!(
            invocations.is_empty(),
            "non-function tool calls should be skipped"
        );
    }

    #[test]
    fn test_message_without_tool_calls() {
        let json = r#"{
            "composerData": {
                "conversations": [{
                    "id": "conv-001",
                    "messages": [{
                        "role": "assistant",
                        "content": "Here is the answer"
                    }]
                }]
            }
        }"#;
        let invocations = parse_cursor_json_value(json, "sess-1").unwrap();
        assert!(invocations.is_empty());
    }
}
