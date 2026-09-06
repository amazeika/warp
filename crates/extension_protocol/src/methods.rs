//! Host methods a plugin may call, and their typed parameters and results.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::capabilities::Capability;
use crate::permissions::Permission;

/// Every call a plugin can make into Warp.
///
/// The wire names are dotted and camel-cased (`workspace.getContext`) and are
/// public API. Adding a method requires extending [`Method::permission`] and
/// [`Method::capability`], which keeps the permission gate total by
/// construction.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "extension.initialize")]
    ExtensionInitialize,
    #[serde(rename = "workspace.getContext")]
    WorkspaceGetContext,
    #[serde(rename = "execution.run")]
    ExecutionRun,
    #[serde(rename = "file.open")]
    FileOpen,
    #[serde(rename = "diff.openWorkingTree")]
    DiffOpenWorkingTree,
    #[serde(rename = "diff.openFile")]
    DiffOpenFile,
    #[serde(rename = "notification.show")]
    NotificationShow,
    #[serde(rename = "dialog.confirm")]
    DialogConfirm,
    #[serde(rename = "dialog.input")]
    DialogInput,
    #[serde(rename = "dialog.select")]
    DialogSelect,
    #[serde(rename = "panel.setState")]
    PanelSetState,
}

impl Method {
    /// Every method, used to prove the permission and capability maps are total.
    pub const ALL: &'static [Method] = &[
        Method::ExtensionInitialize,
        Method::WorkspaceGetContext,
        Method::ExecutionRun,
        Method::FileOpen,
        Method::DiffOpenWorkingTree,
        Method::DiffOpenFile,
        Method::NotificationShow,
        Method::DialogConfirm,
        Method::DialogInput,
        Method::DialogSelect,
        Method::PanelSetState,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Method::ExtensionInitialize => "extension.initialize",
            Method::WorkspaceGetContext => "workspace.getContext",
            Method::ExecutionRun => "execution.run",
            Method::FileOpen => "file.open",
            Method::DiffOpenWorkingTree => "diff.openWorkingTree",
            Method::DiffOpenFile => "diff.openFile",
            Method::NotificationShow => "notification.show",
            Method::DialogConfirm => "dialog.confirm",
            Method::DialogInput => "dialog.input",
            Method::DialogSelect => "dialog.select",
            Method::PanelSetState => "panel.setState",
        }
    }

    /// The permission that must be granted before this method is dispatched.
    ///
    /// `None` means the method is part of the handshake and precedes any grant.
    pub fn permission(self) -> Option<Permission> {
        match self {
            Method::ExtensionInitialize => None,
            Method::WorkspaceGetContext => Some(Permission::WorkspaceRead),
            Method::ExecutionRun => Some(Permission::ProcessExecute),
            Method::FileOpen => Some(Permission::WorkspaceRead),
            Method::DiffOpenWorkingTree | Method::DiffOpenFile => Some(Permission::WorkspaceRead),
            Method::NotificationShow => Some(Permission::UiNotifications),
            Method::DialogConfirm | Method::DialogInput | Method::DialogSelect => {
                Some(Permission::UiDialogs)
            }
            Method::PanelSetState => Some(Permission::UiPanel),
        }
    }

    /// The capability Warp must advertise for this method to be callable.
    pub fn capability(self) -> Option<Capability> {
        match self {
            Method::ExtensionInitialize => None,
            Method::WorkspaceGetContext => Some(Capability::WorkspaceContextV1),
            Method::ExecutionRun => Some(Capability::ExecutionV1),
            Method::FileOpen => Some(Capability::FileOpenV1),
            Method::DiffOpenWorkingTree | Method::DiffOpenFile => Some(Capability::DiffOpenV1),
            Method::NotificationShow => Some(Capability::NotificationV1),
            Method::DialogConfirm | Method::DialogInput | Method::DialogSelect => {
                Some(Capability::DialogConfirmV1)
            }
            Method::PanelSetState => Some(Capability::PanelTreeV1),
        }
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Opening message of the handshake, sent by the plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeParams {
    pub extension_id: String,
    pub api_version: u32,
}

/// Warp's handshake reply, telling the plugin what this build supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitializeResult {
    pub protocol: u32,
    pub api_version: u32,
    pub capabilities: Vec<Capability>,
}

/// Where a plugin's request is carried out.
///
/// Every method that acts on the world carries one explicitly. Warp never
/// substitutes "whatever is focused now", because doing so is how a remote
/// mutation lands on a same-named local path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionTarget {
    Local,
    Ssh { session_id: String },
}

/// The workspace a plugin is currently acting in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceContext {
    pub workspace_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub execution_target: ExecutionTarget,
}

/// Arguments for running one allow-listed executable.
///
/// `args` is an argv vector rather than a shell string: there is no `sh -c`
/// path in this API, so a plugin cannot smuggle a second command through
/// argument text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRunParams {
    pub target: ExecutionTarget,
    pub cwd: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRunResult {
    /// `None` when the process was terminated by a signal rather than exiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    /// Set when output exceeded the host's limit and was cut short.
    #[serde(default)]
    pub truncated: bool,
}

/// Which of Warp's viewers a file should prefer when more than one applies.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferredView {
    Editor,
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileOpenParams {
    pub target: ExecutionTarget,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_view: Option<PreferredView>,
}

/// Which pair of trees a diff compares.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffComparison {
    HeadToWorktree,
    HeadToIndex,
    IndexToWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffOpenWorkingTreeParams {
    pub target: ExecutionTarget,
    pub repository_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffOpenFileParams {
    pub target: ExecutionTarget,
    pub repository_root: String,
    /// Repository-relative path, so the same value round-trips through panel
    /// item ids and Git's own output.
    pub path: String,
    pub comparison: DiffComparison,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub level: NotificationLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogConfirmParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub confirm_label: String,
    pub cancel_label: String,
    /// Renders Warp's destructive treatment. Warp owns this presentation so a
    /// plugin cannot make an irreversible action look routine.
    #[serde(default)]
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogConfirmResult {
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogInputParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_value: Option<String>,
}

/// `None` means the user dismissed the dialog without entering a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogInputResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogSelectOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogSelectParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub options: Vec<DialogSelectOption>,
}

/// `None` means the user dismissed the dialog without choosing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialogSelectResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_id: Option<String>,
}

/// What a panel is currently showing, aside from its content.
///
/// Loading, empty and error presentations belong to Warp so extension panels
/// match built-in ones; the plugin only says which state it is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PanelStatus {
    Loading,
    Ready,
    Empty { message: String },
    Error { message: String },
}

/// One row action, surfaced as a context-menu entry or inline affordance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelItemAction {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelItem {
    /// Stable within its section, and echoed back on `panel.action`.
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<PanelItemAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PanelItem>,
    #[serde(default)]
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelSection {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<PanelItem>,
}

/// A complete snapshot of one panel. Warp renders it with native components;
/// there is no markup or executable UI in this model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelViewState {
    /// Must match a panel the manifest declared.
    pub panel_id: String,
    pub status: PanelStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<PanelSection>,
}

#[cfg(test)]
#[path = "methods_tests.rs"]
mod tests;
