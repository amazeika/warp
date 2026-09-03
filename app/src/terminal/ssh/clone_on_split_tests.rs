use std::path::{Path, PathBuf};

use super::*;

const SOCKET: &str = "/tmp/warp-ssh/9f3c1e";
const COMMAND: &str = "ssh -J bastion mini";

fn remote() -> SessionType {
    SessionType::WarpifiedRemote { host_id: None }
}

/// A master Warp created with `ControlPersist`: it outlives the pane that opened it.
fn warp_persistent_master() -> IsSSHWrapperSession {
    IsSSHWrapperSession::Yes {
        socket_path: PathBuf::from(SOCKET),
        external_control_master: false,
        persist: true,
    }
}

fn source(session_type: SessionType, wrapper: IsSSHWrapperSession) -> SshCloneFacts {
    SshCloneFacts {
        session_type,
        wrapper,
        bound_command: Some(COMMAND.to_owned()),
        wsl_distro: None,
    }
}

#[test]
fn attaches_to_the_source_session_socket() {
    let request = clone_request(&source(remote(), warp_persistent_master()), None, true)
        .expect("a persistent Warp master is attachable");

    assert_eq!(request.socket_path, Path::new(SOCKET));
}

/// The bound command is already alias-expanded and already through
/// `parse_interactive_ssh_command`; replaying anything but the exact string gives that up.
#[test]
fn replays_the_bound_command_verbatim() {
    let request = clone_request(&source(remote(), warp_persistent_master()), None, true)
        .expect("a persistent Warp master is attachable");

    assert_eq!(request.command, COMMAND);
}

#[test]
fn attaches_to_a_user_owned_master() {
    // The user's own master: Warp never created it, and never tears it down.
    let wrapper = IsSSHWrapperSession::Yes {
        socket_path: PathBuf::from(SOCKET),
        external_control_master: true,
        persist: false,
    };

    assert!(
        clone_request(&source(remote(), wrapper), None, true).is_ok(),
        "a master Warp does not own survives the source pane, so a split may attach to it"
    );
}

/// A pane spawned before the feature flag turned on still holds `WARP_SSH_CONTROL_PERSIST=0`,
/// so teardown force-exits its master and a split attached to it would die with the source pane.
#[test]
fn does_not_attach_to_a_master_that_dies_with_the_source_pane() {
    let wrapper = IsSSHWrapperSession::Yes {
        socket_path: PathBuf::from(SOCKET),
        external_control_master: false,
        persist: false,
    };

    assert_eq!(
        clone_request(&source(remote(), wrapper), None, true),
        Err(CloneDeclined::MasterWouldNotOutliveSource)
    );
}

#[test]
fn does_not_attach_to_a_local_session() {
    let source = source(SessionType::Local, warp_persistent_master());

    assert_eq!(
        clone_request(&source, None, true),
        Err(CloneDeclined::NotWarpifiedRemote)
    );
}

/// A session warpified by the RC-file snippet inside an unwrapped `ssh` carries no socket.
#[test]
fn does_not_attach_to_a_session_the_wrapper_did_not_establish() {
    let source = source(remote(), IsSSHWrapperSession::No);

    assert_eq!(
        clone_request(&source, None, true),
        Err(CloneDeclined::NoWrapperSocket)
    );
}

#[test]
fn does_not_attach_without_a_bound_command() {
    let source = SshCloneFacts {
        bound_command: None,
        ..source(remote(), warp_persistent_master())
    };

    assert_eq!(
        clone_request(&source, None, true),
        Err(CloneDeclined::NoBoundCommand)
    );
}

/// The ControlMaster socket lives inside the distro, so no pane outside it can reach the socket.
#[test]
fn does_not_attach_across_wsl_distros() {
    let source = SshCloneFacts {
        wsl_distro: Some("Ubuntu".to_owned()),
        ..source(remote(), warp_persistent_master())
    };

    assert_eq!(
        clone_request(&source, Some("Debian"), true),
        Err(CloneDeclined::WslDistroMismatch)
    );
}

#[test]
fn attaches_within_one_wsl_distro() {
    let source = SshCloneFacts {
        wsl_distro: Some("Ubuntu".to_owned()),
        ..source(remote(), warp_persistent_master())
    };

    assert!(clone_request(&source, Some("Ubuntu"), true).is_ok());
}

#[test]
fn does_not_attach_when_the_feature_is_disabled() {
    let source = source(remote(), warp_persistent_master());

    assert_eq!(
        clone_request(&source, None, false),
        Err(CloneDeclined::Disabled)
    );
}

/// A remote session reports no WSL distro of its own — its `wsl_name` carries the remote host's
/// answer, which a Linux box never gives. The distro therefore has to come from the local pane's
/// shell. Sourcing it from the session instead made this gate unpassable on WSL, and the
/// symmetric cases above could not catch that: they feed both sides the same value.
#[test]
fn does_not_attach_when_the_source_reports_no_distro_but_the_target_is_wsl() {
    let source = source(remote(), warp_persistent_master());
    assert_eq!(source.wsl_distro, None);

    assert_eq!(
        clone_request(&source, Some("Ubuntu"), true),
        Err(CloneDeclined::WslDistroMismatch)
    );
}

/// The mirror of the case above: a local pane outside WSL splitting into a non-WSL pane. Both
/// sides absent must still attach, or the feature is dead everywhere but WSL.
#[test]
fn attaches_when_neither_pane_is_wsl() {
    let source = source(remote(), warp_persistent_master());

    assert!(clone_request(&source, None, true).is_ok());
}

/// The whole point of the phase: the new pane lands where the source pane was, not in the remote
/// The population the fallback rate is about: the user split a warpified SSH pane and got a local
/// one. A pane the feature could never have served still counts — the user experienced the same
/// thing — and stays separable by reason.
#[test]
fn counts_a_refused_ssh_split_as_a_fallback() {
    for declined in [
        CloneDeclined::NoWrapperSocket,
        CloneDeclined::MasterWouldNotOutliveSource,
        CloneDeclined::WslDistroMismatch,
        CloneDeclined::NoBoundCommand,
    ] {
        assert!(declined.is_fallback(), "{declined:?}");
    }
}

/// A split that was never a candidate is not a fallback: counting ordinary local splits, or splits
/// the user opted out of, would drown the rate the feature is judged by.
#[test]
fn does_not_count_a_non_candidate_split_as_a_fallback() {
    for declined in [CloneDeclined::Disabled, CloneDeclined::NotWarpifiedRemote] {
        assert!(!declined.is_fallback(), "{declined:?}");
    }
}

/// Dashboards key on these strings, so a variant rename must not rewrite their history.
#[test]
fn names_each_decline_reason_stably() {
    assert_eq!(CloneDeclined::Disabled.telemetry_reason(), "disabled");
    assert_eq!(
        CloneDeclined::NotWarpifiedRemote.telemetry_reason(),
        "not_warpified_remote"
    );
    assert_eq!(
        CloneDeclined::NoWrapperSocket.telemetry_reason(),
        "no_wrapper_socket"
    );
    assert_eq!(
        CloneDeclined::MasterWouldNotOutliveSource.telemetry_reason(),
        "master_would_not_outlive_source"
    );
    assert_eq!(
        CloneDeclined::WslDistroMismatch.telemetry_reason(),
        "wsl_distro_mismatch"
    );
    assert_eq!(
        CloneDeclined::NoBoundCommand.telemetry_reason(),
        "no_bound_command"
    );
}

/// The gate is the only place the user's setting reaches behavior, so its truth table is pinned
/// here rather than left to the call site. Every condition is necessary: dropping any one of them
/// would clone for people who turned the feature off, or into a pane whose shell cannot attach.
#[test]
fn opens_the_gate_only_when_every_condition_holds() {
    let open = CloneGate {
        feature_flag: true,
        ssh_warpification: true,
        setting: true,
    };

    assert!(open.is_open());

    for (label, gate) in [
        (
            "feature flag off",
            CloneGate {
                feature_flag: false,
                ..open
            },
        ),
        (
            "ssh warpification off",
            CloneGate {
                ssh_warpification: false,
                ..open
            },
        ),
        (
            "setting off",
            CloneGate {
                setting: false,
                ..open
            },
        ),
    ] {
        assert!(!gate.is_open(), "{label} must close the gate");
    }
}

/// The flag is the rollout control, so it has to win even when the user has opted in.
#[test]
fn the_feature_flag_closes_the_gate_over_an_enabled_setting() {
    let gate = CloneGate {
        feature_flag: false,
        ssh_warpification: true,
        setting: true,
    };

    assert!(!gate.is_open());
    assert_eq!(
        clone_request(
            &source(remote(), warp_persistent_master()),
            None,
            gate.is_open()
        ),
        Err(CloneDeclined::Disabled)
    );
}

/// Warpification off means the split's shell never runs `warp_ssh_helper`, so a replayed `ssh`
/// would dial the host itself and prompt for the credentials this feature exists to avoid.
#[test]
fn ssh_warpification_off_closes_the_gate_over_an_enabled_setting() {
    let gate = CloneGate {
        feature_flag: true,
        ssh_warpification: false,
        setting: true,
    };

    assert!(!gate.is_open());
    assert_eq!(
        clone_request(
            &source(remote(), warp_persistent_master()),
            None,
            gate.is_open()
        ),
        Err(CloneDeclined::Disabled)
    );
}

/// `WARP_SSH_CONTROL_PERSIST` changes the lifetime of every Warp SSH connection, so it takes both
/// the flag and the user's setting. Each case names which conjunct it drops, so a rule that
/// silently degenerated to one of them fails here rather than at a user's cost.
#[test]
fn control_persist_requires_both_the_flag_and_the_setting() {
    assert!(
        control_persist_enabled(true, true),
        "flag and setting both on must enable ControlPersist"
    );
    assert!(
        !control_persist_enabled(true, false),
        "the flag alone must not enable ControlPersist for a user who declined the setting"
    );
    assert!(
        !control_persist_enabled(false, true),
        "the setting alone must not enable ControlPersist in a build where the flag is off"
    );
    assert!(
        !control_persist_enabled(false, false),
        "neither on must leave ControlPersist off"
    );
}
