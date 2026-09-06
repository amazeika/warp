//! Argument vectors for the Git commands the plugin runs.
//!
//! Every operation is a pure function returning argv, so the exact command a
//! destructive action will run is assertable in a test without executing it.
//! That matters most for the deletion and push paths, where the difference
//! between two argument vectors is the difference between a recoverable and an
//! unrecoverable outcome.

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// `--` separates paths from revisions, so a file named like a branch cannot be
/// reinterpreted as one.
fn args_with_paths(prefix: &[&str], paths: &[String]) -> Vec<String> {
    let mut argv = args(prefix);
    argv.push("--".to_owned());
    argv.extend(paths.iter().cloned());
    argv
}

pub fn remotes() -> Vec<String> {
    args(&["remote"])
}

pub fn stage(paths: &[String]) -> Vec<String> {
    args_with_paths(&["add"], paths)
}

pub fn stage_all() -> Vec<String> {
    args(&["add", "--all"])
}

/// `restore --staged` rather than `reset`, so unstaging never moves HEAD.
pub fn unstage(paths: &[String]) -> Vec<String> {
    args_with_paths(&["restore", "--staged"], paths)
}

pub fn unstage_all() -> Vec<String> {
    args(&["reset", "--mixed", "HEAD"])
}

/// Discards worktree changes for tracked paths. Irreversible; the caller must
/// confirm first.
pub fn discard(paths: &[String]) -> Vec<String> {
    args_with_paths(&["checkout"], paths)
}

/// Deletes untracked files. Scoped to explicit paths — there is deliberately no
/// `clean -fdx` here, because a button that wipes ignored files is not
/// something a user can undo.
pub fn delete_untracked(paths: &[String]) -> Vec<String> {
    args_with_paths(&["clean", "--force"], paths)
}

/// Commits the index. `--` is not used: there are no paths, and passing one
/// would commit that path rather than the index.
pub fn commit(message: &str) -> Vec<String> {
    vec![
        "commit".to_owned(),
        "--message".to_owned(),
        message.to_owned(),
    ]
}

pub fn fetch() -> Vec<String> {
    args(&["fetch", "--prune"])
}

pub fn pull() -> Vec<String> {
    args(&["pull", "--ff-only"])
}

pub fn push() -> Vec<String> {
    args(&["push"])
}

pub fn push_setting_upstream(remote: &str, branch: &str) -> Vec<String> {
    vec![
        "push".to_owned(),
        "--set-upstream".to_owned(),
        remote.to_owned(),
        branch.to_owned(),
    ]
}

/// A force update never maps to `--force`.
///
/// `--force-with-lease` refuses when the remote moved since the last fetch, so
/// a teammate's push cannot be silently discarded.
pub fn force_push_with_lease() -> Vec<String> {
    args(&["push", "--force-with-lease"])
}

pub fn create_branch(name: &str) -> Vec<String> {
    vec!["switch".to_owned(), "--create".to_owned(), name.to_owned()]
}

pub fn switch_branch(name: &str) -> Vec<String> {
    vec!["switch".to_owned(), name.to_owned()]
}

/// Checks out a remote-tracking branch as a local branch that tracks it.
pub fn switch_to_remote_branch(local: &str, remote_ref: &str) -> Vec<String> {
    vec![
        "switch".to_owned(),
        "--create".to_owned(),
        local.to_owned(),
        "--track".to_owned(),
        remote_ref.to_owned(),
    ]
}

pub fn rename_branch(from: &str, to: &str) -> Vec<String> {
    vec![
        "branch".to_owned(),
        "--move".to_owned(),
        from.to_owned(),
        to.to_owned(),
    ]
}

/// Safe delete: Git refuses if the branch holds unmerged commits.
pub fn delete_branch(name: &str) -> Vec<String> {
    vec!["branch".to_owned(), "--delete".to_owned(), name.to_owned()]
}

/// Force delete, which discards unmerged commits. Only reachable behind a
/// second, stronger confirmation.
pub fn force_delete_branch(name: &str) -> Vec<String> {
    vec![
        "branch".to_owned(),
        "--delete".to_owned(),
        "--force".to_owned(),
        name.to_owned(),
    ]
}

/// Commits on `branch` that are not reachable from `base`, used to tell the
/// user what a force delete would discard.
pub fn unmerged_commit_count(branch: &str, base: &str) -> Vec<String> {
    vec![
        "rev-list".to_owned(),
        "--count".to_owned(),
        format!("{base}..{branch}"),
    ]
}

pub fn rebase_onto(base: &str) -> Vec<String> {
    vec!["rebase".to_owned(), base.to_owned()]
}

pub fn rebase_continue() -> Vec<String> {
    args(&["rebase", "--continue"])
}

pub fn rebase_abort() -> Vec<String> {
    args(&["rebase", "--abort"])
}

pub fn merge_abort() -> Vec<String> {
    args(&["merge", "--abort"])
}

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;
