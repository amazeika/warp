use extension_protocol::PanelSection;

use super::*;

fn action(id: &str, destructive: bool) -> PanelItemAction {
    PanelItemAction {
        id: id.to_owned(),
        title: id.to_owned(),
        icon: None,
        destructive,
    }
}

fn item(id: &str, actions: Vec<PanelItemAction>, children: Vec<PanelItem>) -> PanelItem {
    PanelItem {
        id: id.to_owned(),
        label: id.to_owned(),
        description: None,
        icon: None,
        badge: None,
        actions,
        children,
        expanded: false,
    }
}

fn state(sections: Vec<PanelSection>) -> PanelViewState {
    PanelViewState {
        panel_id: "git".to_owned(),
        status: PanelStatus::Ready,
        sections,
    }
}

fn section(id: &str, items: Vec<PanelItem>) -> PanelSection {
    PanelSection {
        id: id.to_owned(),
        title: id.to_owned(),
        badge: None,
        items,
    }
}

fn keys(rows: &[PanelRow]) -> Vec<(String, Vec<String>)> {
    rows.iter()
        .map(|row| (row.key.section_id.clone(), row.key.path.clone()))
        .collect()
}

#[test]
fn a_row_is_identified_by_its_section_and_the_path_to_it() {
    let rows = build_rows(
        &state(vec![
            section(
                "git.staged",
                vec![item("src/main.rs", Vec::new(), Vec::new())],
            ),
            section(
                "git.unstaged",
                vec![item("src/main.rs", Vec::new(), Vec::new())],
            ),
        ]),
        &HashMap::new(),
    );

    assert_eq!(
        keys(&rows),
        [
            ("git.staged".to_owned(), vec![]),
            ("git.staged".to_owned(), vec!["src/main.rs".to_owned()]),
            ("git.unstaged".to_owned(), vec![]),
            ("git.unstaged".to_owned(), vec!["src/main.rs".to_owned()]),
        ],
        "the same path staged and unstaged is two different rows, which is what the \
         section id in the key is for"
    );
}

#[test]
fn children_are_only_drawn_while_their_parent_is_expanded() {
    let mut parent = item(
        "src",
        Vec::new(),
        vec![item("src/main.rs", Vec::new(), Vec::new())],
    );
    parent.expanded = true;
    let snapshot = state(vec![section("files", vec![parent])]);

    let expanded = build_rows(&snapshot, &HashMap::new());
    assert_eq!(expanded.len(), 3, "section, parent and child");

    // Collapsing the parent is the user's choice, and it outranks the
    // plugin's own `expanded`.
    let mut overrides = HashMap::new();
    overrides.insert(
        RowKey {
            section_id: "files".to_owned(),
            path: vec!["src".to_owned()],
        },
        false,
    );
    let collapsed = build_rows(&snapshot, &overrides);
    assert_eq!(keys(&collapsed).len(), 2, "section and parent only");
}

#[test]
fn collapsing_a_section_hides_everything_under_it() {
    let snapshot = state(vec![section(
        "git.staged",
        vec![item("a", Vec::new(), Vec::new())],
    )]);

    let mut overrides = HashMap::new();
    overrides.insert(RowKey::section("git.staged"), false);
    let rows = build_rows(&snapshot, &overrides);

    assert_eq!(rows.len(), 1);
    assert!(matches!(
        rows[0].kind,
        RowKind::Section {
            collapsed: true,
            ..
        }
    ));
}

#[test]
fn clicking_a_leaf_runs_its_first_action_and_a_parent_row_runs_none() {
    let snapshot = state(vec![section(
        "git.unstaged",
        vec![
            item(
                "a",
                vec![action("open_diff", false), action("discard", true)],
                Vec::new(),
            ),
            item(
                "dir",
                vec![action("open_diff", false)],
                vec![item("dir/b", Vec::new(), Vec::new())],
            ),
        ],
    )]);
    let rows = build_rows(&snapshot, &HashMap::new());

    let leaf = &rows[1];
    assert_eq!(
        leaf.primary_action().map(|action| action.id.as_str()),
        Some("open_diff"),
        "the plugin orders the actions, and the first one is what a click runs"
    );

    let parent = &rows[2];
    assert!(
        parent.primary_action().is_none(),
        "a row with children expands on click, so its action stays in the menu \
         rather than firing when the user meant to look inside"
    );
    assert_eq!(
        parent.actions().len(),
        1,
        "the action is still reachable, just not on the row itself"
    );
}

#[test]
fn a_row_with_no_action_offers_none() {
    let snapshot = state(vec![section("s", vec![item("a", Vec::new(), Vec::new())])]);
    let rows = build_rows(&snapshot, &HashMap::new());
    assert!(rows[1].primary_action().is_none());
    assert!(rows[0].primary_action().is_none(), "nor does a section");
}

#[test]
fn an_icon_outside_warps_vocabulary_renders_without_one() {
    assert_eq!(icon_for("conflict"), Some(Icon::AlertTriangle));
    assert_eq!(icon_for("branch-current"), Some(Icon::GitBranch));
    assert_eq!(
        icon_for("../../etc/passwd"),
        None,
        "the name selects from Warp's set rather than naming an asset"
    );
    assert_eq!(icon_for(""), None);
}
