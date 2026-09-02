//! Deciding whether a split pane may join the SSH connection its source pane already holds.
//!
//! Every gate here is a field read on the source session, so the decision costs nothing and a
//! split stays instant. Master *liveness* is deliberately not among them: the shell wrapper
//! re-runs `ssh -O check` immediately before attaching, so an app-side probe could never be
//! authoritative, and making the split gesture wait on a subprocess would stall the window on
//! exactly the wedged-socket case such a probe would exist for. A master that has gone away
//! surfaces as the wrapper's one-line explanation in an otherwise ordinary local pane.

use std::path::PathBuf;

use crate::terminal::model::session::{IsSSHWrapperSession, SessionType};
use crate::terminal::ssh::util::submittable_remote_cwd;

/// Set on a split pane's local shell to ask the SSH wrapper to attach to an existing
/// ControlMaster rather than create its own. The wrapper consumes and unsets it on its first
/// lines, so it never outlives the one `ssh` it was set for.
pub const ATTACH_CONTROL_PATH_ENV: &str = "WARP_SSH_ATTACH_CONTROL_PATH";

/// What a split pane needs to join its source pane's connection: the ControlMaster socket to
/// multiplex onto, and the `ssh` command to replay verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCloneRequest {
    pub socket_path: PathBuf,
    pub command: String,
    /// The source pane's remote directory for the new pane to enter once its own session has
    /// bootstrapped. Already refused if it cannot be submitted safely; still unquoted, because
    /// the shell that will receive it is not known until that session reports itself. Absent when
    /// the source pane reported no remote directory.
    pub remote_cwd: Option<String>,
}

/// The facts about a source pane's active session that decide whether a split may attach to it.
///
/// Owned rather than borrowed: these are read out of the sessions model under a `ctx` borrow that
/// ends before the split is made, so the facts have to outlive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCloneFacts {
    pub session_type: SessionType,
    pub wrapper: IsSSHWrapperSession,
    /// The `ssh` command bound to this session, from `TerminalView::bound_ssh_command`. Already
    /// alias-expanded and already through `parse_interactive_ssh_command`, which is what makes it
    /// safe to replay unchanged.
    pub bound_command: Option<String>,
    /// The WSL distro the source session runs in, if any.
    pub wsl_distro: Option<String>,
    /// The source pane's remote working directory as the remote host reported it, from the active
    /// block's metadata. Raw and untrusted: it is filtered before any `cd` is built from it.
    pub remote_cwd: Option<String>,
}

/// The clone request for a split of `source`, or `None` to fall back to an ordinary local split.
///
/// Preferring no clone over a wrong clone is the governing rule: every gate below fails closed.
/// `enabled` carries the user setting and the feature flag.
pub fn clone_request(
    source: &SshCloneFacts,
    target_wsl_distro: Option<&str>,
    enabled: bool,
) -> Option<SshCloneRequest> {
    if !enabled {
        return None;
    }

    // A session mid-login is not yet `WarpifiedRemote`, so this covers "SSH is still
    // authenticating" without a separate check. Matched exhaustively so a new session flavour
    // has to make this decision consciously rather than inheriting "no clone" by default.
    match source.session_type {
        SessionType::WarpifiedRemote { .. } => {}
        SessionType::Local => return None,
    }

    // A session warpified by the RC-file snippet inside an unwrapped `ssh` carries no socket.
    let IsSSHWrapperSession::Yes {
        socket_path,
        external_control_master,
        persist,
    } = &source.wrapper
    else {
        return None;
    };

    // Attaching to a master that dies with the source pane would sever the split the moment that
    // pane closed. Teardown force-exits a master only when Warp owns it *and* it is
    // non-persistent, so either flag on its own means the master survives. Reading the session's
    // reported `persist` rather than the feature flag matters: `WARP_SSH_CONTROL_PERSIST` is
    // captured at pane spawn, so a pane older than the flag still holds a non-persistent master.
    if !persist && !external_control_master {
        return None;
    }

    // The socket lives inside the source session's WSL distro, so no pane outside it can reach it.
    if source.wsl_distro.as_deref() != target_wsl_distro {
        return None;
    }

    Some(SshCloneRequest {
        socket_path: socket_path.clone(),
        command: source.bound_command.clone()?,
        // An unknown or unsubmittable directory costs the split nothing: the new pane lands in the
        // remote default, which is where a fresh `ssh` would have put it anyway.
        remote_cwd: source
            .remote_cwd
            .as_deref()
            .and_then(submittable_remote_cwd)
            .map(str::to_owned),
    })
}

#[cfg(test)]
#[path = "clone_on_split_tests.rs"]
mod tests;
