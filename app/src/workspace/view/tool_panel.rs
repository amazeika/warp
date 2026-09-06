//! The registry of panels the tools panel can show.
//!
//! Warp's built-in panels used to be a closed enum, which meant every place
//! that asked "which panels are there?" had to be edited to add one. An
//! extension cannot edit those matches, so the set is a registry instead: the
//! built-in variants and the panels running extensions contribute are the same
//! kind of entry, discovered the same way, and the toolbelt renders whatever
//! [`available`] returns.
//!
//! Extensions are deliberately not a special case here. A built-in panel that
//! is switched off by a setting and an extension panel whose extension stopped
//! disappear by the same mechanism — they stop being returned — so neither can
//! leave a button behind that opens nothing.
use extension_protocol::PanelLocation;
use settings::Setting as _;
use warp_core::features::FeatureFlag;
use warp_core::ui::Icon;
use warpui::{AppContext, SingletonEntity as _};

use super::global_search::view::GlobalSearchEntryFocus;
use crate::auth::AuthStateProvider;
use crate::drive::settings::WarpDriveSettings;
use crate::extensions::{ContributedPanel, ExtensionManager};
use crate::settings::{AISettings, CodeSettings};
use crate::workspace::view::{
    LEFT_PANEL_AGENT_CONVERSATIONS_BINDING_NAME, LEFT_PANEL_GLOBAL_SEARCH_BINDING_NAME,
    LEFT_PANEL_PROJECT_EXPLORER_BINDING_NAME, LEFT_PANEL_WARP_DRIVE_BINDING_NAME,
    OPEN_GLOBAL_SEARCH_BINDING_NAME, TOGGLE_CONVERSATION_LIST_VIEW_BINDING_NAME,
    TOGGLE_PROJECT_EXPLORER_BINDING_NAME, TOGGLE_WARP_DRIVE_BINDING_NAME,
};

/// Which panel the tools panel is showing.
///
/// Not `Copy`: an extension panel is identified by the extension and panel ids
/// it was contributed under, and there is no smaller stable identity for it —
/// an index would be invalidated by the extension next to it stopping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolPanelView {
    ProjectExplorer,
    GlobalSearch {
        entry_focus: GlobalSearchEntryFocus,
    },
    WarpDrive,
    ConversationListView,
    Extension {
        extension_id: String,
        panel_id: String,
    },
}

impl ToolPanelView {
    /// Whether two values name the same panel, ignoring transient state the
    /// panel carries.
    ///
    /// Global search keeps its entry focus in the view value, so comparing for
    /// equality would report the panel as unavailable the moment focus moved
    /// within it.
    pub fn is_same_panel(&self, other: &Self) -> bool {
        match (self, other) {
            (ToolPanelView::GlobalSearch { .. }, ToolPanelView::GlobalSearch { .. }) => true,
            (
                ToolPanelView::Extension {
                    extension_id,
                    panel_id,
                },
                ToolPanelView::Extension {
                    extension_id: other_extension_id,
                    panel_id: other_panel_id,
                },
            ) => extension_id == other_extension_id && panel_id == other_panel_id,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

/// Why a panel the user can select may still refuse to show its content.
///
/// A panel is listed in the toolbelt before it is usable, so the button says
/// what is missing rather than vanishing and leaving the user to guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolPanelAvailability {
    Available,
    RequiresAccount,
    RequiresAi,
}

impl ToolPanelView {
    pub(crate) fn availability(&self, app: &AppContext) -> ToolPanelAvailability {
        match self {
            ToolPanelView::ProjectExplorer | ToolPanelView::GlobalSearch { .. } => {
                ToolPanelAvailability::Available
            }
            ToolPanelView::WarpDrive => {
                if WarpDriveSettings::is_warp_drive_available(app) {
                    ToolPanelAvailability::Available
                } else {
                    ToolPanelAvailability::RequiresAccount
                }
            }
            ToolPanelView::ConversationListView => {
                if AuthStateProvider::as_ref(app)
                    .get()
                    .is_anonymous_or_logged_out()
                {
                    ToolPanelAvailability::RequiresAccount
                } else if AISettings::as_ref(app).is_conversation_history_available(app) {
                    ToolPanelAvailability::Available
                } else {
                    ToolPanelAvailability::RequiresAi
                }
            }
            // A contributed panel is only listed while its extension is
            // running, so by the time it can be selected it is usable. What it
            // has to say about being empty or still loading is part of the
            // snapshot it publishes, not a Warp-side gate.
            ToolPanelView::Extension { .. } => ToolPanelAvailability::Available,
        }
    }
}

/// One entry in the registry: a panel and how the toolbelt presents it.
#[derive(Clone)]
pub struct ToolPanel {
    pub view: ToolPanelView,
    pub icon: Icon,
    /// Optional icon to use when the panel is the active one.
    pub active_icon: Option<Icon>,
    pub title: String,
    /// Ordered binding names used to populate the tooltip keybinding display.
    ///
    /// Earlier bindings are preferred. Contributed panels have none: Warp does
    /// not mint a keybinding for a panel that may not exist next launch.
    pub tooltip_keybinding_names: Vec<&'static str>,
}

/// Every panel the tools panel can currently show, in toolbelt order.
///
/// Built-ins come first and in a fixed order so the toolbelt does not reshuffle
/// when an extension starts; contributed panels follow, ordered by the registry
/// they come from, which is stable across launches.
pub fn available(app: &AppContext) -> Vec<ToolPanel> {
    let mut panels = built_in(app);
    panels.extend(contributed(app));
    panels
}

fn built_in(app: &AppContext) -> Vec<ToolPanel> {
    let mut panels = Vec::new();
    if cfg!(feature = "local_fs") && *CodeSettings::as_ref(app).show_project_explorer.value() {
        panels.push(ToolPanel {
            view: ToolPanelView::ProjectExplorer,
            icon: Icon::FileCopy,
            active_icon: None,
            title: "Project explorer".to_owned(),
            tooltip_keybinding_names: vec![
                LEFT_PANEL_PROJECT_EXPLORER_BINDING_NAME,
                TOGGLE_PROJECT_EXPLORER_BINDING_NAME,
            ],
        });
    }
    if FeatureFlag::AgentViewConversationListView.is_enabled()
        && *AISettings::as_ref(app).show_conversation_history
    {
        panels.push(ToolPanel {
            view: ToolPanelView::ConversationListView,
            icon: Icon::Conversation,
            active_icon: Some(Icon::Conversation),
            title: "Agent conversations".to_owned(),
            tooltip_keybinding_names: vec![
                LEFT_PANEL_AGENT_CONVERSATIONS_BINDING_NAME,
                TOGGLE_CONVERSATION_LIST_VIEW_BINDING_NAME,
            ],
        });
    }
    if cfg!(feature = "local_fs")
        && FeatureFlag::GlobalSearch.is_enabled()
        && *CodeSettings::as_ref(app).show_global_search.value()
    {
        panels.push(ToolPanel {
            view: ToolPanelView::GlobalSearch {
                entry_focus: GlobalSearchEntryFocus::Results,
            },
            icon: Icon::Search,
            active_icon: None,
            title: "Global search".to_owned(),
            tooltip_keybinding_names: vec![
                LEFT_PANEL_GLOBAL_SEARCH_BINDING_NAME,
                OPEN_GLOBAL_SEARCH_BINDING_NAME,
            ],
        });
    }
    if *WarpDriveSettings::as_ref(app).enable_warp_drive {
        panels.push(ToolPanel {
            view: ToolPanelView::WarpDrive,
            icon: Icon::WarpDrive,
            active_icon: None,
            title: "Warp Drive".to_owned(),
            tooltip_keybinding_names: vec![
                LEFT_PANEL_WARP_DRIVE_BINDING_NAME,
                TOGGLE_WARP_DRIVE_BINDING_NAME,
            ],
        });
    }
    panels
}

/// The panels running extensions contribute.
///
/// Empty in a build that never registered an [`ExtensionManager`] — the
/// singleton only exists when the feature is on, and asking for one that was
/// never registered panics. The toolbelt must not be what crashes.
///
/// Only `left` panels are hosted. Warp's right panel is the code review pane,
/// not a second tools panel, so there is nowhere to put a `right` contribution;
/// it is reported when the extension registers rather than being quietly moved
/// to the side the manifest did not ask for.
fn contributed(app: &AppContext) -> Vec<ToolPanel> {
    if !app.has_singleton_model::<ExtensionManager>() {
        return Vec::new();
    }
    ExtensionManager::as_ref(app)
        .panels()
        .filter(|panel| panel.location == PanelLocation::Left)
        .map(entry)
        .collect()
}

/// Whether the registry currently lists this panel.
///
/// Compared by panel rather than by value, so a built-in that carries transient
/// state — global search and its entry focus — is still recognised as listed.
pub fn lists(panels: &[ToolPanel], view: &ToolPanelView) -> bool {
    panels.iter().any(|panel| panel.view.is_same_panel(view))
}

/// What the button that opens the tools panel is called.
///
/// When only one panel is available the button opens that panel and is named
/// after it; with several it opens the panel that holds them all.
pub fn single_panel_title(panels: &[ToolPanel]) -> &str {
    match panels {
        [only] => only.title.as_str(),
        _ => "Tools panel",
    }
}

fn entry(panel: &ContributedPanel) -> ToolPanel {
    ToolPanel {
        view: ToolPanelView::Extension {
            extension_id: panel.extension_id.clone(),
            panel_id: panel.panel_id.clone(),
        },
        // One icon for every contributed panel, because the manifest does not
        // name one and letting a plugin pick from Warp's icon set would let it
        // dress itself up as a built-in surface.
        icon: Icon::Grid,
        active_icon: None,
        title: panel.title.clone(),
        tooltip_keybinding_names: Vec::new(),
    }
}

#[cfg(test)]
#[path = "tool_panel_tests.rs"]
mod tests;
