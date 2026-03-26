//! Multi-agent session infrastructure (#61)
//!
//! Provides agent-agnostic types and the `SessionProvider` trait for scanning
//! AI agent session files. Wave 4 ships the Claude Code provider; future agents
//! are added by implementing the trait -- no conditionals in business logic.

mod claude;
mod codex;
mod copilot;
mod cursor;
mod gemini;
mod opencode;
pub(crate) mod types;

#[allow(unused_imports)] // ToolResult used by learn.rs tests
pub(crate) use types::{
    parse_duration_ago, AgentKind, SessionFile, TimeFilter, ToolInput, ToolInvocation, ToolResult,
};

// ============================================================================
// SessionProvider trait
// ============================================================================

/// Trait implemented by each agent's session file parser.
#[allow(dead_code)] // agent_kind used in tests only; detect_single routes by AgentKind directly
pub(crate) trait SessionProvider {
    fn agent_kind(&self) -> AgentKind;
    fn find_sessions(&self, filter: &TimeFilter) -> anyhow::Result<Vec<SessionFile>>;
    fn parse_session(&self, file: &SessionFile) -> anyhow::Result<Vec<ToolInvocation>>;
}

// ============================================================================
// Auto-detection
// ============================================================================

/// Auto-detect available agents by checking known session paths.
pub(crate) fn detect_agents() -> Vec<Box<dyn SessionProvider>> {
    let mut providers: Vec<Box<dyn SessionProvider>> = Vec::new();
    if let Some(p) = claude::ClaudeCodeProvider::detect() {
        providers.push(Box::new(p));
    }
    if let Some(p) = codex::CodexCliProvider::detect() {
        providers.push(Box::new(p));
    }
    if let Some(p) = copilot::CopilotCliProvider::detect() {
        providers.push(Box::new(p));
    }
    if let Some(p) = cursor::CursorProvider::detect() {
        providers.push(Box::new(p));
    }
    if let Some(p) = gemini::GeminiCliProvider::detect() {
        providers.push(Box::new(p));
    }
    if let Some(p) = opencode::OpenCodeProvider::detect() {
        providers.push(Box::new(p));
    }
    providers
}

/// Detect the single provider for a specific agent kind.
///
/// Short-circuits to only probe the requested agent's session path instead of
/// detecting all providers and filtering.
fn detect_single(kind: AgentKind) -> Vec<Box<dyn SessionProvider>> {
    let opt: Option<Box<dyn SessionProvider>> = match kind {
        AgentKind::ClaudeCode => claude::ClaudeCodeProvider::detect().map(|p| Box::new(p) as _),
        AgentKind::CodexCli => codex::CodexCliProvider::detect().map(|p| Box::new(p) as _),
        AgentKind::CopilotCli => copilot::CopilotCliProvider::detect().map(|p| Box::new(p) as _),
        AgentKind::Cursor => cursor::CursorProvider::detect().map(|p| Box::new(p) as _),
        AgentKind::GeminiCli => gemini::GeminiCliProvider::detect().map(|p| Box::new(p) as _),
        AgentKind::OpenCode => opencode::OpenCodeProvider::detect().map(|p| Box::new(p) as _),
    };
    opt.into_iter().collect()
}

/// Get providers filtered by agent kind, or all detected agents.
pub(crate) fn get_providers(agent_filter: Option<AgentKind>) -> Vec<Box<dyn SessionProvider>> {
    match agent_filter {
        Some(kind) => detect_single(kind),
        None => detect_agents(),
    }
}

/// Collect all tool invocations from the given providers within a time filter.
///
/// Deduplicates invocations across agents using (input_key, timestamp) pairs.
/// This prevents double-counting when multiple agents observe the same command.
pub(crate) fn collect_invocations(
    providers: &[Box<dyn SessionProvider>],
    filter: &TimeFilter,
) -> anyhow::Result<Vec<ToolInvocation>> {
    let mut all_invocations: Vec<ToolInvocation> = Vec::new();
    for provider in providers {
        let sessions = provider.find_sessions(filter)?;
        for session_file in &sessions {
            match provider.parse_session(session_file) {
                Ok(invocations) => all_invocations.extend(invocations),
                Err(e) => {
                    eprintln!(
                        "warning: failed to parse {}: {}",
                        session_file.path.display(),
                        e
                    );
                }
            }
        }
    }

    dedup_invocations(&mut all_invocations);
    Ok(all_invocations)
}

/// Deduplicate invocations by (input_key, timestamp).
///
/// When multiple agents observe the same command at the same time,
/// only the first occurrence is retained. Order is preserved.
fn dedup_invocations(invocations: &mut Vec<ToolInvocation>) {
    let mut seen = std::collections::HashSet::new();
    invocations.retain(|inv| {
        let key = (tool_input_key(&inv.input), inv.timestamp.clone());
        seen.insert(key)
    });
}

/// Extract a string key from a ToolInput for deduplication.
fn tool_input_key(input: &ToolInput) -> String {
    match input {
        ToolInput::Read { file_path } => format!("read:{file_path}"),
        ToolInput::Bash { command } => format!("bash:{command}"),
        ToolInput::Write { file_path } => format!("write:{file_path}"),
        ToolInput::Glob { pattern } => format!("glob:{pattern}"),
        ToolInput::Grep { pattern } => format!("grep:{pattern}"),
        ToolInput::Edit { file_path } => format!("edit:{file_path}"),
        ToolInput::Other { tool_name, raw } => format!("other:{tool_name}:{raw}"),
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_invocation(command: &str, timestamp: &str, agent: AgentKind) -> ToolInvocation {
        ToolInvocation {
            tool_name: "Bash".to_string(),
            input: ToolInput::Bash {
                command: command.to_string(),
            },
            timestamp: timestamp.to_string(),
            session_id: "test-session".to_string(),
            agent,
            result: None,
        }
    }

    #[test]
    fn test_dedup_same_command_same_timestamp() {
        let mut invocations = vec![
            make_invocation("cargo test", "2026-01-01T00:00:00Z", AgentKind::ClaudeCode),
            make_invocation("cargo test", "2026-01-01T00:00:00Z", AgentKind::GeminiCli),
        ];
        dedup_invocations(&mut invocations);
        assert_eq!(invocations.len(), 1, "same cmd+ts should dedup to 1");
        assert_eq!(
            invocations[0].agent,
            AgentKind::ClaudeCode,
            "first occurrence should be retained"
        );
    }

    #[test]
    fn test_dedup_same_command_different_timestamp() {
        let mut invocations = vec![
            make_invocation("cargo test", "2026-01-01T00:00:00Z", AgentKind::ClaudeCode),
            make_invocation("cargo test", "2026-01-01T00:01:00Z", AgentKind::GeminiCli),
        ];
        dedup_invocations(&mut invocations);
        assert_eq!(
            invocations.len(),
            2,
            "same cmd but different ts should be preserved"
        );
    }

    #[test]
    fn test_dedup_different_commands_same_timestamp() {
        let mut invocations = vec![
            make_invocation("cargo test", "2026-01-01T00:00:00Z", AgentKind::ClaudeCode),
            make_invocation("cargo build", "2026-01-01T00:00:00Z", AgentKind::ClaudeCode),
        ];
        dedup_invocations(&mut invocations);
        assert_eq!(
            invocations.len(),
            2,
            "different commands should be preserved"
        );
    }

    #[test]
    fn test_dedup_empty_list() {
        let mut invocations: Vec<ToolInvocation> = Vec::new();
        dedup_invocations(&mut invocations);
        assert!(invocations.is_empty());
    }

    #[test]
    fn test_tool_input_key_variants() {
        assert_eq!(
            tool_input_key(&ToolInput::Bash {
                command: "cargo test".to_string()
            }),
            "bash:cargo test"
        );
        assert_eq!(
            tool_input_key(&ToolInput::Read {
                file_path: "/tmp/test.rs".to_string()
            }),
            "read:/tmp/test.rs"
        );
        assert_eq!(
            tool_input_key(&ToolInput::Write {
                file_path: "/tmp/out.rs".to_string()
            }),
            "write:/tmp/out.rs"
        );
    }
}
