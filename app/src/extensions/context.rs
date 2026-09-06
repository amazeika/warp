//! The workspace, repository and session a plugin is acting in.
//!
//! Assembled from application state each time it is wanted, rather than kept.
//! `workspace.getContext` and every [`ActionOrigin`] want the state at the
//! moment they are asked, and a copy that lags by one frame is how an action
//! ends up attached to the pane the user has just left.
//!
//! The walk over that state is covered by running Warp; everything that
//! decides what the pieces *mean* — how they flatten onto the wire, and which
//! events a change produces — is pure and tested here.
use std::path::PathBuf;

use extension_protocol::{
    ActionOrigin, ContextChangedParams, ErrorCode, EventEnvelope, EventKind, ExecutionTarget,
    ExtensionError, SessionChangedParams, WorkspaceContext,
};
use repo_metadata::repositories::DetectedRepositories;
use warp_core::SessionId;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warp_util::remote_path::RemotePath;
use warp_util::standardized_path::StandardizedPath;
use warpui::{AppContext, SingletonEntity as _, ViewHandle, WindowId};

use super::execution::ExecutionService;
use crate::workspace::{Workspace, WorkspaceRegistry};

/// Everything Warp knows about where the user is acting, before it is
/// flattened onto the wire.
///
/// Paths are kept as [`LocalOrRemotePath`] rather than strings until the last
/// moment, because a remote path and a same-named local one are only
/// distinguishable while they still carry their host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActiveContext {
    pub workspace_id: String,
    pub active_pane_id: Option<String>,
    pub cwd: Option<LocalOrRemotePath>,
    pub repository_root: Option<LocalOrRemotePath>,
    /// The session the active pane is bound to, local or remote alike. A pane
    /// whose shell has not bootstrapped yet has none.
    pub session_id: Option<SessionId>,
    /// Where a request anchored to this context would run.
    pub execution_target: ExecutionTarget,
}

impl ActiveContext {
    /// The wire form of this context, or `None` when the active pane has not
    /// reported a working directory yet.
    ///
    /// `cwd` is not optional in the protocol, and there is no honest value to
    /// invent for a pane whose shell has not said where it is: an empty string
    /// or the process's own directory would both name somewhere the user is
    /// not.
    pub(super) fn to_wire(&self) -> Option<WorkspaceContext> {
        Some(WorkspaceContext {
            workspace_id: self.workspace_id.clone(),
            cwd: self.cwd.as_ref()?.display_path(),
            repository_root: self
                .repository_root
                .as_ref()
                .map(LocalOrRemotePath::display_path),
            active_pane_id: self.active_pane_id.clone(),
            session_id: self.session_id.map(session_id_to_string),
            execution_target: self.execution_target.clone(),
        })
    }

    /// Where an action the user just took is anchored.
    ///
    /// Captured at invocation and carried in the event, so a plugin that is
    /// slow to respond acts on the workspace, session and repository the user
    /// was looking at rather than the ones they moved to.
    pub(super) fn origin(&self) -> ActionOrigin {
        ActionOrigin {
            workspace_id: self.workspace_id.clone(),
            session_id: self.session_id.map(session_id_to_string),
            repository_root: self
                .repository_root
                .as_ref()
                .map(LocalOrRemotePath::display_path),
            execution_target: self.execution_target.clone(),
        }
    }

    fn context_changed_params(&self) -> ContextChangedParams {
        ContextChangedParams {
            workspace_id: self.workspace_id.clone(),
            cwd: self.cwd.as_ref().map(LocalOrRemotePath::display_path),
            repository_root: self
                .repository_root
                .as_ref()
                .map(LocalOrRemotePath::display_path),
        }
    }

    fn session_changed_params(&self) -> SessionChangedParams {
        SessionChangedParams {
            workspace_id: self.workspace_id.clone(),
            session_id: self.session_id.map(session_id_to_string),
            execution_target: self.execution_target.clone(),
        }
    }
}

/// The wire form of a `SessionId`, which is what an `ExecutionTarget::Ssh`
/// carries back when a plugin acts on the session it was told about.
pub(super) fn session_id_to_string(session_id: SessionId) -> String {
    session_id.as_u64().to_string()
}

/// Binds a path a plugin named to the host its execution target names.
///
/// The target decides the host, never the shape of the path: `/srv/repo` is a
/// real directory on both machines and the two are different places. A remote
/// target Warp cannot place is refused here rather than resolved, for the same
/// reason [`super::execution::ExecutionService::resolve`] never falls back to
/// the local machine — opening or acting on the same-named local file is the
/// highest-consequence failure this API has.
///
/// Absolute is required on both paths. A relative one would have to be
/// completed from somewhere, and every candidate — Warp's own process
/// directory, whichever pane is focused — names a place the plugin did not ask
/// for.
pub(super) fn resolve_path(
    target: &ExecutionTarget,
    path: &str,
    ctx: &AppContext,
) -> Result<LocalOrRemotePath, ExtensionError> {
    match target {
        ExecutionTarget::Local => {
            let local = PathBuf::from(path);
            if !local.is_absolute() {
                return Err(not_absolute(path));
            }
            Ok(LocalOrRemotePath::Local(local))
        }
        ExecutionTarget::Ssh { session_id } => {
            let host_id = ExecutionService::host_id(session_id, ctx)?;
            let path = StandardizedPath::try_new(path).map_err(|_| not_absolute(path))?;
            Ok(LocalOrRemotePath::Remote(RemotePath::new(host_id, path)))
        }
    }
}

/// The workspace a plugin's request should land in.
///
/// The active window's, because that is the one in front of the user: a plugin
/// asking Warp to show something is asking for it where the user is looking,
/// and a request that arrives with no window open has nowhere to be shown
/// rather than somewhere arbitrary.
pub(super) fn active_workspace(ctx: &AppContext) -> Result<ViewHandle<Workspace>, ExtensionError> {
    let missing = || {
        ExtensionError::new(
            ErrorCode::WorkspaceMissing,
            "there is no active workspace to show this in",
        )
    };
    let window_id = ctx.windows().active_window().ok_or_else(missing)?;
    ctx.views_of_type::<Workspace>(window_id)
        .and_then(|workspaces| workspaces.first().cloned())
        .ok_or_else(missing)
}

fn not_absolute(path: &str) -> ExtensionError {
    ExtensionError::new(
        ErrorCode::InvalidRequest,
        format!("`{path}` is not an absolute path"),
    )
}

/// One context event, ready to be encoded for every running extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContextEvent {
    pub kind: EventKind,
    pub params: ContextEventParams,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContextEventParams {
    Context(ContextChangedParams),
    Session(SessionChangedParams),
}

impl ContextEvent {
    pub(super) fn encode(&self) -> Result<EventEnvelope, ExtensionError> {
        match &self.params {
            ContextEventParams::Context(params) => EventEnvelope::with_params(self.kind, params),
            ContextEventParams::Session(params) => EventEnvelope::with_params(self.kind, params),
        }
    }
}

/// Assembles the context and says what changed about it.
pub(super) struct WorkspaceContextService;

impl WorkspaceContextService {
    /// Where the user is acting right now, or `None` when there is no window
    /// to be acting in.
    ///
    /// Reads the active window's workspace back out of the registry, which
    /// works for every caller that is not itself that workspace. A workspace
    /// mid-update holds itself borrowed and pushes its own context through
    /// [`Self::of`] instead; a build with no registry at all — a test harness,
    /// or a launch mode that never opens a window — has no context rather than
    /// a guessed one.
    pub(super) fn current(ctx: &AppContext) -> Option<ActiveContext> {
        if !ctx.has_singleton_model::<WorkspaceRegistry>() {
            return None;
        }
        let window_id = ctx.windows().active_window()?;
        let workspace = WorkspaceRegistry::as_ref(ctx).get(window_id, ctx)?;
        Some(Self::of(workspace.try_as_ref(ctx)?, window_id, ctx))
    }

    /// The context of a workspace the caller already holds.
    pub(super) fn of(
        workspace: &Workspace,
        window_id: WindowId,
        ctx: &AppContext,
    ) -> ActiveContext {
        let pane_group = workspace.active_tab_pane_group().clone();
        let terminal = pane_group.as_ref(ctx).active_session_view(ctx);

        let session_id = terminal
            .as_ref()
            .and_then(|view| view.as_ref(ctx).active_block_session_id());
        // A pane with no session yet is treated as local: there is no session
        // to name, and `Local` is the only target that cannot be aimed at the
        // wrong host.
        let execution_target = match session_id {
            Some(session_id)
                if terminal
                    .as_ref()
                    .is_some_and(|view| !view.as_ref(ctx).session_is_local(session_id, ctx)) =>
            {
                ExecutionTarget::Ssh {
                    session_id: session_id_to_string(session_id),
                }
            }
            _ => ExecutionTarget::Local,
        };

        let cwd = terminal
            .as_ref()
            .and_then(|view| view.as_ref(ctx).pwd_as_local_or_remote(ctx));
        let repository_root = cwd.as_ref().and_then(|cwd| repository_root(cwd, ctx));

        ActiveContext {
            workspace_id: window_id.to_string(),
            active_pane_id: terminal.as_ref().map(|view| view.id().to_string()),
            cwd,
            repository_root,
            session_id,
            execution_target,
        }
    }

    /// The events that describe the move from `previous` to `current`.
    ///
    /// Each field is compared on its own, so a `cd` inside one repository says
    /// only that the directory moved, and switching windows does not claim the
    /// repository changed when both windows are in the same one. Nothing is
    /// emitted when there is no context to describe: a window closing is not a
    /// statement about a workspace, and a plugin that acted on the last known
    /// one is better served by the failure its next request gets than by an
    /// event naming a workspace that is gone.
    pub(super) fn changes(
        previous: Option<&ActiveContext>,
        current: Option<&ActiveContext>,
    ) -> Vec<ContextEvent> {
        let Some(current) = current else {
            return Vec::new();
        };
        let mut events = Vec::new();
        let context_params = || ContextEventParams::Context(current.context_changed_params());

        if previous.map(|previous| &previous.workspace_id) != Some(&current.workspace_id) {
            events.push(ContextEvent {
                kind: EventKind::WorkspaceChanged,
                params: context_params(),
            });
        }
        if previous.map(|previous| &previous.cwd) != Some(&current.cwd) {
            events.push(ContextEvent {
                kind: EventKind::CwdChanged,
                params: context_params(),
            });
        }
        if previous.map(|previous| &previous.repository_root) != Some(&current.repository_root) {
            events.push(ContextEvent {
                kind: EventKind::RepositoryChanged,
                params: context_params(),
            });
        }
        let session_moved = previous.map(|previous| &previous.session_id)
            != Some(&current.session_id)
            || previous.map(|previous| &previous.execution_target)
                != Some(&current.execution_target);
        if session_moved {
            events.push(ContextEvent {
                kind: EventKind::SessionChanged,
                params: ContextEventParams::Session(current.session_changed_params()),
            });
        }
        events
    }
}

fn repository_root(cwd: &LocalOrRemotePath, ctx: &AppContext) -> Option<LocalOrRemotePath> {
    if !ctx.has_singleton_model::<DetectedRepositories>() {
        return None;
    }
    DetectedRepositories::as_ref(ctx).get_root_for_path(cwd)
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
