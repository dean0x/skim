//! Agent-agnostic session types (#61)

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Which agent produced this session data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentKind {
    ClaudeCode,
    CodexCli,
    GeminiCli,
    CopilotCli,
    Cursor,
    Crush,
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
            "crush" => Some(AgentKind::Crush),
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
            AgentKind::Crush => "Crush",
        }
    }

    pub(crate) fn cli_name(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::CodexCli => "codex",
            AgentKind::GeminiCli => "gemini",
            AgentKind::CopilotCli => "copilot",
            AgentKind::Cursor => "cursor",
            AgentKind::Crush => "crush",
        }
    }

    /// Parse from a CLI flag value, returning a descriptive error for unknown agents.
    ///
    /// Shared by `discover` and `learn` subcommands to avoid duplicating the
    /// error message with supported agent list.
    ///
    /// Provides a targeted migration hint for removed agents (e.g., `opencode` → `crush`).
    pub(crate) fn parse_cli_arg(s: &str) -> anyhow::Result<Self> {
        // Provide a clear migration error for the removed opencode agent
        if s == "opencode" || s == "open-code" {
            anyhow::bail!(
                "agent 'opencode' has been removed from skim.\n\
                 Use 'crush' instead: skim discover --agent crush\n\
                 Install Crush: https://crushcode.ai"
            );
        }
        Self::from_str(s).ok_or_else(|| {
            let supported: Vec<&str> = Self::all_supported().iter().map(|a| a.cli_name()).collect();
            anyhow::anyhow!(
                "unknown agent: '{}'\nSupported: {}",
                s,
                supported.join(", ")
            )
        })
    }

    /// All supported agent kinds (for dynamic help text and iteration).
    pub(crate) fn all_supported() -> &'static [AgentKind] {
        &[
            AgentKind::ClaudeCode,
            AgentKind::CodexCli,
            AgentKind::GeminiCli,
            AgentKind::CopilotCli,
            AgentKind::Cursor,
            AgentKind::Crush,
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
            AgentKind::Crush => Some(".crush/rules"),
            // These agents use single-file configs -- user pastes content manually
            AgentKind::CodexCli | AgentKind::GeminiCli => None,
        }
    }

    /// The dot-directory name (e.g., ".claude", ".gemini").
    /// Single source of truth for all agent directory names.
    ///
    /// For [`AgentKind::Cursor`], this returns `".cursor"`, which is the
    /// *project-scope* directory only (e.g., `.cursor/rules/` in a project root).
    /// Cursor's *global* config lives at `~/Library/Application Support/Cursor`
    /// (macOS) or `~/.config/Cursor` (Linux) — never under `~/.cursor`. See
    /// [`AgentKind::config_dir`] for the global path resolution.
    pub(crate) fn dot_dir_name(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => ".claude",
            AgentKind::Cursor => ".cursor",
            AgentKind::GeminiCli => ".gemini",
            AgentKind::CopilotCli => ".github",
            AgentKind::CodexCli => ".codex",
            AgentKind::Crush => ".crush",
        }
    }

    /// Global config directory (home-relative).
    ///
    /// Does NOT handle env var overrides — callers (`DetectionEnv::resolve`) add
    /// those via `CURSOR_CONFIG_DIR` / `CLAUDE_CONFIG_DIR` / etc.
    ///
    /// # Cursor — IDE-only integration (WS2B decision)
    ///
    /// skim integrates with Cursor exclusively via the Cursor IDE's PreToolUse-style
    /// hook event and `.mdc` guidance rules. The Cursor CLI has no rewrite-capable
    /// hook event, so skim cannot intercept commands run through `cursor` in a
    /// terminal session. As a result, no permissions file is seeded for Cursor (the
    /// permissions factory returns `None` for this variant).
    ///
    /// **Global config directories** (what this method selects at runtime):
    /// - **macOS (winning path)**: `~/Library/Application Support/Cursor` —
    ///   checked first via `is_dir()`; used when the directory exists.
    /// - **Linux/other (losing path / fallback)**: `~/.config/Cursor` — used when
    ///   the macOS App Support path is absent, whether because the machine is not
    ///   macOS, Cursor IDE is not yet installed, or the directory was removed.
    ///   This fallback is acceptable: `~/.config/Cursor` is the canonical global
    ///   config location on Linux, and returning it even when the directory does
    ///   not yet exist lets the installer create it in the right place.
    ///
    /// **`~/.cursor` is project-scope only** — it is the value returned by
    /// [`dot_dir_name`] and is used for CWD-relative paths (e.g., `.cursor/rules/`).
    /// It is never used as the global config root.
    pub(crate) fn config_dir(&self, home: &Path) -> PathBuf {
        match self {
            AgentKind::Cursor => {
                let macos = home
                    .join("Library")
                    .join("Application Support")
                    .join("Cursor");
                if macos.is_dir() {
                    macos
                } else {
                    home.join(".config").join("Cursor")
                }
            }
            _ => home.join(self.dot_dir_name()),
        }
    }

    /// Project-level config directory (CWD-relative).
    pub(crate) fn project_dir(&self) -> PathBuf {
        PathBuf::from(self.dot_dir_name())
    }

    /// CWD-relative detection path for project-scoped agents.
    /// Returns `Some` for agents detected via CWD (Copilot),
    /// `None` for agents detected via home directory.
    #[allow(dead_code)] // Used in tests; kept for future callers
    pub(crate) fn detect_dir(&self) -> Option<PathBuf> {
        match self {
            AgentKind::CopilotCli => Some(self.project_dir()),
            _ => None,
        }
    }

    /// Return the main instruction file path for guidance injection.
    ///
    /// Each agent has a "main instruction file" that is guaranteed to be loaded.
    /// Returns `None` when the requested scope is not supported by the agent.
    ///
    /// For `global = true`: returns home-relative absolute path (e.g., `~/.claude/CLAUDE.md`).
    ///   Respects agent-specific env var overrides via `env` parameter.
    /// For `global = false`: returns project-relative path (e.g., `CLAUDE.md`).
    pub(crate) fn instruction_file(
        &self,
        global: bool,
        env: &InstructionEnv,
    ) -> Option<std::path::PathBuf> {
        match (self, global) {
            // Global scope — with env var overrides
            (AgentKind::ClaudeCode, true) => {
                let base = env
                    .claude_config_dir
                    .clone()
                    .or_else(|| env.home_dir.as_ref().map(|h| h.join(".claude")));
                base.map(|d| d.join("CLAUDE.md"))
            }
            (AgentKind::GeminiCli, true) => {
                let base = env
                    .gemini_config_dir
                    .clone()
                    .or_else(|| env.home_dir.as_ref().map(|h| h.join(".gemini")));
                base.map(|d| d.join("GEMINI.md"))
            }
            (AgentKind::CodexCli, true) => {
                let base = env
                    .codex_home
                    .clone()
                    .or_else(|| env.home_dir.as_ref().map(|h| h.join(".codex")));
                base.map(|d| d.join("AGENTS.md"))
            }
            (AgentKind::CopilotCli, true) => {
                let base = env
                    .copilot_config_dir
                    .clone()
                    .or_else(|| env.home_dir.as_ref().map(|h| h.join(".copilot")));
                base.map(|d| d.join("copilot-instructions.md"))
            }
            (AgentKind::Crush, true) => {
                let base = env
                    .crush_config_dir
                    .clone()
                    .or_else(|| env.home_dir.as_ref().map(|h| h.join(".crush")));
                base.map(|d| d.join("AGENTS.md"))
            }
            (AgentKind::Cursor, true) => None, // UI-only, no file-based global config
            // Project scope (CWD-relative)
            (AgentKind::ClaudeCode, false) => Some("CLAUDE.md".into()),
            (AgentKind::Cursor, false) => Some(".cursor/rules/skim.mdc".into()),
            (AgentKind::CopilotCli, false) => Some(".github/copilot-instructions.md".into()),
            (AgentKind::CodexCli, false) => Some("AGENTS.md".into()),
            (AgentKind::GeminiCli, false) => Some("GEMINI.md".into()),
            (AgentKind::Crush, false) => Some("AGENTS.md".into()),
        }
    }

    /// Return the rules filename for a given agent.
    pub(crate) fn rules_filename(&self) -> &'static str {
        match self {
            AgentKind::Cursor => "skim-corrections.mdc",
            AgentKind::CopilotCli => "skim-corrections.instructions.md",
            _ => "skim-corrections.md",
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

// ============================================================================
// InstructionEnv — injected environment for instruction_file()
// ============================================================================

/// Injected environment values for [`AgentKind::instruction_file`].
///
/// Created once at a system boundary (e.g., `run_install`) and threaded to
/// callers, eliminating per-call env-var reads and enabling race-free testing.
///
/// ARCHITECTURE: Mirrors the `config_dir(&self, home: &Path)` pattern already
/// used in `AgentKind`. Process env is read exactly once via
/// [`InstructionEnv::from_process`]; test code constructs this struct directly
/// with controlled values.
#[derive(Debug, Default)]
pub(crate) struct InstructionEnv {
    pub home_dir: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` override
    pub claude_config_dir: Option<PathBuf>,
    /// `CODEX_HOME` override
    pub codex_home: Option<PathBuf>,
    /// `CRUSH_CONFIG_DIR` override
    pub crush_config_dir: Option<PathBuf>,
    /// `GEMINI_CONFIG_DIR` override (defence-in-depth: mirrors `DetectionEnv`)
    pub gemini_config_dir: Option<PathBuf>,
    /// `COPILOT_CONFIG_DIR` override (defence-in-depth: mirrors `DetectionEnv`)
    pub copilot_config_dir: Option<PathBuf>,
}

impl InstructionEnv {
    /// Read env once at the system boundary. Call this in `main`-adjacent code,
    /// then thread the struct down to callers — never call from within library functions.
    pub fn from_process() -> Self {
        let read = |name: &str| std::env::var_os(name).map(PathBuf::from);
        Self {
            home_dir: dirs::home_dir(),
            claude_config_dir: read("CLAUDE_CONFIG_DIR"),
            codex_home: read("CODEX_HOME"),
            crush_config_dir: read("CRUSH_CONFIG_DIR"),
            gemini_config_dir: read("GEMINI_CONFIG_DIR"),
            copilot_config_dir: read("COPILOT_CONFIG_DIR"),
        }
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
    fn test_agent_kind_from_str_crush() {
        assert_eq!(AgentKind::from_str("crush"), Some(AgentKind::Crush));
    }

    #[test]
    fn test_agent_kind_from_str_unknown() {
        assert_eq!(AgentKind::from_str("unknown"), None);
        assert_eq!(AgentKind::from_str(""), None);
    }

    // ---- AgentKind::parse_cli_arg ----

    #[test]
    fn test_agent_kind_parse_cli_arg_valid() {
        assert_eq!(
            AgentKind::parse_cli_arg("claude-code").unwrap(),
            AgentKind::ClaudeCode
        );
    }

    #[test]
    fn test_agent_kind_parse_cli_arg_unknown() {
        let err = AgentKind::parse_cli_arg("nonexistent").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown agent"), "got: {msg}");
        assert!(
            msg.contains("claude-code"),
            "should list supported agents, got: {msg}"
        );
    }

    #[test]
    fn test_agent_kind_parse_cli_arg_opencode_migration_hint() {
        // "opencode" and "open-code" must give a targeted migration hint, not generic error.
        for removed in ["opencode", "open-code"] {
            let err = AgentKind::parse_cli_arg(removed).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("opencode"),
                "error should mention 'opencode', got: {msg}"
            );
            assert!(
                msg.contains("crush"),
                "error should mention 'crush' as replacement, got: {msg}"
            );
        }
    }

    // ---- AgentKind::display_name / cli_name ----

    #[test]
    fn test_agent_kind_display_name() {
        assert_eq!(AgentKind::ClaudeCode.display_name(), "Claude Code");
        assert_eq!(AgentKind::CodexCli.display_name(), "Codex CLI");
        assert_eq!(AgentKind::GeminiCli.display_name(), "Gemini CLI");
        assert_eq!(AgentKind::CopilotCli.display_name(), "Copilot CLI");
        assert_eq!(AgentKind::Cursor.display_name(), "Cursor");
        assert_eq!(AgentKind::Crush.display_name(), "Crush");
    }

    #[test]
    fn test_agent_kind_cli_name() {
        assert_eq!(AgentKind::ClaudeCode.cli_name(), "claude-code");
        assert_eq!(AgentKind::CodexCli.cli_name(), "codex");
        assert_eq!(AgentKind::GeminiCli.cli_name(), "gemini");
        assert_eq!(AgentKind::CopilotCli.cli_name(), "copilot");
        assert_eq!(AgentKind::Cursor.cli_name(), "cursor");
        assert_eq!(AgentKind::Crush.cli_name(), "crush");
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
        assert!(all.contains(&AgentKind::Crush));
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
        assert_eq!(AgentKind::Crush.rules_dir(), Some(".crush/rules"));
        assert_eq!(AgentKind::CodexCli.rules_dir(), None);
        assert_eq!(AgentKind::GeminiCli.rules_dir(), None);
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

    // ---- AgentKind::dot_dir_name ----

    #[test]
    fn test_agent_kind_dot_dir_name() {
        assert_eq!(AgentKind::ClaudeCode.dot_dir_name(), ".claude");
        // Cursor's dot_dir_name is ".cursor" — the *project-scope* directory
        // (e.g., .cursor/rules/ inside a project). The global config dir lives at
        // ~/Library/Application Support/Cursor (macOS) or ~/.config/Cursor (Linux);
        // those are returned by config_dir(), not here.
        assert_eq!(AgentKind::Cursor.dot_dir_name(), ".cursor");
        assert_eq!(AgentKind::GeminiCli.dot_dir_name(), ".gemini");
        assert_eq!(AgentKind::CopilotCli.dot_dir_name(), ".github");
        assert_eq!(AgentKind::CodexCli.dot_dir_name(), ".codex");
        assert_eq!(AgentKind::Crush.dot_dir_name(), ".crush");
    }

    // ---- AgentKind::config_dir ----

    #[test]
    fn test_agent_kind_config_dir_simple_agents() {
        let home = PathBuf::from("/fake/home");
        assert_eq!(
            AgentKind::ClaudeCode.config_dir(&home),
            PathBuf::from("/fake/home/.claude")
        );
        assert_eq!(
            AgentKind::CodexCli.config_dir(&home),
            PathBuf::from("/fake/home/.codex")
        );
        assert_eq!(
            AgentKind::GeminiCli.config_dir(&home),
            PathBuf::from("/fake/home/.gemini")
        );
        assert_eq!(
            AgentKind::CopilotCli.config_dir(&home),
            PathBuf::from("/fake/home/.github")
        );
        assert_eq!(
            AgentKind::Crush.config_dir(&home),
            PathBuf::from("/fake/home/.crush")
        );
    }

    #[test]
    fn test_agent_kind_config_dir_cursor_linux_fallback() {
        // Winning path: ~/Library/Application Support/Cursor (macOS) — selected by
        // is_dir() when the directory exists on a real macOS machine with Cursor IDE.
        // Losing path / fallback: ~/.config/Cursor — selected in all other cases:
        //   Linux systems, macOS without Cursor installed, or a fake/test home dir.
        // With a fake home, the macOS App Support path never exists, so the fallback
        // is always returned here. This is the correct Linux global config location.
        let home = PathBuf::from("/fake/home");
        assert_eq!(
            AgentKind::Cursor.config_dir(&home),
            PathBuf::from("/fake/home/.config/Cursor")
        );
    }

    // ---- AgentKind::project_dir ----

    #[test]
    fn test_agent_kind_project_dir() {
        for agent in AgentKind::all_supported() {
            assert_eq!(
                agent.project_dir(),
                PathBuf::from(agent.dot_dir_name()),
                "project_dir mismatch for {:?}",
                agent
            );
        }
    }

    // ---- AgentKind::detect_dir ----

    #[test]
    fn test_agent_kind_detect_dir() {
        assert!(AgentKind::ClaudeCode.detect_dir().is_none());
        assert!(AgentKind::Cursor.detect_dir().is_none());
        assert!(AgentKind::GeminiCli.detect_dir().is_none());
        assert!(AgentKind::CodexCli.detect_dir().is_none());
        assert!(AgentKind::Crush.detect_dir().is_none());
        assert_eq!(
            AgentKind::CopilotCli.detect_dir(),
            Some(PathBuf::from(".github"))
        );
    }

    // ---- AgentKind::instruction_file ----
    //
    // Tests construct InstructionEnv directly with controlled values.
    // No env mutation, no mutex, no unsafe — deterministic and parallel-safe.

    fn fake_home() -> PathBuf {
        PathBuf::from("/fake/home")
    }

    fn default_env() -> InstructionEnv {
        InstructionEnv {
            home_dir: Some(fake_home()),
            ..Default::default()
        }
    }

    #[test]
    fn test_instruction_file_claude_code_global() {
        let env = default_env();
        let path = AgentKind::ClaudeCode.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/fake/home/.claude/CLAUDE.md"));
    }

    #[test]
    fn test_instruction_file_claude_code_project() {
        let env = default_env();
        let path = AgentKind::ClaudeCode.instruction_file(false, &env).unwrap();
        assert_eq!(path, PathBuf::from("CLAUDE.md"));
    }

    #[test]
    fn test_instruction_file_gemini_global() {
        let env = default_env();
        let path = AgentKind::GeminiCli.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/fake/home/.gemini/GEMINI.md"));
    }

    #[test]
    fn test_instruction_file_gemini_project() {
        let env = default_env();
        let path = AgentKind::GeminiCli.instruction_file(false, &env).unwrap();
        assert_eq!(path, PathBuf::from("GEMINI.md"));
    }

    #[test]
    fn test_instruction_file_cursor_project() {
        let env = default_env();
        let path = AgentKind::Cursor.instruction_file(false, &env).unwrap();
        assert_eq!(path, PathBuf::from(".cursor/rules/skim.mdc"));
    }

    #[test]
    fn test_instruction_file_cursor_global_unsupported() {
        let env = default_env();
        assert!(AgentKind::Cursor.instruction_file(true, &env).is_none());
    }

    #[test]
    fn test_instruction_file_copilot_project() {
        let env = default_env();
        let path = AgentKind::CopilotCli.instruction_file(false, &env).unwrap();
        assert_eq!(path, PathBuf::from(".github/copilot-instructions.md"));
    }

    #[test]
    fn test_instruction_file_copilot_global() {
        let env = default_env();
        let path = AgentKind::CopilotCli.instruction_file(true, &env).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/fake/home/.copilot/copilot-instructions.md")
        );
    }

    #[test]
    fn test_instruction_file_codex_global() {
        let env = default_env();
        let path = AgentKind::CodexCli.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/fake/home/.codex/AGENTS.md"));
    }

    #[test]
    fn test_instruction_file_crush_global() {
        let env = default_env();
        let path = AgentKind::Crush.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/fake/home/.crush/AGENTS.md"));
    }

    #[test]
    fn test_instruction_file_claude_code_env_override() {
        let env = InstructionEnv {
            home_dir: Some(fake_home()),
            claude_config_dir: Some(PathBuf::from("/tmp/test-claude")),
            ..Default::default()
        };
        let path = AgentKind::ClaudeCode.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/test-claude/CLAUDE.md"));
    }

    #[test]
    fn test_instruction_file_codex_env_override() {
        let env = InstructionEnv {
            home_dir: Some(fake_home()),
            codex_home: Some(PathBuf::from("/tmp/test-codex")),
            ..Default::default()
        };
        let path = AgentKind::CodexCli.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/test-codex/AGENTS.md"));
    }

    #[test]
    fn test_instruction_file_crush_env_override() {
        let env = InstructionEnv {
            home_dir: Some(fake_home()),
            crush_config_dir: Some(PathBuf::from("/tmp/test-crush")),
            ..Default::default()
        };
        let path = AgentKind::Crush.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/test-crush/AGENTS.md"));
    }

    #[test]
    fn test_instruction_file_gemini_env_override() {
        // GEMINI_CONFIG_DIR overrides home-dir-based resolution for Gemini guidance.
        // This is the defence-in-depth path: tests that set GEMINI_CONFIG_DIR will
        // have InstructionEnv populated, so guidance removal cannot touch the real
        // ~/.gemini/GEMINI.md (avoids PF-009 / PF-015).
        let env = InstructionEnv {
            home_dir: Some(fake_home()),
            gemini_config_dir: Some(PathBuf::from("/tmp/test-gemini")),
            ..Default::default()
        };
        let path = AgentKind::GeminiCli.instruction_file(true, &env).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/test-gemini/GEMINI.md"));
    }

    #[test]
    fn test_instruction_file_copilot_env_override() {
        // COPILOT_CONFIG_DIR overrides home-dir-based resolution for Copilot guidance.
        // Mirrors the GEMINI_CONFIG_DIR pattern — see test above for rationale.
        let env = InstructionEnv {
            home_dir: Some(fake_home()),
            copilot_config_dir: Some(PathBuf::from("/tmp/test-copilot")),
            ..Default::default()
        };
        let path = AgentKind::CopilotCli.instruction_file(true, &env).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-copilot/copilot-instructions.md")
        );
    }

    #[test]
    fn test_instruction_file_codex_project() {
        let env = default_env();
        let path = AgentKind::CodexCli.instruction_file(false, &env).unwrap();
        assert_eq!(path, PathBuf::from("AGENTS.md"));
    }

    #[test]
    fn test_instruction_file_crush_project() {
        let env = default_env();
        let path = AgentKind::Crush.instruction_file(false, &env).unwrap();
        assert_eq!(path, PathBuf::from("AGENTS.md"));
    }

    // ---- AgentKind::rules_filename ----

    #[test]
    fn test_agent_kind_rules_filename() {
        assert_eq!(
            AgentKind::ClaudeCode.rules_filename(),
            "skim-corrections.md"
        );
        assert_eq!(AgentKind::Cursor.rules_filename(), "skim-corrections.mdc");
        assert_eq!(
            AgentKind::CopilotCli.rules_filename(),
            "skim-corrections.instructions.md"
        );
        assert_eq!(AgentKind::CodexCli.rules_filename(), "skim-corrections.md");
        assert_eq!(AgentKind::GeminiCli.rules_filename(), "skim-corrections.md");
        assert_eq!(AgentKind::Crush.rules_filename(), "skim-corrections.md");
    }
}
