use super::WarpifyState;
use crate::terminal::model::session::{BootstrapSessionType, SessionId};

const SSH_COMMAND: &str = "ssh build-host";

const REMOTE: BootstrapSessionType = BootstrapSessionType::WarpifiedRemote;
const LOCAL: BootstrapSessionType = BootstrapSessionType::Local;

const WRAPPER: bool = true;
const NOT_WRAPPER: bool = false;

fn session(id: u64) -> SessionId {
    SessionId::from(id)
}

/// Warpify detection stores the command it validated, alias-expanded, before the session
/// bootstraps. That pending command is what a binding is made of.
fn with_pending_ssh_command(state: &mut WarpifyState, command: &str) {
    state.set_pending_ssh_host(command.to_string(), Some("build-host".to_string()));
}

/// A Warpified remote session established by the Warp SSH wrapper — the only kind a split can
/// attach to, and the only kind that binds.
fn state_bound_to_ssh_session() -> WarpifyState {
    let mut state = WarpifyState::default();
    with_pending_ssh_command(&mut state, SSH_COMMAND);
    state.bind_ssh_command(session(1), &REMOTE, WRAPPER);
    state
}

#[test]
fn test_the_ssh_command_is_bound_when_a_wrapper_established_remote_session_starts() {
    let state = state_bound_to_ssh_session();

    assert_eq!(
        state.bound_ssh_command(Some(session(1))),
        Some(SSH_COMMAND),
        "the session that just started should be handed the command that started it"
    );
}

#[test]
fn test_no_command_is_bound_for_local_subshell_warpification() {
    let mut state = WarpifyState::default();

    with_pending_ssh_command(&mut state, "nix develop");
    state.bind_ssh_command(session(1), &LOCAL, NOT_WRAPPER);

    assert_eq!(state.bound_ssh_command(Some(session(1))), None);
}

#[test]
fn test_no_command_is_bound_for_a_remote_session_the_wrapper_did_not_establish() {
    let mut state = WarpifyState::default();

    // Session type is decided by hostname, so a subshell warpified on the remote host is typed
    // remote too — but it has no Warp ControlMaster, and `nix develop` is no ssh command.
    with_pending_ssh_command(&mut state, "nix develop");
    state.bind_ssh_command(session(1), &REMOTE, NOT_WRAPPER);

    assert_eq!(state.bound_ssh_command(Some(session(1))), None);
}

#[test]
fn test_a_subshell_on_the_remote_host_leaves_the_ssh_binding_intact() {
    let mut state = state_bound_to_ssh_session();

    state.bind_ssh_command(session(2), &REMOTE, NOT_WRAPPER);

    assert_eq!(
        state.bound_ssh_command(Some(session(2))),
        None,
        "the subshell has no ssh command of its own to hand out"
    );
    assert_eq!(
        state.bound_ssh_command(Some(session(1))),
        Some(SSH_COMMAND),
        "the outer ssh session must keep its command while the subshell runs"
    );
}

#[test]
fn test_the_ssh_binding_survives_a_subshell_on_the_remote_host_exiting() {
    let mut state = state_bound_to_ssh_session();
    state.bind_ssh_command(session(2), &REMOTE, NOT_WRAPPER);

    state.release_ssh_command(session(2));

    assert_eq!(
        state.bound_ssh_command(Some(session(1))),
        Some(SSH_COMMAND),
        "leaving the subshell should return to a session that still knows its ssh command"
    );
}

#[test]
fn test_the_command_is_withheld_from_a_different_active_session() {
    let state = state_bound_to_ssh_session();

    assert_eq!(state.bound_ssh_command(Some(session(2))), None);
}

#[test]
fn test_the_command_is_withheld_when_there_is_no_active_session() {
    let state = state_bound_to_ssh_session();

    assert_eq!(state.bound_ssh_command(None), None);
}

#[test]
fn test_an_unbound_state_reports_no_command() {
    let state = WarpifyState::default();

    assert_eq!(state.bound_ssh_command(Some(session(1))), None);
}

#[test]
fn test_a_further_ssh_command_inside_the_session_leaves_the_binding_intact() {
    let mut state = state_bound_to_ssh_session();

    // An `ssh` typed inside the remote session is a Warpification *candidate*: it lands in the
    // pending trigger state, and binds a command only if it goes on to Warpify.
    state.set_pending_ssh_host("ssh other-host".to_string(), Some("other-host".to_string()));

    assert_eq!(state.bound_ssh_command(Some(session(1))), Some(SSH_COMMAND));
}

#[test]
fn test_the_binding_is_released_when_its_session_ends() {
    let mut state = state_bound_to_ssh_session();

    state.release_ssh_command(session(1));

    assert_eq!(state.bound_ssh_command(Some(session(1))), None);
}

#[test]
fn test_another_session_ending_leaves_the_binding_intact() {
    let mut state = state_bound_to_ssh_session();

    state.release_ssh_command(session(2));

    assert_eq!(state.bound_ssh_command(Some(session(1))), Some(SSH_COMMAND));
}

#[test]
fn test_the_bound_command_is_the_validated_pending_one_not_the_text_as_typed() {
    let mut state = WarpifyState::default();
    // Warpify detection runs on the alias-expanded form, so that is what it stores. `m` alone
    // would not survive `parse_interactive_ssh_command` on the way back out.
    with_pending_ssh_command(&mut state, "ssh -J bastion mini");

    state.bind_ssh_command(session(1), &REMOTE, WRAPPER);

    assert_eq!(
        state.bound_ssh_command(Some(session(1))),
        Some("ssh -J bastion mini")
    );
}

#[test]
fn test_nothing_is_bound_without_a_validated_pending_command() {
    let mut state = WarpifyState::default();

    state.bind_ssh_command(session(1), &REMOTE, WRAPPER);

    assert_eq!(
        state.bound_ssh_command(Some(session(1))),
        None,
        "an unvalidated command is not replayable, so the split must fall back to a local pane"
    );
}

#[test]
fn test_pane_teardown_releases_the_binding() {
    let mut state = state_bound_to_ssh_session();

    state.release_bound_ssh_command();

    assert_eq!(state.bound_ssh_command(Some(session(1))), None);
}
