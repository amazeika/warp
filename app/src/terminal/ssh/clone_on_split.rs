//! Deciding whether a split pane may join the SSH connection its source pane already holds.
//!
//! Every gate here is a field read on the source session, so the decision costs nothing and a
//! split stays instant. Master *liveness* is deliberately not among them: the shell wrapper
//! re-runs `ssh -O check` immediately before attaching, so an app-side probe could never be
//! authoritative, and making the split gesture wait on a subprocess would stall the window on
//! exactly the wedged-socket case such a probe would exist for. A master that has gone away
//! surfaces as the wrapper's one-line explanation in an otherwise ordinary local pane.

use std::path::PathBuf;

use settings::Setting as _;
use warpui::{AppContext, SingletonEntity as _};

use crate::features::FeatureFlag;
use crate::settings::SshSettings;
use crate::terminal::model::session::{IsSSHWrapperSession, SessionType};
use crate::terminal::shell::ShellType;

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
}

/// Why a split fell back to an ordinary local pane instead of joining its source's connection.
///
/// Every one of these is an *app-side* refusal, decided from fields already on the source session.
/// A clone this module approves can still end at a local pane: the wrapper re-runs `ssh -O check`
/// and fails closed on a master that has gone away, and no variant here describes that. Success is
/// therefore not this module's to report — see `SshCloneOnSplitTelemetryEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneDeclined {
    /// The feature flag or the user's setting is off.
    Disabled,
    /// The source pane held a local shell.
    NotWarpifiedRemote,
    /// The source was warpified by the RC-file snippet inside an unwrapped `ssh`, so it carries
    /// no socket to attach to.
    NoWrapperSocket,
    /// The master would be torn down with the source pane, severing the split when that pane
    /// closed.
    MasterWouldNotOutliveSource,
    /// The socket lives inside a WSL distro the new pane cannot reach.
    WslDistroMismatch,
    /// The shell the new pane will run defines no `ssh` wrapper, so the replayed command would
    /// dial the host itself.
    TargetShellHasNoWrapper,
    /// No `ssh` command is bound to the source session, so there is nothing to replay.
    NoBoundCommand,
}

impl CloneDeclined {
    /// Whether the user split a warpified SSH pane and got a local one anyway.
    ///
    /// This is the line the fallback rate is drawn at, and it is deliberately generous: a pane
    /// with no wrapper socket, or one older than the flag, is a split the user experienced as a
    /// fallback even though the feature could never have served it. Both stay countable and stay
    /// separable by reason, so an early dashboard can subtract the populations it cannot reach.
    /// Below this line the split was never a candidate at all — the feature is off, or the pane
    /// held a local shell — and counting those would drown the rate in ordinary local splits.
    ///
    /// Two further refusals never reach this enum: `PaneGroup::ssh_clone_request` returns before
    /// calling `clone_request` at all when the split source is not a terminal pane, and when that
    /// pane has no active session. Both are non-candidates by the same rule.
    pub fn is_fallback(self) -> bool {
        !matches!(self, Self::Disabled | Self::NotWarpifiedRemote)
    }

    /// The stable telemetry name for this reason. Spelled out rather than derived, because a
    /// rename of a variant must not silently rewrite a dashboard's history.
    pub fn telemetry_reason(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NotWarpifiedRemote => "not_warpified_remote",
            Self::NoWrapperSocket => "no_wrapper_socket",
            Self::MasterWouldNotOutliveSource => "master_would_not_outlive_source",
            Self::WslDistroMismatch => "wsl_distro_mismatch",
            Self::TargetShellHasNoWrapper => "target_shell_has_no_wrapper",
            Self::NoBoundCommand => "no_bound_command",
        }
    }
}

/// Every condition that must hold before a split may join its source's connection.
///
/// A struct rather than three `bool` arguments because they are interchangeable at a call site and
/// silently swappable; and a named type rather than an inline `&&` because dropping one conjunct
/// there would compile, ship the feature to people who never enabled it, and break no test. Adding
/// a condition here is a compile error at the call site, which is the point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloneGate {
    /// The rollout flag. Off means the feature is absent, whatever the rest say.
    pub feature_flag: bool,
    /// Whether the SSH wrapper is active for new panes. Without it the split's shell never runs
    /// `warp_ssh_helper`, so it would replay `ssh` as a plain command and dial the host itself.
    pub ssh_warpification: bool,
    /// The user's opt-in.
    pub setting: bool,
}

impl CloneGate {
    /// Whether all three hold. Every one is necessary, so there is no precedence between them.
    pub fn is_open(self) -> bool {
        self.feature_flag && self.ssh_warpification && self.setting
    }
}

/// Whether a pane spawned now should keep its Warp-owned `ControlMaster` alive past the
/// foreground `ssh` that created it — that is, the value of `WARP_SSH_CONTROL_PERSIST`.
///
/// Both conjuncts are required, and for different reasons. The flag alone would make a user who
/// declined the feature pay for masters that outlive their `ssh`; the setting alone would turn the
/// lifetime change on in a build where the feature is meant to be inert.
///
/// Deliberately narrower than [`CloneGate`], which also requires SSH warpification. That conjunct
/// is what decides whether a *split* may attach; here it would be redundant, since a pane spawned
/// with warpification off carries `WARP_USE_SSH_WRAPPER=0` and never reaches the helper that reads
/// this variable at all.
pub fn control_persist_enabled(feature_flag: bool, setting: bool) -> bool {
    feature_flag && setting
}

/// Reads both conjuncts of [`control_persist_enabled`] from their real sources.
///
/// Split out for the same reason as `PaneGroup::ssh_clone_gate`: the rule alone proves nothing
/// about the wiring, and a literal left in place of either read would satisfy the rule while
/// ignoring the flag or the user.
pub fn clone_ssh_on_split_enabled(ctx: &AppContext) -> bool {
    control_persist_enabled(
        FeatureFlag::CloneSshOnSplit.is_enabled(),
        *SshSettings::as_ref(ctx).clone_ssh_on_split.value(),
    )
}

/// The clone request for a split of `source`, or the reason to fall back to an ordinary local
/// split.
///
/// Preferring no clone over a wrong clone is the governing rule: every gate below fails closed.
/// `enabled` carries the user setting and the feature flag, already combined — the flag wins by
/// construction, since either being off yields `Disabled`.
///
/// `Ok` means the attach was *requested*, never that it succeeded. The wrapper decides that, and
/// it decides it after this function has returned.
pub fn clone_request(
    source: &SshCloneFacts,
    target_wsl_distro: Option<&str>,
    target_shell: Option<ShellType>,
    enabled: bool,
) -> Result<SshCloneRequest, CloneDeclined> {
    if !enabled {
        return Err(CloneDeclined::Disabled);
    }

    // A session mid-login is not yet `WarpifiedRemote`, so this covers "SSH is still
    // authenticating" without a separate check. Matched exhaustively so a new session flavour
    // has to make this decision consciously rather than inheriting "no clone" by default.
    match source.session_type {
        SessionType::WarpifiedRemote { .. } => {}
        SessionType::Local => return Err(CloneDeclined::NotWarpifiedRemote),
    }

    // A session warpified by the RC-file snippet inside an unwrapped `ssh` carries no socket.
    let IsSSHWrapperSession::Yes {
        socket_path,
        external_control_master,
        persist,
    } = &source.wrapper
    else {
        return Err(CloneDeclined::NoWrapperSocket);
    };

    // Attaching to a master that dies with the source pane would sever the split the moment that
    // pane closed. Teardown force-exits a master only when Warp owns it *and* it is
    // non-persistent, so either flag on its own means the master survives. Reading the session's
    // reported `persist` rather than the feature flag matters: `WARP_SSH_CONTROL_PERSIST` is
    // captured at pane spawn, so a pane older than the flag still holds a non-persistent master.
    if !persist && !external_control_master {
        return Err(CloneDeclined::MasterWouldNotOutliveSource);
    }

    // The socket lives inside the source session's WSL distro, so no pane outside it can reach it.
    if source.wsl_distro.as_deref() != target_wsl_distro {
        return Err(CloneDeclined::WslDistroMismatch);
    }

    // `warp_ssh_helper` — the function that reads the attach request — is defined only in the
    // bash, zsh and fish bootstraps. A pane spawned with any other shell would run the replayed
    // `ssh` as a plain command, dial the host itself, and prompt for exactly the credentials this
    // feature exists to avoid. `None` is the pane inheriting the default shell, which carries the
    // wrapper by construction: the source pane warpified through it.
    if let Some(shell) = target_shell
        && !matches!(shell, ShellType::Bash | ShellType::Zsh | ShellType::Fish)
    {
        return Err(CloneDeclined::TargetShellHasNoWrapper);
    }

    Ok(SshCloneRequest {
        socket_path: socket_path.clone(),
        command: source
            .bound_command
            .clone()
            .ok_or(CloneDeclined::NoBoundCommand)?,
        // An unknown or unsubmittable directory costs the split nothing: the new pane lands in the
        // remote default, which is where a fresh `ssh` would have put it anyway.
    })
}

#[cfg(test)]
#[path = "clone_on_split_tests.rs"]
mod tests;
