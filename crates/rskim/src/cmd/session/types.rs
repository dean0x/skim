//! Agent-agnostic session types (#61)

use std::path::PathBuf;
use std::time::SystemTime;

/// Which agent produced this session data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentKind {
    ClaudeCode,
    // Future: CopilotCli, GeminiCli, CodexCli, Cursor, Cline, ...
}

impl AgentKind {
    /// Parse from CLI flag value.
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "claude-code" | "claude" => Some(AgentKind::ClaudeCode),
            _ => None,
        }
    }

    pub(crate) fn display_name(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
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
#[allow(dead_code)] // Fields populated by providers, consumed by analysis and future learn command
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
#[allow(dead_code)] // Variants populated by provider parsers, consumed by analysis and future learn command
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

#[allow(dead_code)] // Used by tests and future learn command
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
#[allow(dead_code)] // Fields populated by providers, consumed by analysis and future learn command
pub(crate) struct ToolResult {
    pub(crate) content: String,
    pub(crate) is_error: bool,
}
