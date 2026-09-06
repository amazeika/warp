//! Capability tokens Warp advertises so plugins can degrade instead of break.
use serde::{Deserialize, Serialize};

/// One negotiable slice of the extension API.
///
/// A plugin built against a newer Warp must not assume an older build
/// implements everything; a token absent from the handshake means the
/// corresponding methods answer `unsupported_capability`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "commands.v1")]
    CommandsV1,
    #[serde(rename = "panel.tree.v1")]
    PanelTreeV1,
    #[serde(rename = "workspace.context.v1")]
    WorkspaceContextV1,
    #[serde(rename = "execution.v1")]
    ExecutionV1,
    #[serde(rename = "diff.open.v1")]
    DiffOpenV1,
    #[serde(rename = "file.open.v1")]
    FileOpenV1,
    #[serde(rename = "dialog.confirm.v1")]
    DialogConfirmV1,
    #[serde(rename = "notification.v1")]
    NotificationV1,
}

impl Capability {
    pub const ALL: &'static [Capability] = &[
        Capability::CommandsV1,
        Capability::PanelTreeV1,
        Capability::WorkspaceContextV1,
        Capability::ExecutionV1,
        Capability::DiffOpenV1,
        Capability::FileOpenV1,
        Capability::DialogConfirmV1,
        Capability::NotificationV1,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::CommandsV1 => "commands.v1",
            Capability::PanelTreeV1 => "panel.tree.v1",
            Capability::WorkspaceContextV1 => "workspace.context.v1",
            Capability::ExecutionV1 => "execution.v1",
            Capability::DiffOpenV1 => "diff.open.v1",
            Capability::FileOpenV1 => "file.open.v1",
            Capability::DialogConfirmV1 => "dialog.confirm.v1",
            Capability::NotificationV1 => "notification.v1",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod tests;
