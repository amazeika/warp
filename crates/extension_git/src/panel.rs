//! Renders [`RepositoryState`] into the panel model Warp draws.
//!
//! The plugin supplies structure and labels only. Loading, empty and error
//! presentation belong to Warp, so an extension panel looks like a built-in one.
use extension_protocol::{PanelItem, PanelItemAction, PanelSection, PanelStatus, PanelViewState};

use crate::git::{Branch, ChangeKind, Commit, FileChange};
use crate::state::RepositoryState;

pub const PANEL_ID: &str = "git";

pub const SECTION_SUMMARY: &str = "git.summary";
pub const SECTION_STAGED: &str = "git.staged";
pub const SECTION_UNSTAGED: &str = "git.unstaged";
pub const SECTION_UNTRACKED: &str = "git.untracked";
pub const SECTION_CONFLICTS: &str = "git.conflicts";
pub const SECTION_BRANCHES: &str = "git.branches";
pub const SECTION_REMOTE_BRANCHES: &str = "git.remoteBranches";
pub const SECTION_HISTORY: &str = "git.history";

pub const ACTION_OPEN_DIFF: &str = "open_diff";
pub const ACTION_OPEN_FILE: &str = "open_file";
pub const ACTION_STAGE: &str = "stage";
pub const ACTION_UNSTAGE: &str = "unstage";
pub const ACTION_DISCARD: &str = "discard";
pub const ACTION_DELETE_FILE: &str = "delete_file";
pub const ACTION_SWITCH_BRANCH: &str = "switch_branch";
pub const ACTION_DELETE_BRANCH: &str = "delete_branch";
pub const ACTION_RENAME_BRANCH: &str = "rename_branch";
pub const ACTION_CHECKOUT_REMOTE: &str = "checkout_remote";
pub const ACTION_OPEN_COMMIT: &str = "open_commit";

pub fn loading() -> PanelViewState {
    PanelViewState {
        panel_id: PANEL_ID.to_owned(),
        status: PanelStatus::Loading,
        sections: Vec::new(),
    }
}

pub fn error(message: impl Into<String>) -> PanelViewState {
    PanelViewState {
        panel_id: PANEL_ID.to_owned(),
        status: PanelStatus::Error {
            message: message.into(),
        },
        sections: Vec::new(),
    }
}

pub fn no_repository() -> PanelViewState {
    PanelViewState {
        panel_id: PANEL_ID.to_owned(),
        status: PanelStatus::Empty {
            message: "No Git repository in this workspace".to_owned(),
        },
        sections: Vec::new(),
    }
}

pub fn render(state: &RepositoryState) -> PanelViewState {
    if let Some(message) = &state.error {
        return error(message.clone());
    }

    let mut sections = vec![summary_section(state)];
    if !state.status.conflicts.is_empty() {
        sections.push(change_section(
            SECTION_CONFLICTS,
            "Conflicts",
            &state.status.conflicts,
            &[ACTION_OPEN_FILE, ACTION_OPEN_DIFF, ACTION_STAGE],
        ));
    }
    if !state.status.staged.is_empty() {
        sections.push(change_section(
            SECTION_STAGED,
            "Staged",
            &state.status.staged,
            &[ACTION_OPEN_DIFF, ACTION_UNSTAGE, ACTION_OPEN_FILE],
        ));
    }
    if !state.status.unstaged.is_empty() {
        sections.push(change_section(
            SECTION_UNSTAGED,
            "Changes",
            &state.status.unstaged,
            &[
                ACTION_OPEN_DIFF,
                ACTION_STAGE,
                ACTION_DISCARD,
                ACTION_OPEN_FILE,
            ],
        ));
    }
    if !state.status.untracked.is_empty() {
        sections.push(change_section(
            SECTION_UNTRACKED,
            "Untracked",
            &state.status.untracked,
            &[ACTION_OPEN_FILE, ACTION_STAGE, ACTION_DELETE_FILE],
        ));
    }
    sections.push(branch_section(state));
    if state.remote_branches().next().is_some() {
        sections.push(remote_branch_section(state));
    }
    if !state.history.is_empty() {
        sections.push(history_section(&state.history));
    }

    // A clean tree is not an empty panel — branches and history are still
    // worth showing. Empty is reserved for a repository with nothing in it yet.
    let status = if sections.iter().all(|section| section.items.is_empty()) {
        PanelStatus::Empty {
            message: "No changes, branches or history to show yet".to_owned(),
        }
    } else {
        PanelStatus::Ready
    };
    PanelViewState {
        panel_id: PANEL_ID.to_owned(),
        status,
        sections,
    }
}

/// The header row: where HEAD is, and whether an operation is paused.
///
/// A paused rebase or merge is the most important thing on the panel, because
/// nothing else the user does will behave normally until it is resolved.
fn summary_section(state: &RepositoryState) -> PanelSection {
    let mut items = vec![PanelItem {
        id: "head".to_owned(),
        label: state.head_summary(),
        description: state
            .current_branch()
            .map(|branch| format!("{} {}", branch.short_commit, branch.subject)),
        icon: Some("branch-current".to_owned()),
        badge: None,
        actions: Vec::new(),
        children: Vec::new(),
        expanded: false,
    }];
    if state.operation.is_active() {
        items.push(PanelItem {
            id: "operation".to_owned(),
            label: format!("{} in progress", state.operation.label()),
            description: Some("Resolve the conflicts, then continue or abort.".to_owned()),
            icon: Some("conflict".to_owned()),
            badge: None,
            actions: Vec::new(),
            children: Vec::new(),
            expanded: false,
        });
    }
    PanelSection {
        id: SECTION_SUMMARY.to_owned(),
        title: "Repository".to_owned(),
        badge: (!state.status.is_clean()).then(|| state.status.total_changes().to_string()),
        items,
    }
}

fn change_section(id: &str, title: &str, changes: &[FileChange], actions: &[&str]) -> PanelSection {
    PanelSection {
        id: id.to_owned(),
        title: title.to_owned(),
        badge: Some(changes.len().to_string()),
        items: changes
            .iter()
            .map(|change| change_item(change, actions))
            .collect(),
    }
}

fn change_item(change: &FileChange, actions: &[&str]) -> PanelItem {
    let description = change
        .original_path
        .as_ref()
        .map(|original| format!("renamed from {original}"));
    PanelItem {
        // The repository-relative path is the id, so an action event carries
        // straight back into a Git argument with no lookup table in between.
        id: change.path.clone(),
        label: change.path.clone(),
        description,
        icon: Some(icon_for(change.kind).to_owned()),
        badge: Some(change.kind.code().to_owned()),
        actions: actions.iter().map(|action| action_for(action)).collect(),
        children: Vec::new(),
        expanded: false,
    }
}

fn branch_section(state: &RepositoryState) -> PanelSection {
    let branches: Vec<&Branch> = state.local_branches().collect();
    PanelSection {
        id: SECTION_BRANCHES.to_owned(),
        title: "Branches".to_owned(),
        badge: Some(branches.len().to_string()),
        items: branches
            .iter()
            .map(|branch| PanelItem {
                id: branch.name.clone(),
                label: branch.name.clone(),
                description: (!branch.subject.is_empty()).then(|| branch.subject.clone()),
                icon: Some(
                    if branch.is_head {
                        "branch-current"
                    } else {
                        "branch"
                    }
                    .to_owned(),
                ),
                badge: branch.upstream.clone(),
                // The checked-out branch cannot be switched to, deleted, or
                // usefully offered either action.
                actions: if branch.is_head {
                    vec![action_for(ACTION_RENAME_BRANCH)]
                } else {
                    vec![
                        action_for(ACTION_SWITCH_BRANCH),
                        action_for(ACTION_RENAME_BRANCH),
                        action_for(ACTION_DELETE_BRANCH),
                    ]
                },
                children: Vec::new(),
                expanded: false,
            })
            .collect(),
    }
}

fn remote_branch_section(state: &RepositoryState) -> PanelSection {
    let branches: Vec<&Branch> = state.remote_branches().collect();
    PanelSection {
        id: SECTION_REMOTE_BRANCHES.to_owned(),
        title: "Remote branches".to_owned(),
        badge: Some(branches.len().to_string()),
        items: branches
            .iter()
            .map(|branch| PanelItem {
                id: branch.name.clone(),
                label: branch.name.clone(),
                description: (!branch.subject.is_empty()).then(|| branch.subject.clone()),
                icon: Some("branch-remote".to_owned()),
                badge: None,
                actions: vec![action_for(ACTION_CHECKOUT_REMOTE)],
                children: Vec::new(),
                expanded: false,
            })
            .collect(),
    }
}

fn history_section(history: &[Commit]) -> PanelSection {
    PanelSection {
        id: SECTION_HISTORY.to_owned(),
        title: "History".to_owned(),
        badge: None,
        items: history
            .iter()
            .map(|commit| PanelItem {
                // The full hash is the id so the action can address the commit
                // unambiguously; the short hash is only for display.
                id: commit.hash.clone(),
                label: commit.subject.clone(),
                description: Some(format!(
                    "{} · {} · {}",
                    commit.short_hash, commit.author, commit.age
                )),
                icon: Some("commit".to_owned()),
                badge: None,
                actions: vec![action_for(ACTION_OPEN_COMMIT)],
                children: Vec::new(),
                expanded: false,
            })
            .collect(),
    }
}

fn icon_for(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
        ChangeKind::Renamed => "renamed",
        ChangeKind::Copied => "copied",
        ChangeKind::TypeChanged => "modified",
        ChangeKind::Untracked => "untracked",
        ChangeKind::Conflicted => "conflict",
    }
}

fn action_for(id: &str) -> PanelItemAction {
    let (title, destructive) = match id {
        ACTION_OPEN_DIFF => ("Open diff", false),
        ACTION_OPEN_FILE => ("Open file", false),
        ACTION_STAGE => ("Stage", false),
        ACTION_UNSTAGE => ("Unstage", false),
        ACTION_DISCARD => ("Discard changes", true),
        ACTION_DELETE_FILE => ("Delete file", true),
        ACTION_SWITCH_BRANCH => ("Switch to branch", false),
        ACTION_DELETE_BRANCH => ("Delete branch", true),
        ACTION_RENAME_BRANCH => ("Rename branch", false),
        ACTION_CHECKOUT_REMOTE => ("Check out locally", false),
        ACTION_OPEN_COMMIT => ("Open commit diff", false),
        other => (other, false),
    };
    PanelItemAction {
        id: id.to_owned(),
        title: title.to_owned(),
        icon: None,
        destructive,
    }
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
