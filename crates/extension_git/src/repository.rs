//! Reads repository state by running Git through Warp's execution target.
use extension_protocol::{ExecutionRunResult, ExecutionTarget};
use extension_sdk::{Client, ClientError};

use crate::git::{branches, commits, operations, status};
use crate::state::{Operation, RepositoryState};

/// How many commits the history section shows.
///
/// History is paginated rather than loaded whole: a large repository would
/// otherwise spend seconds building a list nobody scrolls to the end of.
pub const HISTORY_LIMIT: u32 = 25;

/// Runs Git commands against one execution target and working directory.
///
/// Bound to the target and root it was created with, so an operation that
/// started in one repository cannot be retargeted by the user moving focus
/// while it runs.
pub struct Repository<'a, R, W> {
    client: &'a mut Client<R, W>,
    target: ExecutionTarget,
    root: String,
}

impl<'a, R: std::io::BufRead, W: std::io::Write> Repository<'a, R, W> {
    pub fn new(client: &'a mut Client<R, W>, target: ExecutionTarget, root: String) -> Self {
        Self {
            client,
            target,
            root,
        }
    }

    /// Runs `git` and returns stdout, treating a non-zero exit as an error
    /// carrying stderr so the caller can show Git's own words.
    pub fn run(&mut self, args: &[String]) -> Result<String, GitError> {
        let result = self
            .client
            .run(&self.target, &self.root, "git", args)
            .map_err(GitError::Client)?;
        Self::require_success(args, result)
    }

    /// Runs `git` and returns the outcome even when it failed, for commands
    /// whose failure is a legitimate answer rather than a fault — `branch -d`
    /// refusing an unmerged branch, for instance.
    pub fn try_run(&mut self, args: &[String]) -> Result<ExecutionRunResult, ClientError> {
        self.client.run(&self.target, &self.root, "git", args)
    }

    fn require_success(args: &[String], result: ExecutionRunResult) -> Result<String, GitError> {
        if result.exit_code == Some(0) {
            return Ok(result.stdout);
        }
        Err(GitError::Command {
            command: format!("git {}", args.join(" ")),
            exit_code: result.exit_code,
            stderr: result.stderr.trim().to_owned(),
        })
    }

    /// Reads everything the panel needs.
    ///
    /// Status is fetched first and its failure is fatal, because without it
    /// there is nothing meaningful to show. Branches, remotes and history are
    /// independent and a failure in one leaves the rest usable.
    pub fn read_state(&mut self) -> Result<RepositoryState, GitError> {
        let status = status::parse(&self.run(&status::status_args())?);

        let remotes = self
            .run(&operations::remotes())
            .map(|output| branches::parse_remotes(&output))
            .unwrap_or_default();
        let branches = self
            .run(&branches::branch_args())
            .map(|output| branches::parse(&output, &remotes))
            .unwrap_or_default();
        let history = if status.branch.is_initial {
            Vec::new()
        } else {
            self.run(&commits::log_args(HISTORY_LIMIT))
                .map(|output| commits::parse(&output))
                .unwrap_or_default()
        };

        Ok(RepositoryState {
            root: self.root.clone(),
            operation: self.read_operation(),
            status,
            branches,
            remotes,
            history,
            error: None,
        })
    }

    /// Detects an in-progress operation from the pseudo-refs Git sets while one
    /// is paused.
    ///
    /// Each is a single `rev-parse --verify` that succeeds only when the ref
    /// exists, so this works unchanged over SSH — there is no filesystem to
    /// stat on the far side of a session.
    fn read_operation(&mut self) -> Operation {
        let present: Vec<String> = ["REBASE_HEAD", "CHERRY_PICK_HEAD", "MERGE_HEAD"]
            .into_iter()
            .filter(|marker| self.ref_exists(marker))
            .map(str::to_owned)
            .collect();
        Operation::from_marker_paths(&present)
    }

    fn ref_exists(&mut self, name: &str) -> bool {
        self.try_run(&[
            "rev-parse".to_owned(),
            "--verify".to_owned(),
            "--quiet".to_owned(),
            name.to_owned(),
        ])
        .is_ok_and(|result| result.exit_code == Some(0))
    }

    /// Commits not reachable from `base`, used to warn before a force delete.
    pub fn unmerged_commit_count(&mut self, branch: &str, base: &str) -> Option<u32> {
        self.run(&operations::unmerged_commit_count(branch, base))
            .ok()
            .and_then(|output| output.trim().parse().ok())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// Git ran and refused. `stderr` is preserved verbatim for the log while
    /// callers show a shorter summary.
    #[error("{command} failed ({}): {stderr}", exit_code.map_or_else(|| "signal".to_owned(), |code| code.to_string()))]
    Command {
        command: String,
        exit_code: Option<i32>,
        stderr: String,
    },
    #[error(transparent)]
    Client(#[from] ClientError),
}

impl GitError {
    /// A short, user-facing summary. The full command and stderr stay in the
    /// extension log rather than in a dialog.
    pub fn summary(&self) -> String {
        match self {
            GitError::Command { stderr, .. } => stderr
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Git command failed")
                .to_owned(),
            GitError::Client(error) => error.to_string(),
        }
    }
}
