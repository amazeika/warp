use super::*;

fn contributed_panel(
    extension_id: &str,
    panel_id: &str,
    location: PanelLocation,
) -> ContributedPanel {
    ContributedPanel {
        extension_id: extension_id.to_owned(),
        extension_name: format!("Name of {extension_id}"),
        panel_id: panel_id.to_owned(),
        title: format!("Title of {panel_id}"),
        location,
    }
}

#[test]
fn a_contributed_panel_becomes_a_registry_entry_named_by_its_manifest() {
    let panel = entry(&contributed_panel(
        "dev.warp.git",
        "git",
        PanelLocation::Left,
    ));

    assert_eq!(panel.title, "Title of git");
    assert_eq!(
        panel.view,
        ToolPanelView::Extension {
            extension_id: "dev.warp.git".to_owned(),
            panel_id: "git".to_owned(),
        }
    );
    assert!(
        panel.tooltip_keybinding_names.is_empty(),
        "Warp does not mint a keybinding for a panel that may not exist next launch"
    );
}

#[test]
fn global_search_stays_the_same_panel_as_its_focus_moves() {
    let results = ToolPanelView::GlobalSearch {
        entry_focus: GlobalSearchEntryFocus::Results,
    };
    let query = ToolPanelView::GlobalSearch {
        entry_focus: GlobalSearchEntryFocus::QueryEditor,
    };

    assert_ne!(results, query, "the focus is part of the value");
    assert!(
        results.is_same_panel(&query),
        "but not part of which panel it is, or moving focus would report the \
         panel as unavailable"
    );
}

#[test]
fn two_extension_panels_are_the_same_only_when_both_ids_match() {
    let git = ToolPanelView::Extension {
        extension_id: "dev.warp.git".to_owned(),
        panel_id: "git".to_owned(),
    };
    let same = git.clone();
    let other_panel = ToolPanelView::Extension {
        extension_id: "dev.warp.git".to_owned(),
        panel_id: "history".to_owned(),
    };
    let other_extension = ToolPanelView::Extension {
        extension_id: "dev.warp.other".to_owned(),
        panel_id: "git".to_owned(),
    };

    assert!(git.is_same_panel(&same));
    assert!(!git.is_same_panel(&other_panel));
    assert!(
        !git.is_same_panel(&other_extension),
        "two extensions may use the same panel id, so both halves of the key count"
    );
    assert!(!git.is_same_panel(&ToolPanelView::WarpDrive));
}

#[test]
fn a_contributed_panel_is_available_as_soon_as_it_is_listed() {
    // Nothing here reads the app context: a panel is only listed while its
    // extension is running, so there is no account or AI gate to consult.
    warpui::App::test((), |mut app| async move {
        app.update(|ctx| {
            let view = ToolPanelView::Extension {
                extension_id: "dev.warp.git".to_owned(),
                panel_id: "git".to_owned(),
            };
            assert_eq!(view.availability(ctx), ToolPanelAvailability::Available);
        });
    });
}

#[test]
fn the_toggle_button_is_named_after_the_only_panel_there_is() {
    let git = entry(&contributed_panel(
        "dev.warp.git",
        "git",
        PanelLocation::Left,
    ));
    let history = entry(&contributed_panel(
        "dev.warp.git",
        "history",
        PanelLocation::Left,
    ));

    assert_eq!(single_panel_title(&[]), "Tools panel");
    assert_eq!(
        single_panel_title(std::slice::from_ref(&git)),
        "Title of git"
    );
    assert_eq!(single_panel_title(&[git, history]), "Tools panel");
}
