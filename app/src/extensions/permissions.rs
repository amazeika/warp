//! What the user has agreed to let an extension do, and how that is remembered.
//!
//! The store is a plain file behind an explicit path rather than a setting,
//! because a grant is a record of a decision the user made about one installed
//! extension, not a preference that should sync between machines: the extension
//! on the other machine is not necessarily the same code.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use extension_protocol::{ExtensionManifest, Permission, PermissionSet};
use serde::{Deserialize, Serialize};

/// Overrides the grant file, used by tests.
pub const GRANTS_FILE_ENV: &str = "WARP_EXTENSION_GRANTS_FILE";

const GRANTS_FILE_NAME: &str = "extension_grants.json";

/// Where grants are recorded.
///
/// Deliberately outside the extensions directory: an extension can write to its
/// own directory, and a permission record an extension can edit is not a
/// permission record.
pub fn grants_path() -> PathBuf {
    if let Some(path) = std::env::var_os(GRANTS_FILE_ENV) {
        return PathBuf::from(path);
    }
    extension_host::warp_home().join(GRANTS_FILE_NAME)
}

/// One remembered decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The version that was installed when the user agreed, kept for display so
    /// a user can see what they said yes to.
    pub version: String,
    /// Exactly the permissions the prompt listed. A later manifest asking for
    /// more is a different question and has to be asked again.
    pub permissions: PermissionSet,
}

/// The answer to "may this extension run right now?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Everything the manifest needs has already been granted.
    Granted,
    /// The user has to be asked. `added` is empty on a first install and names
    /// the newly requested permissions after an upgrade, which is the part of
    /// the prompt that actually matters to someone re-reading it.
    Prompt {
        requested: PermissionSet,
        added: Vec<Permission>,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrantFile {
    #[serde(default)]
    grants: BTreeMap<String, Grant>,
}

/// The persisted set of grants, keyed by extension id.
#[derive(Debug)]
pub struct GrantStore {
    path: PathBuf,
    grants: BTreeMap<String, Grant>,
}

impl GrantStore {
    /// Reads the store at `path`.
    ///
    /// A missing or unreadable file yields an empty store rather than an error.
    /// Losing the record of a grant costs the user one prompt; refusing to
    /// start because a file will not parse costs them every extension.
    pub fn load_from(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let grants = std::fs::read_to_string(&path)
            .ok()
            .and_then(|source| match serde_json::from_str::<GrantFile>(&source) {
                Ok(file) => Some(file.grants),
                Err(err) => {
                    log::warn!("Ignoring unreadable extension grants at {path:?}: {err}");
                    None
                }
            })
            .unwrap_or_default();
        Self { path, grants }
    }

    pub fn load() -> Self {
        Self::load_from(grants_path())
    }

    pub fn granted(&self, extension_id: &str) -> Option<&Grant> {
        self.grants.get(extension_id)
    }

    /// Decides whether `manifest` may run without asking the user again.
    ///
    /// A manifest that has narrowed its permissions is still covered by the
    /// original grant, so an update that asks for less is silent. One that has
    /// widened them is not.
    pub fn decide(&self, manifest: &ExtensionManifest) -> PermissionDecision {
        let requested = manifest.effective_permissions();
        let Some(grant) = self.grants.get(&manifest.id) else {
            return PermissionDecision::Prompt {
                requested,
                added: Vec::new(),
            };
        };
        if grant.permissions.covers(&requested) {
            return PermissionDecision::Granted;
        }
        let added = requested
            .iter()
            .filter(|permission| !grant.permissions.contains(*permission))
            .collect();
        PermissionDecision::Prompt { requested, added }
    }

    /// Records that the user allowed everything `manifest` asks for.
    pub fn grant(&mut self, manifest: &ExtensionManifest) -> std::io::Result<()> {
        self.grants.insert(
            manifest.id.clone(),
            Grant {
                version: manifest.version.clone(),
                permissions: manifest.effective_permissions(),
            },
        );
        self.persist()
    }

    /// Forgets an extension's grant, so it is asked for again next time.
    pub fn revoke(&mut self, extension_id: &str) -> std::io::Result<()> {
        if self.grants.remove(extension_id).is_none() {
            return Ok(());
        }
        self.persist()
    }

    fn persist(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = GrantFile {
            grants: self.grants.clone(),
        };
        let source = serde_json::to_string_pretty(&file)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        write_private(&self.path, &source)
    }
}

/// Writes owner-readable only: the file names what each installed extension is
/// allowed to do, which is worth keeping to the account that decided it.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
