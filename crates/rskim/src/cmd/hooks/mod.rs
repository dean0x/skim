//! Hook protocol abstraction for multi-agent hook integration.
//!
//! Each agent that supports tool interception hooks implements `HookProtocol`.
//! Agents without hook support use awareness-only installation.

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod cursor;
pub(crate) mod gemini;
pub(crate) mod opencode;

use super::session::AgentKind;

/// Whether an agent supports real hooks or awareness-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookSupport {
    /// Agent supports real tool interception hooks.
    RealHook,
    /// Agent has no hook mechanism; install awareness files only.
    AwarenessOnly,
}

/// Input extracted from agent's hook event JSON.
#[derive(Debug, Clone)]
pub(crate) struct HookInput {
    pub(crate) command: String,
}

/// Result of a hook installation.
#[derive(Debug)]
#[allow(dead_code)] // Used in tests; will be consumed when init dispatches via protocol
pub(crate) struct InstallResult {
    pub(crate) script_path: Option<std::path::PathBuf>,
    pub(crate) config_patched: bool,
}

/// Options passed to install/uninstall.
#[derive(Debug)]
#[allow(dead_code)] // Used in tests; will be consumed when init dispatches via protocol
pub(crate) struct InstallOpts {
    pub(crate) binary_path: std::path::PathBuf,
    pub(crate) version: String,
    pub(crate) config_dir: std::path::PathBuf,
    pub(crate) project_scope: bool,
    pub(crate) dry_run: bool,
}

/// Options for uninstall.
#[derive(Debug)]
#[allow(dead_code)] // Used in tests; will be consumed when init dispatches via protocol
pub(crate) struct UninstallOpts {
    pub(crate) config_dir: std::path::PathBuf,
    pub(crate) force: bool,
}

/// Trait for agent-specific hook protocols.
///
/// Each agent's hook system is different. This trait normalizes:
/// - Hook event parsing (agent JSON -> HookInput)
/// - Response formatting (rewritten command -> agent JSON)
/// - Script generation (binary path -> shell script)
/// - Installation/uninstallation
#[allow(dead_code)] // Some methods used only in tests; full dispatch planned for init --agent
pub(crate) trait HookProtocol {
    fn agent_kind(&self) -> AgentKind;
    fn hook_support(&self) -> HookSupport;
    fn parse_input(&self, json: &serde_json::Value) -> Option<HookInput>;
    fn format_response(&self, rewritten_command: &str) -> serde_json::Value;
    fn generate_script(&self, binary_path: &str, version: &str) -> String;
    fn install(&self, opts: &InstallOpts) -> anyhow::Result<InstallResult>;
    fn uninstall(&self, opts: &UninstallOpts) -> anyhow::Result<()>;
}

/// Factory: create the appropriate HookProtocol implementation for a given agent.
pub(crate) fn protocol_for_agent(kind: AgentKind) -> Box<dyn HookProtocol> {
    match kind {
        AgentKind::ClaudeCode => Box::new(claude::ClaudeCodeHook),
        AgentKind::Cursor => Box::new(cursor::CursorHook),
        AgentKind::GeminiCli => Box::new(gemini::GeminiCliHook),
        AgentKind::CopilotCli => Box::new(copilot::CopilotCliHook),
        AgentKind::CodexCli => Box::new(codex::CodexCliHook),
        AgentKind::OpenCode => Box::new(opencode::OpenCodeHook),
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_support_equality() {
        assert_eq!(HookSupport::RealHook, HookSupport::RealHook);
        assert_ne!(HookSupport::RealHook, HookSupport::AwarenessOnly);
    }

    #[test]
    fn test_hook_input_clone() {
        let input = HookInput {
            command: "cargo test".to_string(),
        };
        let cloned = input.clone();
        assert_eq!(cloned.command, "cargo test");
    }
}
