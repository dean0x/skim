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
pub(crate) mod types;

#[allow(unused_imports)] // ToolResult used by learn.rs tests
pub(crate) use types::{
    parse_duration_ago, AgentKind, SessionFile, TimeFilter, ToolInput, ToolInvocation, ToolResult,
};

// ============================================================================
// SessionProvider trait
// ============================================================================

/// Trait implemented by each agent's session file parser.
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
    providers
}

/// Get providers filtered by agent kind, or all detected agents.
pub(crate) fn get_providers(agent_filter: Option<AgentKind>) -> Vec<Box<dyn SessionProvider>> {
    match agent_filter {
        Some(kind) => {
            let all = detect_agents();
            all.into_iter().filter(|p| p.agent_kind() == kind).collect()
        }
        None => detect_agents(),
    }
}

/// Collect all tool invocations from the given providers within a time filter.
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
    Ok(all_invocations)
}
