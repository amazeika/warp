use std::path::{Path, PathBuf};

use super::*;

fn socket() -> PathBuf {
    PathBuf::from("/tmp/warp-control-master.sock")
}

/// The forced `ssh -O exit` exists to stop the interactive `ssh` -- which is
/// also the multiplex master -- from hanging on remote-side channel cleanup.
/// That is exactly the master Warp created without `ControlPersist`, so it is
/// the one case that must still be stopped.
#[test]
fn non_persistent_warp_managed_master_is_force_exited() {
    let control_path = ControlPath::WarpManaged {
        socket_path: socket(),
        persist: false,
    };

    assert_eq!(
        socket_to_force_exit(&control_path),
        Some(socket().as_path()),
        "a Warp-owned master with no ControlPersist must still get the forced exit"
    );
}

/// A persistent master detached from the foreground `ssh` at connect time, so
/// there is no process left to hang -- and other panes are very likely still
/// multiplexed onto it. Forcing it to exit would kill their connection.
#[test]
fn persistent_warp_managed_master_is_left_running() {
    let control_path = ControlPath::WarpManaged {
        socket_path: socket(),
        persist: true,
    };

    assert_eq!(
        socket_to_force_exit(&control_path),
        None,
        "a persistent master must be left to expire on its idle timeout"
    );
}

/// Warp attached to this master; it belongs to the user and tearing it down
/// would break connections Warp knows nothing about.
#[test]
fn user_owned_master_is_never_force_exited() {
    let control_path = ControlPath::UserOwned(socket());

    assert_eq!(
        socket_to_force_exit(&control_path),
        None,
        "a user-owned master must never be torn down by Warp"
    );
}

#[test]
fn absent_control_master_is_a_no_op() {
    assert_eq!(socket_to_force_exit(&ControlPath::None), None);
}

/// Guards the argument list the forced exit is spawned with: it must address
/// the master by socket alone and never fall back to authenticating.
#[test]
fn ssh_args_target_the_control_socket_without_authenticating() {
    let args = ssh_args(Path::new("/tmp/warp-control-master.sock"));

    assert!(
        args.contains(&"PasswordAuthentication=no".to_string()),
        "expected the multiplexed channel to refuse to authenticate, got {args:?}"
    );
    assert!(
        args.contains(&"ControlPath=/tmp/warp-control-master.sock".to_string()),
        "expected the socket to be addressed by ControlPath, got {args:?}"
    );
}
