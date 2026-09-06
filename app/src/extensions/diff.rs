//! What a plugin's diff request compares, decided without a pane to ask.
//!
//! Opening the review panel used to mean holding a `WeakViewHandle<TerminalView>`:
//! `CodeReviewPanelArg` read the session to load the diff over off whichever
//! pane raised the request. An extension has no pane, so the decision is made
//! here instead — repository, session, base, and the file to land on — and
//! handed to the panel as a [`CodeReviewPanelArg`] whose origin names the
//! session outright.
//!
//! The walk that actually opens the panel is covered by running Warp. What is
//! tested here is the decision: which base a comparison maps to, and which
//! paths are allowed to name a file inside a repository.
use extension_protocol::{
    DiffComparison, DiffOpenFileParams, DiffOpenWorkingTreeParams, ErrorCode, ExecutionTarget,
    ExtensionError,
};
use warp_core::SessionId;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warpui::AppContext;

use super::context::{active_workspace, resolve_path};
use super::execution::parse_session_id;
use crate::code_review::diff_state::DiffMode;
use crate::code_review::telemetry_event::CodeReviewPaneEntrypoint;
use crate::code_review::{CodeReviewOrigin, CodeReviewPanelArg};

/// What a diff request resolves to before there is a panel to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiffRequest {
    pub repo_path: LocalOrRemotePath,
    /// The session the diff loads over. `None` is the local machine, which is
    /// also what a repository on it needs no session to read.
    pub session_id: Option<SessionId>,
    pub mode: DiffMode,
    /// Repository-relative, the way Git names a file and the way the review
    /// keys its own — so nothing has to be resolved against a host to match.
    pub file: Option<String>,
}

/// Turns "compare this" into something the review panel can open.
pub(super) struct DiffService;

impl DiffService {
    pub(super) fn working_tree(
        params: &DiffOpenWorkingTreeParams,
        ctx: &AppContext,
    ) -> Result<DiffRequest, ExtensionError> {
        Ok(DiffRequest {
            repo_path: resolve_path(&params.target, &params.repository_root, ctx)?,
            session_id: session_of(&params.target)?,
            mode: DiffMode::Head,
            file: None,
        })
    }

    pub(super) fn file(
        params: &DiffOpenFileParams,
        ctx: &AppContext,
    ) -> Result<DiffRequest, ExtensionError> {
        let file = repo_relative(&params.path)?;
        Ok(DiffRequest {
            repo_path: resolve_path(&params.target, &params.repository_root, ctx)?,
            session_id: session_of(&params.target)?,
            mode: diff_mode(params.comparison),
            file: Some(file),
        })
    }
}

impl DiffRequest {
    /// The argument the review panel opens from.
    ///
    /// The origin is detached rather than a pane handle, which is the whole
    /// point of this module: the session was named by the request and cannot
    /// be moved by a focus change between asking and opening.
    pub(super) fn panel_arg(&self) -> CodeReviewPanelArg {
        CodeReviewPanelArg {
            repo_path: Some(self.repo_path.clone()),
            origin: CodeReviewOrigin::Detached {
                session_id: self.session_id,
            },
            entrypoint: CodeReviewPaneEntrypoint::Extension,
            focus_new_pane: true,
            cli_agent: None,
        }
    }

    /// Opens the review panel on this request.
    ///
    /// A panel that does not end up showing the repository is reported as a
    /// missing repository rather than as a success: from the plugin's side the
    /// two look identical otherwise, and the one it can act on is the failure.
    pub(super) fn show(self, ctx: &mut AppContext) -> Result<(), ExtensionError> {
        let workspace = active_workspace(ctx)?;
        let arg = self.panel_arg();
        let opened = workspace.update(ctx, |workspace, ctx| {
            workspace.open_code_review_panel_detached(&arg, self.mode, self.file, ctx)
        });
        match opened {
            true => Ok(()),
            false => Err(ExtensionError::new(
                ErrorCode::RepositoryMissing,
                format!(
                    "Warp could not open a diff of `{}`",
                    self.repo_path.display_path()
                ),
            )),
        }
    }
}

/// The base today's diff model can actually compare against.
///
/// `LocalDiffStateModel` compares a base *commit* to the working tree; the
/// index is not a base it can name, so neither of the two narrower comparisons
/// is representable and all three resolve to HEAD-to-working-tree. That is a
/// superset of both, and it is the comparison the panel labels itself with, so
/// the user is never told they are looking at only the staged half of a file
/// when they are looking at all of it. Narrowing arrives with the explicit
/// revision pair that `diff.openCommit` and `diff.openComparison` need, which
/// is a separate capability token rather than a widening of this one.
fn diff_mode(comparison: DiffComparison) -> DiffMode {
    match comparison {
        DiffComparison::HeadToWorktree
        | DiffComparison::HeadToIndex
        | DiffComparison::IndexToWorktree => DiffMode::Head,
    }
}

/// The session a target names, without re-checking that Warp still has it.
///
/// Reachability is [`resolve_path`]'s answer to give: it is the call that
/// binds the repository to a host, and a session that cannot place a path
/// cannot load a diff of it either.
fn session_of(target: &ExecutionTarget) -> Result<Option<SessionId>, ExtensionError> {
    match target {
        ExecutionTarget::Local => Ok(None),
        ExecutionTarget::Ssh { session_id } => parse_session_id(session_id).map(Some),
    }
}

/// Checks that a path names a file *inside* the repository it came with.
///
/// It is matched against the review's own repo-relative keys and, further
/// down, joined onto a root that was resolved from the request's target — so a
/// path that is absolute or that climbs out through `..` would name something
/// the target never authorised. Refusing at the boundary keeps the rule where
/// it can be read: what crosses the wire is a repository-relative path, and
/// nothing else is one.
fn repo_relative(path: &str) -> Result<String, ExtensionError> {
    let refuse = |why: &str| {
        Err(ExtensionError::new(
            ErrorCode::InvalidRequest,
            format!("`{path}` is not a path inside the repository: {why}"),
        ))
    };
    if path.is_empty() {
        return refuse("it is empty");
    }
    // Split on both separators on every platform: the path may name a file on
    // a host whose separator is not this machine's.
    let mut components = path.split(['/', '\\']).peekable();
    if path.starts_with('/') || path.starts_with('\\') {
        return refuse("it is absolute");
    }
    while let Some(component) = components.next() {
        match component {
            ".." => return refuse("it climbs out of the repository"),
            // A trailing separator leaves one empty component, which names the
            // directory itself rather than a file outside it.
            "" if components.peek().is_some() => return refuse("it has an empty component"),
            _ => {}
        }
    }
    // A drive-qualified Windows path names a place of its own without any
    // leading separator, so it is not caught by the check above.
    let mut head = path.chars();
    if head.next().is_some_and(|first| first.is_ascii_alphabetic()) && head.next() == Some(':') {
        return refuse("it names a drive");
    }
    Ok(path.to_owned())
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
