//! The repository state the panel is derived from.
use crate::git::{Branch, BranchScope, Commit, Status};

/// A long-running Git operation the working tree is in the middle of.
///
/// Detected from `.git` marker files rather than inferred from status output,
/// because status alone cannot distinguish a paused rebase from an ordinary
/// conflict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Operation {
    #[default]
    Idle,
    Merge,
    Rebase,
    CherryPick,
}

impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Operation::Idle => "",
            Operation::Merge => "Merging",
            Operation::Rebase => "Rebasing",
            Operation::CherryPick => "Cherry-picking",
        }
    }

    pub fn is_active(self) -> bool {
        !matches!(self, Operation::Idle)
    }

    /// Reads the pseudo-refs Git sets while an operation is paused.
    ///
    /// Order matters: a rebase that stopped on a conflict can leave several of
    /// these present at once, and the rebase is the operation the user actually
    /// has to resolve.
    pub fn from_marker_paths(markers: &[String]) -> Self {
        let has = |needle: &str| markers.iter().any(|marker| marker.contains(needle));
        if has("REBASE_HEAD") {
            Operation::Rebase
        } else if has("CHERRY_PICK_HEAD") {
            Operation::CherryPick
        } else if has("MERGE_HEAD") {
            Operation::Merge
        } else {
            Operation::Idle
        }
    }
}

/// Everything the panel needs about one repository.
#[derive(Debug, Clone, Default)]
pub struct RepositoryState {
    pub root: String,
    pub status: Status,
    pub branches: Vec<Branch>,
    pub remotes: Vec<String>,
    pub history: Vec<Commit>,
    pub operation: Operation,
    /// Set when the last refresh failed, so the panel can show why instead of
    /// silently showing stale or empty content.
    pub error: Option<String>,
}

impl RepositoryState {
    pub fn local_branches(&self) -> impl Iterator<Item = &Branch> {
        self.branches
            .iter()
            .filter(|branch| branch.scope == BranchScope::Local)
    }

    pub fn remote_branches(&self) -> impl Iterator<Item = &Branch> {
        self.branches
            .iter()
            .filter(|branch| branch.scope == BranchScope::Remote)
    }

    pub fn current_branch(&self) -> Option<&Branch> {
        self.branches.iter().find(|branch| branch.is_head)
    }

    /// A one-line summary of where HEAD is relative to its upstream.
    pub fn head_summary(&self) -> String {
        let Some(head) = &self.status.branch.head else {
            return "Detached HEAD".to_owned();
        };
        if self.status.branch.is_initial {
            return format!("{head} (no commits yet)");
        }

        let mut summary = head.clone();
        match &self.status.branch.upstream {
            Some(upstream) => {
                summary.push_str(&format!(" → {upstream}"));
                let (ahead, behind) = (self.status.branch.ahead, self.status.branch.behind);
                match (ahead, behind) {
                    (0, 0) => summary.push_str(" (up to date)"),
                    (ahead, 0) => summary.push_str(&format!(" (↑{ahead})")),
                    (0, behind) => summary.push_str(&format!(" (↓{behind})")),
                    (ahead, behind) => summary.push_str(&format!(" (↑{ahead} ↓{behind})")),
                }
            }
            None => summary.push_str(" (no upstream)"),
        }
        summary
    }

    /// The remote to push a new branch to.
    ///
    /// `origin` when it exists, otherwise the only remote; ambiguity with
    /// several non-`origin` remotes is left to the caller to resolve rather
    /// than guessed at.
    pub fn default_remote(&self) -> Option<&str> {
        if self.remotes.iter().any(|remote| remote == "origin") {
            return Some("origin");
        }
        match self.remotes.as_slice() {
            [only] => Some(only.as_str()),
            _ => None,
        }
    }

    /// A base branch to compare against for merge checks, preferring the
    /// conventional names before falling back to any other local branch.
    ///
    /// The checked-out branch is never the answer: "commits on this branch not
    /// in itself" is always zero, which would make a force delete look safe.
    pub fn default_base_branch(&self) -> Option<&str> {
        let candidates: Vec<&Branch> = self
            .local_branches()
            .filter(|branch| !branch.is_head)
            .collect();
        for preferred in ["main", "master", "develop"] {
            if let Some(branch) = candidates.iter().find(|branch| branch.name == preferred) {
                return Some(branch.name.as_str());
            }
        }
        candidates.first().map(|branch| branch.name.as_str())
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
