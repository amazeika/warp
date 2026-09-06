use super::*;
use crate::git::branches::{Branch, BranchScope};
use crate::git::status::BranchStatus;

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

fn state_with(branches: Vec<Branch>, remotes: Vec<&str>) -> RepositoryState {
    RepositoryState {
        root: "/repo".to_owned(),
        branches,
        remotes: remotes.iter().map(|remote| (*remote).to_owned()).collect(),
        ..Default::default()
    }
}

#[test]
fn head_summary_reports_divergence_in_both_directions() {
    let mut state = RepositoryState::default();
    state.status.branch = BranchStatus {
        head: Some("feature".to_owned()),
        upstream: Some("origin/feature".to_owned()),
        ahead: 3,
        behind: 2,
        is_initial: false,
    };
    assert_eq!(state.head_summary(), "feature → origin/feature (↑3 ↓2)");

    state.status.branch.behind = 0;
    assert_eq!(state.head_summary(), "feature → origin/feature (↑3)");

    state.status.branch.ahead = 0;
    state.status.branch.behind = 2;
    assert_eq!(state.head_summary(), "feature → origin/feature (↓2)");

    state.status.branch.behind = 0;
    assert_eq!(
        state.head_summary(),
        "feature → origin/feature (up to date)"
    );
}

#[test]
fn head_summary_names_the_states_that_have_no_upstream() {
    let mut state = RepositoryState::default();
    state.status.branch.head = Some("feature".to_owned());
    assert_eq!(state.head_summary(), "feature (no upstream)");

    state.status.branch.is_initial = true;
    assert_eq!(state.head_summary(), "feature (no commits yet)");

    state.status.branch.head = None;
    assert_eq!(state.head_summary(), "Detached HEAD");
}

#[test]
fn origin_is_preferred_over_any_other_remote() {
    let state = state_with(Vec::new(), vec!["upstream", "origin"]);
    assert_eq!(state.default_remote(), Some("origin"));
}

#[test]
fn a_single_remote_is_used_even_when_it_is_not_called_origin() {
    let state = state_with(Vec::new(), vec!["fork"]);
    assert_eq!(state.default_remote(), Some("fork"));
}

#[test]
fn several_remotes_with_no_origin_are_left_for_the_user_to_resolve() {
    let state = state_with(Vec::new(), vec!["fork", "upstream"]);
    assert_eq!(
        state.default_remote(),
        None,
        "guessing here would push to the wrong place"
    );
    assert_eq!(state_with(Vec::new(), Vec::new()).default_remote(), None);
}

#[test]
fn the_base_branch_prefers_conventional_names() {
    let state = state_with(
        vec![
            branch("feature", BranchScope::Local, true),
            branch("develop", BranchScope::Local, false),
            branch("main", BranchScope::Local, false),
        ],
        vec![],
    );
    assert_eq!(state.default_base_branch(), Some("main"));
}

#[test]
fn the_base_branch_falls_back_to_any_other_local_branch() {
    let state = state_with(
        vec![
            branch("feature", BranchScope::Local, true),
            branch("spike", BranchScope::Local, false),
            branch("origin/main", BranchScope::Remote, false),
        ],
        vec!["origin"],
    );
    assert_eq!(
        state.default_base_branch(),
        Some("spike"),
        "a remote branch is not a local base"
    );
}

#[test]
fn a_lone_branch_has_no_base_to_compare_against() {
    let state = state_with(vec![branch("main", BranchScope::Local, true)], vec![]);
    assert_eq!(state.default_base_branch(), None);
}

#[test]
fn local_and_remote_branches_are_reported_separately() {
    let state = state_with(
        vec![
            branch("main", BranchScope::Local, true),
            branch("origin/main", BranchScope::Remote, false),
        ],
        vec!["origin"],
    );
    assert_eq!(state.local_branches().count(), 1);
    assert_eq!(state.remote_branches().count(), 1);
    assert_eq!(
        state.current_branch().map(|b| b.name.as_str()),
        Some("main")
    );
}

#[test]
fn a_paused_rebase_wins_over_other_markers() {
    // A rebase stopped on a conflict can leave several pseudo-refs present, and
    // the rebase is what the user has to resolve.
    assert_eq!(
        Operation::from_marker_paths(&["MERGE_HEAD".to_owned(), "REBASE_HEAD".to_owned()]),
        Operation::Rebase
    );
    assert_eq!(
        Operation::from_marker_paths(&["CHERRY_PICK_HEAD".to_owned()]),
        Operation::CherryPick
    );
    assert_eq!(
        Operation::from_marker_paths(&["MERGE_HEAD".to_owned()]),
        Operation::Merge
    );
    assert_eq!(Operation::from_marker_paths(&[]), Operation::Idle);
    assert!(!Operation::Idle.is_active());
    assert!(Operation::Rebase.is_active());
}
