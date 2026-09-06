//! The declarative `extension.toml` Warp reads before starting a plugin.
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::permissions::{Permission, PermissionSet};
use crate::protocol::API_VERSION;

/// Manifest file name expected inside every extension directory.
pub const MANIFEST_FILE_NAME: &str = "extension.toml";

/// A parsed, not-yet-validated `extension.toml`.
///
/// Parsing and validation are separate so a malformed manifest can still be
/// listed in the UI with a reason instead of vanishing from discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(deserialize_with = "deserialize_api_version")]
    pub api_version: u32,
    /// Path to the executable, relative to the extension directory.
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<Activation>,
    #[serde(default)]
    pub permissions: ManifestPermissions,
    #[serde(default)]
    pub execution: ExecutionPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandContribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<PanelContribution>,
}

impl ExtensionManifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        toml::from_str(source).map_err(|err| ManifestError::Malformed(err.to_string()))
    }

    /// Checks everything that must hold before Warp will run this extension.
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err(ManifestError::MissingField("name"));
        }
        if self.version.trim().is_empty() {
            return Err(ManifestError::MissingField("version"));
        }
        if self.api_version != API_VERSION {
            return Err(ManifestError::UnsupportedApiVersion {
                requested: self.api_version,
                supported: API_VERSION,
            });
        }
        validate_command(&self.command)?;
        validate_unique_ids(
            self.commands.iter().map(|command| command.id.as_str()),
            ManifestError::DuplicateCommandId,
            ManifestError::EmptyCommandId,
        )?;
        validate_unique_ids(
            self.panels.iter().map(|panel| panel.id.as_str()),
            ManifestError::DuplicatePanelId,
            ManifestError::EmptyPanelId,
        )?;

        if self.permissions.process_execute {
            if self.execution.allowed_executables.is_empty() {
                return Err(ManifestError::MissingExecutableAllowlist);
            }
            for executable in &self.execution.allowed_executables {
                validate_allowed_executable(executable)?;
            }
        }
        Ok(())
    }

    /// Permissions to enforce at dispatch time.
    ///
    /// Declaring a contribution is itself the request for the matching UI
    /// permission, so a manifest does not have to say the same thing twice, and
    /// a plugin still cannot drive a surface it never declared.
    pub fn effective_permissions(&self) -> PermissionSet {
        let mut granted = self.permissions.to_set();
        if !self.commands.is_empty() {
            granted.insert(Permission::UiCommands);
        }
        if !self.panels.is_empty() {
            granted.insert(Permission::UiPanel);
        }
        granted
    }

    /// True when the extension declared this panel and may therefore drive it.
    pub fn declares_panel(&self, panel_id: &str) -> bool {
        self.panels.iter().any(|panel| panel.id == panel_id)
    }

    pub fn declares_command(&self, command_id: &str) -> bool {
        self.commands.iter().any(|command| command.id == command_id)
    }

    /// True when `executable` may be passed to `execution.run`.
    pub fn allows_executable(&self, executable: &str) -> bool {
        self.execution
            .allowed_executables
            .iter()
            .any(|allowed| allowed == executable)
    }
}

/// When an extension becomes eligible to run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    /// Entry names looked for in the working directory and its ancestors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_contains: Vec<String>,
}

/// The `[permissions]` table, written as flags so the file reads as prose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestPermissions {
    #[serde(default)]
    pub workspace_read: bool,
    #[serde(default)]
    pub workspace_write: bool,
    #[serde(default)]
    pub process_execute: bool,
    #[serde(default)]
    pub network_access: bool,
    #[serde(default)]
    pub ui_panel: bool,
    #[serde(default)]
    pub ui_commands: bool,
    #[serde(default)]
    pub ui_notifications: bool,
    #[serde(default)]
    pub ui_dialogs: bool,
    #[serde(default)]
    pub git_read: bool,
    #[serde(default)]
    pub git_mutate: bool,
    #[serde(default)]
    pub session_read: bool,
    #[serde(default)]
    pub session_execute: bool,
}

impl ManifestPermissions {
    pub fn to_set(self) -> PermissionSet {
        let flags = [
            (self.workspace_read, Permission::WorkspaceRead),
            (self.workspace_write, Permission::WorkspaceWrite),
            (self.process_execute, Permission::ProcessExecute),
            (self.network_access, Permission::NetworkAccess),
            (self.ui_panel, Permission::UiPanel),
            (self.ui_commands, Permission::UiCommands),
            (self.ui_notifications, Permission::UiNotifications),
            (self.ui_dialogs, Permission::UiDialogs),
            (self.git_read, Permission::GitRead),
            (self.git_mutate, Permission::GitMutate),
            (self.session_read, Permission::SessionRead),
            (self.session_execute, Permission::SessionExecute),
        ];
        flags
            .into_iter()
            .filter_map(|(requested, permission)| requested.then_some(permission))
            .collect()
    }
}

/// The `[execution]` table narrowing what `process.execute` actually permits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    /// Bare executable names, never paths, so an allowlist cannot be bypassed
    /// by spelling the same program a different way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_executables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContribution {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelLocation {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelContribution {
    pub id: String,
    pub title: String,
    pub location: PanelLocation,
}

/// Why a manifest cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is malformed: {0}")]
    Malformed(String),
    #[error("manifest field `{0}` is required")]
    MissingField(&'static str),
    #[error(
        "extension id `{0}` must be lowercase alphanumeric segments separated by `.`, `-` or `_`"
    )]
    InvalidId(String),
    #[error("api_version {requested} is not supported by this Warp build (supported: {supported})")]
    UnsupportedApiVersion { requested: u32, supported: u32 },
    #[error("command `{0}` must be a relative path inside the extension directory")]
    InvalidCommandPath(String),
    #[error("duplicate command id `{0}`")]
    DuplicateCommandId(String),
    #[error("command entries must declare a non-empty id")]
    EmptyCommandId,
    #[error("duplicate panel id `{0}`")]
    DuplicatePanelId(String),
    #[error("panel entries must declare a non-empty id")]
    EmptyPanelId,
    #[error("process_execute requires a non-empty execution.allowed_executables list")]
    MissingExecutableAllowlist,
    #[error("allowed executable `{0}` must be a bare program name without path separators")]
    InvalidAllowedExecutable(String),
}

fn validate_id(id: &str) -> Result<(), ManifestError> {
    if id.is_empty() {
        return Err(ManifestError::MissingField("id"));
    }
    let segments: Vec<&str> = id.split(['.', '-', '_']).collect();
    let well_formed = segments.iter().all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|char| char.is_ascii_lowercase() || char.is_ascii_digit())
    });
    if well_formed {
        Ok(())
    } else {
        Err(ManifestError::InvalidId(id.to_owned()))
    }
}

/// Rejects any `command` that could name a file outside the extension
/// directory. This is validation only: resolving the path is the host's job,
/// and doing both here would let a symlink decide the answer.
fn validate_command(command: &str) -> Result<(), ManifestError> {
    if command.trim().is_empty() {
        return Err(ManifestError::MissingField("command"));
    }
    let path = Path::new(command);
    if path.is_absolute() {
        return Err(ManifestError::InvalidCommandPath(command.to_owned()));
    }
    let safe = path
        .components()
        .all(|component| matches!(component, Component::CurDir | Component::Normal(_)));
    if safe {
        Ok(())
    } else {
        Err(ManifestError::InvalidCommandPath(command.to_owned()))
    }
}

fn validate_allowed_executable(executable: &str) -> Result<(), ManifestError> {
    let is_bare = !executable.trim().is_empty()
        && !executable.contains('/')
        && !executable.contains('\\')
        && executable != "."
        && executable != "..";
    if is_bare {
        Ok(())
    } else {
        Err(ManifestError::InvalidAllowedExecutable(
            executable.to_owned(),
        ))
    }
}

fn validate_unique_ids<'a>(
    ids: impl Iterator<Item = &'a str>,
    duplicate: impl Fn(String) -> ManifestError,
    empty: ManifestError,
) -> Result<(), ManifestError> {
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if id.trim().is_empty() {
            return Err(empty);
        }
        if !seen.insert(id) {
            return Err(duplicate(id.to_owned()));
        }
    }
    Ok(())
}

/// Accepts `api_version = 1` and `api_version = "1"` so the manifest can be
/// written either way without a confusing type error.
fn deserialize_api_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ApiVersion {
        Number(u32),
        Text(String),
    }

    match ApiVersion::deserialize(deserializer)? {
        ApiVersion::Number(value) => Ok(value),
        ApiVersion::Text(value) => value.trim().parse().map_err(|_| {
            serde::de::Error::custom(format!("api_version `{value}` is not a number"))
        }),
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
