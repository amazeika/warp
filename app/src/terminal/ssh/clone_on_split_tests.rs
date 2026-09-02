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
        clone_request(&source(remote(), wrapper), None, true).is_some(),
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

    assert_eq!(clone_request(&source(remote(), wrapper), None, true), None);
}

#[test]
fn does_not_attach_to_a_local_session() {
    let source = source(SessionType::Local, warp_persistent_master());

    assert_eq!(clone_request(&source, None, true), None);
}

/// A session warpified by the RC-file snippet inside an unwrapped `ssh` carries no socket.
#[test]
fn does_not_attach_to_a_session_the_wrapper_did_not_establish() {
    let source = source(remote(), IsSSHWrapperSession::No);

    assert_eq!(clone_request(&source, None, true), None);
}

#[test]
fn does_not_attach_without_a_bound_command() {
    let source = SshCloneFacts {
        bound_command: None,
        ..source(remote(), warp_persistent_master())
    };

    assert_eq!(clone_request(&source, None, true), None);
}

/// The ControlMaster socket lives inside the distro, so no pane outside it can reach the socket.
#[test]
fn does_not_attach_across_wsl_distros() {
    let source = SshCloneFacts {
        wsl_distro: Some("Ubuntu".to_owned()),
        ..source(remote(), warp_persistent_master())
    };

    assert_eq!(clone_request(&source, Some("Debian"), true), None);
}

#[test]
fn attaches_within_one_wsl_distro() {
    let source = SshCloneFacts {
        wsl_distro: Some("Ubuntu".to_owned()),
        ..source(remote(), warp_persistent_master())
    };

    assert!(clone_request(&source, Some("Ubuntu"), true).is_some());
}

#[test]
fn does_not_attach_when_the_feature_is_disabled() {
    let source = source(remote(), warp_persistent_master());

    assert_eq!(clone_request(&source, None, false), None);
}

/// A remote session reports no WSL distro of its own — its `wsl_name` carries the remote host's
/// answer, which a Linux box never gives. The distro therefore has to come from the local pane's
/// shell. Sourcing it from the session instead made this gate unpassable on WSL, and the
/// symmetric cases above could not catch that: they feed both sides the same value.
#[test]
fn does_not_attach_when_the_source_reports_no_distro_but_the_target_is_wsl() {
    let source = source(remote(), warp_persistent_master());
    assert_eq!(source.wsl_distro, None);

    assert_eq!(clone_request(&source, Some("Ubuntu"), true), None);
}

/// The mirror of the case above: a local pane outside WSL splitting into a non-WSL pane. Both
/// sides absent must still attach, or the feature is dead everywhere but WSL.
#[test]
fn attaches_when_neither_pane_is_wsl() {
    let source = source(remote(), warp_persistent_master());

    assert!(clone_request(&source, None, true).is_some());
}
