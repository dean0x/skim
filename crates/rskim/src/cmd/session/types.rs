//! Agent-agnostic session types (#61)

use std::path::PathBuf;
use std::time::SystemTime;

/// Which agent produced this session data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentKind {
    ClaudeCode,
    CodexCli,
    GeminiCli,
    CopilotCli,
    Cursor,
    OpenCode,
}

impl AgentKind {
    /// Parse from CLI flag value.
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "claude-code" | "claude" => Some(AgentKind::ClaudeCode),
            "codex" | "codex-cli" => Some(AgentKind::CodexCli),
            "gemini" | "gemini-cli" => Some(AgentKind::GeminiCli),
            "copilot" | "copilot-cli" => Some(AgentKind::CopilotCli),
            "cursor" => Some(AgentKind::Cursor),
            "opencode" | "open-code" => Some(AgentKind::OpenCode),
            _ => None,
        }
    }

    pub(crate) fn display_name(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::CodexCli => "Codex CLI",
            AgentKind::GeminiCli => "Gemini CLI",
            AgentKind::CopilotCli => "Copilot CLI",
            AgentKind::Cursor => "Cursor",
            AgentKind::OpenCode => "OpenCode",
        }
    }

    pub(crate) fn cli_name(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::CodexCli => "codex",
            AgentKind::GeminiCli => "gemini",
            AgentKind::CopilotCli => "copilot",
            AgentKind::Cursor => "cursor",
            AgentKind::OpenCode => "opencode",
        }
    }

    /// All supported agent kinds (for dynamic help text and iteration).
    pub(crate) fn all_supported() -> &'static [AgentKind] {
        &[
            AgentKind::ClaudeCode,
            AgentKind::CodexCli,
            AgentKind::GeminiCli,
            AgentKind::CopilotCli,
            AgentKind::Cursor,
            AgentKind::OpenCode,
        ]
    }

    /// Returns the native rules directory/file path convention for this agent.
    /// Returns None for agents that use single-file configs (user pastes content).
    #[allow(dead_code)] // Used by learn.rs per-agent rules (phase 0.5)
    pub(crate) fn rules_dir(&self) -> Option<&'static str> {
        match self {
            AgentKind::ClaudeCode => Some(".claude/rules"),
            AgentKind::Cursor => Some(".cursor/rules"),
            AgentKind::CopilotCli => Some(".github/instructions"),
            // These agents use single-file configs -- user pastes content manually
            AgentKind::CodexCli | AgentKind::GeminiCli | AgentKind::OpenCode => None,
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// Time-based filter for session scanning.
#[derive(Debug, Clone)]
pub(crate) struct TimeFilter {
    /// Only include sessions modified after this time.
    pub(crate) since: Option<SystemTime>,
    /// Only the most recent session.
    pub(crate) latest_only: bool,
}

impl Default for TimeFilter {
    fn default() -> Self {
        // Default: last 24 hours
        Self {
            since: Some(SystemTime::now() - std::time::Duration::from_secs(24 * 3600)),
            latest_only: false,
        }
    }
}

/// A session file discovered by a provider.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields used by SessionProvider implementations and tests
pub(crate) struct SessionFile {
    pub(crate) path: PathBuf,
    pub(crate) modified: SystemTime,
    pub(crate) agent: AgentKind,
    pub(crate) session_id: String,
}

/// Agent-agnostic tool invocation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields populated by providers, consumed by discover/learn commands
pub(crate) struct ToolInvocation {
    pub(crate) tool_name: String,
    pub(crate) input: ToolInput,
    pub(crate) timestamp: String,
    pub(crate) session_id: String,
    pub(crate) agent: AgentKind,
    pub(crate) result: Option<ToolResult>,
}

/// Normalized tool input.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants populated by provider parsers, consumed by discover/learn commands
pub(crate) enum ToolInput {
    Read {
        file_path: String,
    },
    Bash {
        command: String,
    },
    Write {
        file_path: String,
    },
    Glob {
        pattern: String,
    },
    Grep {
        pattern: String,
    },
    Edit {
        file_path: String,
    },
    Other {
        tool_name: String,
        raw: serde_json::Value,
    },
}

#[allow(dead_code)] // Used by provider parsers and discover/learn commands
impl ToolInput {
    /// Extract file path if this is a file-related operation.
    pub(crate) fn file_path(&self) -> Option<&str> {
        match self {
            ToolInput::Read { file_path }
            | ToolInput::Write { file_path }
            | ToolInput::Edit { file_path } => Some(file_path),
            _ => None,
        }
    }
}

/// Tool execution result.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields populated by providers, consumed by discover/learn commands
pub(crate) struct ToolResult {
    pub(crate) content: String,
    pub(crate) is_error: bool,
}

// ============================================================================
// Shared duration parsing
// ============================================================================

/// Parse a human-readable duration string into a `SystemTime` in the past.
///
/// Supports: `Nd` (days), `Nh` (hours), `Nw` (weeks).
///
/// Shared by `discover` and `learn` subcommands.
pub(crate) fn parse_duration_ago(s: &str) -> anyhow::Result<SystemTime> {
    let s = s.trim();
    let (num_str, unit) = if let Some(stripped) = s.strip_suffix('d') {
        (stripped, "d")
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, "h")
    } else if let Some(stripped) = s.strip_suffix('w') {
        (stripped, "w")
    } else {
        anyhow::bail!("invalid duration format: '{s}' (expected Nd, Nh, or Nw)");
    };

    let num: u64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid number in duration: '{s}'"))?;

    let secs = match unit {
        "h" => num.checked_mul(3600),
        "d" => num.checked_mul(86400),
        "w" => num.checked_mul(7 * 86400),
        _ => unreachable!(),
    }
    .ok_or_else(|| anyhow::anyhow!("duration value too large: '{s}'"))?;

    Ok(SystemTime::now() - std::time::Duration::from_secs(secs))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- AgentKind::from_str ----

    #[test]
    fn test_agent_kind_from_str_claude_code() {
        assert_eq!(
            AgentKind::from_str("claude-code"),
            Some(AgentKind::ClaudeCode)
        );
        assert_eq!(AgentKind::from_str("claude"), Some(AgentKind::ClaudeCode));
    }

    #[test]
    fn test_agent_kind_from_str_codex() {
        assert_eq!(AgentKind::from_str("codex"), Some(AgentKind::CodexCli));
        assert_eq!(AgentKind::from_str("codex-cli"), Some(AgentKind::CodexCli));
    }

    #[test]
    fn test_agent_kind_from_str_gemini() {
        assert_eq!(AgentKind::from_str("gemini"), Some(AgentKind::GeminiCli));
        assert_eq!(
            AgentKind::from_str("gemini-cli"),
            Some(AgentKind::GeminiCli)
        );
    }

    #[test]
    fn test_agent_kind_from_str_copilot() {
        assert_eq!(AgentKind::from_str("copilot"), Some(AgentKind::CopilotCli));
        assert_eq!(
            AgentKind::from_str("copilot-cli"),
            Some(AgentKind::CopilotCli)
        );
    }

    #[test]
    fn test_agent_kind_from_str_cursor() {
        assert_eq!(AgentKind::from_str("cursor"), Some(AgentKind::Cursor));
    }

    #[test]
    fn test_agent_kind_from_str_opencode() {
        assert_eq!(AgentKind::from_str("opencode"), Some(AgentKind::OpenCode));
        assert_eq!(AgentKind::from_str("open-code"), Some(AgentKind::OpenCode));
    }

    #[test]
    fn test_agent_kind_from_str_unknown() {
        assert_eq!(AgentKind::from_str("unknown"), None);
        assert_eq!(AgentKind::from_str(""), None);
    }

    // ---- AgentKind::display_name / cli_name ----

    #[test]
    fn test_agent_kind_display_name() {
        assert_eq!(AgentKind::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(AgentKind::CodexCli.display_name(), "Codex CLI");
        assert_eq!(AgentKind::GeminiCli.display_name(), "Gemini CLI");
        assert_eq!(AgentKind::CopilotCli.display_name(), "Copilot CLI");
        assert_eq!(AgentKind::Cursor.display_name(), "Cursor");
        assert_eq!(AgentKind::OpenCode.display_name(), "OpenCode");
    }

    #[test]
    fn test_agent_kind_cli_name() {
        assert_eq!(AgentKind::ClaudeCode.cli_name(), "claude-code");
        assert_eq!(AgentKind::CodexCli.cli_name(), "codex");
        assert_eq!(AgentKind::GeminiCli.cli_name(), "gemini");
        assert_eq!(AgentKind::CopilotCli.cli_name(), "copilot");
        assert_eq!(AgentKind::Cursor.cli_name(), "cursor");
        assert_eq!(AgentKind::OpenCode.cli_name(), "opencode");
    }

    // ---- AgentKind::all_supported ----

    #[test]
    fn test_agent_kind_all_supported() {
        let all = AgentKind::all_supported();
        assert_eq!(all.len(), 6);
        assert!(all.contains(&AgentKind::ClaudeCode));
        assert!(all.contains(&AgentKind::CodexCli));
        assert!(all.contains(&AgentKind::GeminiCli));
        assert!(all.contains(&AgentKind::CopilotCli));
        assert!(all.contains(&AgentKind::Cursor));
        assert!(all.contains(&AgentKind::OpenCode));
    }

    // ---- AgentKind::rules_dir ----

    #[test]
    fn test_agent_kind_rules_dir() {
        assert_eq!(AgentKind::ClaudeCode.rules_dir(), Some(".claude/rules"));
        assert_eq!(AgentKind::Cursor.rules_dir(), Some(".cursor/rules"));
        assert_eq!(
            AgentKind::CopilotCli.rules_dir(),
            Some(".github/instructions")
        );
        assert_eq!(AgentKind::CodexCli.rules_dir(), None);
        assert_eq!(AgentKind::GeminiCli.rules_dir(), None);
        assert_eq!(AgentKind::OpenCode.rules_dir(), None);
    }

    // ---- Display impl ----

    #[test]
    fn test_agent_kind_display() {
        assert_eq!(format!("{}", AgentKind::ClaudeCode), "Claude Code");
        assert_eq!(format!("{}", AgentKind::Cursor), "Cursor");
    }

    // ---- Round-trip: cli_name -> from_str ----

    #[test]
    fn test_agent_kind_roundtrip() {
        for agent in AgentKind::all_supported() {
            let parsed = AgentKind::from_str(agent.cli_name());
            assert_eq!(parsed, Some(*agent), "round-trip failed for {:?}", agent);
        }
    }
}
