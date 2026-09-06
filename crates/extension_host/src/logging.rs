//! Per-extension log files and the redaction applied on the way in.
use std::io::Write;
use std::path::PathBuf;

use crate::discovery::home_dir;

/// Overrides the extension log root, used by tests and packaging.
pub const EXTENSION_LOG_DIR_ENV: &str = "WARP_EXTENSION_LOG_DIR";

/// Substrings that mark a line as carrying a secret.
///
/// Redaction is by key rather than by value shape because a token is not
/// recognisable on sight, but the field that carries it almost always is.
const SECRET_MARKERS: &[&str] = &[
    "password",
    "passphrase",
    "secret",
    "token",
    "authorization",
    "credential",
    "api_key",
    "apikey",
    "private_key",
];

pub fn extension_log_root() -> PathBuf {
    if let Some(path) = std::env::var_os(EXTENSION_LOG_DIR_ENV) {
        return PathBuf::from(path);
    }
    home_dir().join(".warp").join("logs").join("extensions")
}

/// Log file for one extension id, under [`extension_log_root`].
pub fn extension_log_path(extension_id: &str) -> PathBuf {
    extension_log_path_in(&extension_log_root(), extension_id)
}

/// Log file for one extension id under an explicit root.
///
/// The id is sanitised because it reaches this function from a manifest, and a
/// manifest is not trusted to stay inside the log directory.
pub fn extension_log_path_in(root: &std::path::Path, extension_id: &str) -> PathBuf {
    let file_name: String = extension_id
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() || char == '.' || char == '-' || char == '_' {
                char
            } else {
                '_'
            }
        })
        .collect();
    root.join(format!("{file_name}.log"))
}

/// Writes lines to an extension log, dropping any that name a secret.
///
/// Dropping a whole line rather than masking part of it is deliberate: plugin
/// output has no schema, so there is no reliable way to keep the safe half.
pub struct RedactingWriter<W: Write> {
    inner: W,
}

impl<W: Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    pub fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        if Self::is_sensitive(line) {
            writeln!(
                self.inner,
                "[redacted line containing a secret-shaped field]"
            )
        } else {
            writeln!(self.inner, "{line}")
        }
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }

    pub fn is_sensitive(line: &str) -> bool {
        let lowered = line.to_ascii_lowercase();
        SECRET_MARKERS.iter().any(|marker| lowered.contains(marker))
    }
}

#[cfg(test)]
#[path = "logging_tests.rs"]
mod tests;
