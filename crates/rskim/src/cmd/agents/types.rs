//! Agent detection types used by the `skim agents` subcommand.

use crate::cmd::session::AgentKind;

/// Detected agent status report.
pub(super) struct AgentStatus {
    pub(super) kind: AgentKind,
    pub(super) detected: bool,
    pub(super) sessions: Option<SessionInfo>,
    pub(super) hooks: HookStatus,
    pub(super) rules: Option<RulesInfo>,
}

/// Session file information.
pub(super) struct SessionInfo {
    pub(super) path: String,
    pub(super) detail: String, // e.g., "42 files" or "1.2 GB"
}

/// Hook installation status.
#[derive(Debug)]
pub(super) enum HookStatus {
    Installed {
        version: Option<String>,
        integrity: &'static str,
    },
    NotInstalled,
    NotSupported {
        note: &'static str,
    },
}

/// Rules directory information.
pub(super) struct RulesInfo {
    pub(super) path: String,
    pub(super) exists: bool,
}
