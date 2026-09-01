//! Tests for the SSH wrapper's ControlMaster attach mode (GH5409).
//!
//! These live apart from `ssh.rs` deliberately: no test here needs a reachable
//! remote host. Some assert that the wrapper refuses before dialing at all;
//! others deliberately let a plain `ssh` dial an unresolvable name and assert on
//! the failure. The tests in `ssh.rs` tunnel to a GCP VM and cannot run without
//! credentials for it.

use regex::Regex;
use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::subshell::enter_ssh_command;
use warp::integration_testing::terminal::util::{
    ExpectedExitStatus, current_shell_starter_and_version,
};
use warp::integration_testing::terminal::{
    execute_command_for_single_terminal_in_tab, validate_block_output,
    wait_until_bootstrapped_single_pane_for_tab,
};
use warp::integration_testing::view_getters::single_terminal_view_for_tab;
use warp::terminal::model::blocks::BlockFilter;
use warp::terminal::shell::ShellType;
use warpui_core::async_assert;
use warpui_core::integration::AssertionCallback;

use super::new_builder;
use crate::Builder;

/// Asserts the last command block's output does *not* match `pattern`.
pub(super) fn assert_last_command_block_does_not_match(pattern: &'static str) -> AssertionCallback {
    Box::new(move |app, window_id| {
        let regex = Regex::new(pattern).expect("regex should not fail to compile");
        let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
        terminal_view.read(app, |view, _| {
            let model = view.model.lock();
            let output = model
                .block_list()
                .last_matching_block_by_index(BlockFilter::commands())
                .and_then(|index| model.block_list().block_at(index))
                .map(|block| {
                    block
                        .output_grid()
                        .contents_to_string_with_secrets_unobfuscated(
                            false, /*include_escape_sequences*/
                            None,  /*max_rows*/
                        )
                })
                .unwrap_or_default();
            async_assert!(
                !regex.is_match(&output),
                "Expected the attach request to have been consumed, but the output still \
                 matches {pattern:?}: {output}"
            )
        })
    })
}

/// Asserts the last command block's output matches `pattern`.
///
/// The completed `ssh` block is not the *active* block by the time assertions
/// run, because the shell has already opened the next one, so this looks at the
/// last command block instead.
pub(super) fn assert_last_command_block_matches(pattern: &'static str) -> AssertionCallback {
    Box::new(move |app, window_id| {
        let regex = Regex::new(pattern).expect("regex should not fail to compile");
        validate_block_output(
            &regex, 0, /*tab_idx*/
            0, /*pane_idx*/
            window_id, app,
        )
    })
}

/// `WARP_SSH_ATTACH_CONTROL_PATH` pointing at a socket that does not exist must
/// fail closed. Falling through to a fresh ControlMaster would prompt the user
/// for credentials in a pane Warp opened on their behalf, which is exactly what
/// attaching exists to avoid.
pub fn test_ssh_wrapper_attach_fails_closed_on_dead_control_socket() -> Builder {
    new_builder()
        // TODO(CORE-2333) PowerShell has no SSH wrapper.
        .set_should_run_test(|| {
            let (starter, _) = current_shell_starter_and_version();
            starter.shell_type() != ShellType::PowerShell
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            "export WARP_SSH_ATTACH_CONTROL_PATH=/tmp/warp-no-such-control-socket".to_string(),
            ExpectedExitStatus::Success,
            (),
        ))
        .with_step(enter_ssh_command("bash"))
        .with_step(
            new_step_with_default_assertions("Assert the wrapper refused to dial a new connection")
                .add_assertion(assert_last_command_block_matches(
                    r"cannot reuse the SSH connection this pane was split from \(that connection is gone\)",
                )),
        )
}

/// A control path containing characters that cannot be embedded in the SSH hook
/// JSON must be rejected rather than smuggled into the hook, and must likewise
/// fail closed instead of dialing a fresh connection.
pub fn test_ssh_wrapper_attach_rejects_unsupported_control_path_characters() -> Builder {
    new_builder()
        // TODO(CORE-2333) PowerShell has no SSH wrapper.
        .set_should_run_test(|| {
            let (starter, _) = current_shell_starter_and_version();
            starter.shell_type() != ShellType::PowerShell
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            "export WARP_SSH_ATTACH_CONTROL_PATH='/tmp/warp control socket'".to_string(),
            ExpectedExitStatus::Success,
            (),
        ))
        .with_step(enter_ssh_command("bash"))
        .with_step(
            new_step_with_default_assertions("Assert the unsupported control path was rejected")
                .add_assertion(assert_last_command_block_matches(
                    r"its socket path has unsupported characters",
                )),
        )
}

/// The attach request is consumed by the first wrapped `ssh` and must not leak
/// into later ones. Without this, logging out of a split pane and running
/// `ssh some-other-host` in it would silently multiplex onto the *original*
/// host's connection, landing the user on a machine they did not ask for.
pub fn test_ssh_wrapper_attach_request_is_one_shot() -> Builder {
    // A host that cannot resolve. The first `ssh` never reaches DNS, because the
    // wrapper fails closed before dialing. The second is expected to fail while
    // connecting, which is the evidence we want: it dialed normally instead of
    // reusing the attach request.
    const UNRESOLVABLE: &str = "ssh warp-attach-test-invalid-host.invalid";

    new_builder()
        // TODO(CORE-2333) PowerShell has no SSH wrapper.
        .set_should_run_test(|| {
            let (starter, _) = current_shell_starter_and_version();
            starter.shell_type() != ShellType::PowerShell
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            "export WARP_SSH_ATTACH_CONTROL_PATH=/tmp/warp-no-such-control-socket".to_string(),
            ExpectedExitStatus::Success,
            (),
        ))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            UNRESOLVABLE.to_string(),
            ExpectedExitStatus::Failure,
            (),
        ))
        .with_step(
            new_step_with_default_assertions("Assert the first attempt consumed the request")
                .add_assertion(assert_last_command_block_matches(
                    r"cannot reuse the SSH connection this pane was split from \(that connection is gone\)",
                )),
        )
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            UNRESOLVABLE.to_string(),
            ExpectedExitStatus::Failure,
            (),
        ))
        .with_step(
            new_step_with_default_assertions(
                "Assert the second attempt dialed normally instead of reusing the request",
            )
            .add_assertion(assert_last_command_block_does_not_match(
                r"cannot reuse the SSH connection this pane was split from \(that connection is gone\)",
            ))
            // Deliberately not asserting a resolution error specifically: a
            // resolver that hijacks NXDOMAIN answers `.invalid` and ssh fails
            // while connecting instead. Either way it dialed, which is the
            // point.
            .add_assertion(assert_last_command_block_matches(r"ssh: ")),
        )
}

/// The attach request must be consumed before *any* path that falls back to
/// plain `ssh`. `warp_ssh_helper` returns early when the destination configures
/// its own `RemoteCommand`, because OpenSSH cannot then also run Warp's
/// bootstrap. If that path ran plain `ssh` while an attach request was pending,
/// it would dial a fresh connection and prompt for the credentials the split
/// exists to avoid. It would also leave the request set for the next `ssh` in
/// the pane.
pub fn test_ssh_wrapper_attach_fails_closed_on_early_return_path() -> Builder {
    const FORCES_EARLY_RETURN: &str =
        "ssh -o RemoteCommand=true warp-attach-test-invalid-host.invalid";

    new_builder()
        // TODO(CORE-2333) PowerShell has no SSH wrapper.
        .set_should_run_test(|| {
            let (starter, _) = current_shell_starter_and_version();
            starter.shell_type() != ShellType::PowerShell
        })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            "export WARP_SSH_ATTACH_CONTROL_PATH=/tmp/warp-no-such-control-socket".to_string(),
            ExpectedExitStatus::Success,
            (),
        ))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            FORCES_EARLY_RETURN.to_string(),
            ExpectedExitStatus::Failure,
            (),
        ))
        .with_step(
            new_step_with_default_assertions(
                "Assert the RemoteCommand fallback refused rather than dialing",
            )
            .add_assertion(assert_last_command_block_matches(
                r"cannot reuse the SSH connection this pane was split from",
            )),
        )
        // The request must also have been consumed on that path, so a later
        // ssh in this pane dials normally instead of reusing the socket.
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            "ssh warp-attach-test-invalid-host.invalid".to_string(),
            ExpectedExitStatus::Failure,
            (),
        ))
        .with_step(
            new_step_with_default_assertions("Assert the request did not survive the early return")
                .add_assertion(assert_last_command_block_does_not_match(
                    r"cannot reuse the SSH connection this pane was split from",
                ))
                .add_assertion(assert_last_command_block_matches(r"ssh: ")),
        )
}
