//! Finding extensions on disk and deciding which of them Warp will run.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use extension_protocol::{ExtensionManifest, MANIFEST_FILE_NAME, ManifestError};

/// Overrides the extensions root, used by tests and packaging.
pub const EXTENSIONS_DIR_ENV: &str = "WARP_EXTENSIONS_DIR";

/// Where Warp looks for installed extensions.
pub fn extensions_root() -> PathBuf {
    if let Some(path) = std::env::var_os(EXTENSIONS_DIR_ENV) {
        return PathBuf::from(path);
    }
    home_dir().join(".warp").join("extensions")
}

pub(crate) fn home_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home)
}

/// One directory that claimed to be an extension.
///
/// Invalid extensions are kept rather than dropped so the UI can tell the user
/// why an extension they installed is not running.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredExtension {
    pub directory: PathBuf,
    pub record: ExtensionRecord,
}

impl DiscoveredExtension {
    pub fn manifest(&self) -> Option<&ExtensionManifest> {
        match &self.record {
            ExtensionRecord::Valid { manifest, .. } => Some(manifest),
            ExtensionRecord::Invalid { .. } => None,
        }
    }

    pub fn id(&self) -> Option<&str> {
        self.manifest().map(|manifest| manifest.id.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionRecord {
    Valid {
        manifest: Box<ExtensionManifest>,
        /// The manifest's `command`, resolved inside the extension directory.
        executable: PathBuf,
    },
    Invalid {
        reason: ValidationError,
    },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("{0}")]
    Manifest(#[from] ManifestError),
    #[error("could not read {MANIFEST_FILE_NAME}: {0}")]
    Unreadable(String),
    #[error("declared command `{0}` does not exist in the extension directory")]
    MissingExecutable(String),
    #[error("extension id `{id}` is already provided by `{first}`")]
    DuplicateId { id: String, first: String },
}

/// Discovers extensions under [`extensions_root`].
pub fn discover() -> Vec<DiscoveredExtension> {
    discover_in(&extensions_root())
}

/// Discovers extensions under an explicit root.
///
/// A missing root yields no extensions rather than an error: not having
/// installed anything is the common case, not a failure.
pub fn discover_in(root: &Path) -> Vec<DiscoveredExtension> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| extension_directory_is_candidate(path))
        .collect();
    // Sorted so the winner of an id collision is the same on every machine.
    directories.sort();

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut first_claimant: Vec<(String, String)> = Vec::new();
    let mut discovered = Vec::with_capacity(directories.len());

    for directory in directories {
        let mut record = load(&directory);
        if let ExtensionRecord::Valid { manifest, .. } = &record {
            let id = manifest.id.clone();
            if !claimed.insert(id.clone()) {
                let first = first_claimant
                    .iter()
                    .find(|(claimed_id, _)| *claimed_id == id)
                    .map(|(_, directory)| directory.clone())
                    .unwrap_or_default();
                record = ExtensionRecord::Invalid {
                    reason: ValidationError::DuplicateId { id, first },
                };
            } else {
                first_claimant.push((id, directory_name(&directory)));
            }
        }
        discovered.push(DiscoveredExtension { directory, record });
    }
    discovered
}

/// True when a path is a directory holding a manifest.
///
/// Directories without a manifest are not extensions at all, so they are
/// skipped silently rather than reported as broken installs.
pub fn extension_directory_is_candidate(path: &Path) -> bool {
    path.is_dir() && path.join(MANIFEST_FILE_NAME).is_file()
}

fn load(directory: &Path) -> ExtensionRecord {
    let manifest_path = directory.join(MANIFEST_FILE_NAME);
    let source = match std::fs::read_to_string(&manifest_path) {
        Ok(source) => source,
        Err(err) => {
            return ExtensionRecord::Invalid {
                reason: ValidationError::Unreadable(err.to_string()),
            };
        }
    };

    let manifest = match ExtensionManifest::parse(&source) {
        Ok(manifest) => manifest,
        Err(err) => {
            return ExtensionRecord::Invalid {
                reason: ValidationError::Manifest(err),
            };
        }
    };
    if let Err(err) = manifest.validate() {
        return ExtensionRecord::Invalid {
            reason: ValidationError::Manifest(err),
        };
    }

    let executable = directory.join(&manifest.command);
    if !executable.is_file() {
        return ExtensionRecord::Invalid {
            reason: ValidationError::MissingExecutable(manifest.command.clone()),
        };
    }

    ExtensionRecord::Valid {
        manifest: Box::new(manifest),
        executable,
    }
}

fn directory_name(directory: &Path) -> String {
    directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
