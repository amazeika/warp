use extension_protocol::PanelStatus;

use super::*;
use crate::git::branches::{Branch, BranchScope};
use crate::git::status::{ChangeKind, FileChange};
use crate::state::Operation;

fn change(path: &str, kind: ChangeKind) -> FileChange {
    FileChange {
        path: path.to_owned(),
        kind,
        original_path: None,
    }
}

fn branch(name: &str, scope: BranchScope, is_head: bool) -> Branch {
    Branch {
        name: name.to_owned(),
        scope,
        upstream: None,
        short_commit: "abc1234".to_owned(),
        subject: "Subject".to_owned(),
        is_head,
    }
}

fn populated() -> RepositoryState {
    let mut state = RepositoryState {
        root: "/repo".to_owned(),
        branches: vec![
            branch("main", BranchScope::Local, true),
            branch("feature", BranchScope::Local, false),
            branch("origin/main", BranchScope::Remote, false),
        ],
        remotes: vec!["origin".to_owned()],
        ..Default::default()
    };
    state.status.branch.head = Some("main".to_owned());
    state.status.staged = vec![change("staged.rs", ChangeKind::Added)];
    state.status.unstaged = vec![change("changed.rs", ChangeKind::Modified)];
    state.status.untracked = vec![change("new.rs", ChangeKind::Untracked)];
    state
}

fn section<'a>(view: &'a extension_protocol::PanelViewState, id: &str) -> &'a PanelSection {
    view.sections
        .iter()
        .find(|section| section.id == id)
        .unwrap_or_else(|| panic!("section {id} is present"))
}

fn action_ids(item: &PanelItem) -> Vec<&str> {
    item.actions
        .iter()
        .map(|action| action.id.as_str())
        .collect()
}

#[test]
fn the_panel_declares_the_id_the_manifest_declares() {
    assert_eq!(render(&populated()).panel_id, PANEL_ID);
    assert_eq!(loading().panel_id, PANEL_ID);
    assert_eq!(error("boom").panel_id, PANEL_ID);
    assert_eq!(no_repository().panel_id, PANEL_ID);
}

#[test]
fn changes_are_grouped_into_staged_unstaged_and_untracked() {
    let view = render(&populated());

    assert_eq!(section(&view, SECTION_STAGED).items[0].id, "staged.rs");
    assert_eq!(section(&view, SECTION_UNSTAGED).items[0].id, "changed.rs");
    assert_eq!(section(&view, SECTION_UNTRACKED).items[0].id, "new.rs");
    assert_eq!(
        section(&view, SECTION_STAGED).badge.as_deref(),
        Some("1"),
        "a section badge counts its rows"
    );
}

#[test]
fn a_row_id_is_the_repository_relative_path() {
    // The id travels back on panel.action and straight into a Git argument, so
    // it must not be decorated for display.
    let mut state = populated();
    state.status.unstaged = vec![change("src/deep/name with spaces.rs", ChangeKind::Modified)];
    let view = render(&state);
    assert_eq!(
        section(&view, SECTION_UNSTAGED).items[0].id,
        "src/deep/name with spaces.rs"
    );
}

#[test]
fn staged_rows_offer_unstage_and_unstaged_rows_offer_stage() {
    let view = render(&populated());
    assert!(action_ids(&section(&view, SECTION_STAGED).items[0]).contains(&ACTION_UNSTAGE));
    assert!(action_ids(&section(&view, SECTION_UNSTAGED).items[0]).contains(&ACTION_STAGE));
    assert!(
        !action_ids(&section(&view, SECTION_STAGED).items[0]).contains(&ACTION_DISCARD),
        "discarding a staged row would silently drop the staged content too"
    );
}

#[test]
fn destructive_row_actions_are_marked_destructive() {
    let view = render(&populated());
    let unstaged = &section(&view, SECTION_UNSTAGED).items[0];
    let discard = unstaged
        .actions
        .iter()
        .find(|action| action.id == ACTION_DISCARD)
        .expect("discard is offered");
    assert!(
        discard.destructive,
        "Warp renders the warning, not the plugin"
    );

    let untracked = &section(&view, SECTION_UNTRACKED).items[0];
    let delete = untracked
        .actions
        .iter()
        .find(|action| action.id == ACTION_DELETE_FILE)
        .expect("delete is offered");
    assert!(delete.destructive);
}

#[test]
fn the_checked_out_branch_cannot_be_switched_to_or_deleted() {
    let view = render(&populated());
    let branches = section(&view, SECTION_BRANCHES);

    let head = branches
        .items
        .iter()
        .find(|item| item.id == "main")
        .expect("the current branch is listed");
    assert!(!action_ids(head).contains(&ACTION_SWITCH_BRANCH));
    assert!(!action_ids(head).contains(&ACTION_DELETE_BRANCH));

    let other = branches
        .items
        .iter()
        .find(|item| item.id == "feature")
        .expect("the other branch is listed");
    assert!(action_ids(other).contains(&ACTION_SWITCH_BRANCH));
    assert!(action_ids(other).contains(&ACTION_DELETE_BRANCH));
}

#[test]
fn remote_branches_are_offered_a_local_checkout_only() {
    let view = render(&populated());
    let remote = &section(&view, SECTION_REMOTE_BRANCHES).items[0];
    assert_eq!(remote.id, "origin/main");
    assert_eq!(action_ids(remote), [ACTION_CHECKOUT_REMOTE]);
}

#[test]
fn conflicts_are_shown_before_anything_else_that_can_be_acted_on() {
    let mut state = populated();
    state.status.conflicts = vec![change("both.rs", ChangeKind::Conflicted)];
    let view = render(&state);

    let conflict_index = view
        .sections
        .iter()
        .position(|section| section.id == SECTION_CONFLICTS)
        .expect("conflicts are shown");
    let staged_index = view
        .sections
        .iter()
        .position(|section| section.id == SECTION_STAGED)
        .expect("staged changes are shown");
    assert!(conflict_index < staged_index);
}

#[test]
fn a_paused_operation_is_surfaced_in_the_summary() {
    let mut state = populated();
    state.operation = Operation::Rebase;
    let view = render(&state);

    let summary = section(&view, SECTION_SUMMARY);
    assert!(
        summary
            .items
            .iter()
            .any(|item| item.label.contains("Rebasing")),
        "a paused rebase is the most important thing on the panel"
    );
}

#[test]
fn history_rows_are_addressed_by_full_hash_and_labelled_by_subject() {
    let mut state = populated();
    state.history = vec![crate::git::Commit {
        hash: "abc123def456".to_owned(),
        short_hash: "abc123d".to_owned(),
        author: "Ada".to_owned(),
        age: "2 hours ago".to_owned(),
        subject: "Add the parser".to_owned(),
    }];
    let view = render(&state);

    let commit = &section(&view, SECTION_HISTORY).items[0];
    assert_eq!(commit.id, "abc123def456", "a short hash can collide");
    assert_eq!(commit.label, "Add the parser");
    assert!(
        commit
            .description
            .as_deref()
            .is_some_and(|d| d.contains("Ada"))
    );
}

#[test]
fn a_failed_refresh_shows_the_error_state_rather_than_stale_content() {
    let mut state = populated();
    state.error = Some("not a git repository".to_owned());
    let view = render(&state);

    assert_eq!(
        view.status,
        PanelStatus::Error {
            message: "not a git repository".to_owned()
        }
    );
    assert!(view.sections.is_empty());
}

#[test]
fn a_clean_repository_still_shows_its_branches_and_history() {
    let mut state = populated();
    state.status.staged.clear();
    state.status.unstaged.clear();
    state.status.untracked.clear();
    let view = render(&state);

    assert_eq!(view.status, PanelStatus::Ready);
    assert!(!section(&view, SECTION_BRANCHES).items.is_empty());
}

#[test]
fn a_workspace_without_a_repository_is_empty_rather_than_broken() {
    assert!(matches!(no_repository().status, PanelStatus::Empty { .. }));
    assert!(matches!(loading().status, PanelStatus::Loading));
}
