use std::time::Duration;

use instant::Instant;

use super::*;

fn machine() -> StateMachine {
    StateMachine::new(RestartPolicy::default())
}

fn crashed() -> StopReason {
    StopReason::Crashed {
        detail: "exited with status 1".to_owned(),
    }
}

#[test]
fn a_validated_extension_walks_the_documented_states() {
    let mut machine = machine();
    assert_eq!(machine.state(), &ExtensionState::Discovered);

    machine.validated();
    assert_eq!(machine.state(), &ExtensionState::Inactive);

    assert!(machine.start());
    assert_eq!(machine.state(), &ExtensionState::Starting);

    machine.running();
    assert!(machine.state().is_running());

    machine.stopping();
    assert_eq!(machine.state(), &ExtensionState::Stopping);

    let decision = machine.stopped(StopReason::Requested, Instant::now());
    assert_eq!(decision, RestartDecision::StayInactive);
    assert_eq!(machine.state(), &ExtensionState::Inactive);
}

#[test]
fn an_invalid_manifest_keeps_the_extension_from_ever_starting() {
    let mut machine = machine();
    machine.invalidate("api_version 99 is not supported");
    assert!(machine.state().needs_user_action());

    machine.validated();
    assert!(
        matches!(machine.state(), ExtensionState::Invalid { .. }),
        "validation cannot clear an invalid manifest"
    );
    assert!(!machine.start());
}

#[test]
fn deactivation_is_not_counted_as_a_failure() {
    let mut machine = machine();
    machine.validated();
    for _ in 0..10 {
        assert!(machine.start());
        machine.running();
        assert_eq!(
            machine.stopped(StopReason::Deactivated, Instant::now()),
            RestartDecision::StayInactive
        );
    }
    assert_eq!(machine.state(), &ExtensionState::Inactive);
}

#[test]
fn an_unexpected_exit_is_restartable_until_the_budget_runs_out() {
    let mut machine = machine();
    machine.validated();
    let start = Instant::now();

    assert_eq!(machine.stopped(crashed(), start), RestartDecision::Restart);
    assert_eq!(
        machine.stopped(crashed(), start + Duration::from_secs(1)),
        RestartDecision::Restart
    );

    let decision = machine.stopped(crashed(), start + Duration::from_secs(2));
    let RestartDecision::GiveUp { reason } = decision else {
        panic!("the third failure inside the window must give up");
    };
    assert!(reason.contains("3 times"), "{reason}");
    assert!(matches!(machine.state(), ExtensionState::Failed { .. }));
    assert!(
        !machine.start(),
        "a failed extension is not restarted automatically"
    );
}

#[test]
fn failures_outside_the_window_do_not_accumulate() {
    let mut machine = machine();
    machine.validated();
    let mut now = Instant::now();

    for _ in 0..10 {
        assert_eq!(
            machine.stopped(crashed(), now),
            RestartDecision::Restart,
            "an isolated failure must stay restartable"
        );
        now += DEFAULT_FAILURE_WINDOW + Duration::from_secs(1);
    }
}

#[test]
fn a_manual_restart_gives_a_failed_extension_a_fresh_budget() {
    let mut machine = machine();
    machine.validated();
    let start = Instant::now();
    for offset in 0..DEFAULT_MAX_FAILURES {
        machine.stopped(crashed(), start + Duration::from_secs(offset as u64));
    }
    assert!(matches!(machine.state(), ExtensionState::Failed { .. }));

    assert!(machine.manual_restart());
    assert_eq!(machine.state(), &ExtensionState::Starting);
    assert_eq!(
        machine.stopped(crashed(), start + Duration::from_secs(10)),
        RestartDecision::Restart
    );
}

#[test]
fn a_manual_restart_cannot_revive_an_invalid_manifest() {
    let mut machine = machine();
    machine.invalidate("malformed manifest");
    assert!(!machine.manual_restart());
    assert!(matches!(machine.state(), ExtensionState::Invalid { .. }));
}

#[test]
fn a_start_timeout_counts_against_the_budget() {
    assert!(StopReason::StartTimeout.is_failure());
    assert!(
        StopReason::ProtocolViolation {
            detail: "flooding".to_owned()
        }
        .is_failure()
    );
    assert!(!StopReason::Requested.is_failure());
}
