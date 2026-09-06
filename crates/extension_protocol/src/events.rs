//! One-way notifications Warp sends to a running plugin.
use serde::{Deserialize, Serialize};

use crate::methods::ExecutionTarget;

/// Every event a plugin can receive.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    #[serde(rename = "workspace.changed")]
    WorkspaceChanged,
    #[serde(rename = "cwd.changed")]
    CwdChanged,
    #[serde(rename = "repository.changed")]
    RepositoryChanged,
    #[serde(rename = "session.changed")]
    SessionChanged,
    #[serde(rename = "ssh.connected")]
    SshConnected,
    #[serde(rename = "ssh.disconnected")]
    SshDisconnected,
    #[serde(rename = "command.invoked")]
    CommandInvoked,
    #[serde(rename = "panel.action")]
    PanelAction,
    /// Sent once before Warp stops the plugin, so it can exit cleanly rather
    /// than being killed at the stop timeout.
    #[serde(rename = "extension.shutdown")]
    ExtensionShutdown,
}

impl EventKind {
    pub const ALL: &'static [EventKind] = &[
        EventKind::WorkspaceChanged,
        EventKind::CwdChanged,
        EventKind::RepositoryChanged,
        EventKind::SessionChanged,
        EventKind::SshConnected,
        EventKind::SshDisconnected,
        EventKind::CommandInvoked,
        EventKind::PanelAction,
        EventKind::ExtensionShutdown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::WorkspaceChanged => "workspace.changed",
            EventKind::CwdChanged => "cwd.changed",
            EventKind::RepositoryChanged => "repository.changed",
            EventKind::SessionChanged => "session.changed",
            EventKind::SshConnected => "ssh.connected",
            EventKind::SshDisconnected => "ssh.disconnected",
            EventKind::CommandInvoked => "command.invoked",
            EventKind::PanelAction => "panel.action",
            EventKind::ExtensionShutdown => "extension.shutdown",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identity every actionable event carries.
///
/// An action stays attached to the workspace, session and repository it started
/// in, so a plugin that is slow to respond cannot have its work retargeted by a
/// focus change it never saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionOrigin {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<String>,
    pub execution_target: ExecutionTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextChangedParams {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionChangedParams {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub execution_target: ExecutionTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInvokedParams {
    /// A command id the manifest declared.
    pub command_id: String,
    pub origin: ActionOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelActionParams {
    pub panel_id: String,
    pub item_id: String,
    pub action_id: String,
    pub origin: ActionOrigin,
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
