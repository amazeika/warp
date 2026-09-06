//! Commands and panels contributed by the extensions that are running.
//!
//! The registry is separate from the manager because the surfaces that read it
//! — the command palette and the panel switcher — should not have to know how
//! an extension is supervised, only what it currently offers. Entries appear
//! when an extension reaches `Running` and disappear the moment it stops, so a
//! palette entry never invokes a plugin that is not there to answer.
use std::collections::BTreeMap;

use extension_protocol::{ExtensionManifest, PanelLocation};

/// One command an extension asked to appear in the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedCommand {
    pub extension_id: String,
    /// The extension's name, shown as the palette entry's source.
    pub extension_name: String,
    pub command_id: String,
    pub title: String,
}

/// One panel an extension asked to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedPanel {
    pub extension_id: String,
    pub extension_name: String,
    pub panel_id: String,
    pub title: String,
    pub location: PanelLocation,
}

/// What every running extension currently contributes.
#[derive(Debug, Default)]
pub struct Contributions {
    by_extension: BTreeMap<String, Registered>,
}

#[derive(Debug, Default)]
struct Registered {
    commands: Vec<ContributedCommand>,
    panels: Vec<ContributedPanel>,
}

impl Contributions {
    /// Publishes everything `manifest` declares, replacing any earlier entries
    /// for the same extension so a restart after an edit cannot leave a stale
    /// command behind.
    pub fn register(&mut self, manifest: &ExtensionManifest) {
        let commands = manifest
            .commands
            .iter()
            .map(|command| ContributedCommand {
                extension_id: manifest.id.clone(),
                extension_name: manifest.name.clone(),
                command_id: command.id.clone(),
                title: command.title.clone(),
            })
            .collect();
        let panels = manifest
            .panels
            .iter()
            .map(|panel| ContributedPanel {
                extension_id: manifest.id.clone(),
                extension_name: manifest.name.clone(),
                panel_id: panel.id.clone(),
                title: panel.title.clone(),
                location: panel.location,
            })
            .collect();
        self.by_extension
            .insert(manifest.id.clone(), Registered { commands, panels });
    }

    pub fn unregister(&mut self, extension_id: &str) {
        self.by_extension.remove(extension_id);
    }

    /// Every contributed command, ordered by extension id then declaration
    /// order, so the palette does not reshuffle between launches.
    pub fn commands(&self) -> impl Iterator<Item = &ContributedCommand> {
        self.by_extension
            .values()
            .flat_map(|registered| registered.commands.iter())
    }

    pub fn panels(&self) -> impl Iterator<Item = &ContributedPanel> {
        self.by_extension
            .values()
            .flat_map(|registered| registered.panels.iter())
    }

    /// Resolves a palette selection back to the extension that offered it.
    pub fn command(&self, extension_id: &str, command_id: &str) -> Option<&ContributedCommand> {
        self.by_extension
            .get(extension_id)?
            .commands
            .iter()
            .find(|command| command.command_id == command_id)
    }

    pub fn is_empty(&self) -> bool {
        self.by_extension.is_empty()
    }
}

#[cfg(test)]
#[path = "contributions_tests.rs"]
mod tests;
