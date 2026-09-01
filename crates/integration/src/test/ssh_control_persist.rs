//! Tests for `ControlPersist` on Warp-owned ControlMasters (GH5409).
//!
//! Like the attach-mode tests next door, none of these needs a reachable
//! remote host: they assert on what the wrapper *emits*, not on what a host
//! does with it. A stub `ssh` earlier on `PATH` prints the ControlMaster
//! options it was handed and the `persist` field of the SSH hook embedded in
//! the remote bootstrap, which is exactly the contract this phase changes.

use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::terminal::util::{
    ExpectedExitStatus, current_shell_starter_and_version,
};
use warp::integration_testing::terminal::{
    execute_command_for_single_terminal_in_tab, wait_until_bootstrapped_single_pane_for_tab,
};
use warp::terminal::shell::ShellType;

use super::new_builder;
use super::ssh_control_master_attach::{
    assert_last_command_block_does_not_match, assert_last_command_block_matches,
};
use crate::Builder;

/// The stub `ssh`. Deliberately written without a single quote anywhere: the
/// installer below passes each line as a single-quoted argument, and the three
/// shells disagree about how (or whether) a single quote can be escaped inside
/// one.
///
/// `-G` answers the wrapper's `RemoteCommand` probe. `-O` answers its
/// `ssh -O check` liveness probe, and is the one line that differs between the
/// two stubs. Everything else prints what the wrapper asked for.
fn fake_ssh_lines(control_check_succeeds: bool) -> Vec<String> {
    let control_check = if control_check_succeeds { 0 } else { 1 };
    vec![
        "#!/bin/sh".to_string(),
        "if [ \"$1\" = \"-G\" ]; then echo \"remotecommand none\"; exit 0; fi".to_string(),
        format!("if [ \"$1\" = \"-O\" ]; then exit {control_check}; fi"),
        "for a in \"$@\"; do".to_string(),
        "  case \"$a\" in".to_string(),
        "    ControlMaster=*|ControlPath=*|ControlPersist=*) echo \"FAKESSH_OPT $a\" ;;".to_string(),
        "  esac".to_string(),
        "done".to_string(),
        // The remote bootstrap script is the last argument. The SSH hook it
        // prints back carries `persist`, so grepping the script is the only way
        // to see what the session will report without a host to run it.
        "last=".to_string(),
        "for a in \"$@\"; do last=\"$a\"; done".to_string(),
        // Anchored on the JSON key, not the bare word: the control path is on
        // the same line, and a socket path containing "persist" would otherwise
        // match first. The `.` stands in for the backslash the hook's escaped
        // quote carries.
        "printf \"%s\" \"$last\" | grep -o \"persist.\\\": [a-z]*\" | head -1 | sed \"s/^/FAKESSH_HOOK /\""
            .to_string(),
        "exit 0".to_string(),
    ]
}

/// Writes the stub into `dir` and puts `dir` first on `PATH`. The wrapper calls
/// `command ssh`, which skips shell functions but still resolves through
/// `PATH`.
fn install_fake_ssh(dir: &str, control_check_succeeds: bool) -> String {
    let lines = fake_ssh_lines(control_check_succeeds)
        .into_iter()
        .map(|line| format!("'{line}'"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("mkdir -p {dir} && printf '%s\\n' {lines} > {dir}/ssh && chmod +x {dir}/ssh")
}

/// `export NAME=value` in every shell that has it, `set -x` in fish.
fn export_command(name: &str, value: &str) -> String {
    let (starter, _) = current_shell_starter_and_version();
    match starter.shell_type() {
        ShellType::Fish => format!("set -x {name} {value}"),
        _ => format!("export {name}={value}"),
    }
}

fn prepend_to_path(dir: &str) -> String {
    let (starter, _) = current_shell_starter_and_version();
    match starter.shell_type() {
        ShellType::Fish => format!("set -x PATH {dir} $PATH"),
        _ => format!("export PATH={dir}:$PATH"),
    }
}

fn should_run() -> bool {
    // TODO(CORE-2333) PowerShell has no SSH wrapper.
    let (starter, _) = current_shell_starter_and_version();
    starter.shell_type() != ShellType::PowerShell
}

/// Sets up a pane with the stub `ssh` installed and `WARP_SSH_CONTROL_PERSIST`
/// set, then runs one wrapped `ssh`.
fn wrapped_ssh_with_stub(
    dir: &str,
    persist_enabled: bool,
    control_check_succeeds: bool,
) -> Builder {
    new_builder()
        .set_should_run_test(should_run)
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            install_fake_ssh(dir, control_check_succeeds),
            ExpectedExitStatus::Success,
            (),
        ))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            prepend_to_path(dir),
            ExpectedExitStatus::Success,
            (),
        ))
        .with_step(execute_command_for_single_terminal_in_tab(
            0,
            export_command(
                "WARP_SSH_CONTROL_PERSIST",
                if persist_enabled { "1" } else { "0" },
            ),
            ExpectedExitStatus::Success,
            (),
        ))
}

/// With the feature on, a master Warp creates outlives the foreground `ssh`
/// that created it, and the session reports that so teardown knows to skip the
/// forced `ssh -O exit`.
pub fn test_ssh_wrapper_persist_adds_control_persist_when_enabled() -> Builder {
    wrapped_ssh_with_stub(
        "/tmp/warp-persist-on",
        true,  /*persist_enabled*/
        false, /*control_check_succeeds*/
    )
    .with_step(execute_command_for_single_terminal_in_tab(
        0,
        "ssh warp-persist-test-host".to_string(),
        ExpectedExitStatus::Success,
        (),
    ))
    .with_step(
        new_step_with_default_assertions("Assert the Warp-owned master persists")
            .add_assertion(assert_last_command_block_matches(
                r"FAKESSH_OPT ControlMaster=yes",
            ))
            .add_assertion(assert_last_command_block_matches(
                r"FAKESSH_OPT ControlPersist=60",
            ))
            .add_assertion(assert_last_command_block_matches(
                r"FAKESSH_HOOK persist.*true",
            )),
    )
}

/// With the feature off, master lifetime and teardown must be exactly what they
/// were before this feature existed: no `ControlPersist`, and a session that
/// reports `persist: false` so the forced `ssh -O exit` still runs.
pub fn test_ssh_wrapper_persist_omitted_when_disabled() -> Builder {
    wrapped_ssh_with_stub(
        "/tmp/warp-persist-off",
        false, /*persist_enabled*/
        false, /*control_check_succeeds*/
    )
    .with_step(execute_command_for_single_terminal_in_tab(
        0,
        "ssh warp-persist-test-host".to_string(),
        ExpectedExitStatus::Success,
        (),
    ))
    .with_step(
        new_step_with_default_assertions("Assert no ControlPersist is added with the feature off")
            .add_assertion(assert_last_command_block_matches(
                r"FAKESSH_OPT ControlMaster=yes",
            ))
            .add_assertion(assert_last_command_block_does_not_match(r"ControlPersist"))
            .add_assertion(assert_last_command_block_matches(
                r"FAKESSH_HOOK persist.*false",
            )),
    )
}

/// An attaching session joined a master it did not create. Extending that
/// master's lifetime is not its call, and neither is tearing it down -- so it
/// adds no `ControlPersist` and reports `persist: false`, even with the feature
/// on.
pub fn test_ssh_wrapper_persist_not_added_when_attaching() -> Builder {
    wrapped_ssh_with_stub(
        "/tmp/warp-persist-attach",
        true, /*persist_enabled*/
        true, /*control_check_succeeds*/
    )
    .with_step(execute_command_for_single_terminal_in_tab(
        0,
        export_command(
            "WARP_SSH_ATTACH_CONTROL_PATH",
            "/tmp/warp-persist-attach/socket",
        ),
        ExpectedExitStatus::Success,
        (),
    ))
    .with_step(execute_command_for_single_terminal_in_tab(
        0,
        "ssh warp-persist-test-host".to_string(),
        ExpectedExitStatus::Success,
        (),
    ))
    .with_step(
        new_step_with_default_assertions("Assert an attached master is not given ControlPersist")
            .add_assertion(assert_last_command_block_matches(
                r"FAKESSH_OPT ControlMaster=no",
            ))
            .add_assertion(assert_last_command_block_does_not_match(r"ControlPersist"))
            .add_assertion(assert_last_command_block_matches(
                r"FAKESSH_HOOK persist.*false",
            )),
    )
}
