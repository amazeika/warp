//! Permissions a manifest requests and Warp enforces on every dispatch.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// One category of access a user can grant an extension.
///
/// Categories are coarse on purpose: a prompt the user cannot read is not
/// consent. Finer control lives below this layer — an executable allowlist for
/// [`Permission::ProcessExecute`], and per-action confirmation dialogs for
/// operations that are hard to undo.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "workspace.read")]
    WorkspaceRead,
    #[serde(rename = "workspace.write")]
    WorkspaceWrite,
    #[serde(rename = "process.execute")]
    ProcessExecute,
    #[serde(rename = "network.access")]
    NetworkAccess,
    #[serde(rename = "ui.panel")]
    UiPanel,
    #[serde(rename = "ui.commands")]
    UiCommands,
    #[serde(rename = "ui.notifications")]
    UiNotifications,
    #[serde(rename = "ui.dialogs")]
    UiDialogs,
    #[serde(rename = "git.read")]
    GitRead,
    #[serde(rename = "git.mutate")]
    GitMutate,
    #[serde(rename = "session.read")]
    SessionRead,
    #[serde(rename = "session.execute")]
    SessionExecute,
}

impl Permission {
    pub const ALL: &'static [Permission] = &[
        Permission::WorkspaceRead,
        Permission::WorkspaceWrite,
        Permission::ProcessExecute,
        Permission::NetworkAccess,
        Permission::UiPanel,
        Permission::UiCommands,
        Permission::UiNotifications,
        Permission::UiDialogs,
        Permission::GitRead,
        Permission::GitMutate,
        Permission::SessionRead,
        Permission::SessionExecute,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Permission::WorkspaceRead => "workspace.read",
            Permission::WorkspaceWrite => "workspace.write",
            Permission::ProcessExecute => "process.execute",
            Permission::NetworkAccess => "network.access",
            Permission::UiPanel => "ui.panel",
            Permission::UiCommands => "ui.commands",
            Permission::UiNotifications => "ui.notifications",
            Permission::UiDialogs => "ui.dialogs",
            Permission::GitRead => "git.read",
            Permission::GitMutate => "git.mutate",
            Permission::SessionRead => "session.read",
            Permission::SessionExecute => "session.execute",
        }
    }

    /// One-line description shown in the permission prompt.
    pub fn description(self) -> &'static str {
        match self {
            Permission::WorkspaceRead => "Read files and repository state in the active workspace",
            Permission::WorkspaceWrite => "Modify files in the active workspace",
            Permission::ProcessExecute => "Run allowed programs in the active workspace",
            Permission::NetworkAccess => "Make network requests",
            Permission::UiPanel => "Add a panel to Warp",
            Permission::UiCommands => "Add entries to the command palette",
            Permission::UiNotifications => "Show notifications",
            Permission::UiDialogs => "Show confirmation and input dialogs",
            Permission::GitRead => "Read Git repository state",
            Permission::GitMutate => "Change Git repository state",
            Permission::SessionRead => "Read the active session and its connection",
            Permission::SessionExecute => "Run programs inside the active session",
        }
    }
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The set of permissions actually granted to a running extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PermissionSet(BTreeSet<Permission>);

impl PermissionSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, permission: Permission) -> bool {
        self.0.contains(&permission)
    }

    pub fn insert(&mut self, permission: Permission) -> bool {
        self.0.insert(permission)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = Permission> + '_ {
        self.0.iter().copied()
    }

    /// True when every permission in `other` is also granted here, which is the
    /// test for whether a re-prompt is needed after a manifest changes.
    pub fn covers(&self, other: &PermissionSet) -> bool {
        other.0.is_subset(&self.0)
    }
}

impl FromIterator<Permission> for PermissionSet {
    fn from_iter<T: IntoIterator<Item = Permission>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
